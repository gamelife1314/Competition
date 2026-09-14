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

/// The survival objective is the one figure in `scoreAttr` a reader is most
/// likely to take as a single day's term rather than a running sum, because
/// the mistake only shows up two days in. These are the task book's own
/// numbers (chapter 6, `Σ 10 × day` with 存活系数 = 1 while the base stands):
/// a base destroyed on day 3 keeps 10 + 20 = 30, and surviving the full half
/// is worth 550.
#[test]
fn the_survival_objective_is_a_running_sum_not_one_days_term() {
    use coregeek::brain::survival_score;

    // Base standing: the sum of every day so far.
    assert_eq!(survival_score(1, None), 10, "day 1: 10");
    assert_eq!(survival_score(2, None), 30, "day 2: 10 + 20, not 20");
    assert_eq!(survival_score(3, None), 60, "day 3: 10 + 20 + 30, not 30");
    assert_eq!(survival_score(10, None), 550, "a full half is worth 550, not 100");

    // Base gone: frozen one day short of the day it fell, for every round after.
    assert_eq!(survival_score(3, Some(3)), 30, "fell on day 3: keeps days 1-2");
    assert_eq!(survival_score(7, Some(3)), 30, "and keeps exactly that");
    assert_eq!(survival_score(2, Some(2)), 10, "fell on day 2: keeps day 1");
    assert_eq!(survival_score(1, Some(1)), 0, "fell on day 1: nothing accrued");

    // The shape of the bug this replaced: `day * 10` is the day's *increment*
    // for day 1 only. Reading it as the running total under-reports by 450 at
    // day 10 — larger than any task score on record — and that difference lands
    // in `residual`, which `abreport` presents as the task score.
    assert!(survival_score(10, None) - 10 * 10 == 450);
}

// ---------------------------------------------------------------------------
// P2-4: the fourth attribution component — what the task line actually earned.
// ---------------------------------------------------------------------------

/// A `round` record carrying the running banked task gold.
fn gold_line(day: i64, round: i64, score: i64, task_gold: i64) -> String {
    line(
        "round",
        json!({
            "round": round,
            "day": day,
            "score": score,
            "scoreAttr": {"kill": 0, "survival": 0, "residual": 0},
            "taskGoldEarned": task_gold,
        }),
    )
}

#[test]
fn the_task_gold_column_carries_the_last_banked_total() {
    // `taskGoldEarned` is a cumulative curve like `score`, so the day's value is
    // the last round's — not the sum over the day's rounds, which would
    // multiply the same reward by the number of rounds in the day.
    let report = parse_battle(
        "gold",
        vec![
            gold_line(1, 1, 10, 0),
            gold_line(1, 130, 20, 0),
            gold_line(2, 131, 60, 120),
            gold_line(2, 260, 90, 120),
            gold_line(3, 261, 95, 200),
        ],
    );
    assert_eq!(report.days[0].task_gold, 0, "day 1 banked nothing");
    assert_eq!(report.days[1].task_gold, 120);
    assert_eq!(report.days[2].task_gold, 200);
    assert_eq!(report.task_gold_earned, 200, "the match total banked");

    let table = render(&[report]);
    assert!(table.contains("gold"), "the column is labelled: {table}");
    assert!(table.contains("200"), "{table}");
}

#[test]
fn a_round_record_without_task_gold_does_not_reset_the_curve() {
    // The field is only written once the log carries it; a capture from an
    // older build (or a round whose record stayed quiet) must carry the last
    // figure forward rather than snapping the curve back to zero.
    let report = parse_battle(
        "carry",
        vec![
            gold_line(2, 131, 60, 120),
            line(
                "round",
                json!({
                    "round": 132, "day": 2, "score": 61,
                    "scoreAttr": {"kill": 0, "survival": 0, "residual": 0},
                }),
            ),
        ],
    );
    assert_eq!(report.days[0].task_gold, 120);
    assert_eq!(report.task_gold_earned, 120);
}

#[test]
fn the_reports_split_informed_retries_from_repeated_giveups() {
    // P1-1's whole claim is that the judger's rejection text reaches the retry
    // and that the retry is given up on only when the judger repeats itself.
    // The two counters are the falsifiable halves of that claim: if `inf` stays
    // at zero while `rep` climbs, the feedback loop is not reaching the retry.
    let report = parse_battle(
        "retry",
        vec![
            line("task_retry_informed", json!({"session": 1, "round": 61, "feedback": 1})),
            line("task_retry_informed", json!({"session": 1, "round": 78, "feedback": 2})),
            line(
                "task_ended",
                json!({"session": 2, "success": false, "reason": "wrong_answers"}),
            ),
            line(
                "task_ended",
                json!({"session": 3, "success": false, "reason": "timeout"}),
            ),
            line("task_ended", json!({"session": 4, "success": true, "reason": "confirmed_success"})),
        ],
    );
    assert_eq!(report.retries_informed, 2);
    assert_eq!(
        report.giveups_repeated, 1,
        "only `wrong_answers` is a give-up on a repeated verdict"
    );
    assert_eq!(report.tasks_confirmed, 1);
    assert_eq!(report.tasks_failed, 2);

    let table = render(&[report]);
    assert!(table.contains("inf"), "the split is labelled: {table}");
    assert!(table.contains("rep"), "{table}");
}

#[test]
fn the_report_says_which_sop_reuse_took_the_compressed_path() {
    // P1-3: `sop` now reads `staged/total`. A pair that ran its exploration
    // first is the compressed path; a single-script reuse is the old one, and
    // only the ratio tells whether the two-stage pair is being taken at all.
    let report = parse_battle(
        "sop",
        vec![
            line("sop_reuse", json!({"taskType": "自进化类1", "staged": true})),
            line("sop_reuse", json!({"taskType": "自进化类1", "staged": false})),
            line("sop_reuse", json!({"taskType": "自进化类1"})),
        ],
    );
    assert_eq!(report.sop_reuses, 3);
    assert_eq!(
        report.sop_staged_reuses, 1,
        "an absent `staged` is the single-script path, not the pair"
    );
    assert!(render(&[report]).contains("1/3"), "staged/total");
}

#[test]
fn both_bases_hp_are_carried_forward_to_the_end_of_the_capture() {
    // Both are change-gated in the `round` record, so the day a base stops
    // being mentioned is exactly the day it is most interesting. The report
    // carries the last number seen rather than reading the gap as zero.
    let report = parse_battle(
        "hp",
        vec![
            round_line(1, 1, 10, 0, 10, 0, json!(1500), json!(1500), vec![], false),
            // Day 2 writes our HP only; the enemy's is last seen at 1500.
            line(
                "round",
                json!({
                    "round": 131, "day": 2, "score": 40,
                    "scoreAttr": {"kill": 0, "survival": 10, "residual": 0},
                    "stationHp": 1100,
                }),
            ),
        ],
    );
    assert_eq!(report.station_hp_last, Some(1100));
    assert_eq!(
        report.enemy_station_hp_last,
        Some(1500),
        "an unwritten enemy HP carries the last one seen"
    );
    assert_eq!(report.days[1].station_hp, Some(1100));
    assert_eq!(report.days[1].enemy_station_hp, Some(1500));

    let table = render(&[report]);
    assert!(table.contains("hp"), "{table}");
    assert!(table.contains("1100"), "{table}");
}

#[test]
fn a_night_volley_carries_the_enemy_hp_the_round_record_left_out() {
    // The night-side copy is the fallback: a round record that stayed quiet
    // must not lose the number the volley block already reported.
    let report = parse_battle(
        "volley",
        vec![line(
            "round",
            json!({
                "round": 131, "day": 2, "score": 40,
                "scoreAttr": {"kill": 0, "survival": 10, "residual": 0},
                "volley": {"rejected": [], "noRobotDamage": false, "enemyStationHp": 900},
            }),
        )],
    );
    assert_eq!(report.enemy_station_hp_last, Some(900));
}
