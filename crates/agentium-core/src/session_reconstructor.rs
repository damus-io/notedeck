//! Reconstruct JSONL from kind-1989 source-data nostr events stored in ndb.
//!
//! Queries events by session ID (`d` tag), sorts by `seq` tag,
//! extracts `source-data` tags, and returns the original JSONL lines.

use crate::messages::UsageInfo;
use crate::session_events::{get_tag_value, AI_SOURCE_DATA_KIND};
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
        .kinds([AI_SOURCE_DATA_KIND as u64])
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

/// The latest cumulative token usage for a session, reconstructed from its
/// kind-1989 JSONL archive — or `None` when the archive has no completed turn
/// to report (a session still on its first turn, or a backend like codex that
/// doesn't emit a `result` line).
///
/// The claude CLI's stream-json emits a `type: "result"` line at the end of
/// each turn carrying the turn's *cumulative* `usage`, `total_cost_usd`, and
/// `num_turns`. We reconstruct the archive and hand it to [`parse_latest_usage`]
/// — the freshest such line is the current usage snapshot. This is the only
/// route to usage from ndb: the kind-1988 conversation stream drops it (an
/// assistant event reconstructs to plain text), so the lossless source-data
/// archive is where it survives.
pub fn latest_session_usage(ndb: &Ndb, txn: &Transaction, session_id: &str) -> Option<UsageInfo> {
    let lines = reconstruct_jsonl_lines(ndb, txn, session_id).ok()?;
    parse_latest_usage(&lines)
}

/// Parse the most recent cumulative [`UsageInfo`] out of reconstructed JSONL
/// lines, or `None` if none carry a `result` message.
///
/// Split from [`latest_session_usage`] so the field mapping can be unit-tested
/// without an ndb. The field names mirror the live claude backend's extraction
/// (`input_tokens` / `cache_creation_input_tokens` / `cache_read_input_tokens`
/// / `output_tokens`, plus `total_cost_usd` and `num_turns`) so a reconstructed
/// snapshot matches what the desktop showed live. Walks newest-first and stops
/// at the first `result` line — later turns supersede earlier ones.
fn parse_latest_usage(lines: &[String]) -> Option<UsageInfo> {
    for line in lines.iter().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("result") {
            continue;
        }
        let usage = v.get("usage");
        let extract = |key: &str| {
            usage
                .and_then(|u| u.get(key))
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
        };
        return Some(UsageInfo {
            input_tokens: extract("input_tokens"),
            cache_creation_input_tokens: extract("cache_creation_input_tokens"),
            cache_read_input_tokens: extract("cache_read_input_tokens"),
            output_tokens: extract("output_tokens"),
            cost_usd: v.get("total_cost_usd").and_then(|c| c.as_f64()),
            num_turns: v.get("num_turns").and_then(|n| n.as_u64()).unwrap_or(0) as u32,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_line(turns: u32, input: u64, output: u64, cost: f64) -> String {
        format!(
            r#"{{"type":"result","num_turns":{turns},"total_cost_usd":{cost},"usage":{{"input_tokens":{input},"cache_creation_input_tokens":10,"cache_read_input_tokens":20,"output_tokens":{output}}}}}"#
        )
    }

    #[test]
    fn parses_result_usage_fields() {
        let lines = vec![
            r#"{"type":"assistant","message":{"content":"hi"}}"#.to_string(),
            result_line(3, 100, 50, 0.25),
        ];
        let usage = parse_latest_usage(&lines).expect("a result line yields usage");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.cache_creation_input_tokens, 10);
        assert_eq!(usage.cache_read_input_tokens, 20);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.num_turns, 3);
        assert_eq!(usage.cost_usd, Some(0.25));
        // context = input + both cache buckets, matching the live context bar.
        assert_eq!(usage.context_tokens(), 130);
    }

    #[test]
    fn takes_the_last_result_when_several_turns_archived() {
        let lines = vec![result_line(1, 100, 50, 0.1), result_line(2, 300, 90, 0.4)];
        let usage = parse_latest_usage(&lines).expect("usage present");
        // The freshest cumulative snapshot wins, not the first turn's.
        assert_eq!(usage.num_turns, 2);
        assert_eq!(usage.input_tokens, 300);
        assert_eq!(usage.cost_usd, Some(0.4));
    }

    #[test]
    fn none_when_no_result_line() {
        // A session still mid-turn (or a backend that never emits `result`)
        // has no cumulative snapshot to report.
        let lines = vec![
            r#"{"type":"user","message":{"content":"hello"}}"#.to_string(),
            r#"{"type":"assistant","message":{"content":"working"}}"#.to_string(),
        ];
        assert!(parse_latest_usage(&lines).is_none());
    }

    #[test]
    fn skips_malformed_lines() {
        let lines = vec!["not json at all".to_string(), result_line(1, 42, 7, 0.05)];
        let usage = parse_latest_usage(&lines).expect("the valid result line still parses");
        assert_eq!(usage.input_tokens, 42);
    }

    #[test]
    fn result_without_usage_object_yields_zeroed_tokens() {
        // Some result lines carry only cost/turns; missing token fields read 0
        // rather than dropping the snapshot entirely.
        let line = r#"{"type":"result","num_turns":1,"total_cost_usd":0.02}"#.to_string();
        let usage = parse_latest_usage(&[line]).expect("still a usable snapshot");
        assert_eq!(usage.context_tokens(), 0);
        assert_eq!(usage.num_turns, 1);
        assert_eq!(usage.cost_usd, Some(0.02));
    }
}
