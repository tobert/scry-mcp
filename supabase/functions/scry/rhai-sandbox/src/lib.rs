use rhai::{Engine, Scope, AST, Dynamic, ImmutableString};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use wasm_bindgen::prelude::*;

/// Result of executing a Rhai script, serialized as JSON for the TypeScript host.
#[derive(Serialize, Deserialize)]
struct ExecResult {
    /// SVG content set via the `svg()` callback, if any.
    svg: Option<String>,
    /// Captured output from `print()` calls.
    stdout: String,
    /// Serialized Rhai Scope for namespace persistence (JSON object).
    scope: String,
    /// Error message, if execution failed.
    error: Option<String>,
}

thread_local! {
    static SVG_CONTENT: RefCell<Option<String>> = RefCell::new(None);
    static STDOUT_BUF: RefCell<String> = RefCell::new(String::new());
}

// Sandbox limits — used by build_engine() and exported via metadata()
const MAX_OPERATIONS: u64 = 100_000;
const MAX_CALL_LEVELS: usize = 32;
const MAX_STRING_SIZE: usize = 1_000_000;
const MAX_ARRAY_SIZE: usize = 10_000;
const MAX_MAP_SIZE: usize = 1_000;

fn build_engine() -> Engine {
    let mut engine = Engine::new();

    engine.set_max_operations(MAX_OPERATIONS);
    engine.set_max_call_levels(MAX_CALL_LEVELS);
    engine.set_max_string_size(MAX_STRING_SIZE);
    engine.set_max_array_size(MAX_ARRAY_SIZE);
    engine.set_max_map_size(MAX_MAP_SIZE);

    // Override print/debug to capture stdout
    engine.on_print(|s| {
        STDOUT_BUF.with(|buf| {
            let mut buf = buf.borrow_mut();
            buf.push_str(s);
            buf.push('\n');
        });
    });
    engine.on_debug(|s, source, pos| {
        STDOUT_BUF.with(|buf| {
            let mut buf = buf.borrow_mut();
            if let Some(src) = source {
                buf.push_str(&format!("[{src}] "));
            }
            if !pos.is_none() {
                buf.push_str(&format!("{pos:?} | "));
            }
            buf.push_str(s);
            buf.push('\n');
        });
    });

    // Register svg() function to capture SVG output
    engine.register_fn("svg", |content: ImmutableString| {
        SVG_CONTENT.with(|cell| {
            *cell.borrow_mut() = Some(content.to_string());
        });
    });

    // Register helpful math functions not in Rhai core
    engine.register_fn("sin", |x: f64| x.sin());
    engine.register_fn("cos", |x: f64| x.cos());
    engine.register_fn("tan", |x: f64| x.tan());
    engine.register_fn("asin", |x: f64| x.asin());
    engine.register_fn("acos", |x: f64| x.acos());
    engine.register_fn("atan", |x: f64| x.atan());
    engine.register_fn("atan2", |y: f64, x: f64| y.atan2(x));
    engine.register_fn("sqrt", |x: f64| x.sqrt());
    engine.register_fn("abs_f", |x: f64| x.abs());
    engine.register_fn("floor", |x: f64| x.floor());
    engine.register_fn("ceil", |x: f64| x.ceil());
    engine.register_fn("round", |x: f64| x.round());
    engine.register_fn("min_f", |a: f64, b: f64| a.min(b));
    engine.register_fn("max_f", |a: f64, b: f64| a.max(b));
    engine.register_fn("PI", || std::f64::consts::PI);
    engine.register_fn("TAU", || std::f64::consts::TAU);

    // String/number conversion helpers
    engine.register_fn("to_float", |x: i64| x as f64);
    engine.register_fn("to_int", |x: f64| x as i64);

    engine
}

/// Deserialize a JSON string into a Rhai Scope.
fn scope_from_json(json: &str) -> Scope<'static> {
    let mut scope = Scope::new();
    if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json) {
        for (key, value) in map {
            let dynamic = json_to_dynamic(&value);
            scope.push_dynamic(key, dynamic);
        }
    }
    scope
}

/// Convert a serde_json Value to a Rhai Dynamic.
fn json_to_dynamic(value: &serde_json::Value) -> Dynamic {
    match value {
        serde_json::Value::Null => Dynamic::UNIT,
        serde_json::Value::Bool(b) => Dynamic::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from(i)
            } else if let Some(f) = n.as_f64() {
                Dynamic::from(f)
            } else {
                Dynamic::UNIT
            }
        }
        serde_json::Value::String(s) => Dynamic::from(s.clone()),
        serde_json::Value::Array(arr) => {
            let items: Vec<Dynamic> = arr.iter().map(json_to_dynamic).collect();
            Dynamic::from(items)
        }
        serde_json::Value::Object(obj) => {
            let mut map = rhai::Map::new();
            for (k, v) in obj {
                map.insert(k.clone().into(), json_to_dynamic(v));
            }
            Dynamic::from(map)
        }
    }
}

/// Convert a Rhai Dynamic to a serde_json Value.
fn dynamic_to_json(value: &Dynamic) -> serde_json::Value {
    if value.is_unit() {
        serde_json::Value::Null
    } else if let Ok(b) = value.as_bool() {
        serde_json::Value::Bool(b)
    } else if let Ok(i) = value.as_int() {
        serde_json::Value::Number(i.into())
    } else if let Ok(f) = value.as_float() {
        serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)
    } else if let Ok(s) = value.clone().into_string() {
        serde_json::Value::String(s)
    } else if value.is_array() {
        if let Ok(arr) = value.clone().into_typed_array::<Dynamic>() {
            serde_json::Value::Array(arr.iter().map(dynamic_to_json).collect())
        } else {
            serde_json::Value::Null
        }
    } else if value.is_map() {
        if let Some(map) = value.clone().try_cast::<rhai::Map>() {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.to_string(), dynamic_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        } else {
            serde_json::Value::Null
        }
    } else {
        // Fall back to string representation for other types
        serde_json::Value::String(value.to_string())
    }
}

/// Serialize a Rhai Scope to a JSON string, skipping constants (WIDTH/HEIGHT).
fn scope_to_json(scope: &Scope) -> String {
    let mut map = serde_json::Map::new();
    for (name, is_constant, value) in scope.iter() {
        // Skip constants (WIDTH, HEIGHT) — they're injected each call
        if is_constant {
            continue;
        }
        map.insert(name.to_string(), dynamic_to_json(&value));
    }
    serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string())
}

/// Returns sandbox metadata as JSON: limits and registered builtins.
/// Used by the MCP server to generate resource content dynamically.
#[wasm_bindgen]
pub fn metadata() -> String {
    serde_json::json!({
        "limits": {
            "max_operations": MAX_OPERATIONS,
            "max_call_levels": MAX_CALL_LEVELS,
            "max_string_size": MAX_STRING_SIZE,
            "max_array_size": MAX_ARRAY_SIZE,
            "max_map_size": MAX_MAP_SIZE,
        },
        "builtins": [
            { "name": "svg",     "sig": "svg(content: string)",     "doc": "Set board SVG content. Call once per execution." },
            { "name": "print",   "sig": "print(value)",             "doc": "Print to stdout (returned in tool response)." },
            { "name": "sin",     "sig": "sin(x: f64) -> f64",      "doc": "Sine." },
            { "name": "cos",     "sig": "cos(x: f64) -> f64",      "doc": "Cosine." },
            { "name": "tan",     "sig": "tan(x: f64) -> f64",      "doc": "Tangent." },
            { "name": "asin",    "sig": "asin(x: f64) -> f64",     "doc": "Arc sine." },
            { "name": "acos",    "sig": "acos(x: f64) -> f64",     "doc": "Arc cosine." },
            { "name": "atan",    "sig": "atan(x: f64) -> f64",     "doc": "Arc tangent." },
            { "name": "atan2",   "sig": "atan2(y: f64, x: f64) -> f64", "doc": "Two-argument arc tangent." },
            { "name": "sqrt",    "sig": "sqrt(x: f64) -> f64",     "doc": "Square root." },
            { "name": "abs_f",   "sig": "abs_f(x: f64) -> f64",    "doc": "Absolute value." },
            { "name": "floor",   "sig": "floor(x: f64) -> f64",    "doc": "Floor." },
            { "name": "ceil",    "sig": "ceil(x: f64) -> f64",     "doc": "Ceiling." },
            { "name": "round",   "sig": "round(x: f64) -> f64",    "doc": "Round to nearest integer." },
            { "name": "min_f",   "sig": "min_f(a: f64, b: f64) -> f64", "doc": "Minimum of two floats." },
            { "name": "max_f",   "sig": "max_f(a: f64, b: f64) -> f64", "doc": "Maximum of two floats." },
            { "name": "PI",      "sig": "PI() -> f64",             "doc": "Returns \u{03c0} (3.14159...)." },
            { "name": "TAU",     "sig": "TAU() -> f64",            "doc": "Returns \u{03c4} (6.28318...)." },
            { "name": "to_float","sig": "to_float(x: i64) -> f64", "doc": "Integer to float." },
            { "name": "to_int",  "sig": "to_int(x: f64) -> i64",  "doc": "Float to integer (truncates toward zero)." },
        ],
        "constants": [
            { "name": "WIDTH",  "type": "i64", "doc": "Board width in pixels (read-only, set per execution)." },
            { "name": "HEIGHT", "type": "i64", "doc": "Board height in pixels (read-only, set per execution)." },
        ],
    }).to_string()
}

/// Execute a Rhai script with the given scope and board dimensions.
///
/// Returns a JSON string with the shape:
/// ```json
/// { "svg": "...", "stdout": "...", "scope": "{...}", "error": null }
/// ```
#[wasm_bindgen]
pub fn execute(code: &str, scope_json: &str, width: i64, height: i64) -> String {
    // Clear thread-local state
    SVG_CONTENT.with(|cell| *cell.borrow_mut() = None);
    STDOUT_BUF.with(|buf| buf.borrow_mut().clear());

    let engine = build_engine();

    // Build scope from persisted JSON + inject constants
    let mut scope = scope_from_json(scope_json);
    scope.push_constant("WIDTH", width);
    scope.push_constant("HEIGHT", height);

    // Compile first to catch syntax errors cheaply
    let ast: AST = match engine.compile(code) {
        Ok(ast) => ast,
        Err(e) => {
            let stdout = STDOUT_BUF.with(|buf| buf.borrow().clone());
            let result = ExecResult {
                svg: None,
                stdout,
                scope: scope_json.to_string(),
                error: Some(format!("Compile error: {e}")),
            };
            return serde_json::to_string(&result).unwrap();
        }
    };

    // Execute
    match engine.run_ast_with_scope(&mut scope, &ast) {
        Ok(()) => {
            let svg = SVG_CONTENT.with(|cell| cell.borrow().clone());
            let stdout = STDOUT_BUF.with(|buf| buf.borrow().clone());
            let scope_out = scope_to_json(&scope);
            let result = ExecResult {
                svg,
                stdout,
                scope: scope_out,
                error: None,
            };
            serde_json::to_string(&result).unwrap()
        }
        Err(e) => {
            let svg = SVG_CONTENT.with(|cell| cell.borrow().clone());
            let stdout = STDOUT_BUF.with(|buf| buf.borrow().clone());
            let scope_out = scope_to_json(&scope);
            let result = ExecResult {
                svg,
                stdout,
                scope: scope_out,
                error: Some(format!("{e}")),
            };
            serde_json::to_string(&result).unwrap()
        }
    }
}
