use crate::board::{SharedState, html_escape, url_encode};
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::stream::Stream;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/", get(|| async { Redirect::permanent("/gallery/") }))
        .route("/gallery/", get(gallery_index))
        .route("/gallery/board/{name}", get(board_detail))
        .route("/gallery/board/{name}/png", get(board_png))
        .route("/gallery/board/{name}/svg", get(board_svg))
        .route("/gallery/events", get(sse_handler))
        .with_state(state)
}

async fn gallery_index(State(state): State<SharedState>) -> Html<String> {
    let boards = state.boards.read().await;
    let mut cards = String::new();

    let mut board_list: Vec<_> = boards.values().collect();
    board_list.sort_by_key(|b| std::cmp::Reverse(b.updated_at));

    for board in board_list {
        let name_url = url_encode(&board.name);
        let name_html = html_escape(&board.name);
        let has_image = !board.png.is_empty();
        let img_tag = if has_image {
            format!(
                r#"<img src="/gallery/board/{}/png" alt="{}" loading="lazy">"#,
                name_url, name_html
            )
        } else {
            "<div class=\"placeholder\">No render yet</div>".to_string()
        };

        cards.push_str(&format!(
            r#"<div class="card" onclick="location.href='/gallery/board/{name_url}'">
                <div class="card-img">{img_tag}</div>
                <div class="card-info">
                    <h2>{name_html}</h2>
                    <span class="dim">{w}x{h} &middot; {updated}</span>
                </div>
            </div>"#,
            name_url = name_url,
            img_tag = img_tag,
            name_html = name_html,
            w = board.width,
            h = board.height,
            updated = board.updated_at.format("%H:%M:%S"),
        ));
    }

    if cards.is_empty() {
        cards = "<p class=\"empty\">No boards yet. Use the whiteboard tool to create one.</p>".to_string();
    }

    Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Scry Gallery</title>
<style>{CSS}</style>
</head>
<body>
<header><h1>Scry Gallery</h1></header>
<main class="grid">{cards}</main>
<script>{SSE_JS}</script>
</body>
</html>"#,
        CSS = CSS,
        cards = cards,
        SSE_JS = SSE_RELOAD_JS,
    ))
}

async fn board_detail(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Response {
    let boards = state.boards.read().await;
    let name_html = html_escape(&name);
    let name_url = url_encode(&name);
    let Some(board) = boards.get(&name) else {
        return Html(format!(
            r#"<!DOCTYPE html><html><head><style>{CSS}</style></head>
            <body><h1>Board not found: {name_html}</h1>
            <a href="/gallery/">Back to gallery</a></body></html>"#,
            CSS = CSS,
            name_html = name_html,
        ))
        .into_response();
    };

    let img_section = if !board.png.is_empty() {
        let b64 = BASE64.encode(&board.png);
        format!(
            r#"<div class="board-img">
                <img src="data:image/png;base64,{b64}" alt="{name_html}">
            </div>
            <div class="links">
                <a href="/gallery/board/{name_url}/png">Raw PNG</a>
                <a href="/gallery/board/{name_url}/svg">Raw SVG</a>
            </div>"#,
            b64 = b64,
            name_html = name_html,
            name_url = name_url,
        )
    } else {
        "<p>No render yet.</p>".to_string()
    };

    let svg_escaped = html_escape(&board.svg);

    Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Scry — {name_html}</title>
<style>{CSS}</style>
</head>
<body>
<header>
    <a href="/gallery/" class="back">&larr; Gallery</a>
    <h1>{name_html}</h1>
    <span class="dim">{w}x{h} &middot; Updated {updated} &middot; {history_len} snapshots</span>
</header>
<main>
    {img_section}
    <details>
        <summary>SVG Source</summary>
        <pre><code>{svg_escaped}</code></pre>
    </details>
</main>
<script>{SSE_JS}</script>
</body>
</html>"#,
        CSS = CSS,
        name_html = name_html,
        w = board.width,
        h = board.height,
        updated = board.updated_at.format("%Y-%m-%d %H:%M:%S UTC"),
        history_len = board.history.len(),
        img_section = img_section,
        svg_escaped = svg_escaped,
        SSE_JS = sse_board_js(&board.name),
    ))
    .into_response()
}

async fn board_png(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Response {
    let boards = state.boards.read().await;
    match boards.get(&name) {
        Some(board) if !board.png.is_empty() => {
            (
                [(axum::http::header::CONTENT_TYPE, "image/png")],
                board.png.clone(),
            )
                .into_response()
        }
        _ => (axum::http::StatusCode::NOT_FOUND, "Board not found or no render").into_response(),
    }
}

async fn board_svg(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Response {
    let boards = state.boards.read().await;
    match boards.get(&name) {
        Some(board) if !board.svg.is_empty() => {
            (
                [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
                board.svg.clone(),
            )
                .into_response()
        }
        _ => (axum::http::StatusCode::NOT_FOUND, "Board not found or no SVG").into_response(),
    }
}

async fn sse_handler(
    State(state): State<SharedState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(event) => {
                let data = serde_json::json!({
                    "board": event.board_name,
                    "type": format!("{:?}", event.event_type),
                });
                Some(Ok(Event::default().data(data.to_string())))
            }
            Err(_) => None, // lagged, skip
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn sse_board_js(board_name: &str) -> String {
    // JSON-encode the board name to safely embed in a JS string literal
    let js_safe = serde_json::to_string(board_name).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"const es = new EventSource('/gallery/events');
const _boardName = {js_safe};
es.onmessage = function(e) {{
    const data = JSON.parse(e.data);
    if (data.board === _boardName) {{
        location.reload();
    }}
}};"#,
        js_safe = js_safe,
    )
}

const SSE_RELOAD_JS: &str = r#"const es = new EventSource('/gallery/events');
es.onmessage = function(e) { location.reload(); };"#;

const CSS: &str = r#"
:root {
    --bg: #1a1a2e;
    --surface: #16213e;
    --border: #0f3460;
    --text: #e0e0e0;
    --dim: #888;
    --accent: #e94560;
}
* { margin: 0; padding: 0; box-sizing: border-box; }
body {
    font-family: 'SF Mono', 'Cascadia Code', 'Fira Code', monospace;
    background: var(--bg);
    color: var(--text);
    min-height: 100vh;
}
header {
    padding: 1.5rem 2rem;
    border-bottom: 1px solid var(--border);
}
header h1 { font-size: 1.4rem; margin-bottom: 0.3rem; }
.back {
    color: var(--accent);
    text-decoration: none;
    font-size: 0.9rem;
}
.back:hover { text-decoration: underline; }
.dim { color: var(--dim); font-size: 0.85rem; }
main { padding: 2rem; }
.grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 1.5rem;
}
.card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
    cursor: pointer;
    transition: border-color 0.2s;
}
.card:hover { border-color: var(--accent); }
.card-img {
    aspect-ratio: 4/3;
    display: flex;
    align-items: center;
    justify-content: center;
    background: #111;
    overflow: hidden;
}
.card-img img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
}
.card-info { padding: 0.8rem 1rem; }
.card-info h2 { font-size: 1rem; margin-bottom: 0.2rem; }
.placeholder {
    color: var(--dim);
    font-size: 0.9rem;
}
.empty {
    color: var(--dim);
    text-align: center;
    padding: 4rem;
    font-size: 1.1rem;
}
.board-img {
    text-align: center;
    margin: 1rem 0;
    background: #111;
    padding: 1rem;
    border-radius: 8px;
}
.board-img img {
    max-width: 100%;
    height: auto;
}
.links {
    margin: 1rem 0;
    display: flex;
    gap: 1rem;
}
.links a {
    color: var(--accent);
    text-decoration: none;
    font-size: 0.9rem;
}
.links a:hover { text-decoration: underline; }
details {
    margin: 1.5rem 0;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1rem;
}
summary {
    cursor: pointer;
    font-weight: bold;
    margin-bottom: 0.5rem;
}
pre {
    overflow-x: auto;
    font-size: 0.8rem;
    line-height: 1.4;
    padding: 1rem;
    background: #111;
    border-radius: 4px;
}
"#;
