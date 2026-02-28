// Custom Wasm loader for Rhai sandbox that works in Supabase Edge Functions.
// The wasm-pack generated JS uses fetch(import.meta.url) which breaks after
// bundling. This module loads the wasm binary via Deno.readFile() instead.

// Re-export the glue code internals but replace the wasm loading mechanism.
// We read the .wasm file from the static_files path and instantiate manually.

const wasmPath = new URL("./rhai-sandbox/pkg/rhai_sandbox_bg.wasm", import.meta.url);

let wasmBytes: BufferSource;
try {
  // In deployed Edge Functions, static_files are co-located
  wasmBytes = await Deno.readFile(wasmPath);
} catch {
  // Fallback: try fetching (works in local dev with supabase functions serve)
  const resp = await fetch(wasmPath);
  wasmBytes = await resp.arrayBuffer();
}

// --- Begin inlined wasm-pack glue (from rhai_sandbox.js) ---

let cachedUint8ArrayMemory0: Uint8Array | null = null;
function getUint8ArrayMemory0() {
  if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
    cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
  }
  return cachedUint8ArrayMemory0;
}

const cachedTextDecoder = new TextDecoder("utf-8", { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
function decodeText(ptr: number, len: number) {
  return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();
let WASM_VECTOR_LEN = 0;

function getStringFromWasm0(ptr: number, len: number) {
  ptr = ptr >>> 0;
  return decodeText(ptr, len);
}

function getArrayU8FromWasm0(ptr: number, len: number) {
  ptr = ptr >>> 0;
  return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

function isLikeNone(x: unknown) {
  return x === undefined || x === null;
}

// deno-lint-ignore no-explicit-any
function addToExternrefTable0(obj: any) {
  const idx = wasm.__externref_table_alloc();
  wasm.__wbindgen_externrefs.set(idx, obj);
  return idx;
}

// deno-lint-ignore no-explicit-any
function handleError(f: Function, args: any) {
  try {
    return f.apply(null, args);
  } catch (e) {
    const idx = addToExternrefTable0(e);
    wasm.__wbindgen_exn_store(idx);
  }
}

function passStringToWasm0(
  arg: string,
  malloc: (len: number, align: number) => number,
  realloc?: (ptr: number, oldLen: number, newLen: number, align: number) => number,
) {
  if (realloc === undefined) {
    const buf = cachedTextEncoder.encode(arg);
    const ptr = malloc(buf.length, 1) >>> 0;
    getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
    WASM_VECTOR_LEN = buf.length;
    return ptr;
  }

  let len = arg.length;
  let ptr = malloc(len, 1) >>> 0;
  const mem = getUint8ArrayMemory0();
  let offset = 0;

  for (; offset < len; offset++) {
    const code = arg.charCodeAt(offset);
    if (code > 0x7f) break;
    mem[ptr + offset] = code;
  }
  if (offset !== len) {
    if (offset !== 0) {
      arg = arg.slice(offset);
    }
    ptr = realloc(ptr, len, (len = offset + arg.length * 3), 1) >>> 0;
    const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
    const ret = cachedTextEncoder.encodeInto(arg, view);
    offset += ret.written!;
    ptr = realloc(ptr, len, offset, 1) >>> 0;
  }

  WASM_VECTOR_LEN = offset;
  return ptr;
}

// Build the import object the wasm module expects
function getImports() {
  const import0 = {
    __proto__: null,
    // deno-lint-ignore no-explicit-any
    __wbg___wbindgen_is_undefined_52709e72fb9f179c: function (arg0: any) {
      return arg0 === undefined;
    },
    __wbg___wbindgen_throw_6ddd609b62940d55: function (arg0: number, arg1: number) {
      throw new Error(getStringFromWasm0(arg0, arg1));
    },
    __wbg_getRandomValues_3f44b700395062e5: function () {
      return handleError(function (arg0: number, arg1: number) {
        globalThis.crypto.getRandomValues(getArrayU8FromWasm0(arg0, arg1));
      }, arguments);
    },
    // deno-lint-ignore no-explicit-any
    __wbg_now_e7c6795a7f81e10f: function (arg0: any) {
      return arg0.now();
    },
    // deno-lint-ignore no-explicit-any
    __wbg_performance_3fcf6e32a7e1ed0a: function (arg0: any) {
      return arg0.performance;
    },
    __wbg_static_accessor_GLOBAL_8adb955bd33fac2f: function () {
      const ret = typeof globalThis !== "undefined" ? globalThis : null;
      return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
    },
    __wbg_static_accessor_GLOBAL_THIS_ad356e0db91c7913: function () {
      const ret = typeof globalThis !== "undefined" ? globalThis : null;
      return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
    },
    __wbg_static_accessor_SELF_f207c857566db248: function () {
      const ret = typeof self !== "undefined" ? self : null;
      return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
    },
    __wbg_static_accessor_WINDOW_bb9f1ba69d61b386: function () {
      return 0; // no window in Deno
    },
    __wbindgen_init_externref_table: function () {
      const table = wasm.__wbindgen_externrefs;
      const offset = table.grow(4);
      table.set(0, undefined);
      table.set(offset + 0, undefined);
      table.set(offset + 1, null);
      table.set(offset + 2, true);
      table.set(offset + 3, false);
    },
  };
  return {
    __proto__: null,
    "./rhai_sandbox_bg.js": import0,
  };
}

// --- Instantiate the Wasm module ---

// deno-lint-ignore no-explicit-any
const wasmInstance = (await WebAssembly.instantiate(wasmBytes, getImports() as any)).instance;
// deno-lint-ignore no-explicit-any
const wasm: any = wasmInstance.exports;
wasm.__wbindgen_start();

// --- Public API ---

export function execute(code: string, scopeJson: string, width: bigint, height: bigint): string {
  let deferred0: number | undefined;
  let deferred1: number | undefined;
  try {
    const ptr0 = passStringToWasm0(code, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(scopeJson, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.execute(ptr0, len0, ptr1, len1, width, height);
    deferred0 = ret[0];
    deferred1 = ret[1];
    return getStringFromWasm0(ret[0], ret[1]);
  } finally {
    if (deferred0 !== undefined && deferred1 !== undefined) {
      wasm.__wbindgen_free(deferred0, deferred1, 1);
    }
  }
}
