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
                let row = DayRow {
                    day,
                    kill: attr.get("kill").and_then(Value::as_i64).unwrap_or(0),
                    survival: attr.get("survival").and_then(Value::as_i64).unwrap_or(0),
                    residual: attr.get("residual").and_then(Value::as_i64).unwrap_or(0),
                    total: data.get("score").and_then(Value::as_i64).unwrap_or(0),
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

                // `stationHp` is change-gated in the `round` record: it is
                // written on the round it first appears, whenever the number
                // moves, and on the round the base falls (as an explicit `0`).
                // On the rounds in between it is absent, which means "unchanged"
                // and NOT "gone" — so the last value seen carries forward. Read
                // as a bare `data.get` this would call every quiet round the day
                // the base was lost.
                if let Some(hp) = data.get("stationHp").and_then(Value::as_i64) {
                    last_station_hp = Some(hp);
                }
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
            }
            "sop_reuse" => report.sop_reuses += 1,
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
    out.push_str(&format!(
        "{:<20} {:>6} {:>8} {:>6} {:>6} {:>7} {:>9} {:>4} {:>6} {:>6} {:>7} {:>5}\n",
        "battle",
        "rounds",
        "final",
        "kill",
        "surv",
        "s1+err",
        "tasks ok/bad",
        "sop",
        "lost",
        "enemy",
        "reject",
        "dry"
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
        out.push_str(&format!(
            "{:<20} {:>6} {:>8} {:>6} {:>6} {:>7} {:>9} {:>4} {:>6} {:>6} {:>7} {:>5}\n",
            report.label,
            report.rounds,
            report.final_total,
            report.kill,
            report.survival,
            report.score1_proxy(),
            format!("{}/{}", report.tasks_confirmed, report.tasks_failed),
            report.sop_reuses,
            lost,
            enemy,
            report.volleys_rejected,
            report.volleys_no_damage,
        ));
    }
    if reports.len() < 2 {
        return out;
    }
    let baseline = &reports[0];
    out.push('\n');
    for report in &reports[1..] {
        out.push_str(&format!(
            "delta {:<14} {:>6} {:>8} {:>6} {:>6} {:>7} {:>9} {:>4}\n",
            format!("{} - {}", report.label, baseline.label),
            "",
            signed(report.final_total - baseline.final_total),
            signed(report.kill - baseline.kill),
            signed(report.survival - baseline.survival),
            signed(report.score1_proxy() - baseline.score1_proxy()),
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
