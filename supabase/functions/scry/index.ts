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
        .select("name, namespace, width, height, svg")
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
      if (isNewBoard) {
        await supabase.from("boards").insert({
          name,
          width: w,
          height: h,
          svg: result.svg,
          namespace: newNamespace,
        });
      } else {
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

      const parts: string[] = [`Board: ${name}\nSize: ${w}x${h}`];
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
        .select("name, width, height, created_at, updated_at")
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
// HTTP handler
// ---------------------------------------------------------------------------
Deno.serve(async (req) => {
  const server = createMcpServer();
  const transport = new WebStandardStreamableHTTPServerTransport();
  await server.connect(transport);
  return transport.handleRequest(req);
});
