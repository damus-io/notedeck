//! Orchestrates converting a claude-code session JSONL file into nostr events.
//!
//! Reads the JSONL file line-by-line, builds kind-1988 nostr events with
//! proper threading, and ingests them into the local nostr database.

use crate::session_events::{self, BuiltEvent, ThreadingState};
use crate::session_jsonl::JsonlLine;
use nostrdb::{IngestMetadata, Ndb};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Convert a session JSONL file into nostr events and ingest them locally.
///
/// Returns the ordered list of note IDs for the ingested events.
pub fn convert_session_to_events(
    jsonl_path: &Path,
    ndb: &Ndb,
    secret_key: &[u8; 32],
) -> Result<Vec<[u8; 32]>, ConvertError> {
    let file = File::open(jsonl_path).map_err(ConvertError::Io)?;
    let reader = BufReader::new(file);

    let mut threading = ThreadingState::new();
    let mut note_ids = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line.map_err(ConvertError::Io)?;
        if line.trim().is_empty() {
            continue;
        }

        let parsed = JsonlLine::parse(&line)
            .map_err(|e| ConvertError::Parse(format!("line {}: {}", line_num + 1, e)))?;

        let events = session_events::build_events(&parsed, &mut threading, secret_key)
            .map_err(|e| ConvertError::Build(format!("line {}: {}", line_num + 1, e)))?;

        for event in events {
            ingest_event(ndb, &event)?;
            note_ids.push(event.note_id);
        }
    }

    Ok(note_ids)
}

/// Ingest a single built event into the local ndb.
fn ingest_event(ndb: &Ndb, event: &BuiltEvent) -> Result<(), ConvertError> {
    ndb.process_event_with(&event.json, IngestMetadata::new().client(true))
        .map_err(|e| ConvertError::Ingest(format!("{:?}", e)))?;
    Ok(())
}

#[derive(Debug)]
pub enum ConvertError {
    Io(std::io::Error),
    Parse(String),
    Build(String),
    Ingest(String),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::Io(e) => write!(f, "IO error: {}", e),
            ConvertError::Parse(e) => write!(f, "parse error: {}", e),
            ConvertError::Build(e) => write!(f, "build error: {}", e),
            ConvertError::Ingest(e) => write!(f, "ingest error: {}", e),
        }
    }
}
