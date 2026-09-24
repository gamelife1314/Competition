//! Fight: the shared survival predicates the role mainlines call — the
//! withdrawal rule that keeps a wounded controller off a gun it cannot hold,
//! the night potion, the reload-round mend and the one hostile-wave predicate
//! every night chain reads.
//!
//! Moved out of `night.rs` by issue #221's phase 2. The controller↔tower
//! pairing machinery that used to live here died in phase 5c: the night is a
//! SINGLE OPERATOR on the L's inner corner (design D14-D16), so there is no
//! pairing left to stabilize — the wall worker's `plan_night` owns the post by
//! construction, and the trap vetoes are role-based ([`super::geometry`]).

use crate::brain::combat;
use crate::model::{chebyshev, Turn, Unit, UnitKind};
use crate::state::BotState;
/// A robot inside this many cells of a role can reach it this round or the
/// next: the radius at which a wound stops being an inconvenience.
pub(crate) const THREAT_RADIUS: i32 = 3;
/// Max HP in tenths below which a controller with no Medicine breaks contact
/// instead of holding its post (3 = the 30% the day rule heals at, so the two
/// agree on what "critically wounded" means).
pub const WITHDRAW_HEALTH_TENTHS: i64 = 3;
/// Rounds without a robot inside the threat radius before a withdrawn
/// controller is eligible to man a gun again (P1-4 hysteresis). Without the
/// memory, a wounded controller re-enters the pairing the moment a robot steps
/// out of the radius, walks back toward the gun, re-enters the radius and is
/// withdrawn again — every round spent pacing is a round the gun does not fire
/// (issues #22/#23: 30+ `controller_withdrawn` events with the tower silent in
/// between).
const WITHDRAW_HYSTERESIS_ROUNDS: i64 = 5;

/// Refresh the withdrawal holdout set for this round (P1-4).
///
/// A controller enters the set when the withdrawal rule owns it (critically
/// wounded, no Medicine, robots in reach). It leaves only on real evidence
/// that it can hold a post again: healed back above the withdraw line
/// (Medicine — the spare duty in `spare_night` is what delivers it), or the
/// threat has been gone for [`WITHDRAW_HYSTERESIS_ROUNDS`] straight rounds.
/// Holdout controllers are excluded from tower pairings and fall through to
/// the spare duties — shelter and self-heal — which is exactly the loop that
/// gets them back onto a gun. Timestamps carry across days, so a quiet day
/// clears a stale holdout on the first night round.
pub(crate) fn update_withdraw_holdout(turn: &Turn, state: &mut BotState) {
    for role in turn.controllable() {
        let threatened = turn
            .robots
            .iter()
            .any(|robot| robot.health > 0 && chebyshev(robot.pos, role.pos) <= THREAT_RADIUS);
        if threatened {
            state.withdraw_last_threat.insert(role.id, turn.round_no);
        }
        if withdrawing(turn, role) {
            state.withdraw_holdout.insert(role.id);
        }
        if state.withdraw_holdout.contains(&role.id) {
            let max_hp = match role.kind {
                UnitKind::Worker => 220,
                UnitKind::Pioneer => 200,
                _ => 0,
            };
            let recovered = max_hp > 0 && role.health * 10 >= max_hp * WITHDRAW_HEALTH_TENTHS;
            let calm = match state.withdraw_last_threat.get(&role.id) {
                Some(last) => turn.round_no - last >= WITHDRAW_HYSTERESIS_ROUNDS,
                None => true,
            };
            if recovered || calm {
                state.withdraw_holdout.remove(&role.id);
            }
        }
    }
}

/// Is this controller one the night's withdrawal rule will pull off its gun?
///
/// Split out of [`night_withdraw`] so the pairing can ask the same question
/// before it hands out a tower. The two must agree exactly: a pairing that
/// ignores this predicate parks a gun on a controller the very next branch
/// refuses to let fire, and the tower goes silent with a healthy controller
/// standing idle next to it — issue #22's 20012, 20 HP and frozen, holding
/// tower 20020's pairing for 235 rounds while `controller_withdrawn` fired
/// 35 times and the gun never fired at all.
pub fn withdrawing(turn: &Turn, role: &Unit) -> bool {
    if role.count_item("Medicine") > 0 {
        return false;
    }
    let max_hp = match role.kind {
        UnitKind::Worker => 220,
        UnitKind::Pioneer => 200,
        _ => return false,
    };
    if role.health * 10 >= max_hp * WITHDRAW_HEALTH_TENTHS {
        return false;
    }
    turn.robots
        .iter()
        .any(|robot| robot.health > 0 && chebyshev(robot.pos, role.pos) <= THREAT_RADIUS)
}

/// Is a hostile wave on the board? Robots are only hostile to us when their
/// `targetTeam` names our team (the rest fight each other or the enemy). The
/// wall worker's night recall and the pioneer's Q5 backup wait read the SAME
/// predicate, so "the wave is gone" can never mean two different things in two
/// mainlines inside one round.
pub(crate) fn hostile_wave(turn: &Turn) -> bool {
    turn.robots
        .iter()
        .any(|robot| robot.health > 0 && robot.target_team == turn.team_type)
}

/// Heal at night, while the role is still worth saving.
///
/// `shop::use_medicine` waits for 30% health, which is tuned for the day: a
/// role that takes a hit at noon has hours to walk it off, and a potion spent
/// early is 10 gold never coming back. At night there is no walking it off — a
/// focused controller goes from 30% to dead inside the round it is shot in,
/// and the tower it was manning goes silent with it. Issue #18 lost 20011 in
/// six rounds (HP 200→0) and 20010 in TWO (HP 220→0); issue #19 lost all
/// three on D2 night and the base fell from 1500 to 105 HP behind them.
/// Medicine restores FULL health, so a potion spent at 60% buys a gun that
/// fires all night and a survival score that keeps paying — the potion saved
/// buys nothing.
///
/// Below the threat radius (no robot close enough to finish the job this
/// round) the day threshold still applies: a scratch at 3 a.m. can wait.
pub(crate) fn night_medicine(turn: &Turn, role: &Unit) -> Option<crate::protocol::RoleCommand> {
    if role.count_item("Medicine") < 1 {
        return None;
    }
    let max_hp = match role.kind {
        UnitKind::Worker => 220,
        UnitKind::Pioneer => 200,
        _ => return None,
    };
    let threatened = turn
        .robots
        .iter()
        .any(|robot| robot.health > 0 && chebyshev(robot.pos, role.pos) <= THREAT_RADIUS);
    let threshold = if threatened { 7 } else { 3 };
    if role.health * 10 < max_hp * threshold {
        return Some(crate::protocol::RoleCommand::use_item("Medicine"));
    }
    None
}

/// Mend the weakest wall this operator is standing beside, if the gun it mans
/// cannot fire this round anyway.
///
/// See the call sites for why a round the gun cannot fire is a round the wall
/// gets. There are two such windows and both call this: the reload
/// (`tower.cooldown != 0`), and a ready gun whose trigger came up empty
/// because every robot hunting us is outside every tower's reach
/// (`no_target_reserved_for_robots` — 表 6a's second-largest silence bucket in
/// issues #131-#135, 18-42 rounds a match). Neither trades a shot for a mend.
///
/// The predicates live in [`combat::night_mend_target`]: the role carries a
/// `WallFixer`, no live robot is ADJACENT (chebyshev <= 1) to it, and an own
/// wall is ADJACENT and below its level's maximum — weakest first, the same
/// ranking the day's `repair_target` uses. Bounded to one mend per round,
/// which is what one action per role allows.
pub(crate) fn cooldown_repair(
    turn: &Turn,
    role: &Unit,
) -> Option<crate::protocol::RoleCommand> {
    let target = combat::night_mend_target(turn, role)?;
    crate::log::event(
        "wall_mend",
        serde_json::json!({
            "round": turn.round_no,
            "role": role.id,
            "target": [target.x, target.y],
            "duty": "cooldown",
        }),
    );
    Some(crate::protocol::RoleCommand::use_item_at("WallFixer", target))
}

