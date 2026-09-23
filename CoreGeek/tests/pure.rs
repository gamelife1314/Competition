//! Unit tests for pure helpers: ballistics, task-loop parsing, treasure-plan
//! parsing and the day/night calendar.

use coregeek::brain::combat::{line_cells, within_cone};
use coregeek::brain::task::{
    answer_for_log, exit_code, extract_answer, extract_command, strip_status_line, ANSWER_LOG_CAP,
};
use coregeek::brain::treasure::parse_plan;
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request};
use coregeek::state::keywords_of;

fn pos(x: i32, y: i32) -> Pos {
    Pos { x, y }
}

#[test]
fn cone_constraint() {
    let tower = pos(0, 0);
    assert!(within_cone(tower, &[pos(3, 0), pos(3, 3)])); // 45° apart
    assert!(within_cone(tower, &[pos(3, 0), pos(0, 3)])); // exactly 90°
    assert!(!within_cone(tower, &[pos(3, 0), pos(-3, 0)])); // 180°
    assert!(!within_cone(tower, &[pos(3, 0), pos(-1, 3), pos(3, 3)])); // pair >90°
    assert!(within_cone(tower, &[pos(2, 1)])); // single always ok
}

#[test]
fn line_enumeration() {
    let cells = line_cells(pos(0, 0), pos(3, 3));
    assert_eq!(cells, vec![pos(0, 0), pos(1, 1), pos(2, 2), pos(3, 3)]);
    let cells = line_cells(pos(0, 0), pos(0, 2));
    assert_eq!(cells, vec![pos(0, 0), pos(0, 1), pos(0, 2)]);
    assert_eq!(line_cells(pos(5, 5), pos(5, 5)), vec![pos(5, 5)]);
}

#[test]
fn command_extraction() {
    let resp = "分析如下\n```python\nprint('ANSWER: 42')\n```\n完毕";
    let cmd = extract_command(resp).unwrap();
    assert!(cmd.starts_with("python3 - <<'PYEOF'"), "{cmd}");
    assert!(cmd.contains("ANSWER: 42"));

    let resp = "```bash\ncurl -s localhost:8000/weather?city=beijing\n```";
    let cmd = extract_command(resp).unwrap();
    assert!(cmd.starts_with("curl"), "{cmd}");

    // A bare command with no fence is NOT executable any more: the judger
    // sandbox needs an unambiguous block to run, so prose that merely looks
    // like a command is treated as no command at all.
    assert!(extract_command("ls -la /tmp").is_none());

    assert!(extract_command("我不确定该怎么做").is_none());
}

#[test]
fn result_parsing() {
    assert_eq!(strip_status_line("[exitCode:0]\nhello"), "hello");
    assert_eq!(strip_status_line("[TIMEOUT]\npartial"), "partial");
    assert_eq!(strip_status_line("raw output"), "raw output");

    let output = "querying...\nANSWER: {\"temp\": 21}\n[TRUNCATED]";
    assert_eq!(extract_answer(output).as_deref(), Some("{\"temp\": 21}"));
    // Raw stdout with no ANSWER marker is exploratory output, not an answer.
    let output = "only line";
    assert_eq!(extract_answer(output), None);
    assert_eq!(extract_answer(""), None);
}

#[test]
fn treasure_plan_parsing() {
    let resp = "推理过程……\n{\"pos\":{\"x\":20,\"y\":16},\"items\":[\"StarSand\",\"IronWhistle\"],\"openDay\":5}\n以上";
    let plan = parse_plan(resp, 3).unwrap();
    assert_eq!(plan.pos, pos(20, 16));
    assert_eq!(
        plan.items,
        vec!["StarSand".to_string(), "IronWhistle".to_string()]
    );
    assert_eq!(plan.open_day, 5);

    // Items outside the catalog are dropped; empty item list → no plan.
    assert!(parse_plan(r#"{"pos":{"x":1,"y":1},"items":["Rock"],"openDay":2}"#, 1).is_none());
    // openDay before today is clamped.
    let plan = parse_plan(
        r#"{"pos":{"x":1,"y":1},"items":["StarSand"],"openDay":0}"#,
        4,
    )
    .unwrap();
    assert_eq!(plan.open_day, 4);
    // Out-of-map coordinates rejected.
    assert!(parse_plan(
        r#"{"pos":{"x":99,"y":1},"items":["StarSand"],"openDay":2}"#,
        1
    )
    .is_none());
}

#[test]
fn treasure_plan_keeps_item_multiplicity() {
    // "门需三钥" style clues: the same item repeated must survive parsing —
    // the judger checks the sacrifice set 不能多、不能少.
    let plan = parse_plan(
        r#"{"pos":{"x":1,"y":1},"items":["StarSand","StarSand","IronWhistle"],"openDay":2}"#,
        1,
    )
    .unwrap();
    assert_eq!(
        plan.items,
        vec![
            "StarSand".to_string(),
            "StarSand".to_string(),
            "IronWhistle".to_string()
        ]
    );
}

#[test]
fn exit_code_parsing() {
    assert_eq!(exit_code("[exitCode:0]\nok"), Some(0));
    assert_eq!(exit_code("[exitCode:127]\nnope"), Some(127));
    assert_eq!(exit_code("[TIMEOUT]\npartial"), None);
    assert_eq!(exit_code("[JUDGER_ERROR]\nboom"), None);
    assert_eq!(exit_code("raw output"), None);
}

#[test]
fn day_night_calendar() {
    let base = r#"{"roundNo":1,"mapInfo":{"width":41,"height":32,"zones":[]},
        "teamOur":{"type":"challenger","roles":[]},"teamEnemy":{"roles":[]},
        "robot":{"roles":[]}}"#;
    let turn_at = |round: i64| -> Turn {
        let mut value: serde_json::Value = serde_json::from_str(base).unwrap();
        value["roundNo"] = serde_json::json!(round);
        let req: Request = serde_json::from_value(value).unwrap();
        Turn::from_request(req)
    };
    assert!(turn_at(1).is_day && turn_at(1).day == 1);
    assert!(turn_at(70).is_day); // last day round
    assert!(!turn_at(71).is_day); // first night round
    assert!(!turn_at(130).is_day); // last night round
    let d2 = turn_at(131);
    assert!(d2.is_day && d2.day == 2 && d2.in_day_round == 0);
    assert_eq!(turn_at(1300).day, 10);
}

#[test]
fn keyword_extraction_bigrams() {
    let keywords = keywords_of("请查询北京天气");
    assert!(keywords.iter().any(|kw| kw == "北京"), "{keywords:?}");
    let keywords = keywords_of("query weather in Beijing");
    assert!(keywords.contains(&"beijing".to_string()), "{keywords:?}");
}

#[test]
fn error_descriptions_are_kept_not_dropped() {
    // The judger's rejection text (e.g. `MissingNamedInput`) is the one
    // authoritative schema signal for task answers — the opponent used it to
    // fix theirs (issue #10). It used to be dropped at the parse layer; the
    // analysis workflow reads it from the round log now (errorDescs).
    let payload = serde_json::json!({
        "roundNo": 1,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {"type": "challenger", "roles": []},
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "errors": [
            {"errorCode": 2, "description": "MissingNamedInput: city"},
            {"errorCode": 1, "description": "任务超时"},
        ],
    });
    let req: Request = serde_json::from_value(payload).unwrap();
    let turn = Turn::from_request(req);
    assert_eq!(turn.error_codes, vec![2, 1]);
    assert_eq!(
        turn.error_descriptions,
        vec!["MissingNamedInput: city".to_string(), "任务超时".to_string()]
    );
}

#[test]
fn the_submitted_answer_is_logged_in_full() {
    // 判题器说 MissingNamedInput 时，缺的是哪个字段只能从提交原文里看出来——
    // 那不是可以截到 160 字符的东西。
    let answer = format!(
        "{{\"city\": \"北京\", \"reason\": \"{}\"}}",
        "因为该地全年降水集中于夏季且地形抬升显著".repeat(8)
    );
    assert!(answer.chars().count() > 160, "构造的答案必须超过旧上限");
    let (logged, chars) = answer_for_log(&answer);
    assert_eq!(logged, answer, "正常长度的答案逐字进日志");
    assert_eq!(chars, answer.chars().count());

    // 异常巨大的回包才截断，且真实长度必须留下，让截断可被认出来。
    let huge = "x".repeat(ANSWER_LOG_CAP + 500);
    let (logged, chars) = answer_for_log(&huge);
    assert_eq!(logged.chars().count(), ANSWER_LOG_CAP);
    assert_eq!(chars, ANSWER_LOG_CAP + 500);
    assert!(
        logged.chars().count() < chars,
        "截断必须可以被识别：实际字符数 < 真实字符数"
    );
}
