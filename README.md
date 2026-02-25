<p align="center">
  <img src="images/banner.png" alt="scry — computational scrying glass" width="800">
</p>

Computational scrying glass — an MCP server that lets Claude generate SVG visuals via Python code, render them to PNG, and serve them in a live web gallery.

## Architecture

```
Claude ──stdio──> scry-mcp (single Rust binary)
                    ├─ PyO3 embedded CPython (persistent namespace per board)
                    ├─ resvg (SVG → PNG, in-process, pure Rust)
                    ├─ In-memory board store (HashMap<String, Board>)
                    └─ axum webserver (localhost:3333, SSE live updates)
```

## Tools

**`whiteboard`** — Execute Python code to generate SVG visuals on a named board. Call `svg('<svg>...</svg>')` to set content. Variables persist between calls.

**`whiteboard_list`** — List all active boards with thumbnails, URLs, and metadata.

## Install

```
cargo build --release
```

### Add to Claude Code

```
claude mcp add -s user scry -- /home/atobey/src/aimboard/scry-mcp/target/release/scry-mcp --address 127.0.0.1 --port 3333
```

Or manually add to your MCP config:

```json
{
  "mcpServers": {
    "scry": {
      "command": "/home/atobey/src/aimboard/scry-mcp/target/release/scry-mcp",
      "args": ["--address", "127.0.0.1", "--port", "3333"]
    }
  }
}
```

## Usage

Once configured, ask Claude to use the whiteboard tool:

> "Draw a red circle on a white background using the whiteboard tool"

The gallery is live at http://localhost:3333/gallery/ — it auto-refreshes via SSE when boards update.

### CLI Options

```
scry-mcp [OPTIONS]

Options:
      --address <ADDRESS>  Gallery bind address [default: 127.0.0.1]
      --port <PORT>        Gallery port [default: 3333]
```

## Python Environment

Each board gets a persistent Python namespace with these pre-imported:

`math`, `random`, `json`, `re`, `textwrap`, `itertools`, `functools`, `collections`, `colorsys`, `hashlib`, `string`, `dataclasses`

`WIDTH` and `HEIGHT` are set to board dimensions (default 800x600).

Dangerous modules (`os`, `subprocess`, `socket`, etc.) are blocked.
