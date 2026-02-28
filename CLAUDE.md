# Scry MCP

scry-mcp is an MCP with two tools: `whiteboard()` and `whiteboard_list()`.

## Branches

### `main` — Rust/stdio version
A whiteboard call includes Python code for generating SVG. Uses PyO3 with advisory
sandboxing. Runs as stdio MCP transport.

### `prototype-supabase-service` — Supabase Edge Function version
Runs as HTTP MCP (Streamable HTTP transport) on Supabase Edge Functions. Uses Rhai
(not Python) via a Wasm module for script execution. Boards persist to Postgres.

Key files:
- `supabase/functions/scry/index.ts` — MCP server (Hono + MCP SDK)
- `supabase/functions/scry/rhai_wasm.ts` — Custom Wasm loader for Rhai sandbox
- `supabase/functions/scry/rhai-sandbox/` — Rust crate compiled to Wasm via wasm-pack

Board viewer: Each board gets a `share_id` (UUID). View URLs are returned by both
tools and served at `GET /scry/view/:share_id` (wrapper SVG with dark background +
title) and `GET /scry/view/:share_id/svg` (raw SVG). Note: Supabase Edge Functions
rewrite `text/html` to `text/plain`, so the viewer uses `image/svg+xml` instead.

Rebuild Wasm: `cd supabase/functions/scry/rhai-sandbox && wasm-pack build --target deno --release`
Deploy: `SUPABASE_ACCESS_TOKEN=$(< ~/.supabase-key) npx supabase functions deploy scry --no-verify-jwt`

