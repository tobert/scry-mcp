import "jsr:@supabase/functions-js/edge-runtime.d.ts";
import { McpServer } from "npm:@modelcontextprotocol/sdk@1.25.3/server/mcp.js";
import { WebStandardStreamableHTTPServerTransport } from "npm:@modelcontextprotocol/sdk@1.25.3/server/webStandardStreamableHttp.js";
import { z } from "npm:zod@^3.25.63";
import { createClient } from "npm:@supabase/supabase-js@2";
import { execute as rhaiExecute, metadata as rhaiMetadata } from "./rhai_wasm.ts";

// ---------------------------------------------------------------------------
// Bearer token auth — checked before any request is processed
// ---------------------------------------------------------------------------
const SCRY_API_KEY = Deno.env.get("SCRY_API_KEY");

function checkAuth(req: Request): Response | null {
  if (!SCRY_API_KEY) return null; // no key configured = open (dev mode)
  const auth = req.headers.get("authorization");
  if (auth === `Bearer ${SCRY_API_KEY}`) return null;
  return new Response("Unauthorized", { status: 401, headers: { "Content-Type": "text/plain" } });
}

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
        "Execute Rhai code to generate SVG visuals on a named board. Call svg(`<svg>...</svg>`) to set SVG content. " +
        "Variables persist between calls to the same board. WIDTH and HEIGHT constants are preset to board dimensions.\n\n" +
        "Rhai syntax essentials: `let x = 1;` for variables, `for i in range(0, n)` for loops, " +
        "`if x > 0 { ... } else { ... }` for branches. String interpolation: `${expr}` inside backtick strings. " +
        "No `const`, no `++/--`, no ternary. Use `+` to concatenate strings.\n\n" +
        "Read the scry://rhai-primer and scry://builtins resources for full syntax and available functions.\n\n" +
        "For complex/dense visuals, use `stroke-opacity`/`fill-opacity` (not `opacity`) and batch lines into `<path>` or `<polyline>` — this avoids per-element compositing layers in the PNG renderer.\n\n" +
        "Always provide `alt` text describing the visual for accessibility.",
      inputSchema: {
        name: z.string().describe("Name of the board (creates new if doesn't exist)"),
        code: z.string().describe("Rhai code to execute. Call svg(`<svg>...</svg>`) to set SVG content."),
        width: z.number().int().optional().describe("Board width in pixels (default 800)"),
        height: z.number().int().optional().describe("Board height in pixels (default 600)"),
        alt: z.string().optional().describe(
          "Alt text describing the visual. Placed last in output for easy copy/paste. " +
          "Recommended: always provide a concise description for accessibility (embedded as <desc> in SVG, iTXt in PNG).",
        ),
      },
    },
    async ({ name, code, width, height, alt }) => {
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
      const MAX_ALT_LEN = 2000;
      if (alt && new TextEncoder().encode(alt).length > MAX_ALT_LEN) {
        return {
          content: [
            { type: "text" as const, text: `Alt text too long (max ${MAX_ALT_LEN} bytes)` },
          ],
          isError: true,
        };
      }
      const altText = alt ?? "";

      // Load existing board or prepare defaults
      const { data: existing } = await supabase
        .from("boards")
        .select("name, namespace, width, height, svg, share_id, alt")
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
            alt: altText,
          });
        } else {
          await supabase
            .from("boards")
            .update({ namespace: newNamespace, width: w, height: h, alt: altText })
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
          .insert({ name, width: w, height: h, svg: result.svg, namespace: newNamespace, alt: altText })
          .select("share_id")
          .single();
        shareId = inserted!.share_id;
      } else {
        shareId = existing.share_id;
        // Push old SVG + alt to history before overwriting
        if (existing.svg) {
          await supabase.from("board_history").insert({
            board_name: name,
            svg: existing.svg,
            alt: existing.alt ?? "",
          });
        }
        await supabase
          .from("boards")
          .update({
            svg: result.svg,
            namespace: newNamespace,
            width: w,
            height: h,
            alt: altText,
          })
          .eq("name", name);
      }

      // Build response
      const svgSnippet =
        result.svg.length > 200 ? result.svg.slice(0, 200) + "..." : result.svg;

      const parts: string[] = [
        `Board: ${name}\nSize: ${w}x${h}\nView: ${viewUrl(shareId)}\nPNG: ${viewUrl(shareId)}/png`,
      ];
      if (result.stdout) {
        parts.push(`--- stdout ---\n${result.stdout}`);
      }
      parts.push(`--- SVG (snippet) ---\n${svgSnippet}`);
      if (altText) {
        parts.push(`--- Alt ---\n${altText}`);
      }

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
        .select("name, width, height, created_at, updated_at, share_id, alt")
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
          `PNG: ${viewUrl(b.share_id)}/png\n` +
          `Created: ${b.created_at}\n` +
          `Updated: ${b.updated_at}\n` +
          `History: ${snapshots} snapshots` +
          (b.alt ? `\nAlt: ${b.alt}` : "")
        );
      });

      return {
        content: [{ type: "text" as const, text: lines.join("\n\n") }],
      };
    },
  );

  // =========================================================================
  // Resources — generated from Rhai sandbox metadata
  // =========================================================================
  const meta = JSON.parse(rhaiMetadata());

  server.registerResource(
    "rhai-primer",
    "scry://rhai-primer",
    { description: "Compact Rhai language primer for generating SVG on Scry boards", mimeType: "text/plain" },
    () => {
      const lim = meta.limits;
      return {
        contents: [{
          uri: "scry://rhai-primer",
          mimeType: "text/plain",
          text:
`Rhai Primer for Scry
=====================
Rhai is a Rust-embedded scripting language. Syntax resembles Rust/JS but differs in key ways.

Variables & Types
  let x = 42;          // integer (i64)
  let y = 3.14;        // float (f64)
  let s = "hello";     // string (double quotes)
  let a = [1, 2, 3];   // array
  let m = #{ k: "v" }; // object map (note the #)
  // No 'const'. No 'var'. Only 'let'.

Strings & Interpolation
  let name = "world";
  let msg = \`hello \${name}\`;  // backtick strings support \${expr}
  let cat = "a" + "b";         // concatenation with +
  // Double-quoted strings do NOT interpolate.

Control Flow
  if x > 0 { "pos" } else { "neg" }   // if/else (braces required)
  for i in range(0, 10) { ... }        // for loop (exclusive end)
  while x > 0 { x -= 1; }             // while loop
  loop { if done { break; } }         // infinite loop + break
  // No ternary (?:), no for(;;), no ++/--.

Functions & Closures
  fn double(x) { x * 2 }       // fn keyword, no type annotations
  let add = |a, b| a + b;      // closures

Arrays & Maps
  let a = [10, 20, 30];
  a.len();  a.push(40);  for v in a { print(v); }
  let m = #{ x: 1, y: 2 };
  m.x;  m.keys();

SVG Pattern
  let body = "";
  for i in range(0, 5) {
    let cx = 100 + i * 150;
    body += \`<circle cx="\${cx}" cy="300" r="40" fill="teal"/>\`;
  }
  svg(\`<svg xmlns="http://www.w3.org/2000/svg" width="\${WIDTH}" height="\${HEIGHT}" viewBox="0 0 \${WIDTH} \${HEIGHT}">\${body}</svg>\`);

  XML comments (<!-- -->) are unsupported in SVG output.

Color Functions (palette crate)
  hsl(h, s, l)              → "#rrggbb"   h=0-360, s/l=0-100
  hsla(h, s, l, a)          → "#rrggbbaa"  a=0.0-1.0
  rgb(r, g, b)              → "#rrggbb"   0-255 per channel
  rgba(r, g, b, a)          → "#rrggbbaa"
  oklch(l, c, h)            → "#rrggbb"   perceptually uniform (l=0-1, c=0-0.4, h=0-360)
  oklcha(l, c, h, a)        → "#rrggbbaa"
  color_mix(hex1, hex2, t)  → "#rrggbb"   mix in Oklab space (t=0→hex1, t=1→hex2)
  color_lighten(hex, amt)   → "#rrggbb"   lighten in Oklch
  color_darken(hex, amt)    → "#rrggbb"   darken in Oklch
  color_saturate(hex, amt)  → "#rrggbb"   boost chroma in Oklch
  color_desaturate(hex, amt)→ "#rrggbb"   reduce chroma in Oklch
  hue_shift(hex, degrees)   → "#rrggbb"   rotate hue in Oklch

  All color functions return hex strings for direct use in SVG attributes.
  hsla/rgba/oklcha return 8-digit hex with alpha baked in — use these
  instead of the opacity attribute for PNG-friendly rendering.

  Oklch tips: cycle hue at fixed l/c for perceptually even palettes.
  color_mix interpolates through Oklab — no muddy midpoints like HSL lerp.

PNG-Friendly SVG Patterns
  The PNG renderer (resvg) allocates a compositing buffer for EVERY element
  that uses the \`opacity\` attribute. Dense scenes (hundreds of translucent
  lines) can exhaust memory. Follow these patterns instead:

  AVOID: <line stroke="red" opacity="0.5"/>          → compositing layer per element
  GOOD:  <line stroke="red" stroke-opacity="0.5"/>   → alpha on paint, no buffer
  BEST:  <line stroke="\${hsla(0, 80, 60, 0.5)}"/>    → alpha baked into hex color

  Batch geometry to reduce element count:
  AVOID: 500 separate <line x1=... y1=... x2=... y2=.../> elements
  GOOD:  <polyline points="x1,y1 x2,y2 ..."/>       → one element for connected points
  GOOD:  <path d="M x1 y1 L x2 y2 M x3 y3 L x4 y4"/>  → batched disconnected segments

Sandbox Limits
  Max operations: ${lim.max_operations}
  Max call depth: ${lim.max_call_levels}
  Max string size: ${lim.max_string_size} bytes
  Max array size: ${lim.max_array_size} elements
  Max map size: ${lim.max_map_size} entries`,
        }],
      };
    },
  );

  server.registerResource(
    "builtins",
    "scry://builtins",
    { description: "All functions available in the Scry Rhai sandbox", mimeType: "text/plain" },
    () => {
      const lines: string[] = ["Scry Rhai Builtins", "==================", ""];

      // Group builtins by category based on name patterns
      for (const fn of meta.builtins) {
        lines.push(`  ${fn.sig.padEnd(34)} ${fn.doc}`);
      }

      lines.push("");
      lines.push("Constants (read-only, set per execution)");
      for (const c of meta.constants) {
        lines.push(`  ${c.name.padEnd(10)} ${c.type.padEnd(6)} ${c.doc}`);
      }

      lines.push("");
      lines.push("Standard Rhai (built-in, not listed above)");
      lines.push("  Arithmetic:  + - * / %  on i64 and f64");
      lines.push("  Comparison:  == != < > <= >=");
      lines.push("  Logic:       && || !");
      lines.push("  Strings:     .len() .contains(s) .trim() .to_upper() .to_lower()");
      lines.push("               .sub_string(start, len) .replace(old, new)");
      lines.push("  Arrays:      .len() .push(v) .pop() .insert(i, v) .remove(i)");
      lines.push("               .reverse() .sort() .map(|v| ...) .filter(|v| ...)");
      lines.push("               .reduce(|acc, v| ...) .for_each(|v| ...)");
      lines.push("  Maps:        .keys() .values() .len() .contains(key) .remove(key)");
      lines.push("  range(start, end)          // exclusive end, returns iterator");
      lines.push("  range(start, end, step)    // with step");

      return {
        contents: [{
          uri: "scry://builtins",
          mimeType: "text/plain",
          text: lines.join("\n"),
        }],
      };
    },
  );

  return server;
}

// ---------------------------------------------------------------------------
// View URL helper — points to the separate scry-view function
// ---------------------------------------------------------------------------
function viewUrl(shareId: string): string {
  const supabaseUrl = Deno.env.get("SUPABASE_URL")!;
  return `${supabaseUrl}/functions/v1/scry-view/${shareId}`;
}

// ---------------------------------------------------------------------------
// HTTP handler — pure MCP transport
// ---------------------------------------------------------------------------
Deno.serve(async (req) => {
  const denied = checkAuth(req);
  if (denied) return denied;

  const server = createMcpServer();
  const transport = new WebStandardStreamableHTTPServerTransport();
  await server.connect(transport);
  return transport.handleRequest(req);
});
