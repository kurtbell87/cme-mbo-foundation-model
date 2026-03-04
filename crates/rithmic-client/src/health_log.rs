//! Structured health event logger.
//!
//! Writes JSON Lines to a file — one event per line.  Every line has at
//! minimum `ts_ms` (Unix milliseconds) and `event` (string).
//!
//! Designed for machine-readable post-run analysis across multiple sessions
//! and instruments.  Parse with `jq`, pandas, or any JSON Lines reader.
//!
//! Example output:
//!   {"ts_ms":1741010700123,"event":"startup","symbol":"MESH6","exchange":"CME"}
//!   {"ts_ms":1741010701456,"event":"snapshot_complete","levels":1688,"buffered":0}
//!   {"ts_ms":1741010710000,"event":"stats","dbo":1200,"bbo":40,"validations":900}
//!   {"ts_ms":1741010711234,"event":"divergence","book_bid":6820000000000,"bbo_bid":6820250000000}
//!   {"ts_ms":1741010711235,"event":"recovery_triggered","post_initial_recoveries":1}
//!   {"ts_ms":1741010711890,"event":"snapshot_complete","levels":1690,"buffered":12}
//!   {"ts_ms":1741010800000,"event":"shutdown","reason":"Ctrl+C"}

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// JSON Lines health event logger.  Clone the Arc to share across tasks.
#[derive(Clone)]
pub struct HealthLogger(Arc<Inner>);

struct Inner {
    writer: Mutex<BufWriter<File>>,
    path: String,
}

impl HealthLogger {
    /// Open (or create) a JSON Lines log file at `path`.
    pub fn open(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        eprintln!("[health] logging to {path}");
        Ok(Self(Arc::new(Inner {
            writer: Mutex::new(BufWriter::new(file)),
            path: path.to_string(),
        })))
    }

    pub fn path(&self) -> &str {
        &self.0.path
    }

    /// Write a single JSON event line.
    ///
    /// `fields` should be a `serde_json::json!({...})` object.  `ts_ms` and
    /// `event` are injected automatically — do not include them in `fields`.
    pub fn log(&self, event: &str, fields: serde_json::Value) {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Merge ts_ms + event into fields object
        let mut map = match fields {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        map.insert("ts_ms".into(), serde_json::json!(ts_ms));
        map.insert("event".into(), serde_json::json!(event));

        let line = serde_json::to_string(&serde_json::Value::Object(map))
            .unwrap_or_else(|_| format!(r#"{{"ts_ms":{ts_ms},"event":"{event}","error":"serialize_failed"}}"#));

        if let Ok(mut w) = self.0.writer.lock() {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }
}
