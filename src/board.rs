use chrono::{DateTime, Utc};
use pyo3::Py;
use pyo3::types::PyDict;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

#[derive(Clone, Debug)]
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
        format!("http://{}:{}/gallery/board/{}", self.address, self.port, name)
    }
}
