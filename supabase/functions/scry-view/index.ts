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
// Load bundled fonts for PNG rendering (Edge Functions have no system fonts)
// ---------------------------------------------------------------------------
async function loadFont(name: string): Promise<Uint8Array> {
  const fontPath = new URL(`./fonts/${name}`, import.meta.url);
  try {
    return await Deno.readFile(fontPath);
  } catch {
    const resp = await fetch(fontPath);
    return new Uint8Array(await resp.arrayBuffer());
  }
}

const fontBuffers = await Promise.all([
  loadFont("DejaVuSans.ttf"),
  loadFont("DejaVuSansMono.ttf"),
]);

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
function viewerSvg(boardName: string, boardSvg: string, width: number, height: number, alt?: string): string {
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

  const roleAttr = alt ? ` role="img"` : "";
  const descEl = alt
    ? `\n  <desc>${alt.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")}</desc>`
    : "";

  return `<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${totalW} ${totalH}" width="${totalW}" height="${totalH}"${roleAttr}>${descEl}
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
// CRC-32 (ISO 3309 / PNG) — lookup-table implementation
// ---------------------------------------------------------------------------
const crcTable = new Uint32Array(256);
for (let n = 0; n < 256; n++) {
  let c = n;
  for (let k = 0; k < 8; k++) {
    c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  }
  crcTable[n] = c;
}

function crc32(buf: Uint8Array): number {
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    crc = crcTable[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

// ---------------------------------------------------------------------------
// Inject iTXt chunk into PNG before IEND — standard PNG text metadata
// ---------------------------------------------------------------------------
function injectPngText(png: Uint8Array, key: string, value: string): Uint8Array {
  const encoder = new TextEncoder();
  const keyBytes = encoder.encode(key);
  const langBytes = encoder.encode("en");
  const valueBytes = encoder.encode(value);

  // iTXt data: keyword\0 compressionFlag(0) compressionMethod(0) languageTag\0 translatedKeyword\0 text
  const dataLen = keyBytes.length + 1 + 1 + 1 + langBytes.length + 1 + 1 + valueBytes.length;
  const chunkData = new Uint8Array(dataLen);
  let offset = 0;
  chunkData.set(keyBytes, offset); offset += keyBytes.length;
  chunkData[offset++] = 0; // null separator after keyword
  chunkData[offset++] = 0; // compression flag (no compression)
  chunkData[offset++] = 0; // compression method
  chunkData.set(langBytes, offset); offset += langBytes.length;
  chunkData[offset++] = 0; // null separator after language tag
  // translated keyword: empty (null separator)
  chunkData[offset++] = 0;
  chunkData.set(valueBytes, offset);

  // Build the full chunk: length(4) + "iTXt" + data + CRC(4)
  const typeBytes = encoder.encode("iTXt");
  const crcInput = new Uint8Array(4 + dataLen);
  crcInput.set(typeBytes, 0);
  crcInput.set(chunkData, 4);
  const crcVal = crc32(crcInput);

  const chunk = new Uint8Array(4 + 4 + dataLen + 4);
  const view = new DataView(chunk.buffer);
  view.setUint32(0, dataLen);
  chunk.set(typeBytes, 4);
  chunk.set(chunkData, 8);
  view.setUint32(8 + dataLen, crcVal);

  // Splice before IEND (last 12 bytes of a valid PNG)
  const result = new Uint8Array(png.length + chunk.length);
  result.set(png.subarray(0, png.length - 12), 0);
  result.set(chunk, png.length - 12);
  result.set(png.subarray(png.length - 12), png.length - 12 + chunk.length);
  return result;
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
    .select("name, width, height, svg, alt")
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
    let rawSvg = board.svg;
    if (board.alt) {
      const escaped = board.alt.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
      rawSvg = rawSvg.replace(/(<svg[^>]*>)/, `$1\n  <desc>${escaped}</desc>`);
    }
    return new Response(rawSvg, { headers: svgHeaders });
  }

  if (suffix === "/png") {
    const MAX_PNG_DIM = 4096;
    if (board.width > MAX_PNG_DIM || board.height > MAX_PNG_DIM) {
      return new Response(
        `Board too large for PNG rendering (${board.width}x${board.height}, max ${MAX_PNG_DIM}x${MAX_PNG_DIM})`,
        { status: 422, headers: { "Content-Type": "text/plain" } },
      );
    }
    const resvg = new Resvg(board.svg, {
      fitTo: { mode: "original" },
      font: {
        fontBuffers,
        defaultFontFamily: "DejaVu Sans",
        sansSerifFamily: "DejaVu Sans",
        monospaceFamily: "DejaVu Sans Mono",
      },
    });
    const rendered = resvg.render();
    let pngBytes: Uint8Array = rendered.asPng();
    rendered.free();
    resvg.free();
    if (board.alt) {
      pngBytes = injectPngText(pngBytes, "Description", board.alt);
    }
    return new Response(pngBytes, {
      headers: { "Content-Type": "image/png", "Cache-Control": "public, max-age=300" },
    });
  }

  return new Response(viewerSvg(board.name, board.svg, board.width, board.height, board.alt), {
    headers: svgHeaders,
  });
});
