import "jsr:@supabase/functions-js/edge-runtime.d.ts";
import { McpServer } from "npm:@modelcontextprotocol/sdk@1.25.3/server/mcp.js";
import { WebStandardStreamableHTTPServerTransport } from "npm:@modelcontextprotocol/sdk@1.25.3/server/webStandardStreamableHttp.js";
import { z } from "npm:zod@^3.25.63";
import { createClient } from "npm:@supabase/supabase-js@2";
import { execute as rhaiExecute } from "./rhai_wasm.ts";

// ---------------------------------------------------------------------------
// Supabase client (uses auto-injected env vars, service role bypasses RLS)
// ---------------------------------------------------------------------------
const supabase = createClient(
  Deno.env.get("SUPABASE_URL")!,
  Deno.env.get("SUPABASE_SERVICE_ROLE_KEY")!,
);

// ---------------------------------------------------------------------------
// Board name validation — ported from board.rs:54-68
// ---------------------------------------------------------------------------
const MAX_NAME_LEN = 128;

function validateBoardName(name: string): string | null {
  if (!name) return "Board name cannot be empty";
  if (new TextEncoder().encode(name).length > MAX_NAME_LEN) {
    return `Board name too long (max ${MAX_NAME_LEN} bytes)`;
  }
  if (/[\/\0\n\r]/.test(name)) {
    return "Board name cannot contain /, null, or newline characters";
  }
  if (name.startsWith(".") || name.startsWith(" ") || name.endsWith(" ")) {
    return "Board name cannot start with '.' or have leading/trailing spaces";
  }
  return null;
}

// ---------------------------------------------------------------------------
// MCP server factory — creates a fresh server per request to avoid transport
// conflicts with concurrent invocations.
// ---------------------------------------------------------------------------
function createMcpServer(): McpServer {
  const server = new McpServer({
    name: "scry-mcp",
    version: "0.2.0",
  });

  // =========================================================================
  // whiteboard tool
  // =========================================================================
  server.registerTool(
    "whiteboard",
    {
      title: "Whiteboard",
      description:
        "Execute Rhai code to generate SVG visuals on a named board. " +
        "Call svg('<svg>...</svg>') in your code to set the board's SVG content. " +
        "Variables persist between calls to the same board. " +
        "Available: math functions (sin, cos, sqrt, etc.), print(), string interpolation. " +
        "WIDTH and HEIGHT are preset to board dimensions.",
      inputSchema: {
        name: z.string().describe("Name of the board (creates new if doesn't exist)"),
        code: z.string().describe(
          "Rhai code to execute. Call svg(`<svg>...</svg>`) to set SVG content. " +
            "Variables persist across calls to the same board. " +
            "WIDTH and HEIGHT are preset to board dimensions.",
        ),
        width: z.number().int().optional().describe("Board width in pixels (default 800)"),
        height: z.number().int().optional().describe("Board height in pixels (default 600)"),
      },
    },
    async ({ name, code, width, height }) => {
      const w = width ?? 800;
      const h = height ?? 600;

      // Validate inputs
      const nameErr = validateBoardName(name);
      if (nameErr) {
        return { content: [{ type: "text" as const, text: nameErr }], isError: true };
      }
      if (w <= 0 || h <= 0) {
        return {
          content: [{ type: "text" as const, text: "Width and height must be greater than zero" }],
          isError: true,
        };
      }
      if (w > 8192 || h > 8192) {
        return {
          content: [{ type: "text" as const, text: "Width and height must be at most 8192" }],
          isError: true,
        };
      }
      const MAX_CODE_LEN = 1_000_000;
      if (code.length > MAX_CODE_LEN) {
        return {
          content: [
            {
              type: "text" as const,
              text: `Code too large (${code.length} bytes, max ${MAX_CODE_LEN})`,
            },
          ],
          isError: true,
        };
      }

      // Load existing board or prepare defaults
      const { data: existing } = await supabase
        .from("boards")
        .select("name, namespace, width, height, svg, share_id")
        .eq("name", name)
        .maybeSingle();

      const scopeJson = existing?.namespace ? JSON.stringify(existing.namespace) : "{}";

      // Execute Rhai code via Wasm
      const resultJson = rhaiExecute(code, scopeJson, BigInt(w), BigInt(h));
      let result: {
        svg: string | null;
        stdout: string;
        scope: string;
        error: string | null;
      };
      try {
        result = JSON.parse(resultJson);
      } catch {
        return {
          content: [
            { type: "text" as const, text: "Internal error: failed to parse Rhai result" },
          ],
          isError: true,
        };
      }

      // If Rhai errored, return the error to the model
      if (result.error) {
        let msg = "";
        if (result.stdout) {
          msg += `--- stdout ---\n${result.stdout}\n--- error ---\n`;
        }
        msg += result.error;
        return { content: [{ type: "text" as const, text: msg }], isError: true };
      }

      // Parse the updated scope for persistence
      let newNamespace: Record<string, unknown>;
      try {
        newNamespace = JSON.parse(result.scope);
      } catch {
        newNamespace = {};
      }

      const isNewBoard = !existing;

      if (!result.svg) {
        // No SVG produced — still save the updated namespace
        if (isNewBoard) {
          await supabase.from("boards").insert({
            name,
            width: w,
            height: h,
            svg: "",
            namespace: newNamespace,
          });
        } else {
          await supabase
            .from("boards")
            .update({ namespace: newNamespace, width: w, height: h })
            .eq("name", name);
        }

        let msg = "Code executed successfully but svg() was not called.";
        if (result.stdout) {
          msg += `\n\n--- stdout ---\n${result.stdout}`;
        }
        return { content: [{ type: "text" as const, text: msg }] };
      }

      // SVG was produced — persist board + history
      let shareId: string;
      if (isNewBoard) {
        const { data: inserted } = await supabase
          .from("boards")
          .insert({ name, width: w, height: h, svg: result.svg, namespace: newNamespace })
          .select("share_id")
          .single();
        shareId = inserted!.share_id;
      } else {
        shareId = existing.share_id;
        // Push old SVG to history before overwriting
        if (existing.svg) {
          await supabase.from("board_history").insert({
            board_name: name,
            svg: existing.svg,
          });
        }
        await supabase
          .from("boards")
          .update({
            svg: result.svg,
            namespace: newNamespace,
            width: w,
            height: h,
          })
          .eq("name", name);
      }

      // Build response
      const svgSnippet =
        result.svg.length > 200 ? result.svg.slice(0, 200) + "..." : result.svg;

      const parts: string[] = [
        `Board: ${name}\nSize: ${w}x${h}\nView: ${viewUrl(shareId)}`,
      ];
      if (result.stdout) {
        parts.push(`--- stdout ---\n${result.stdout}`);
      }
      parts.push(`--- SVG (snippet) ---\n${svgSnippet}`);

      return {
        content: [{ type: "text" as const, text: parts.join("\n\n") }],
      };
    },
  );

  // =========================================================================
  // whiteboard_list tool
  // =========================================================================
  server.registerTool(
    "whiteboard_list",
    {
      title: "Whiteboard List",
      description: "List all active boards with their metadata.",
      inputSchema: {},
    },
    async () => {
      const { data: boards, error } = await supabase
        .from("boards")
        .select("name, width, height, created_at, updated_at, share_id")
        .order("updated_at", { ascending: false });

      if (error) {
        return {
          content: [
            {
              type: "text" as const,
              text: `Database error: ${error.message}`,
            },
          ],
          isError: true,
        };
      }

      if (!boards || boards.length === 0) {
        return {
          content: [
            {
              type: "text" as const,
              text: "No boards yet. Use the whiteboard tool to create one.",
            },
          ],
        };
      }

      // Count history entries per board
      const { data: historyCounts } = await supabase
        .from("board_history")
        .select("board_name")
        .in("board_name", boards.map((b) => b.name));

      const countMap: Record<string, number> = {};
      if (historyCounts) {
        for (const row of historyCounts) {
          countMap[row.board_name] = (countMap[row.board_name] || 0) + 1;
        }
      }

      const lines = boards.map((b) => {
        const snapshots = countMap[b.name] || 0;
        return (
          `Board: ${b.name}\n` +
          `Size: ${b.width}x${b.height}\n` +
          `View: ${viewUrl(b.share_id)}\n` +
          `Created: ${b.created_at}\n` +
          `Updated: ${b.updated_at}\n` +
          `History: ${snapshots} snapshots`
        );
      });

      return {
        content: [{ type: "text" as const, text: lines.join("\n\n") }],
      };
    },
  );

  return server;
}

// ---------------------------------------------------------------------------
// View URL helpers
// ---------------------------------------------------------------------------
function viewBaseUrl(): string {
  const supabaseUrl = Deno.env.get("SUPABASE_URL")!;
  // SUPABASE_URL is like https://<ref>.supabase.co — rewrite to functions path
  return `${supabaseUrl}/functions/v1/scry/view`;
}

function viewUrl(shareId: string): string {
  return `${viewBaseUrl()}/${shareId}`;
}

// ---------------------------------------------------------------------------
// Board viewer SVG wrapper
// ---------------------------------------------------------------------------
// Supabase Edge Functions rewrite text/html → text/plain, so the viewer is a
// standalone SVG with dark background + title that browsers render natively.
function viewerSvg(boardName: string, boardSvg: string, width: number, height: number): string {
  const escapedName = boardName.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const padding = 32;
  const titleHeight = 40;
  const metaHeight = 28;
  const totalW = width + padding * 2;
  const totalH = height + padding * 2 + titleHeight + metaHeight;
  const boardY = padding + titleHeight;

  // Strip any <?xml?> or <!DOCTYPE> from the inner SVG, and extract just the <svg ...>...</svg>
  const innerSvg = boardSvg
    .replace(/<\?xml[^?]*\?>/gi, "")
    .replace(/<!DOCTYPE[^>]*>/gi, "")
    .trim();

  return `<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${totalW} ${totalH}" width="${totalW}" height="${totalH}">
  <rect width="100%" height="100%" fill="#1a1a2e" rx="8"/>
  <text x="${totalW / 2}" y="${padding + 20}" text-anchor="middle"
        font-family="system-ui, sans-serif" font-size="16" fill="#e0e0e0" opacity="0.7">
    ${escapedName}
  </text>
  <rect x="${padding}" y="${boardY}" width="${width}" height="${height}"
        fill="#16213e" rx="6"/>
  <svg x="${padding}" y="${boardY}" width="${width}" height="${height}">
    ${innerSvg}
  </svg>
  <text x="${totalW / 2}" y="${totalH - 8}" text-anchor="middle"
        font-family="system-ui, sans-serif" font-size="11" fill="#e0e0e0" opacity="0.35">
    ${width}×${height} · Scry
  </text>
</svg>`;
}

// ---------------------------------------------------------------------------
// HTTP handler
// ---------------------------------------------------------------------------
Deno.serve(async (req) => {
  const url = new URL(req.url);
  const path = url.pathname;

  // Match /scry/view/:share_id or /scry/view/:share_id/svg
  const viewMatch = path.match(/^\/scry\/view\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})(\/svg)?$/i);
  if (viewMatch && req.method === "GET") {
    const shareId = viewMatch[1];
    const rawSvg = !!viewMatch[2];

    const { data: board, error } = await supabase
      .from("boards")
      .select("name, width, height, svg")
      .eq("share_id", shareId)
      .maybeSingle();

    if (error || !board) {
      return new Response("Board not found", { status: 404, headers: { "Content-Type": "text/plain" } });
    }

    if (!board.svg) {
      return new Response("Board has no SVG content", { status: 404, headers: { "Content-Type": "text/plain" } });
    }

    const svgHeaders = { "Content-Type": "image/svg+xml", "Cache-Control": "public, max-age=60" };

    if (rawSvg) {
      return new Response(board.svg, { headers: svgHeaders });
    }

    return new Response(viewerSvg(board.name, board.svg, board.width, board.height), {
      headers: svgHeaders,
    });
  }

  // Everything else → MCP transport
  const server = createMcpServer();
  const transport = new WebStandardStreamableHTTPServerTransport();
  await server.connect(transport);
  return transport.handleRequest(req);
});
