//! Unit tests for pure helpers: ballistics, task-loop parsing, treasure-plan
//! parsing and the day/night calendar.

use coregeek::brain::combat::{line_cells, within_cone};
use coregeek::brain::task::{exit_code, extract_answer, extract_command, strip_status_line};
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

    let resp = "ls -la /tmp";
    assert_eq!(extract_command(resp).as_deref(), Some("ls -la /tmp"));

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
