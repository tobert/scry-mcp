import "jsr:@supabase/functions-js/edge-runtime.d.ts";
import { createClient } from "npm:@supabase/supabase-js@2";
import { Resvg, initWasm } from "npm:@resvg/resvg-wasm@2.6.2";

// ---------------------------------------------------------------------------
// Initialize resvg Wasm — same pattern as rhai_wasm.ts
// ---------------------------------------------------------------------------
const resvgWasmPath = new URL("./resvg_bg.wasm", import.meta.url);
let resvgWasmBytes: BufferSource;
try {
  resvgWasmBytes = await Deno.readFile(resvgWasmPath);
} catch {
  const resp = await fetch(resvgWasmPath);
  resvgWasmBytes = await resp.arrayBuffer();
}
await initWasm(resvgWasmBytes);

// ---------------------------------------------------------------------------
// Supabase client (service role to bypass RLS for read-only queries)
// ---------------------------------------------------------------------------
const supabase = createClient(
  Deno.env.get("SUPABASE_URL")!,
  Deno.env.get("SUPABASE_SERVICE_ROLE_KEY")!,
);

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

  // Strip any <?xml?> or <!DOCTYPE> from the inner SVG
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
// HTTP handler — read-only board viewer
// ---------------------------------------------------------------------------
const UUID_RE = /([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})(\/(?:svg|png))?$/i;

Deno.serve(async (req) => {
  if (req.method !== "GET") {
    return new Response("Method not allowed", { status: 405, headers: { "Content-Type": "text/plain" } });
  }

  const url = new URL(req.url);
  const match = url.pathname.match(UUID_RE);
  if (!match) {
    return new Response("Not found", { status: 404, headers: { "Content-Type": "text/plain" } });
  }

  const shareId = match[1];
  const suffix = match[2]; // "/svg", "/png", or undefined

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

  if (suffix === "/svg") {
    return new Response(board.svg, { headers: svgHeaders });
  }

  if (suffix === "/png") {
    const MAX_PNG_DIM = 4096;
    if (board.width > MAX_PNG_DIM || board.height > MAX_PNG_DIM) {
      return new Response(
        `Board too large for PNG rendering (${board.width}x${board.height}, max ${MAX_PNG_DIM}x${MAX_PNG_DIM})`,
        { status: 422, headers: { "Content-Type": "text/plain" } },
      );
    }
    const resvg = new Resvg(board.svg, { fitTo: { mode: "original" } });
    const rendered = resvg.render();
    const pngBytes = rendered.asPng();
    rendered.free();
    resvg.free();
    return new Response(pngBytes, {
      headers: { "Content-Type": "image/png", "Cache-Control": "public, max-age=300" },
    });
  }

  return new Response(viewerSvg(board.name, board.svg, board.width, board.height), {
    headers: svgHeaders,
  });
});
