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
- `supabase/functions/scry/index.ts` — MCP server (MCP SDK, Bearer token auth)
- `supabase/functions/scry/rhai_wasm.ts` — Custom Wasm loader for Rhai sandbox
- `supabase/functions/scry/rhai-sandbox/` — Rust crate compiled to Wasm via wasm-pack
- `supabase/functions/scry-view/index.ts` — Read-only board viewer (public)

Architecture: Two Edge Functions, both deployed with `--no-verify-jwt` (no gateway
JWT check). `scry` does in-function Bearer token auth against `SCRY_API_KEY` env
secret — requests without a valid `Authorization: Bearer <key>` get 401. `scry-view`
is fully public (read-only board viewer). Key stored at `~/.scry-api-key`.

Board viewer: Each board gets a `share_id` (UUID). View URLs point to the scry-view
function at `GET /scry-view/:share_id` (wrapper SVG with dark background + title)
and `GET /scry-view/:share_id/svg` (raw SVG). Supabase Edge Functions rewrite
`text/html` to `text/plain`, so the viewer uses `image/svg+xml` instead.

Rebuild Wasm: `cd supabase/functions/scry/rhai-sandbox && wasm-pack build --target deno --release`
Deploy:
```bash
SUPABASE_ACCESS_TOKEN=$(< ~/.supabase-key) npx supabase functions deploy scry --no-verify-jwt
SUPABASE_ACCESS_TOKEN=$(< ~/.supabase-key) npx supabase functions deploy scry-view --no-verify-jwt
```

