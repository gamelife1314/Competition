//! JSONL file logging for post-match analysis.
//!
//! We never see a live match, so every round writes one compact `round`
//! record (state digest + issued commands + timing) and strategic events
//! (news parsing, blacklisting, task/treasure transitions, LLM traffic)
//! append their own records. Default file: `./coregeek.log`, override with
//! the `COREGEEK_LOG` environment variable. Logging never fails the round:
//! every error path is ignored.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn log_file() -> Option<&'static Mutex<File>> {
    static FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    FILE.get_or_init(|| {
        let mut paths: Vec<String> = Vec::new();
        if let Ok(custom) = std::env::var("COREGEEK_LOG") {
            paths.push(custom);
        }
        paths.push("coregeek.log".into());
        paths.push("/tmp/coregeek.log".into());
        for path in paths {
            if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
                return Some(Mutex::new(file));
            }
        }
        None
    })
    .as_ref()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Append one JSONL record: {"ts":..., "event":..., "data":{...}}
pub fn event(name: &str, data: serde_json::Value) {
    let Some(mutex) = log_file() else { return };
    let mut file = mutex.lock().unwrap_or_else(|err| err.into_inner());
    let record = serde_json::json!({ "ts": now_ms(), "event": name, "data": data });
    let _ = writeln!(file, "{record}");
}

/// Truncate long strings (LLM traffic, task texts) before logging.
pub fn brief(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect::<String>() + "…"
    }
}
