//! The stdout traffic log: the whole request, and the two response fields the
//! round is actually about under their own prefixes.
//!
//! The owner, after the first round of a match: 「现在的 REQ 日志会被截断，修复下」
//! and 「第一回合收到新闻之后我发现并没有第一时间向大模型请求」. The second one was
//! answered by `tests/news_llm.rs` — the ask DOES go out on round 1 — so what
//! was left is the first: the round asked, and the log line that would have
//! shown it stopped at 500 characters.
//!
//! It stopped there for a reason that is about the two payloads' SHAPE rather
//! than their size. Our response serialises `roleCommandMap`, then `prompt`,
//! then `executeCmd` — so the two fields worth reading are the last two, and a
//! prefix of the blob is the command map and nothing else. The platform's
//! request puts `worldNews` (and `llmResp`, when the model has answered) at the
//! same end. A slice taken from the front is a log that reliably shows the part
//! nobody asked about.
//!
//! `server::request_line` / `server::response_lines` are pure functions for
//! exactly this test's sake: stdout belongs to the process that writes it, so
//! the assertion is on the lines the server would print, driven by a real round
//! through the same entry point the HTTP handler uses.

use serde_json::json;

use coregeek::brain::decide_with;
use coregeek::server::{request_line, response_lines, TRAFFIC_CAP};
use coregeek::state::{BotState, TaskStage};

/// The day's news, parked at the end of the request the way the platform sends
/// it — and the way the old 500-character slice was guaranteed to miss it.
const NEWS: &str = "矿业协会通告：铜矿库存充足，需求下降，预计价格下跌，各矿区请提前安排出货。";

/// A daytime board with a worker, a pioneer and one iron vein, plus the day's
/// official news. The pioneer is not decoration: the news read is asked for in
/// the pioneer's step of the day plan, so a board without one asks nothing.
fn board(round_no: i64) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 5, "y": 9}, "neutralType": "iron"},
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": 75, "totalScore": 0, "playerTasks": [],
            "roles": [
                {"id": 10010, "pos": {"x": 5, "y": 5}, "roleType": "worker",
                 "health": 220, "attackPower": 0, "attackRange": 0,
                 "level": 1, "backPackCapability": 4, "backpack": []},
                {"id": 10011, "pos": {"x": 14, "y": 14}, "roleType": "pioneer",
                 "health": 200, "attackPower": 0, "attackRange": 0,
                 "level": 1, "backPackCapability": 40, "backpack": []},
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "vendorShopList": [
            {"name": "stone", "price": 2},
            {"name": "iron", "price": 8},
        ],
        "weaponShopList": [],
        "worldNews": {"officialNews": NEWS},
    }))
    .expect("payload serialises")
}

/// A session holding a script, so the round goes out with an `executeCmd`.
fn session_with_a_script() -> BotState {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.timeout_round = 999;
    state.task.description = "请阅读task_1_alpha.md，获取任务信息".into();
    state.task.description_round = 4;
    state.task.stage = TaskStage::HavePlan {
        cmd: "find /tmp/selfEvolutionTask -maxdepth 4\n".into(),
    };
    state
}

/// The payload of a `[PREFIX …]` line, i.e. everything after the first `] `.
fn payload(line: &str) -> &str {
    line.split_once("] ").map(|(_, rest)| rest).unwrap_or("")
}

/// The one line under `prefix`, or a panic naming what was there instead.
fn line_with<'a>(lines: &'a [String], prefix: &str) -> &'a str {
    let found: Vec<&String> = lines.iter().filter(|line| line.starts_with(prefix)).collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one `{prefix}` line, got {found:#?} out of {lines:#?}"
    );
    found[0]
}

#[test]
fn the_request_is_printed_whole_not_up_to_the_interesting_part() {
    // The owner's complaint, reproduced: the news is the LAST key of the
    // request, so a 500-character prefix of it is the map and the role list.
    let body = board(1);
    assert!(
        body.len() > 500,
        "test setup: this board is small enough that a 500-char slice would hold it"
    );
    let line = request_line(&body);
    assert!(
        line.starts_with("[REQ "),
        "the request line has to stay greppable: {line:?}"
    );
    assert!(
        line.contains(NEWS),
        "the day's news is not on the request line — the platform's request is \
         still being cut before the fields that move the round:\n{line}"
    );
    // ...and the whole body, not just the news: every key of the request is
    // somewhere on the line.
    for key in ["roundNo", "mapInfo", "teamOur", "teamEnemy", "worldNews"] {
        assert!(line.contains(key), "the request line lost `{key}`:\n{line}");
    }
    let text = std::str::from_utf8(&body).expect("the board is UTF-8");
    assert!(
        line.ends_with(text),
        "the line is the request, or it is not:\n{line}"
    );
}

#[test]
fn a_pretty_printed_body_still_costs_one_line() {
    // The platform sometimes sends the body indented. The record is one round
    // per line — a body that spans forty of them is a body no grep finds.
    let body = br#"{
        "roundNo": 1,
        "mapInfo": {"width": 41, "height": 32}
    }"#;
    let line = request_line(body);
    assert!(
        !line.contains('\n'),
        "the request line is not one line:\n{line}"
    );
    assert!(
        line.contains(r#""roundNo": 1"#),
        "flattening a body must not eat the values:\n{line}"
    );
    assert!(
        line.contains(r#""width": 41"#),
        "flattening a body must not eat the values:\n{line}"
    );
}

#[test]
fn the_prompt_is_on_its_own_line_and_it_is_the_whole_prompt() {
    // A real round through the entry point the HTTP handler uses.
    let mut state = BotState::default();
    let body = board(1);
    let out = decide_with(&mut state, &body).expect("round 1 decides");
    let response: serde_json::Value = serde_json::from_str(&out).expect("response is JSON");
    let prompt = response["prompt"]
        .as_str()
        .expect("a prompt went out on round 1");
    assert!(
        prompt.contains(NEWS),
        "test setup: this round's prompt is not the news read"
    );

    let lines = response_lines(&out);
    assert_eq!(
        lines[0],
        format!("[RESP {} bytes] {out}", out.len()),
        "the first response line is the whole response"
    );
    let field = line_with(&lines, "[RESP prompt ");
    assert!(
        field.contains(&format!("[RESP prompt {} chars]", prompt.chars().count())),
        "the prompt line does not declare the prompt's own length:\n{field}"
    );
    // The field line is a JSON string, so it can be read back — that is what
    // makes "the whole prompt is here" checkable rather than eyeballed.
    let quoted = payload(field);
    let decoded: String =
        serde_json::from_str(quoted).expect("the prompt line is the prompt, JSON-quoted");
    assert_eq!(
        decoded, prompt,
        "the prompt on its own line is not the prompt that went on the wire"
    );
    assert!(
        field.contains(NEWS),
        "the news is not visible in the prompt line:\n{field}"
    );
    assert!(
        !field.contains('\n'),
        "the prompt's own line breaks have to stay escaped, or the record \
         stops being one line:\n{field}"
    );
}

#[test]
fn the_execute_cmd_is_on_its_own_line_and_it_is_the_whole_script() {
    // A round that sends a script, driven through the same entry point.
    let mut state = session_with_a_script();
    let body = board(5);
    let out = decide_with(&mut state, &body).expect("round 5 decides");
    let response: serde_json::Value = serde_json::from_str(&out).expect("response is JSON");
    let cmd = response["executeCmd"]
        .as_str()
        .expect("a command went out with the session");
    assert!(cmd.contains("find /tmp/selfEvolutionTask"), "test setup");

    let lines = response_lines(&out);
    let field = line_with(&lines, "[RESP executeCmd ");
    let decoded: String = serde_json::from_str(payload(field)).expect("the command line is quoted");
    assert_eq!(
        decoded, cmd,
        "the command on its own line is not the command that went on the wire"
    );
    // The sandbox prelude is prepended at the boundary, so the line has to show
    // the bytes the judger actually receives, not the model's bare script. This
    // is the half a reader could never see before: the connection between what
    // the model wrote and what ran.
    assert!(
        decoded.starts_with("#!/bin/sh") || decoded.contains("PYTHONUTF8=1"),
        "the line shows the model's script rather than what was sent:\n{field}"
    );
    assert!(decoded.ends_with("find /tmp/selfEvolutionTask -maxdepth 4\n"));
}

#[test]
fn a_round_with_neither_field_prints_one_response_line_and_no_empty_ones() {
    // The control. An empty prompt and an empty `executeCmd` are the ordinary
    // case for most of a match's rounds, and a `[RESP prompt 0 chars] ""` line
    // per round would be 2600 lines of nothing — the same mistake the `round`
    // record's `prune` exists to avoid.
    let plain = r#"{"roleCommandMap":{},"prompt":"","executeCmd":""}"#;
    let lines = response_lines(plain);
    assert_eq!(
        lines,
        vec![format!("[RESP {} bytes] {plain}", plain.len())],
        "an empty field still got a line of its own"
    );
    // ...and a response that is not JSON at all still logs, because the log is
    // the only witness and it must not be the thing that fails.
    let lines = response_lines("not json");
    assert_eq!(lines.len(), 1, "a non-JSON response must still be logged");
}

#[test]
fn a_body_past_the_cap_says_so_instead_of_trailing_off() {
    // The cap exists because a match is 2600 rounds; the marker exists because
    // a silent cut reads as a fact about the round. Asserted at the boundary
    // rather than at 128 KiB so the test stays about the rule.
    let long = "x".repeat(TRAFFIC_CAP + 10);
    let body = serde_json::to_vec(&json!({"roundNo": 1, "pad": long})).expect("payload");
    let line = request_line(&body);
    assert!(
        line.contains("[TRUNCATED: first"),
        "a body over the cap has to declare the cut:\n{}",
        &line[..200]
    );
    let total = std::str::from_utf8(&body).expect("UTF-8").chars().count();
    assert!(
        line.contains(&format!("of {total} chars]")),
        "the marker has to name the real length, not the cap"
    );
}
