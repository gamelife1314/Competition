//! Multi-opponent A/B report: JSONL capture → per-objective comparison.

use serde_json::{json, Value};

use coregeek::abreport::{parse_battle, render, BattleReport};

fn line(event: &str, data: Value) -> String {
    json!({"ts": 0, "event": event, "data": data}).to_string()
}

/// One `round` record with the fields the report reads.
#[allow(clippy::too_many_arguments)]
fn round_line(
    day: i64,
    round: i64,
    score: i64,
    kill: i64,
    survival: i64,
    residual: i64,
    station_hp: Value,
    enemy_station_hp: Value,
    rejected: Vec<i64>,
    dry: bool,
) -> String {
    line(
        "round",
        json!({
            "round": round,
            "day": day,
            "score": score,
            "scoreAttr": {"kill": kill, "killThisRound": 0, "survival": survival, "residual": residual},
            "stationHp": station_hp,
            "volley": {
                "rejected": rejected,
                "noRobotDamage": dry,
                "enemyStationHp": enemy_station_hp,
            },
        }),
    )
}

fn two_day_capture() -> Vec<String> {
    vec![
        round_line(1, 1, 10, 0, 10, 0, json!(1500), json!(1500), vec![], false),
        round_line(1, 2, 25, 3, 10, 12, json!(1400), json!(1500), vec![], false),
        round_line(2, 131, 60, 9, 20, 31, json!(1200), json!(0), vec![], false),
    ]
}

#[test]
fn days_keep_the_last_cumulative_value_of_the_day() {
    let report = parse_battle("alpha", two_day_capture());
    assert_eq!(report.rounds, 3);
    assert_eq!(report.days.len(), 2);
    assert_eq!(report.days[0].day, 1);
    assert_eq!(report.days[0].kill, 3);
    assert_eq!(report.days[0].total, 25);
    assert_eq!(report.days[1].day, 2);
    assert_eq!(report.days[1].residual, 31);
    assert_eq!(report.final_total, 60);
    assert_eq!(report.kill, 9);
    assert_eq!(report.survival, 20);
    assert_eq!(report.score1_proxy(), 31);
}

#[test]
fn enemy_station_falling_is_the_win_condition_and_is_dated() {
    let report = parse_battle("alpha", two_day_capture());
    assert_eq!(report.enemy_station_down_day, Some(2));
    assert_eq!(report.station_lost_day, None);
}

#[test]
fn losing_our_station_is_dated_too() {
    // A fallen base is written as an explicit `0` by `log_round` (`turn.station()`
    // returning `None` is what 0 means there), so that is what the capture says.
    let capture = vec![
        round_line(1, 1, 10, 0, 10, 0, json!(1500), json!(1500), vec![], false),
        round_line(2, 131, 10, 0, 0, 10, json!(0), json!(1500), vec![], false),
    ];
    let report = parse_battle("alpha", capture);
    assert_eq!(report.station_lost_day, Some(2));
}

#[test]
fn a_round_that_does_not_mention_the_base_is_not_a_loss() {
    // `stationHp` is change-gated: it is written when it first appears, when it
    // moves, and when the base falls — and omitted on every round in between,
    // which is most of them. Read as a bare `data.get` those absences look
    // exactly like a station that disappeared, and every battle would be dated
    // a day-1 loss on its second quiet round.
    let capture = vec![
        round_line(1, 1, 10, 0, 10, 0, json!(1500), json!(1500), vec![], false),
        // Four quiet rounds: the record says nothing about the base at all.
        line("round", json!({"round": 2, "day": 1, "score": 12,
            "scoreAttr": {"kill": 0, "killThisRound": 0, "survival": 10, "residual": 2}})),
        line("round", json!({"round": 3, "day": 1, "score": 14,
            "scoreAttr": {"kill": 0, "killThisRound": 0, "survival": 10, "residual": 4}})),
        // Then it comes back damaged, and finally falls.
        round_line(1, 4, 16, 0, 10, 6, json!(900), json!(1500), vec![], false),
        line("round", json!({"round": 5, "day": 1, "score": 18,
            "scoreAttr": {"kill": 0, "killThisRound": 0, "survival": 10, "residual": 8}})),
        round_line(2, 131, 18, 0, 0, 8, json!(0), json!(1500), vec![], false),
    ];
    let report = parse_battle("alpha", capture);
    assert_eq!(
        report.station_lost_day,
        Some(2),
        "the base fell on day 2; day 1 ended with it merely damaged"
    );
}

#[test]
fn task_sop_and_volley_counters_are_summed() {
    let mut capture = two_day_capture();
    capture.push(line(
        "task_ended",
        json!({"success": true, "reason": "confirmed_success"}),
    ));
    capture.push(line(
        "task_ended",
        json!({"success": true, "reason": "confirmed_success"}),
    ));
    capture.push(line(
        "task_ended",
        json!({"success": false, "reason": "timeout"}),
    ));
    capture.push(line("sop_reuse", json!({"taskType": "T1"})));
    capture.push(line("sop_reuse", json!({"taskType": "T1"})));
    capture.push(round_line(
        3,
        261,
        70,
        9,
        30,
        31,
        json!(1200),
        json!(1500),
        vec![10020, 10021],
        true,
    ));

    let report = parse_battle("alpha", capture);
    assert_eq!(report.tasks_confirmed, 2);
    assert_eq!(report.tasks_failed, 1);
    assert_eq!(report.sop_reuses, 2);
    assert_eq!(report.volleys_rejected, 2);
    assert_eq!(report.volleys_no_damage, 1);
}

#[test]
fn unparseable_and_unknown_records_are_skipped() {
    let mut capture = vec!["not json at all".to_string(), String::new()];
    capture.push(json!([1, 2, 3]).to_string());
    capture.push(line("startup", json!({"port": 8080})));
    capture.extend(two_day_capture());
    capture.push("{\"event\":\"round\",\"data\":".to_string()); // truncated tail

    let report = parse_battle("alpha", capture);
    assert_eq!(report.rounds, 3);
    assert_eq!(report.days.len(), 2);
}

#[test]
fn render_compares_every_later_battle_against_the_first() {
    let before = parse_battle("before", two_day_capture());
    let after = parse_battle(
        "after",
        vec![
            round_line(1, 1, 40, 0, 10, 30, json!(1500), json!(1500), vec![], false),
            round_line(2, 131, 90, 4, 20, 66, json!(1200), json!(0), vec![], false),
        ],
    );
    let table = render(&[before, after]);
    assert!(table.contains("before"));
    assert!(table.contains("after"));
    assert!(table.contains("s1+err"));
    assert!(table.contains("delta"));
    // 90 - 60, 4 - 9, 66 - 31: per objective, not just on the total.
    assert!(table.contains("+30"), "{table}");
    assert!(table.contains("-5"), "{table}");
    assert!(table.contains("+35"), "{table}");
}

#[test]
fn a_single_battle_renders_without_a_delta_block() {
    let table = render(&[parse_battle("solo", two_day_capture())]);
    assert!(table.contains("solo"));
    assert!(!table.contains("delta"));
}

#[test]
fn an_empty_capture_is_an_empty_report_not_a_panic() {
    let report = parse_battle("empty", Vec::<String>::new());
    assert_eq!(report.rounds, 0);
    assert_eq!(report.final_total, 0);
    assert_eq!(report.days, Vec::new());
    let table = render(&[report]);
    assert!(table.contains("empty"));
}

#[test]
fn report_defaults_are_neutral() {
    let report = BattleReport::default();
    assert_eq!(report.score1_proxy(), 0);
    assert_eq!(report.enemy_station_down_day, None);
}
