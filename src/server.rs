use crate::board::{Board, BoardEvent, BoardEventType, SharedState, Snapshot, sanitize_filename, validate_board_name};
use pyo3::Python;
use crate::python;
use crate::render;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::Utc;
use rmcp::ServerHandler;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::schemars;
use rmcp::{tool, tool_handler, tool_router};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WhiteboardParams {
    /// Name of the board (creates new if doesn't exist)
    pub name: String,
    /// Python code to execute. Call svg('<svg>...</svg>') to set SVG content.
    /// Variables persist across calls to the same board.
    /// Available: math, random, json, re, textwrap, itertools, functools,
    /// collections, colorsys, hashlib, string, dataclasses.
    /// WIDTH and HEIGHT are preset to board dimensions.
    pub code: String,
    /// Board width in pixels (default 800)
    pub width: Option<u32>,
    /// Board height in pixels (default 600)
    pub height: Option<u32>,
}

#[derive(Clone)]
pub struct ScryServer {
    tool_router: ToolRouter<Self>,
    state: SharedState,
}

#[tool_router]
impl ScryServer {
    pub fn new(state: SharedState) -> Self {
        let tool_router = Self::tool_router();
        Self { tool_router, state }
    }

    #[tool(
        name = "whiteboard",
        description = "Execute Python code to generate SVG visuals on a named board. Call svg('<svg>...</svg>') in your code to set the board's SVG content, which gets rendered to PNG automatically. Variables persist between calls to the same board. Returns the rendered PNG image and a gallery URL."
    )]
    // NOTE: tool description above is static; actual response adapts based on --port/--output-dir
    async fn whiteboard(
        &self,
        Parameters(params): Parameters<WhiteboardParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let name = params.name;
        let code = params.code;
        let w = params.width.unwrap_or(800);
        let h = params.height.unwrap_or(600);

        // Validate inputs
        if let Err(msg) = validate_board_name(&name) {
            return Ok(CallToolResult::error(vec![Content::text(msg)]));
        }
        if w == 0 || h == 0 {
            return Ok(CallToolResult::error(vec![Content::text(
                "Width and height must be greater than zero",
            )]));
        }
        if w > 8192 || h > 8192 {
            return Ok(CallToolResult::error(vec![Content::text(
                "Width and height must be at most 8192",
            )]));
        }
        const MAX_CODE_LEN: usize = 1_000_000; // 1 MB
        if code.len() > MAX_CODE_LEN {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Code too large ({} bytes, max {MAX_CODE_LEN})",
                code.len()
            ))]));
        }

        // Get or create namespace atomically under write lock to prevent
        // TOCTOU race where two concurrent requests for a new board both
        // create independent namespaces.
        //
        // Py<PyDict>::clone requires the thread to be attached to the Python
        // interpreter, so we must do it inside Python::attach.
        let (namespace, is_new_board) = {
            let mut boards = self.state.boards.write().await;
            if boards.contains_key(&name) {
                let ns = Python::attach(|py| boards.get(&name).unwrap().namespace.clone_ref(py));
                (ns, false)
            } else {
                // Create namespace and placeholder board under the lock
                let ns = python::create_namespace_async(w, h)
                    .await
                    .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
                let ns_copy = Python::attach(|py| ns.clone_ref(py));
                let now = Utc::now();
                boards.insert(
                    name.clone(),
                    Board {
                        name: name.clone(),
                        width: w,
                        height: h,
                        svg: String::new(),
                        png: Vec::new(),
                        namespace: ns,
                        created_at: now,
                        updated_at: now,
                        history: Vec::new(),
                    },
                );
                (ns_copy, true)
            }
        };

        // Execute Python code
        let (result, namespace) = match python::run_python(namespace, code, w, h).await {
            Ok(r) => r,
            Err(e) => {
                // Python errors → CallToolResult::error so the model sees the traceback
                return Ok(CallToolResult::error(vec![Content::text(e.to_string())]));
            }
        };

        // If no SVG was produced, return stdout-only result
        let svg_content = match result.svg_content {
            Some(svg) => svg,
            None => {
                let mut msg =
                    String::from("Code executed successfully but svg() was not called.\n");
                if !result.stdout.is_empty() {
                    msg.push_str("\n--- stdout ---\n");
                    msg.push_str(&result.stdout);
                }
                // Save updated namespace back to board
                let mut boards = self.state.boards.write().await;
                if let Some(board) = boards.get_mut(&name) {
                    board.namespace = namespace;
                    board.updated_at = Utc::now();
                }
                return Ok(CallToolResult::success(vec![Content::text(msg)]));
            }
        };

        // Render SVG to PNG
        let png_bytes = match render::svg_to_png(&svg_content) {
            Ok(png) => png,
            Err(e) => {
                // Render errors are also tool-level so the model can fix its SVG
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "SVG render failed: {e}"
                ))]));
            }
        };

        let png_base64 = BASE64.encode(&png_bytes);

        // Clone bytes for file output before the board lock takes ownership
        let png_for_file = if self.state.output_dir.is_some() {
            Some(png_bytes.clone())
        } else {
            None
        };

        // Store results in board (board always exists — created in get-or-create above)
        let now = Utc::now();
        {
            let mut boards = self.state.boards.write().await;
            if let Some(board) = boards.get_mut(&name) {
                if !board.svg.is_empty() {
                    const MAX_HISTORY: usize = 50;
                    if board.history.len() >= MAX_HISTORY {
                        board.history.remove(0);
                    }
                    board.history.push(Snapshot {
                        svg: board.svg.clone(),
                        png: board.png.clone(),
                        timestamp: board.updated_at,
                    });
                }
                board.svg = svg_content.clone();
                board.png = png_bytes;
                board.namespace = namespace;
                board.width = w;
                board.height = h;
                board.updated_at = now;
            }
        }

        // Broadcast event
        let event_type = if is_new_board {
            BoardEventType::Created
        } else {
            BoardEventType::Updated
        };
        let _ = self.state.event_tx.send(BoardEvent {
            board_name: name.clone(),
            event_type,
        });

        // Write files to output_dir if configured (best-effort)
        let mut png_path = None;
        let mut svg_path = None;
        if let Some(ref dir) = self.state.output_dir {
            let safe_name = sanitize_filename(&name);
            let png_file = dir.join(format!("{safe_name}.png"));
            let svg_file = dir.join(format!("{safe_name}.svg"));
            match std::fs::write(&png_file, png_for_file.as_ref().unwrap()) {
                Ok(()) => png_path = Some(png_file),
                Err(e) => tracing::warn!("Failed to write {}: {e}", png_file.display()),
            }
            match std::fs::write(&svg_file, &svg_content) {
                Ok(()) => svg_path = Some(svg_file),
                Err(e) => tracing::warn!("Failed to write {}: {e}", svg_file.display()),
            }
        }

        // Build response
        let svg_snippet = if svg_content.len() > 200 {
            let mut end = 200;
            while end > 0 && !svg_content.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &svg_content[..end])
        } else {
            svg_content
        };

        let mut header = format!("Board: {name}\nSize: {w}x{h}");
        if let Some(url) = self.state.board_url(&name) {
            header.push_str(&format!("\nURL: {url}"));
        }
        if let Some(ref p) = png_path {
            header.push_str(&format!("\nPNG: {}", p.display()));
        }
        if let Some(ref p) = svg_path {
            header.push_str(&format!("\nSVG: {}", p.display()));
        }

        let mut text_parts = vec![header];
        if !result.stdout.is_empty() {
            text_parts.push(format!("--- stdout ---\n{}", result.stdout));
        }
        text_parts.push(format!("--- SVG (snippet) ---\n{svg_snippet}"));

        Ok(CallToolResult::success(vec![
            Content::image(png_base64, "image/png"),
            Content::text(text_parts.join("\n\n")),
        ]))
    }

    #[tool(
        name = "whiteboard_list",
        description = "List all active boards with their thumbnails, URLs, and metadata."
    )]
    async fn whiteboard_list(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        // Collect data under read lock, release before base64 encoding
        struct BoardSummary {
            name: String,
            url: Option<String>,
            width: u32,
            height: u32,
            created: String,
            updated: String,
            history_len: usize,
            png: Vec<u8>,
        }

        let board_data: Vec<BoardSummary> = {
            let boards = self.state.boards.read().await;
            if boards.is_empty() {
                return Ok(CallToolResult::success(vec![Content::text(
                    "No boards yet. Use the whiteboard tool to create one.",
                )]));
            }
            let mut list: Vec<_> = boards.values().collect();
            list.sort_by_key(|b| b.created_at);
            list.into_iter()
                .map(|b| BoardSummary {
                    name: b.name.clone(),
                    url: self.state.board_url(&b.name),
                    width: b.width,
                    height: b.height,
                    created: b.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                    updated: b.updated_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                    history_len: b.history.len(),
                    png: b.png.clone(),
                })
                .collect()
        }; // read lock released

        let mut content = Vec::new();
        for b in board_data {
            let mut info = format!(
                "Board: {}\nSize: {}x{}\nCreated: {}\nUpdated: {}\nHistory: {} snapshots",
                b.name, b.width, b.height, b.created, b.updated, b.history_len,
            );
            if let Some(ref url) = b.url {
                info.push_str(&format!("\nURL: {url}"));
            }
            content.push(Content::text(info));
            if !b.png.is_empty() {
                content.push(Content::image(BASE64.encode(&b.png), "image/png"));
            }
        }

        Ok(CallToolResult::success(content))
    }
}

#[tool_handler]
impl ServerHandler for ScryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_03_26,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "scry-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: Some("Scry — Computational Scrying Glass".into()),
                description: Some("MCP server for generating SVG visuals via Python code with live web gallery".into()),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Scry: computational scrying glass. Use 'whiteboard' to execute Python code \
                 that generates SVG visuals. Call svg('<svg>...</svg>') in your code to render. \
                 Variables persist per board."
                    .into(),
            ),
        }
    }
}
