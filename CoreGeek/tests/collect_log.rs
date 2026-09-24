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
    // Recursive walk (issue #221): emitters now also live in subdirectories
    // like `brain/action/` and `brain/role/`, and a flat scan silently loses
    // them — the audit then reports a live event as "emitted nowhere". Sorted
    // so the first-match-by-name payload lookups stay deterministic.
    let mut files = vec![src.join("main.rs"), src.join("log.rs")];
    let mut stack = vec![src.to_path_buf()];
    let mut walked = Vec::new();
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().map_or(false, |ext| ext == "rs") {
                    walked.push(path);
                }
            }
        }
    }
    walked.sort();
    files.extend(walked);

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

/// Every `log::event("<name>", <payload>)` call site in `src/`, as
/// `(name, payload source text)`.
///
/// The payload is sliced by parenthesis depth rather than by line, because the
/// call sites are multi-line `serde_json::json!({...})` blocks and the question
/// here is which FIELDS are inside them — not how they are formatted.
fn emitted_payloads() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    // Recursive walk (issue #221): emitters now also live in subdirectories
    // like `brain/action/` and `brain/role/`, and a flat scan silently loses
    // them — the audit then reports a live event as "emitted nowhere". Sorted
    // so the first-match-by-name payload lookups stay deterministic.
    let mut files = vec![src.join("main.rs"), src.join("log.rs")];
    let mut stack = vec![src.to_path_buf()];
    let mut walked = Vec::new();
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().map_or(false, |ext| ext == "rs") {
                    walked.push(path);
                }
            }
        }
    }
    walked.sort();
    files.extend(walked);

    let mut out = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut rest = text.as_str();
        while let Some(at) = rest.find("log::event(") {
            rest = &rest[at + "log::event(".len()..];
            let Some(open) = rest.find('"') else { continue };
            let after = &rest[open + 1..];
            let Some(close) = after.find('"') else { continue };
            let name = after[..close].to_string();
            // Walk to the `)` that closes `log::event(`, counting depth.
            let mut depth = 1usize;
            let mut end = rest.len();
            for (i, ch) in rest.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.push((name, rest[..end].to_string()));
            rest = &rest[end.min(rest.len())..];
        }
    }
    out
}

/// The body of a method, from `fn <name>(` to its closing brace at the same
/// indent. `function_body` (below) only finds top-level `def`s, i.e. the Python
/// side; this is the Rust side's `impl` method.
fn method_body(text: &str, name: &str) -> String {
    let opener = format!("fn {name}(");
    let at = text
        .find(&opener)
        .unwrap_or_else(|| panic!("src/ no longer defines `{name}`"));
    let rest = &text[at..];
    let mut depth = 0usize;
    let mut started = false;
    for (i, ch) in rest.char_indices() {
        match ch {
            '{' => {
                depth += 1;
                started = true;
            }
            '}' => {
                depth -= 1;
                if started && depth == 0 {
                    return rest[..i].to_string();
                }
            }
            _ => {}
        }
    }
    rest.to_string()
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
    // The failure this exists for: rename `night_debug` in night.rs and the
    // night tables keep printing, in the same shape, with "（0 行）" where the
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
        "表 6a · 夜间沉默",
        "表 7 · 对手建造节奏",
        "表 8 · 我方分数三块",
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
    // stay inside the lines §7.3 and §8 promise (700 since v18, reached exactly
    // at v22). Issue #221 4b retired the seal tables (表 5a/5b/5c, 200 lines)
    // and the far-edge table (表 13, 12) together with their events — the
    // entrance is permanent and the wall order is fixed, so "was the gate
    // sealed" and "which shoulder closed" are no longer things that can
    // happen — and the budget went with them: seventeen tables, 488 lines.
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
        total <= 700,
        "the caps add up to {total} lines, over the 700 the docs promise"
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

/// The body of a top-level `def name(...)` in the collector, up to the next
/// top-level definition.
fn function_body(text: &str, name: &str) -> String {
    let opener = format!("\ndef {name}(");
    let at = text
        .find(&opener)
        .unwrap_or_else(|| panic!("the collector defines `{name}`"))
        + 1;
    let rest = &text[at..];
    let end = rest[1..]
        .find("\ndef ")
        .map(|offset| offset + 1)
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn the_base_columns_are_read_through_the_carry_forward_not_the_raw_key() {
    // `stationHp` / `stationLvl` / `enemyStationHp` are written by a DELTA
    // mechanism: `brain::mod::respond`'s `gated` loop only re-sends the `base`
    // block on the round it CHANGES (`chg` names it), so on a quiet round the
    // key is absent from the record entirely. Reading it with a bare
    // `block.get(...)` therefore renders "the base took no damage this round"
    // as "the base's HP is unknown".
    //
    // That is what happened in the ten reports behind issues #161-#170: table
    // 0's 我方基地 column is blank in nine of them, and table 8's likewise,
    // because the round the script samples is a day's last round and the base
    // had usually last been hit a round or two earlier. The column is the only
    // direct evidence of `score_3` (capped at 550) and of whether a station
    // upgrade ever landed, so losing it costs the next batch its measurement.
    //
    // These two tables must resolve those keys with `carried(rounds, ...)`,
    // which replays the deltas. The assertion is on the table builders and not
    // on the helper's existence: a bare `block.get("stationHp")` anywhere in
    // either body fails this test, whatever else the body does.
    let text = collector();
    assert!(
        function_body(&text, "score_rows").contains("carried("),
        "table 0 has to replay the delta-coded base fields"
    );
    assert!(
        function_body(&text, "score_split_rows").contains("carried("),
        "table 8 has to replay the delta-coded base fields"
    );
    for name in ["score_rows", "score_split_rows"] {
        let body = function_body(&text, name);
        for key in ["stationHp", "stationLvl", "enemyStationHp"] {
            let raw = format!("block.get(\"{key}\")");
            assert!(
                !body.contains(&raw),
                "{name} reads `{raw}` directly; that key is only present on the \
                 round it changes, so the column blanks out on quiet rounds"
            );
        }
    }
}

#[test]
fn the_base_level_is_a_column_so_an_upgrade_is_visible() {
    // `stationLvl` is the one field that says whether a station upgrade was
    // ever bought — 任务书 4.6.1: 1500 HP at level 1, 3000 at level 2, 4500 at
    // level 3, and no item in the game restores station HP. Without it in the
    // tables the next batch can only infer the purchase from a slower HP
    // decay. Both headers must name the column they print.
    let text = collector();
    for (name, marker) in [("表 0", "我方基地等级"), ("表 8", "我方基地等级")] {
        assert!(
            text.contains(marker),
            "{name} has to name the {marker} column it prints"
        );
        let _ = name;
    }
    for name in ["score_rows", "score_split_rows"] {
        assert!(
            function_body(&text, name).contains("stationLvl"),
            "{name} prints the level column"
        );
    }
}

#[test]
fn the_fields_the_new_tables_join_on_are_actually_emitted() {
    // A table that prints `（0 行）` because its event is missing is one kind of
    // silent failure; a table that prints a column of blanks because the EVENT is
    // there but the FIELD is not is the other, and it looks like a fact about the
    // match. 表 12 joins the news ask to its answer by the round each happened on,
    // and 表 12's `问给谁` column reads the consumer that won the round's prompt
    // slot — so each of those fields has to be in the record, not merely intended.
    //
    // v22's whole point is that these were the blind spots: `news_read` and
    // `news_read_failed` carried a `day` and no `round`, so "asked at X, answered
    // at Y" could not be asked of the log at all; `prompt_sent` did not say who
    // asked; and `day_earn` did not exist. (`wall_far_edge` joined this list at
    // v22 and left again with issue #221 4b, which fixed the wall order.)
    let payloads = emitted_payloads();
    for (event, fields) in [
        ("news_read_ask", &["day", "round"][..]),
        ("news_read", &["day", "round", "readings"][..]),
        ("news_read_failed", &["day", "round"][..]),
        ("prompt_sent", &["round", "day", "purpose", "chars"][..]),
    ] {
        let body = payloads
            .iter()
            .find(|(name, _)| name == event)
            .map(|(_, body)| body)
            .unwrap_or_else(|| panic!("src/ emits no `{event}` record"));
        for field in fields {
            assert!(
                body.contains(&format!("\"{field}\"")),
                "the `{event}` record has no `{field}`, so the table that reads it \
                 prints a blank column where the answer was:\n{body}"
            );
        }
    }

    // `day_earn` is the exception: its payload is the one call,
    // `state.earn.record(&turn)`, so the fields live on `DayEarn::record` instead.
    let state = read("CoreGeek/src/state.rs");
    let record = method_body(&state, "record");
    for field in ["day", "round", "mined", "sold", "soldGold", "unlabelled", "gold"] {
        assert!(
            record.contains(&format!("\"{field}\"")),
            "`DayEarn::record` does not write `{field}`, so the row 表 12 prints \
             for that column is blank:\n{record}"
        );
    }
}

/// An interpreter that actually runs, or `None`.
///
/// `tools/collect_log.sh` probes for one the same way and for the same reason:
/// `command -v python3` alone accepts the Windows Store stub, which is found on
/// PATH and opens a shop window when executed. The script has to run on a
/// Windows box with no `python3` alias, so both names are tried.
fn interpreter() -> Option<String> {
    for name in ["python3", "python"] {
        let ok = std::process::Command::new(name)
            .arg("-c")
            .arg("import sys; sys.exit(0 if sys.version_info[0] == 3 else 1)")
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if ok {
            return Some(name.to_string());
        }
    }
    None
}

#[test]
fn the_earning_table_prints_the_rows_the_events_carry() {
    // The budget test above holds the SCRIPT'S TEXT against the promise; this
    // one runs it. The two are not the same check: a section can be present,
    // spent, counted and spelled right and still print the wrong column, crash
    // on a `None`, or lose a row to a join key that does not match — and none of
    // that raises anything in Python, which is a language where a missing field
    // is an empty cell and an empty cell reads as a fact about the match.
    //
    // The fixture is one day-round per day, hand-written from the emitters'
    // payloads in `src/`: 表 12 joins `day_earn` to the news events by day, and
    // both drills read `prompt_sent` / `cmd_sent` / the news events back by
    // round. (The `wall_far_edge` rows this fixture used to carry left with
    // issue #221 4b, which fixed the wall order and deleted the event.)
    let Some(python) = interpreter() else {
        eprintln!("（没有可用的 python3/python，跳过：这一格查的是脚本跑起来的样子）");
        return;
    };
    let log = std::env::temp_dir().join("collect_log_contract_fixture.jsonl");
    std::fs::write(
        &log,
        concat!(
            r#"{"event":"news_read_ask","data":{"day":1,"round":1,"attempt":1,"keyword":3}}"#,
            "\n",
            r#"{"event":"prompt_sent","data":{"round":1,"day":1,"purpose":"news","chars":812,"head":"你是"}}"#,
            "\n",
            r#"{"event":"news_read","data":{"day":1,"round":4,"readings":3,"replaced":0}}"#,
            "\n",
            r#"{"event":"day_earn","data":{"round":70,"day":1,"mined":["stone",21],"sold":[],"soldGold":0,"gold":25,"unlabelled":0}}"#,
            "\n",
            r#"{"event":"news_read_failed","data":{"day":2,"round":131,"attempts":1,"head":"not json"}}"#,
            "\n",
            r#"{"event":"day_earn","data":{"round":200,"day":2,"mined":["iron",4],"sold":["iron",4],"soldGold":32,"gold":57,"unlabelled":2}}"#,
            "\n",
        ),
    )
    .expect("the fixture log is writable");
    let script = repo_root().join("tools/collect_log.py");
    let run = |extra: &[&str]| -> String {
        let out = std::process::Command::new(&python)
            .arg(&script)
            .arg(&log)
            .args(extra)
            .output()
            .expect("the collector runs");
        assert!(
            out.status.success(),
            "collect_log.py exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let text = run(&[]);

    // 表 12: the day's earning on the same row as the news round trip. Day 1
    // mined 21 stone, sold nothing, and asked on the day's FIRST round; day 2
    // lost an answer and sold 4 iron for 32.
    let earn = text
        .split("表 12 ·")
        .nth(1)
        .expect("表 12 is in the output");
    let rows: Vec<&str> = earn
        .lines()
        .filter(|line| line.starts_with('1') || line.starts_with('2'))
        .collect();
    assert!(
        rows.iter()
            .any(|row| row.contains(r#"["stone",21]"#) && row.split('\t').nth(8) == Some("1")),
        "表 12 did not print day 1's earning beside the day-round the news was \
         asked on:\n{earn}"
    );
    assert!(
        rows.iter().any(|row| row.contains("32") && row.split('\t').nth(10) == Some("1")),
        "表 12 did not print day 2's sale or its one lost answer:\n{earn}"
    );
    assert!(
        rows.iter().any(|row| row.split('\t').nth(7) == Some("-")),
        "表 12 does not say `-` for a day whose news was never asked about — a \
         blank cell there would read as a fact about the match:\n{earn}"
    );

    // And the per-round half, which only `--day` prints: the purpose the prompt
    // went out under, and the ask's round beside the answer's.
    let drill = run(&["--day", "1"]);
    assert!(
        drill.contains("drill · 第 1 天模型往返逐回合"),
        "`--day 1` does not print the per-round LLM traffic"
    );
    assert!(
        drill.contains("news\t812"),
        "the prompt record's purpose or length is missing from the drill:\n{drill}"
    );
    let news = drill
        .split("drill · 第 1 天新闻往返")
        .nth(1)
        .expect("the news round trip is in the drill");
    assert!(
        news.contains("news_read_ask") && news.contains("news_read"),
        "the news drill does not print the ask beside the answer:\n{news}"
    );
}
