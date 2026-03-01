# Supabase Product Feedback: Edge Functions + Wasm + MCP

**Context**: Porting a Rust/stdio MCP server (scry-mcp) to run as a Supabase Edge
Function with HTTP MCP transport. The function executes user code via a Rhai Wasm
module and persists results to Postgres.

**Date**: 2026-02-28
**CLI version**: 2.76.15
**Runtime**: Deno 1.46 (Edge Functions production)

---

## What worked well

**Supabase MCP tool** — `apply_migration`, `execute_sql`, `list_tables`, and `get_logs`
were excellent for the database workflow. Being able to apply migrations and verify
schema without leaving the development flow was smooth.

**Edge Function deployment** — Once we got past the Wasm issue (see below), deployment
via `npx supabase functions deploy` was fast and reliable. The Docker-based bundler
handled static files correctly.

**Auto-injected env vars** — `SUPABASE_URL`, `SUPABASE_SERVICE_ROLE_KEY` just being
there in the function environment is a great DX decision. No config ceremony.

**MCP SDK + Hono** — The `WebStandardStreamableHTTPServerTransport` from the MCP SDK
worked perfectly with `Deno.serve()`. Stateless per-request MCP servers are a clean
pattern for Edge Functions.

---

## Issues encountered

### 1. Wasm loading fails silently after bundling (high severity)

The wasm-pack `--target deno` generates JS that loads the `.wasm` binary via:
```js
const wasmUrl = new URL('rhai_sandbox_bg.wasm', import.meta.url);
await WebAssembly.instantiateStreaming(fetch(wasmUrl), imports);
```

After the ESBuild bundler inlines this JS into the function bundle, `import.meta.url`
no longer points to the original file location. The `fetch()` fails at runtime with
no useful error message — just `WORKER_ERROR`.

**Workaround**: We wrote a custom TypeScript wasm loader (`rhai_wasm.ts`) that reads
the wasm binary via `Deno.readFile(new URL(..., import.meta.url))` instead of `fetch()`.
This works because `Deno.readFile()` resolves the URL against the filesystem where
`static_files` are placed.

**Suggestion**: Either:
- Document this gotcha in the [Wasm guide](https://supabase.com/docs/guides/functions/wasm)
  with a recommended loader pattern
- Have the bundler preserve `import.meta.url` semantics for Wasm imports
- Ship a `@supabase/wasm-loader` helper that handles this

The existing Wasm docs show the simple `import { add } from "./pkg/add_wasm.js"`
pattern, which presumably works for small modules. It did NOT work for us with
rhai (1.7MB Wasm), but the failure mode was completely opaque.

### 2. Edge Function error messages are unhelpful (high severity)

Every runtime error returns:
```json
{"code":"WORKER_ERROR","message":"Function exited due to an error (please check logs)"}
```

The logs endpoint (`get_logs`) only shows HTTP status codes and timing — no stack
traces, no error messages, no stderr capture. This made debugging the Wasm loading
issue extremely painful. We had to deploy 5+ iterations of progressively simpler
functions to bisect the problem.

**Suggestion**: Surface the actual error message (or at least the first line of it)
in the logs. Even something like "TypeError: fetch failed" would have saved 20+
minutes of debugging.

### 3. `deploy_edge_function` MCP tool can't handle binary static files

The MCP-based deploy tool (`mcp__supabase__deploy_edge_function`) takes files as
`{name: string, content: string}[]`. This works for TypeScript but can't handle
binary `.wasm` files. We had to fall back to the CLI for deployment.

**Suggestion**: Support base64-encoded content for binary files, e.g.:
```json
{"name": "module.wasm", "content": "AGFzbQ...", "encoding": "base64"}
```

### 4. `static_files` deployment size reporting is confusing (low severity)

The CLI reports "script size: 404.4kB" for the JS bundle, but static files (1.7MB
Wasm binary) are deployed separately and not included in that number. The total
deployment is ~2.1MB but you can't tell from the CLI output. This led us to
incorrectly believe the Wasm wasn't being deployed.

**Suggestion**: Report total deployment size including static files, or at least
log "Including N static files (X MB)" during deployment.

### 5. `supabase link` required before config.toml is respected

We had to run `supabase link --project-ref ...` before the CLI would pick up
`config.toml` settings like `static_files`. Without linking, the config was
silently ignored. This isn't documented for the deploy workflow.

---

## Minor notes

- The `search_docs` MCP tool correctly returned the Wasm guide, which confirmed
  our approach was right (just the loader was wrong)
- Zod 4 (`npm:zod@^4.1.13`) doesn't work with the MCP SDK v1.25.3 which depends
  on Zod 3. The import fails silently. Had to downgrade to `npm:zod@^3.25.63`.
- The `config.toml` format with `[project] id = ""` section causes a parse error.
  Only `[functions.NAME]` sections are valid. This isn't obvious from the docs.

---

## What we built

A working MCP server as a Supabase Edge Function that:
- Exposes `whiteboard` and `whiteboard_list` tools via HTTP MCP transport
- Executes Rhai scripts in a Wasm sandbox (1.7MB, ~300ms cold start)
- Persists board state (SVG, namespace/scope) to Postgres
- Tracks SVG history for undo/replay
- Namespace round-trips through Postgres JSONB for variable persistence

Endpoint: `https://iajwetvzoifutmsxwxcd.supabase.co/functions/v1/scry`
