// Databento .dbn.zst file ingestion (Phase 1 — to be implemented)
//
// Will provide:
// - Read .dbn.zst files using the first-party `dbn` crate
// - StreamingBookBuilder: pipe MBO events into BookBuilder, emit snapshots
// - Day-level iteration over data directory
//
// The `dbn` crate is Databento's official Rust library (first-party).

use thiserror::Error;

#[derive(Error, Debug)]
pub enum IngestError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("DBN decode error: {0}")]
    Dbn(String),
}

/// Read MBO events from a .dbn.zst file and process them.
pub fn read_dbn_file(_path: &str) -> Result<(), IngestError> {
    // Placeholder — Phase 1 implementation
    Err(IngestError::Dbn("Not yet implemented".to_string()))
}
