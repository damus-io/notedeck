//! Reconstruct JSONL from kind-1988 nostr events stored in ndb.
//!
//! Queries events by session ID (`d` tag), sorts by `seq` tag,
//! extracts `source-data` tags, and returns the original JSONL lines.

use crate::session_events::{get_tag_value, AI_CONVERSATION_KIND};
use nostrdb::{Filter, Ndb, Transaction};

#[derive(Debug)]
pub enum ReconstructError {
    Query(String),
    Io(String),
}

impl std::fmt::Display for ReconstructError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReconstructError::Query(e) => write!(f, "ndb query failed: {}", e),
            ReconstructError::Io(e) => write!(f, "io error: {}", e),
        }
    }
}

/// Reconstruct JSONL lines from ndb events for a given session ID.
///
/// Returns lines in original order (sorted by `seq` tag), suitable for
/// writing to a JSONL file or feeding to `claude --resume`.
pub fn reconstruct_jsonl_lines(
    ndb: &Ndb,
    txn: &Transaction,
    session_id: &str,
) -> Result<Vec<String>, ReconstructError> {
    let filters = [Filter::new()
        .kinds([AI_CONVERSATION_KIND as u64])
        .tags([session_id], 'd')
        .limit(10000)
        .build()];

    // Use ndb.fold to iterate events without collecting QueryResults
    let mut entries: Vec<(u32, String)> = Vec::new();

    let _ = ndb.fold(txn, &filters, &mut entries, |entries, note| {
        let seq = get_tag_value(&note, "seq").and_then(|s| s.parse::<u32>().ok());
        let source_data = get_tag_value(&note, "source-data");

        // Only events with source-data contribute JSONL lines.
        // Split events only have source-data on the first event (i=0),
        // so we naturally get one JSONL line per original JSONL line.
        if let (Some(seq), Some(data)) = (seq, source_data) {
            entries.push((seq, data.to_string()));
        }

        entries
    });

    // Sort by seq for original ordering
    entries.sort_by_key(|(seq, _)| *seq);

    // Deduplicate by source-data content (safety net for re-ingestion)
    entries.dedup_by(|a, b| a.1 == b.1);

    Ok(entries.into_iter().map(|(_, data)| data).collect())
}

/// Reconstruct JSONL and write to a file.
///
/// Returns the number of lines written.
pub fn reconstruct_jsonl_file(
    ndb: &Ndb,
    txn: &Transaction,
    session_id: &str,
    output_path: &std::path::Path,
) -> Result<usize, ReconstructError> {
    let lines = reconstruct_jsonl_lines(ndb, txn, session_id)?;
    let count = lines.len();

    use std::io::Write;
    let mut file =
        std::fs::File::create(output_path).map_err(|e| ReconstructError::Io(e.to_string()))?;

    for line in &lines {
        writeln!(file, "{}", line).map_err(|e| ReconstructError::Io(e.to_string()))?;
    }

    Ok(count)
}
