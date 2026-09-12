//! JSONL stdout logging for post-match analysis.
//!
//! We never see a live match, so every round writes one compact `round`
//! record (state digest + issued commands + timing) and strategic events
//! (news parsing, blacklisting, task/treasure transitions, LLM traffic)
//! append their own records. Records are JSONL lines on stdout so the
//! judger's captured process output contains the full match history.
//! Logging never fails the round: every error path is ignored.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Append one JSONL record to stdout: {"ts":..., "event":..., "data":{...}}
///
/// `std::io::Stdout` is internally synchronized and line-buffered, so the
/// record is flushed on the trailing newline without an explicit lock.
pub fn event(name: &str, data: serde_json::Value) {
    let record = serde_json::json!({ "ts": now_ms(), "event": name, "data": data });
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{record}");
}

/// Truncate long strings (LLM traffic, task texts) before logging.
pub fn brief(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect::<String>() + "…"
    }
}
