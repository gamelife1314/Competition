//! `tools/collect_log.sh` is the delivery interface for the six evidence tables.
//!
//! WORKFLOW_REQUEST §7.3 hands the workflow driver ONE command whose output is
//! pasted into the issue whole. That makes the script an interface in the same
//! sense `log::ledger_record` is one, and it fails in the same silent way: the
//! recipes select on event names, and a renamed event does not produce an
//! error — it produces an empty section, which every reader downstream is
//! entitled to read as "this did not happen".
//!
//! Nothing in the Rust build sees the script, so nothing else can catch that.
//! These tests read it and hold it against the emitters in `src/`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate lives in a repository directory")
        .to_path_buf()
}

fn script() -> String {
    let path = repo_root().join("tools/collect_log.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{} is the single command WORKFLOW_REQUEST §7.3 asks the workflow \
             driver to run, so it has to be there: {err}",
            path.display()
        )
    })
}

/// Every `select(.event=="x")` in the script, i.e. every event it reads.
fn selected_events(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let needle = ".event==\"";
    let mut rest = text;
    while let Some(at) = rest.find(needle) {
        rest = &rest[at + needle.len()..];
        let end = rest.find('"').expect("the event name is quoted");
        out.insert(rest[..end].to_string());
    }
    out
}

/// Every event name some `log::event("...")` call site emits.
fn emitted_events() -> BTreeSet<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![src.join("main.rs"), src.join("log.rs")];
    for dir in ["brain", "model", ""] {
        let dir = src.join(dir);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
    }

    let mut out = BTreeSet::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // `log::event(` then the first quoted token after it. The call sites
        // write the name inline or on the next line, so this is the name, not
        // some later string.
        let mut rest = text.as_str();
        while let Some(at) = rest.find("log::event(") {
            rest = &rest[at..];
            if let Some(open) = rest.find('"') {
                let after = &rest[open + 1..];
                if let Some(close) = after.find('"') {
                    out.insert(after[..close].to_string());
                }
            }
            rest = &rest["log::event(".len()..];
        }
    }
    out
}

#[test]
fn every_event_the_collector_reads_is_an_event_we_actually_emit() {
    // The failure this exists for: rename `wall_gate_open` in day.rs and the
    // gate table keeps printing, in the same shape, with "（0 行）" where the
    // answer was. A dropped `round` field reads as a fact about the match; a
    // dropped event name reads as a fact about the match too, and neither
    // raises anything.
    let read = selected_events(&script());
    assert!(!read.is_empty(), "the script selects on no events at all");
    let emitted = emitted_events();
    assert!(
        !emitted.is_empty(),
        "found no log::event call sites — the scan, not the script, is wrong"
    );

    let missing: Vec<&String> = read.difference(&emitted).collect();
    assert!(
        missing.is_empty(),
        "these events are selected by tools/collect_log.sh but emitted \
         nowhere in src/ (renamed? typo?): {missing:?}\n\
         emitted: {emitted:?}"
    );
}

#[test]
fn the_collector_still_carries_all_six_tables() {
    // §7.3 names six tables and the issue is read as "six tables or an
    // explanation". A section quietly dropped from the script is a table
    // quietly dropped from every future report.
    let text = script();
    for marker in [
        "表 1 · 造塔计划",
        "表 2a",
        "表 3a",
        "表 4a",
        "表 5a · 封门",
        "表 6a · 夜间沉默",
    ] {
        assert!(text.contains(marker), "the collector lost its `{marker}` section");
    }
    assert!(
        text.contains("（0 行）"),
        "an empty table must say so: 'the recipe matched zero rows' and 'it \
         did not happen' are different claims, and the script is where the \
         driver learns to tell them apart"
    );
}

#[test]
fn the_collector_caps_itself_and_says_when_it_did() {
    // The budget is enforced here rather than left to the driver's judgement,
    // because the driver is not the one who pays when the issue body is a file
    // dump. A cap that truncates silently would be worse than no cap.
    let text = script();
    assert!(
        text.contains("section "),
        "the tables go out through the capping helper"
    );
    assert!(
        text.contains("本表共"),
        "a truncated table has to declare its real length"
    );
    // `section "<title>" <cap>` — the cap is the last token on the line, and
    // the title is full of spaces and full-width punctuation, so it is the only
    // thing that can be read positionally.
    let caps: Vec<usize> = text
        .lines()
        .filter(|line| line.trim_start().starts_with("section \""))
        .filter_map(|line| line.rsplit('"').next()?.trim().parse::<usize>().ok())
        .collect();
    assert!(caps.len() >= 13, "expected thirteen capped sections, got {}", caps.len());
    let total: usize = caps.iter().sum();
    assert!(
        total <= 560,
        "the caps add up to {total} lines, over the 560 the docs promise"
    );
}
