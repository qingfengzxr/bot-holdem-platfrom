use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use poker_domain::{HandId, RoomId, SeatId};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum LocalAuditError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("audit logger unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientAuditRecord {
    pub ts_unix_ms: u128,
    pub event_kind: String,
    pub room_id: Option<RoomId>,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub payload: Value,
}

#[derive(Debug)]
pub struct LocalAuditLogger {
    file: Mutex<File>,
}

impl LocalAuditLogger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LocalAuditError> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    pub fn append(&self, record: &ClientAuditRecord) -> Result<(), LocalAuditError> {
        let mut guard = self.file.lock().map_err(|_| LocalAuditError::Unavailable)?;
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        guard.write_all(&line)?;
        guard.flush()?;
        Ok(())
    }
}

#[must_use]
pub fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}
