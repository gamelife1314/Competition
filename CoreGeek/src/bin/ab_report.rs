//! Multi-opponent A/B report over captured stdout logs.
//!
//! ```text
//! cargo run --release --bin ab_report -- before=logs/before.jsonl after=logs/after.jsonl
//! cargo run --release --bin ab_report -- logs/opponentA.jsonl logs/opponentB.jsonl
//! ```
//!
//! Each `label=path` pair is one battle against one opponent (the label
//! defaults to the file stem). With two or more battles the report appends the
//! delta of each against the first, per objective, because a change that only
//! moves the final total can still be losing score1 to win score2.

use std::process::ExitCode;

fn label_for(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: ab_report [<label>=]<capture.jsonl> [<label>=<capture.jsonl> ...]");
        return ExitCode::from(2);
    }
    let mut reports = Vec::new();
    for arg in &args {
        let (label, path) = match arg.split_once('=') {
            Some((label, path)) => (label.to_string(), path.to_string()),
            None => (label_for(arg), arg.clone()),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("ab_report: {path}: {err}");
                return ExitCode::from(1);
            }
        };
        reports.push(coregeek::abreport::parse_battle(
            &label,
            text.lines().map(str::to_string),
        ));
    }
    print!("{}", coregeek::abreport::render(&reports));
    ExitCode::SUCCESS
}
