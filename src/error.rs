use std::fmt;

#[derive(Debug)]
pub enum ScryError {
    Python(String),
    SvgParse(String),
    Render(String),
}

impl fmt::Display for ScryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScryError::Python(msg) => write!(f, "Python error: {msg}"),
            ScryError::SvgParse(msg) => write!(f, "SVG parse error: {msg}"),
            ScryError::Render(msg) => write!(f, "Render error: {msg}"),
        }
    }
}

impl std::error::Error for ScryError {}

impl From<pyo3::PyErr> for ScryError {
    fn from(err: pyo3::PyErr) -> Self {
        ScryError::Python(err.to_string())
    }
}

impl From<usvg::Error> for ScryError {
    fn from(err: usvg::Error) -> Self {
        ScryError::SvgParse(err.to_string())
    }
}
