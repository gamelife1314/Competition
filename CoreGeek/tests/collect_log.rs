//! `tools/collect_log.py` is the delivery interface for the six evidence tables.
//!
//! WORKFLOW_REQUEST §7.3 hands the workflow driver ONE command whose output is
//! pasted into the issue whole. That makes the script an interface in the same
//! sense `log::ledger_record` is one, and it fails in the same silent way: the
//! tables select on event names, and a renamed event does not produce an error
//! — it produces an empty section, which every reader downstream is entitled to
//! read as "this did not happen".
//!
//! Nothing in the Rust build sees the script, so nothing else can catch that.
//! These tests read it and hold it against the emitters in `src/`, against the
//! row budget the docs promise, and against the command the docs tell the
//! driver to run.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate lives in a repository directory")
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("{rel} is part of the delivery path, so it has to be there: {err}")
    })
}

fn collector() -> String {
    read("tools/collect_log.py")
}

/// Every event name the collector filters on, i.e. every `pick(records, "x")`.
///
/// The script routes *all* event filtering through one helper precisely so this
/// extraction has a single shape to look for; a table that reached into
/// `r.get("event")` itself would be invisible here.
fn selected_events(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find("pick(") {
        rest = &rest[at + "pick(".len()..];
        let end = rest
            .find(')')
            .expect("a pick(...) call is closed on the same line");
        // `records, "buy", "sell"` — the quoted arguments are the event names.
        for (i, part) in rest[..end].split('"').enumerate() {
            if i % 2 == 1 {
                out.insert(part.to_string());
            }
        }
        rest = &rest[end..];
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

/// The `CAPS` table: one row budget per table, the single source of the budget.
fn caps(text: &str) -> Vec<(String, usize)> {
    let start = text
        .find("CAPS = {")
        .expect("the script declares a CAPS table — it is where the row budget lives");
    let body = &text[start..];
    let end = body.find('}').expect("the CAPS table is closed");
    body[..end]
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once("\":")?;
            let name = name.rsplit('"').next()?.to_string();
            if name.is_empty() {
                return None;
            }
            let value = rest.split('#').next()?.trim().trim_end_matches(',').trim();
            Some((name, value.parse().ok()?))
        })
        .collect()
}

#[test]
fn every_event_the_collector_reads_is_an_event_we_actually_emit() {
    // The failure this exists for: rename `wall_gate_open` in day.rs and the
    // gate table keeps printing, in the same shape, with "（0 行）" where the
    // answer was. A dropped `round` field reads as a fact about the match; a
    // dropped event name reads as a fact about the match too, and neither
    // raises anything.
    let read = selected_events(&collector());
    assert!(!read.is_empty(), "the collector selects on no events at all");
    let emitted = emitted_events();
    assert!(
        !emitted.is_empty(),
        "found no log::event call sites — the scan, not the script, is wrong"
    );

    let missing: Vec<&String> = read.difference(&emitted).collect();
    assert!(
        missing.is_empty(),
        "these events are selected by tools/collect_log.py but emitted \
         nowhere in src/ (renamed? typo?): {missing:?}\n\
         emitted: {emitted:?}"
    );
}

#[test]
fn the_collector_still_carries_every_section() {
    // §7.3 names six tables printed as thirteen sections, and the issue is read
    // as "all of them, or an explanation". A section quietly dropped from the
    // script is a table quietly dropped from every future report.
    let text = collector();
    for marker in [
        "表 0 · 分数归属",
        "表 1 · 造塔计划",
        "表 2a",
        "表 3a",
        "表 4a",
        "表 4c · 每条沙盒命令",
        "表 5a · 封门",
        "表 6a · 夜间沉默",
        "表 7 · 对手建造节奏",
    ] {
        assert!(text.contains(marker), "the collector lost its `{marker}` section");
    }
    assert!(
        text.contains("（0 行）"),
        "an empty table must say so: 'the selector matched zero rows' and 'it \
         did not happen' are different claims, and the script is where the \
         driver learns to tell them apart"
    );
}

#[test]
fn the_row_budget_lives_in_the_script_and_adds_up() {
    // The budget is enforced in the script rather than left to the driver's
    // judgement, because the driver is not the one who pays when the issue body
    // is a file dump. A cap that truncates silently would be worse than no cap,
    // so the script also has to declare the real length — and the caps have to
    // stay inside the 560 lines §7.3 and §8 promise.
    let text = collector();
    assert!(
        text.contains("本表共"),
        "a truncated table has to declare its real length"
    );

    let table = caps(&text);
    assert_eq!(
        table.len(),
        17,
        "expected one budget per section, got {table:?}"
    );
    assert!(
        text.contains("CAP_TOTAL = sum(CAPS.values())"),
        "the total must be summed, not typed out a second time where it can \
         drift from the table"
    );

    for (name, _) in &table {
        assert!(
            text.contains(&format!("CAPS[\"{name}\"]")),
            "`{name}` has a budget but no section spends it"
        );
    }
    assert_eq!(
        text.matches("CAPS[\"").count(),
        17,
        "a section is spending a budget that is not in the CAPS table"
    );

    let total: usize = table.iter().map(|(_, cap)| cap).sum();
    assert!(
        total <= 660,
        "the caps add up to {total} lines, over the 660 the docs promise"
    );
}

#[test]
fn the_documented_command_points_at_files_that_exist() {
    // The doc is the only thing the driver reads. A command naming a file that
    // is not in the checkout is worse than no command: the driver reports
    // "跑不了" and the batch arrives with no evidence at all.
    let doc = read("docs/WORKFLOW_REQUEST.md");
    assert!(
        doc.contains("collect_log.py"),
        "§7.3 has to name the collector the driver runs"
    );
    for token in doc.split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '.' || c == '_')) {
        if let Some(rel) = token.strip_prefix("tools/") {
            let rel = format!("tools/{rel}");
            assert!(
                repo_root().join(&rel).exists(),
                "the doc tells the driver to run `{rel}`, which does not exist"
            );
        }
    }
}

#[test]
fn the_shell_wrapper_only_finds_an_interpreter() {
    // The wrapper exists for Git Bash and for the Linux/macOS side of the
    // project; the Windows path is `python tools\collect_log.py` directly. It
    // must not grow a second implementation of any table — two implementations
    // is a choice the driver cannot make and a drift nothing would catch.
    let wrapper = read("tools/collect_log.sh");
    assert!(
        wrapper.contains("collect_log.py"),
        "the wrapper has to hand off to the collector"
    );
    assert!(
        !wrapper.contains(".event==") && !wrapper.contains("pick("),
        "the wrapper is a launcher, not a second copy of the tables"
    );
    assert!(
        // `command -v` alone would accept the Windows Store stub, which is
        // found on PATH and opens a shop window when executed.
        wrapper.contains("sys.version_info"),
        "the wrapper has to prove the interpreter actually runs"
    );
}
