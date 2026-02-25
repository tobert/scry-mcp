use crate::error::ScryError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use std::ffi::CString;
use std::sync::{Arc, Mutex};

#[pyclass]
struct SvgCallback {
    inner: Arc<Mutex<Option<String>>>,
}

#[pymethods]
impl SvgCallback {
    fn __call__(&self, content: String) -> PyResult<()> {
        *self.inner.lock().unwrap() = Some(content);
        Ok(())
    }
}

pub struct ExecResult {
    pub svg_content: Option<String>,
    pub stdout: String,
}

/// Builtins that are removed from the sandbox. These provide escape routes
/// out of the restricted environment (filesystem access, dynamic imports,
/// code generation).
const BLOCKED_BUILTINS: &[&str] = &[
    "__import__", // dynamic imports bypass sys.modules blocklist
    "open",       // direct filesystem access
    "exec",       // arbitrary code execution from strings
    "eval",       // arbitrary expression evaluation
    "compile",    // compile strings to code objects
    "input",      // reads from stdin (blocks MCP transport)
    "breakpoint", // drops into debugger (blocks)
];

/// Modules blocked by setting to None in sys.modules.
/// This prevents `from X import Y` patterns. Combined with __import__
/// removal, this closes the standard import paths.
const BLOCKED_MODULES: &[&str] = &[
    "os",
    "subprocess",
    "socket",
    "shutil",
    "pathlib",
    "importlib",
    "ctypes",
    "_ctypes",
    "multiprocessing",
    "signal",
    "threading",
    "io",      // FileIO gives raw filesystem access
    "_io",     // C implementation of io
    "tempfile",
    "webbrowser",
    "http",
    "urllib",
    "ftplib",
    "smtplib",
    "poplib",
    "imaplib",
    "xmlrpc",
    "code",
    "codeop",
    "pty",
    "pipes",
    "resource",
];

/// Create a new Python namespace for a board with safe stdlib imports and sandbox.
pub fn create_namespace(py: Python<'_>, width: u32, height: u32) -> PyResult<Py<PyDict>> {
    let globals = PyDict::new(py);

    // Create a sanitized builtins dict (not the module itself)
    let builtins_module = PyModule::import(py, "builtins")?;
    let builtins_dict: Bound<'_, PyDict> = builtins_module.getattr("__dict__")?.cast_into()?;
    let safe_builtins = builtins_dict.copy()?;

    // Remove dangerous builtins
    for name in BLOCKED_BUILTINS {
        let _ = safe_builtins.del_item(*name); // ignore if missing
    }

    globals.set_item("__builtins__", safe_builtins)?;

    // Pre-import safe stdlib modules
    let safe_modules = [
        "math", "random", "json", "re", "textwrap", "itertools", "functools",
        "collections", "colorsys", "hashlib", "string", "dataclasses",
    ];
    for module_name in &safe_modules {
        match PyModule::import(py, *module_name) {
            Ok(m) => { globals.set_item(*module_name, m)?; }
            Err(e) => {
                tracing::warn!("Failed to import {module_name}: {e}");
            }
        }
    }

    // Set canvas dimensions
    globals.set_item("WIDTH", width)?;
    globals.set_item("HEIGHT", height)?;

    // Block dangerous modules in sys.modules
    let sys = PyModule::import(py, "sys")?;
    let sys_modules = sys.getattr("modules")?;
    for module_name in BLOCKED_MODULES {
        sys_modules.set_item(*module_name, py.None())?;
    }

    Ok(globals.into())
}

/// Set up stdout capture. Returns (captured_out, old_stdout) handles.
/// This is done via sys module which we import fresh each time — it does
/// NOT get exposed to user code since io is blocked in sys.modules and
/// __import__ is removed from builtins.
fn setup_stdout_capture<'py>(py: Python<'py>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    // Temporarily unblock io for our own use
    let sys = PyModule::import(py, "sys")?;
    let sys_modules = sys.getattr("modules")?;

    // Save the blocked state, allow io temporarily
    sys_modules.del_item("io")?;
    sys_modules.del_item("_io")?;

    let io_module = PyModule::import(py, "io")?;
    let string_io = io_module.getattr("StringIO")?;
    let captured_out = string_io.call0()?;
    let old_stdout = sys.getattr("stdout")?;
    sys.setattr("stdout", &captured_out)?;

    // Re-block io so user code can't access it
    sys_modules.set_item("io", py.None())?;
    sys_modules.set_item("_io", py.None())?;

    Ok((captured_out, old_stdout))
}

/// Execute Python code in a board's namespace, capturing SVG output and stdout.
pub fn execute_python(
    py: Python<'_>,
    namespace: &Py<PyDict>,
    code: &str,
    width: u32,
    height: u32,
) -> Result<ExecResult, ScryError> {
    let globals = namespace.bind(py);

    // Update dimensions in case they changed
    globals.set_item("WIDTH", width).map_err(ScryError::from)?;
    globals.set_item("HEIGHT", height).map_err(ScryError::from)?;

    // Create SVG callback
    let svg_storage: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let callback = Py::new(
        py,
        SvgCallback {
            inner: Arc::clone(&svg_storage),
        },
    )
    .map_err(ScryError::from)?;
    globals.set_item("svg", callback).map_err(ScryError::from)?;

    // Set up stdout capture (io is temporarily unblocked then re-blocked)
    let (captured_out, old_stdout) = setup_stdout_capture(py).map_err(ScryError::from)?;
    let sys = PyModule::import(py, "sys").map_err(ScryError::from)?;

    // Convert code to CString for py.run
    let c_code = CString::new(code)
        .map_err(|e| ScryError::Python(format!("Code contains null byte: {e}")))?;

    // Execute user code
    let exec_result = py.run(&c_code, Some(globals), None);

    // Restore stdout
    let _ = sys.setattr("stdout", old_stdout);

    // Capture stdout content
    let stdout: String = captured_out
        .call_method0("getvalue")
        .and_then(|v| v.extract())
        .unwrap_or_default();

    // Check execution result
    match exec_result {
        Ok(()) => {
            let svg_content = svg_storage.lock().unwrap().take();
            Ok(ExecResult {
                svg_content,
                stdout,
            })
        }
        Err(py_err) => {
            // Format the traceback for the model to see
            let traceback = py_err.to_string();
            let mut msg = String::new();
            if !stdout.is_empty() {
                msg.push_str("--- stdout ---\n");
                msg.push_str(&stdout);
                msg.push_str("\n--- error ---\n");
            }
            msg.push_str(&traceback);
            Err(ScryError::Python(msg))
        }
    }
}

/// Run Python code in a blocking context, suitable for calling from async code.
pub async fn run_python(
    namespace: Py<PyDict>,
    code: String,
    width: u32,
    height: u32,
) -> Result<(ExecResult, Py<PyDict>), ScryError> {
    tokio::task::spawn_blocking(move || {
        Python::attach(|py| {
            let result = execute_python(py, &namespace, &code, width, height)?;
            Ok((result, namespace))
        })
    })
    .await
    .map_err(|e| ScryError::Python(format!("Task join error: {e}")))?
}

/// Create a new namespace in a blocking context.
pub async fn create_namespace_async(width: u32, height: u32) -> Result<Py<PyDict>, ScryError> {
    tokio::task::spawn_blocking(move || {
        Python::attach(|py| create_namespace(py, width, height).map_err(ScryError::from))
    })
    .await
    .map_err(|e| ScryError::Python(format!("Task join error: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_blocks_import() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            let result = execute_python(py, &ns, "import os", 800, 600);
            assert!(result.is_err(), "import os should fail in sandbox");
        });
    }

    #[test]
    fn test_sandbox_blocks_dunder_import() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            let result = execute_python(py, &ns, "__import__('os')", 800, 600);
            assert!(result.is_err(), "__import__ should not be available");
        });
    }

    #[test]
    fn test_sandbox_blocks_open() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            let result = execute_python(py, &ns, "open('/etc/passwd')", 800, 600);
            assert!(result.is_err(), "open() should not be available");
        });
    }

    #[test]
    fn test_sandbox_blocks_exec() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            let result = execute_python(py, &ns, "exec('x = 1')", 800, 600);
            assert!(result.is_err(), "exec() should not be available");
        });
    }

    #[test]
    fn test_sandbox_blocks_eval() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            let result = execute_python(py, &ns, "eval('1+1')", 800, 600);
            assert!(result.is_err(), "eval() should not be available");
        });
    }

    #[test]
    fn test_sandbox_blocks_io() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            let result = execute_python(py, &ns, "import io", 800, 600);
            assert!(result.is_err(), "import io should fail");
        });
    }

    #[test]
    fn test_safe_modules_available() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            let result = execute_python(py, &ns, "x = math.sqrt(16)\nprint(x)", 800, 600);
            assert!(result.is_ok(), "math should be available: {:?}", result.err());
            let r = result.unwrap();
            assert!(r.stdout.contains("4.0"), "should print 4.0, got: {}", r.stdout);
        });
    }

    #[test]
    fn test_svg_callback() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            let result = execute_python(py, &ns, "svg('<svg></svg>')", 800, 600).unwrap();
            assert_eq!(result.svg_content, Some("<svg></svg>".to_string()));
        });
    }

    #[test]
    fn test_namespace_persistence() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            execute_python(py, &ns, "counter = 1", 800, 600).unwrap();
            let result = execute_python(py, &ns, "counter += 1\nprint(counter)", 800, 600).unwrap();
            assert!(result.stdout.contains('2'), "counter should be 2, got: {}", result.stdout);
        });
    }

    #[test]
    fn test_stdout_capture() {
        Python::attach(|py| {
            let ns = create_namespace(py, 800, 600).unwrap();
            let result = execute_python(py, &ns, "print('hello world')", 800, 600).unwrap();
            assert_eq!(result.stdout.trim(), "hello world");
        });
    }
}
