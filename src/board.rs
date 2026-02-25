use chrono::{DateTime, Utc};
use pyo3::Py;
use pyo3::types::PyDict;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

#[derive(Clone, Debug)]
#[allow(dead_code)] // fields stored for future history/undo support
pub struct Snapshot {
    pub svg: String,
    pub png: Vec<u8>,
    pub timestamp: DateTime<Utc>,
}

pub struct Board {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub svg: String,
    pub png: Vec<u8>,
    pub namespace: Py<PyDict>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub history: Vec<Snapshot>,
}

#[derive(Clone, Debug)]
pub struct BoardEvent {
    pub board_name: String,
    pub event_type: BoardEventType,
}

#[derive(Clone, Debug)]
pub enum BoardEventType {
    Created,
    Updated,
}

pub struct AppState {
    pub boards: RwLock<HashMap<String, Board>>,
    pub event_tx: broadcast::Sender<BoardEvent>,
    pub address: String,
    pub port: u16,
}

pub type SharedState = Arc<AppState>;

/// Maximum board name length in bytes.
const MAX_NAME_LEN: usize = 128;

/// Validate a board name. Returns Ok(()) or an error message.
pub fn validate_board_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Board name cannot be empty".into());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!("Board name too long ({} bytes, max {MAX_NAME_LEN})", name.len()));
    }
    if name.contains('/') || name.contains('\0') || name.contains('\n') || name.contains('\r') {
        return Err("Board name cannot contain /, null, or newline characters".into());
    }
    // Must be valid as a URL path segment
    if name.starts_with('.') || name.starts_with(' ') || name.ends_with(' ') {
        return Err("Board name cannot start with '.' or have leading/trailing spaces".into());
    }
    Ok(())
}

/// Escape a string for safe embedding in HTML text content and attributes.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Percent-encode a board name for use in URL paths.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

impl AppState {
    pub fn new(address: String, port: u16) -> SharedState {
        let (event_tx, _) = broadcast::channel(64);
        Arc::new(AppState {
            boards: RwLock::new(HashMap::new()),
            event_tx,
            address,
            port,
        })
    }

    pub fn board_url(&self, name: &str) -> String {
        format!(
            "http://{}:{}/gallery/board/{}",
            self.address,
            self.port,
            url_encode(name)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_board_name() {
        assert!(validate_board_name("hello").is_ok());
        assert!(validate_board_name("my-board-123").is_ok());
        assert!(validate_board_name("").is_err());
        assert!(validate_board_name("a/b").is_err());
        assert!(validate_board_name(".hidden").is_err());
        assert!(validate_board_name(&"x".repeat(200)).is_err());
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("a&b"), "a&amp;b");
        assert_eq!(html_escape(r#"x"y'z"#), "x&quot;y&#x27;z");
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello"), "hello");
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a/b"), "a%2Fb");
    }
}
