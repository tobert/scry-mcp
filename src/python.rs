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

/// Create a new Python namespace for a board with safe stdlib imports and sandbox.
pub fn create_namespace(py: Python<'_>, width: u32, height: u32) -> PyResult<Py<PyDict>> {
    let globals = PyDict::new(py);

    // Set __builtins__
    let builtins = PyModule::import(py, "builtins")?;
    globals.set_item("__builtins__", builtins)?;

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

    // Block dangerous modules
    let sys = PyModule::import(py, "sys")?;
    let sys_modules = sys.getattr("modules")?;
    let blocked = [
        "os", "subprocess", "socket", "shutil", "pathlib",
        "importlib", "ctypes", "multiprocessing", "signal", "threading",
    ];
    for module_name in &blocked {
        sys_modules.set_item(*module_name, py.None())?;
    }

    Ok(globals.into())
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

    // Redirect stdout to capture prints
    let io_module = PyModule::import(py, "io").map_err(ScryError::from)?;
    let string_io = io_module.getattr("StringIO").map_err(ScryError::from)?;
    let captured_out = string_io.call0().map_err(ScryError::from)?;

    let sys = PyModule::import(py, "sys").map_err(ScryError::from)?;
    let old_stdout = sys.getattr("stdout").map_err(ScryError::from)?;
    sys.setattr("stdout", &captured_out).map_err(ScryError::from)?;

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
