//! Attack-result verification (「攻击结果核对」).
//!
//! Night volleys were the hardest thing to diagnose from a captured match:
//! `night_debug` says whether a tower *tried* to fire, but nothing said whether
//! the judger accepted the order or whether the shot accomplished anything.
//! This module joins the two channels the next round hands us —
//! `lastRoundRoleActionResults` (accepted / rejected, keyed by the role the
//! command map used, which for an attack is the TOWER id) and the robot HP
//! snapshot taken last round — into one review record per round.
//!
//! It is a pure observer: it never feeds back into target selection. A rejected
//! volley is almost always a legality/geometry problem rather than a bad target
//! choice, and the next round rebuilds the target list from a changed board, so
//! "correcting" selection from a rejection can oscillate. The review is
//! surfaced as telemetry and only wired into selection if a real battle shows a
//! tower repeating a rejected order.
//!
//! Attribution is deliberately conservative: robot damage is reported as a
//! round total, never attributed to an individual tower, because several towers
//! share every round and a robot can also die to an item or leave the board.
//! The one unambiguous case — every volley accepted, yet not a single robot
//! lost HP — is what `no_robot_damage_round` reports.

use std::collections::HashMap;

use crate::model::{Turn, UnitKind};
use crate::protocol::Pos;
use crate::state::IssuedCmd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolleyResult {
    /// The judger accepted last round's attack command for this tower.
    Executed,
    /// The judger rejected it (`lastRoundRoleActionResults[tower] == false`).
    Rejected,
    /// The judger reported nothing for this tower. Distinct from a rejection:
    /// the order may never have reached the judger, or the tower was dead.
    Unreported,
}

impl VolleyResult {
    pub fn as_str(self) -> &'static str {
        match self {
            VolleyResult::Executed => "executed",
            VolleyResult::Rejected => "rejected",
            VolleyResult::Unreported => "unreported",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Volley {
    pub tower: i64,
    /// First target position we asked for. For a gatling/rocket volley this is
    /// the primary target; the remaining projectiles are not repeated here.
    pub target: Option<Pos>,
    pub result: VolleyResult,
}

#[derive(Debug, Clone, Default)]
pub struct VolleyReview {
    pub volleys: Vec<Volley>,
    /// Robot id → HP lost since the previous round's snapshot (positive).
    pub robots_damaged: Vec<(i64, i64)>,
    /// Robots that were alive last round and are gone (or at 0 HP) now. A robot
    /// can also leave the board without being killed, so this is an upper bound
    /// on last round's kills.
    pub kills: Vec<i64>,
    /// Our own station's HP, for context on how the night went.
    pub station_hp: Option<i64>,
    /// The opponent's station HP — the half is won by destroying it, so a run's
    /// progress on the win condition is visible here and nowhere else.
    pub enemy_station_hp: Option<i64>,
}

impl VolleyReview {
    pub fn rejected_towers(&self) -> Vec<i64> {
        self.volleys
            .iter()
            .filter(|volley| volley.result == VolleyResult::Rejected)
            .map(|volley| volley.tower)
            .collect()
    }

    pub fn executed(&self) -> usize {
        self.volleys
            .iter()
            .filter(|volley| volley.result == VolleyResult::Executed)
            .count()
    }

    pub fn unreported(&self) -> usize {
        self.volleys
            .iter()
            .filter(|volley| volley.result == VolleyResult::Unreported)
            .count()
    }

    pub fn robot_damage(&self) -> i64 {
        self.robots_damaged.iter().map(|(_, lost)| *lost).sum()
    }

    /// At least one volley was confirmed accepted, none was rejected, and yet
    /// no robot lost a single HP and none died.
    ///
    /// Requiring a confirmed acceptance is what keeps a match where the judger
    /// never fills `lastRoundRoleActionResults` from flooding the log with
    /// rounds we cannot actually judge. An accepted volley aimed at an enemy
    /// building — the no-robot fallback in `combat::choose_attack` — does land
    /// here, which is a false alarm rather than a defect; the recorded target
    /// positions are what tell the two apart, which is why the summary keeps
    /// them.
    pub fn no_robot_damage_round(&self) -> bool {
        self.executed() > 0
            && self.rejected_towers().is_empty()
            && self.robots_damaged.is_empty()
            && self.kills.is_empty()
    }

    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "fired": self.volleys.len(),
            "executed": self.executed(),
            "rejected": self.rejected_towers(),
            "unreported": self.unreported(),
            "volleys": self
                .volleys
                .iter()
                .map(|volley| {
                    serde_json::json!({
                        "tower": volley.tower,
                        "target": volley.target,
                        "result": volley.result.as_str(),
                    })
                })
                .collect::<Vec<_>>(),
            "damage": self.robot_damage(),
            "damaged": self.robots_damaged,
            "kills": self.kills,
            "stationHp": self.station_hp,
            "enemyStationHp": self.enemy_station_hp,
            "noRobotDamage": self.no_robot_damage_round(),
        })
    }
}

/// Join last round's attack orders with this round's outcome report.
///
/// `previous_issued` must be the command map sent LAST round (the caller has to
/// snapshot it before overwriting `BotState::last_issued`), and `prev_robot_hp`
/// the robot HP snapshot taken at the end of last round. Both live in
/// `BotState` and are both overwritten as the round closes, so the call belongs
/// before that update.
pub fn review_volley(
    turn: &Turn,
    previous_issued: &HashMap<i64, IssuedCmd>,
    prev_robot_hp: &HashMap<i64, i64>,
) -> VolleyReview {
    let mut review = VolleyReview {
        station_hp: turn.station().map(|station| station.health),
        enemy_station_hp: turn
            .enemy
            .iter()
            .find(|unit| unit.kind == UnitKind::Station)
            .map(|station| station.health),
        ..Default::default()
    };

    // An attack command is keyed by the TOWER id (the controller only travels
    // in `controllerId`), so the action is the thing that identifies a volley.
    let mut towers: Vec<i64> = previous_issued
        .iter()
        .filter(|(_, cmd)| cmd.action == "attack")
        .map(|(id, _)| *id)
        .collect();
    towers.sort_unstable();
    for tower in towers {
        let cmd = &previous_issued[&tower];
        let result = match turn.last_action_results.get(&tower) {
            Some(true) => VolleyResult::Executed,
            Some(false) => VolleyResult::Rejected,
            None => VolleyResult::Unreported,
        };
        review.volleys.push(Volley {
            tower,
            target: cmd.target,
            result,
        });
    }

    let alive: HashMap<i64, i64> = turn
        .robots
        .iter()
        .filter(|robot| robot.health > 0)
        .map(|robot| (robot.id, robot.health))
        .collect();
    for (id, before) in prev_robot_hp {
        match alive.get(id) {
            Some(after) if after < before => review.robots_damaged.push((*id, before - after)),
            Some(_) => {}
            None => review.kills.push(*id),
        }
    }
    review.robots_damaged.sort_unstable();
    review.kills.sort_unstable();
    review
}
