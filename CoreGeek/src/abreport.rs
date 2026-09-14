//! Offline multi-opponent A/B report over captured JSONL logs.
//!
//! Tuning against one opponent is how a change that only helps against that
//! opponent's opening gets shipped. The three scoring objectives are the ones
//! the match is actually graded on, so a change is only worth keeping when it
//! moves them in the same direction against every opponent — which means the
//! comparison has to be per-objective, not on the final total alone.
//!
//! `brain::mod::log_round` already attributes the running total to
//! `score2` (robots destroyed: 1/2/4/10) and `score3` (survival, `Σ 10 x day`
//! over the days the station has stood — a SUM, not a day's increment, so
//! nothing here ever has to add the days up). What is left over is `score1`
//! (tasks) plus the attribution error, so the report calls it `s1+err` rather
//! than pretending to a precision it does not have.
//!
//! This module is pure: it turns log lines into structs and structs into a
//! table. `src/bin/ab_report.rs` is the file-reading wrapper.

use serde_json::Value;

/// Cumulative objective scores at the end of one day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DayRow {
    pub day: i64,
    pub kill: i64,
    pub survival: i64,
    pub residual: i64,
    pub total: i64,
    /// Advertised task gold banked so far (P2-4). The fourth attribution
    /// component: `scoreAttr` splits the running total into three objectives,
    /// and this is the only one of them whose *income* was invisible.
    pub task_gold: i64,
    /// Our base's HP at the end of this day, and the opponent's — carried
    /// forward, because both are change-gated in the `round` record.
    pub station_hp: Option<i64>,
    pub enemy_station_hp: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct BattleReport {
    pub label: String,
    pub rounds: usize,
    pub days: Vec<DayRow>,
    /// Objective totals at the end of the match.
    pub kill: i64,
    pub survival: i64,
    pub residual: i64,
    pub final_total: i64,
    /// Day our station first went down (None = it survived the match).
    pub station_lost_day: Option<i64>,
    /// Day the opponent's station first went down — the win condition.
    pub enemy_station_down_day: Option<i64>,
    pub tasks_confirmed: usize,
    pub tasks_failed: usize,
    /// Advertised gold of the task points that were confirmed (P2-4).
    pub task_gold_earned: i64,
    /// Rounds on which a cached SOP ran, split by whether it was the two-stage
    /// pair or the single-script one (P1-3) — the direct measure of whether the
    /// compressed path is being taken.
    pub sop_staged_reuses: usize,
    /// Rejections that carried information the session did not already hold
    /// (P1-1), and sessions abandoned because every rejection repeated one.
    /// The ratio says whether the feedback loop is reaching the retry at all.
    pub retries_informed: usize,
    pub giveups_repeated: usize,
    /// Our base's HP where the log left it, and the opponent's.
    pub station_hp_last: Option<i64>,
    pub enemy_station_hp_last: Option<i64>,
    pub sop_reuses: usize,
    /// Volleys the judger rejected, summed over the match.
    pub volleys_rejected: usize,
    /// Rounds where every volley was accepted yet no robot lost HP.
    pub volleys_no_damage: usize,
}

impl BattleReport {
    /// `score1` proxy: task score plus attribution error (see module docs).
    pub fn score1_proxy(&self) -> i64 {
        self.residual
    }
}

/// One battle = the stdout capture of one match against one opponent.
///
/// Unparseable lines are skipped: a capture can contain a banner or a partial
/// final line, and neither should sink the whole comparison.
pub fn parse_battle<I: IntoIterator<Item = String>>(label: &str, lines: I) -> BattleReport {
    let mut report = BattleReport {
        label: label.to_string(),
        ..Default::default()
    };
    let mut station_seen = false;
    let mut enemy_station_seen = false;
    // Last `stationHp` the record carried; see the carry-forward note below.
    let mut last_station_hp: Option<i64> = None;
    let mut last_enemy_station_hp: Option<i64> = None;

    for line in lines {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let event = record.get("event").and_then(Value::as_str).unwrap_or("");
        let data = record.get("data").unwrap_or(&Value::Null);
        match event {
            "round" => {
                report.rounds += 1;
                let day = data.get("day").and_then(Value::as_i64).unwrap_or(0);
                let attr = data.get("scoreAttr").unwrap_or(&Value::Null);
                // P2-4: both bases' HP are change-gated, so they are carried
                // forward exactly like the loss-day probe below. `enemyStationHp`
                // is written by the round record; `volley.enemyStationHp` is the
                // night-side copy and is read as the fallback, so a day whose
                // record stayed quiet still reports the last number seen.
                if let Some(hp) = data
                    .get("enemyStationHp")
                    .or_else(|| data.get("volley").and_then(|volley| volley.get("enemyStationHp")))
                    .and_then(Value::as_i64)
                {
                    last_enemy_station_hp = Some(hp);
                }
                // `stationHp` is change-gated in the `round` record: it is
                // written on the round it first appears, whenever the number
                // moves, and on the round the base falls (as an explicit `0`).
                // On the rounds in between it is absent, which means "unchanged"
                // and NOT "gone" — so the last value seen carries forward. Read
                // as a bare `data.get` this would call every quiet round the day
                // the base was lost.
                //
                // Read BEFORE the row is built, exactly like the enemy's above:
                // the row carries "where each base stood at the end of this
                // day", and a read taken after the row would put the previous
                // round's number in it — the day a base fell would report the
                // HP it had while it was still standing.
                if let Some(hp) = data.get("stationHp").and_then(Value::as_i64) {
                    last_station_hp = Some(hp);
                }
                let row = DayRow {
                    day,
                    kill: attr.get("kill").and_then(Value::as_i64).unwrap_or(0),
                    survival: attr.get("survival").and_then(Value::as_i64).unwrap_or(0),
                    residual: attr.get("residual").and_then(Value::as_i64).unwrap_or(0),
                    total: data.get("score").and_then(Value::as_i64).unwrap_or(0),
                    task_gold: data
                        .get("taskGoldEarned")
                        .and_then(Value::as_i64)
                        .unwrap_or(report.task_gold_earned),
                    station_hp: last_station_hp,
                    enemy_station_hp: last_enemy_station_hp,
                };
                // Cumulative within a day, so the last round of the day wins.
                match report.days.iter_mut().find(|existing| existing.day == day) {
                    Some(existing) => *existing = row,
                    None => report.days.push(row),
                }
                report.kill = row.kill;
                report.survival = row.survival;
                report.residual = row.residual;
                report.final_total = row.total;
                report.task_gold_earned = row.task_gold;

                report.station_hp_last = last_station_hp;
                report.enemy_station_hp_last = last_enemy_station_hp;
                match last_station_hp {
                    Some(hp) if hp > 0 => station_seen = true,
                    _ if station_seen && report.station_lost_day.is_none() => {
                        report.station_lost_day = Some(day);
                    }
                    _ => {}
                }
                let volley = data.get("volley").unwrap_or(&Value::Null);
                report.volleys_rejected += volley
                    .get("rejected")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                if volley
                    .get("noRobotDamage")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    report.volleys_no_damage += 1;
                }
                let enemy_hp = volley.get("enemyStationHp").and_then(Value::as_i64);
                match enemy_hp {
                    Some(hp) if hp > 0 => enemy_station_seen = true,
                    _ if enemy_station_seen && report.enemy_station_down_day.is_none() => {
                        report.enemy_station_down_day = Some(day);
                    }
                    _ => {}
                }
            }
            "task_ended" => {
                if data
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    report.tasks_confirmed += 1;
                } else {
                    report.tasks_failed += 1;
                }
                // P1-1: a session given up on because the judger kept saying
                // the same thing. Counted apart from every other failure mode
                // (timeout, dusk recall, sterile) because it is the one the
                // information-driven retry is supposed to make rare.
                if data.get("reason").and_then(Value::as_str) == Some("wrong_answers") {
                    report.giveups_repeated += 1;
                }
            }
            "sop_reuse" => {
                report.sop_reuses += 1;
                if data
                    .get("staged")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    report.sop_staged_reuses += 1;
                }
            }
            // P1-1: one half of the pair — a rejection that taught the retry
            // something. The other half is `task_ended.reason ==
            // "wrong_answers"` below (a session abandoned because the judger
            // repeated itself), and the ratio between them is the falsifiable
            // part of "information-driven retry".
            "task_retry_informed" => report.retries_informed += 1,
            _ => {}
        }
    }
    report
}

fn signed(value: i64) -> String {
    format!("{value:+}")
}

/// Fixed-width table, one row per battle, then the delta of every later row
/// against the first — the A/B comparison itself.
pub fn render(reports: &[BattleReport]) -> String {
    let mut out = String::new();
    // P2-4: the attribution dashboard is the point of this table, so the four
    // components sit together and the task-income column (`gold`) sits beside
    // the task-score proxy (`s1+err`) it is supposed to explain. `hp`/`ehp` are
    // the two bases as the log left them, and `inf/rep` splits the rejections
    // into ones that taught the retry something and sessions given up on
    // because the judger repeated itself.
    out.push_str(&format!(
        "{:<20} {:>6} {:>8} {:>6} {:>6} {:>7} {:>6} {:>9} {:>4} {:>6} {:>6} {:>7} {:>5} {:>9} {:>9} {:>3}/{:<3}\n",
        "battle",
        "rounds",
        "final",
        "kill",
        "surv",
        "s1+err",
        "gold",
        "tasks ok/bad",
        "sop",
        "lost",
        "enemy",
        "reject",
        "dry",
        "hp",
        "ehp",
        "inf",
        "rep"
    ));
    for report in reports {
        let lost = report
            .station_lost_day
            .map(|day| format!("d{day}"))
            .unwrap_or_else(|| "-".to_string());
        let enemy = report
            .enemy_station_down_day
            .map(|day| format!("d{day}"))
            .unwrap_or_else(|| "-".to_string());
        let hp = |value: Option<i64>| {
            value
                .map(|hp| hp.to_string())
                .unwrap_or_else(|| "-".to_string())
        };
        out.push_str(&format!(
            "{:<20} {:>6} {:>8} {:>6} {:>6} {:>7} {:>6} {:>9} {:>4} {:>6} {:>6} {:>7} {:>5} {:>9} {:>9} {:>3}/{:<3}\n",
            report.label,
            report.rounds,
            report.final_total,
            report.kill,
            report.survival,
            report.score1_proxy(),
            report.task_gold_earned,
            format!("{}/{}", report.tasks_confirmed, report.tasks_failed),
            format!("{}/{}", report.sop_staged_reuses, report.sop_reuses),
            lost,
            enemy,
            report.volleys_rejected,
            report.volleys_no_damage,
            hp(report.station_hp_last),
            hp(report.enemy_station_hp_last),
            report.retries_informed,
            report.giveups_repeated,
        ));
    }
    if reports.len() < 2 {
        return out;
    }
    let baseline = &reports[0];
    out.push('\n');
    for report in &reports[1..] {
        out.push_str(&format!(
            "delta {:<14} {:>6} {:>8} {:>6} {:>6} {:>7} {:>6} {:>9} {:>4}\n",
            format!("{} - {}", report.label, baseline.label),
            "",
            signed(report.final_total - baseline.final_total),
            signed(report.kill - baseline.kill),
            signed(report.survival - baseline.survival),
            signed(report.score1_proxy() - baseline.score1_proxy()),
            signed(report.task_gold_earned - baseline.task_gold_earned),
            format!(
                "{:+}/{:+}",
                report.tasks_confirmed as i64 - baseline.tasks_confirmed as i64,
                report.tasks_failed as i64 - baseline.tasks_failed as i64
            ),
            signed(report.sop_reuses as i64 - baseline.sop_reuses as i64),
        ));
    }
    out
}
