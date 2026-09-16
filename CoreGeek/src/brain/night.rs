//! Nighttime planning (60 rounds): pair controllers with towers, man them,
//! and shoot; spare controllers use items or shelter near the base.

use std::collections::HashSet;

use crate::brain::{
    break_out, combat, economy, task, tower_stand_cells, walk_or_remove_wall, walk_toward, Plan,
};
use crate::model::{chebyshev, footprint_distance, Turn, Unit, UnitKind};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// A robot inside this many cells of a role can reach it this round or the
/// next: the radius at which a wound stops being an inconvenience.
const THREAT_RADIUS: i32 = 3;
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
fn update_withdraw_holdout(turn: &Turn, state: &mut BotState) {
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

/// Greedy pairing: every living tower gets the closest free controller that
/// can actually REACH it. A pioneer busy with a self-evolution task must stay
/// at the task point and is therefore excluded. Towers under the heaviest
/// pressure pair first so a long walk never starves the position that matters
/// most.
///
/// Distance alone is not enough once the ring is up. The station, the other
/// towers and the wall line between them can leave a gun whose nearest
/// controller sits in a pocket with no route to it: the role then spends the
/// night in `walk_or_remove_wall` returning nothing (issue #13's "0 角色站桩
/// 闲置") while the gun never fires, and `wall_would_trap` refuses to close the
/// ring because THAT role — already cut off — would still be cut off
/// afterwards. Preferring a reachable tower fixes the cause instead of the
/// symptom. When no controller can reach the gun the nearest one is still
/// taken, so the pairing is never worse than the distance-only one.
/// The controllers a pairing may draw from, in one place.
///
/// Every reader of "how many controllers are there" has to agree with this
/// list, or the count answers a different question from the one the pairing
/// asks. It did not, and the disagreement is what made `tower_unpaired` report
/// `pairing_invariant` — "enough controllers existed, yet a tower went
/// unmanned" — on boards where a controller was simply in the withdrawal
/// holdout: `night::plan` counted `turn.controllable()` minus a task-busy
/// pioneer, and `pairing` then refused the held-out controller as well, so the
/// tally said three controllers for three guns while only two were usable. The
/// label sent the reader looking for an ordering bug that did not exist and hid
/// the real one (a gun with nobody to hold it). Both now read this list.
pub fn pairing_controllers<'a>(turn: &'a Turn, state: &BotState) -> Vec<&'a Unit> {
    turn.controllable()
        .into_iter()
        .filter(|role| !(state.task.active && role.kind == UnitKind::Pioneer))
        // P1-4 hysteresis: a controller in withdrawal holdout mans nothing —
        // it shelters and heals as a spare instead of pacing between the gun
        // and the threat radius all night.
        .filter(|role| !state.withdraw_holdout.contains(&role.id))
        .collect()
}

/// Why no controller is holding a gun, as a partition over the three ways it
/// can happen. The same string the `tower_unpaired` log event carries.
///
/// `pairing_invariant` is the assertion, not a diagnosis: `pairing` hands out
/// `min(towers, controllers)` guns, so once the two counts are read off the
/// same list it can never be true. It survives as a label so that a future
/// regression that reintroduces a disagreement is visible in the log instead of
/// silently relabelled.
fn unpair_reason(turn: &Turn, state: &BotState) -> &'static str {
    if state.task.active && turn.pioneer().is_some() {
        "pioneer_task_occupied"
    } else if pairing_controllers(turn, state).len() < turn.towers().len() {
        "no_live_controller"
    } else {
        "pairing_invariant"
    }
}

/// Every tower no controller is holding, with the reason, in tower order.
///
/// The question the `tower_unpaired` event exists to answer, asked of the same
/// pairing the night actually uses — so a test can assert on it directly
/// instead of grepping a captured match.
pub fn unpaired_towers(turn: &Turn, state: &BotState) -> Vec<(i64, &'static str)> {
    let pairs = pairing(turn, state);
    unpaired_of(turn, state, &pairs)
}

/// [`unpaired_towers`] against a pairing that has already been computed (the
/// night planner uses the cached one, so that the log describes the pairing
/// that actually fired).
pub fn unpaired_of(turn: &Turn, state: &BotState, pairs: &[(i64, i64)]) -> Vec<(i64, &'static str)> {
    let paired: HashSet<i64> = pairs.iter().map(|(_, tower)| *tower).collect();
    let reason = unpair_reason(turn, state);
    turn.towers()
        .iter()
        .filter(|tower| !paired.contains(&tower.id))
        .map(|tower| (tower.id, reason))
        .collect()
}

pub fn pairing(turn: &Turn, state: &BotState) -> Vec<(i64, i64)> {
    let mut towers = turn.towers();
    towers.sort_by_cached_key(|tower| std::cmp::Reverse(combat::threat_load(turn, tower)));
    let mut controllers: Vec<&Unit> = pairing_controllers(turn, state);
    let mut pairs: Vec<(i64, i64)> = Vec::new();
    for tower in towers {
        if controllers.is_empty() {
            break;
        }
        let stands = tower_stand_cells(turn, tower.pos);
        let nearest = |wanted: &dyn Fn(&Unit) -> bool| -> Option<usize> {
            let mut best: Option<(i32, usize)> = None;
            for (index, role) in controllers.iter().enumerate() {
                if !wanted(role) {
                    continue;
                }
                let dist = chebyshev(role.pos, tower.pos);
                if best.map(|(best_dist, _)| dist < best_dist).unwrap_or(true) {
                    best = Some((dist, index));
                }
            }
            best.map(|(_, index)| index)
        };
        let reachable = |role: &Unit| crate::brain::can_reach_any(turn, role, &stands);
        // A controller the night will withdraw mans nothing, so it must not
        // take a gun away from one that can hold it: issue #22's 20012 spent
        // 235 rounds at 20 HP holding tower 20020's pairing, and the tower was
        // silent the whole time. Preferring a FIT reachable controller keeps
        // the gun firing and lets the wounded one fall through to the spare
        // duties — which is where the shelter and the heal live. The two
        // fallbacks after it stay: reachable-but-wounded still beats a
        // controller that cannot get to the tower at all, and distance still
        // decides when nobody is fit.
        let index = nearest(&|role: &Unit| reachable(role) && !withdrawing(turn, role))
            .or_else(|| nearest(&reachable))
            .or_else(|| nearest(&|_| true))
            .expect("controllers is non-empty");
        let role = controllers.remove(index);
        pairs.push((role.id, tower.id));
    }
    pioneer_post(turn, &mut pairs);
    pairs
}

/// Which gun the pioneer should be holding (issue #206 §7).
///
/// The greedy pass above hands every gun the controller standing nearest it,
/// which is the right question for a worker and the wrong one for the pioneer.
/// Two facts make the pioneer special, and both are structural rather than
/// tactical:
///
/// * **It cannot cut its own way out.** 任务书 4.4 gives `remove` to 工人 only,
///   `validate.rs` enforces it (a pioneer `remove` is dropped), and the whole
///   escape machinery — `walk_or_remove_wall`, `break_out`, `open_door` — is
///   reached from `worker_day` alone. A worker walled off its gun digs through;
///   a pioneer walled off its gun stands there. The owner's picture —
///   「开拓者如果夹在武器、基地以及城墙之间，除非有人来救否则就出不去了」 — is
///   exactly this, and it is a fact about the rulebook, not about the board.
/// * **It is the role whose day is spent outside.** Task points, the altar and
///   the shop are all outside the ring, so the pioneer is the controller most
///   likely to be arriving at dusk while the last ring cells go down.
///
/// So its post is chosen for it, over every gun it could hold, on the two
/// questions the owner asked of the post itself — 「最安全的位置」 and 「让他操作
/// 可远程攻击的导弹」:
///
/// 1. **Is it the safest one?** Distance from the enemy station, best over that
///    gun's stands, maximised. Robots come from the enemy bearing, so the gun
///    furthest from it is the one whose operator spends the fewest nights under
///    fire.
/// 2. **Is it the long-range one?** The rocket reaches 10 / 15 / 全图 and is
///    the only weapon that does not care where it is sited.
///
/// The third thing the owner asked for — 「给它留个门」 — is not a property of
/// the post and is not decided here: the door is cut for the pioneer by the
/// crew every morning it is sealed in (`day::open_door`), because the pioneer
/// is the one role that cannot cut it for itself.
///
/// The move is a SWAP of two controllers, never a re-run of the greedy pass.
/// A permutation cannot unman a gun, so "every tower has a controller" is held
/// by construction rather than by argument — the invariant `tower_unpaired`
/// exists to watch. A swap is taken only when it strictly improves the key and
/// only when BOTH controllers can still reach their new guns, so it cannot
/// trade one silent gun for another. Ties fall through to `(x, y)`, the
/// codebase's usual last word, so the same board always yields the same post.
///
/// The improvement has to be MATERIAL (`post_upgrade`), and that word is doing
/// real work: the greedy pass hands a gun to the controller standing next to
/// it, and moving the pioneer onto a gun one cell further from the enemy takes
/// that gun away from whoever was beside it and leaves them walking. That trade
/// was measured — on the interface doc's own board it handed the gatling to a
/// pioneer eight cells away and left the worker who had been standing on it
/// with nothing to shoot with (`tests/replay.rs`). A one-cell difference in
/// exposure is not a safety difference; it is a tie that the ranking already
/// breaks without moving anybody.
fn pioneer_post(turn: &Turn, pairs: &mut [(i64, i64)]) {
    let Some(pioneer) = turn.pioneer() else {
        return;
    };
    let Some(mine) = pairs
        .iter()
        .position(|(controller, _)| *controller == pioneer.id)
    else {
        return;
    };
    let here = pairs[mine].1;
    let Some(current) = turn.role_by_id(here) else {
        return;
    };
    let mut best: Option<(usize, PostRank)> = None;
    for (index, (controller, tower_id)) in pairs.iter().enumerate() {
        if index == mine {
            continue;
        }
        let (Some(tower), Some(other)) = (turn.role_by_id(*tower_id), turn.role_by_id(*controller))
        else {
            continue;
        };
        if !post_upgrade(turn, current, tower) {
            continue;
        }
        if !crate::brain::can_reach_any(turn, pioneer, &tower_stand_cells(turn, tower.pos)) {
            continue;
        }
        if !crate::brain::can_reach_any(turn, other, &tower_stand_cells(turn, current.pos)) {
            continue;
        }
        let rank = post_rank(turn, tower);
        if best.map(|(_, best_rank)| post_better(rank, best_rank)).unwrap_or(true) {
            best = Some((index, rank));
        }
    }
    if let Some((index, _)) = best {
        let (controller, tower_id) = pairs[index];
        pairs[index] = (controller, here);
        pairs[mine] = (pioneer.id, tower_id);
    }
}

/// How much further from the enemy bearing a gun has to be before moving the
/// pioneer onto it is a SAFETY decision rather than noise.
///
/// One cell is not a difference at all: two cells a Chebyshev step apart share
/// four neighbours, so a robot standing on any of them is equally close to both
/// and the two posts are the same post. Two is the smallest separation at which
/// they share no neighbour — and it is also the largest this decision can
/// express, because the interior band of a 2x2 station is four columns wide and
/// a gun's operating cells reach one column past its own. A larger margin would
/// make 「最安全的位置」 unreachable on every board this game produces, which is
/// the same as not asking the question.
const PIONEER_SAFE_MARGIN: i32 = 2;

/// A gun's post, by [`pioneer_post`]'s two questions.
#[derive(Clone, Copy)]
struct PostRank {
    /// Distance from the enemy bearing, best over the gun's stands. Bigger is
    /// safer.
    exposure: i32,
    /// 1 for everything but the rocket, which reaches 10 / 15 / 全图.
    short_range: i32,
    x: i32,
    y: i32,
}

fn post_rank(turn: &Turn, tower: &Unit) -> PostRank {
    let stands = tower_stand_cells(turn, tower.pos);
    // A board with no enemy station left to measure against is treated as
    // maximally safe rather than as zero, so reach still decides.
    let exposure = turn
        .enemy_station()
        .map(|station| {
            stands
                .iter()
                .map(|pos| chebyshev(*pos, station.pos))
                .min()
                .unwrap_or(0)
        })
        .unwrap_or(i32::MAX);
    PostRank {
        exposure,
        short_range: i32::from(tower.kind != UnitKind::Rocket),
        x: tower.pos.x,
        y: tower.pos.y,
    }
}

/// The order two posts are preferred in: safer first, then the longer reach,
/// then `(x, y)` — the codebase's usual last word.
fn post_better(a: PostRank, b: PostRank) -> bool {
    let key = |rank: PostRank| {
        (
            rank.exposure,
            std::cmp::Reverse(rank.short_range),
            std::cmp::Reverse(rank.x),
            std::cmp::Reverse(rank.y),
        )
    };
    key(a) > key(b)
}

/// Is `candidate` worth moving the pioneer off `current` for?
///
/// Reach and shelter trade only inside the noise band; outside it, shelter
/// wins, because a rocket the pioneer cannot live long enough to fire is not a
/// better gun.
fn post_upgrade(turn: &Turn, current: &Unit, candidate: &Unit) -> bool {
    let now = post_rank(turn, current);
    let new = post_rank(turn, candidate);
    if new.exposure >= now.exposure + PIONEER_SAFE_MARGIN {
        return true; // materially further from the enemy bearing
    }
    if new.exposure + PIONEER_SAFE_MARGIN <= now.exposure {
        return false; // materially closer to it: not worth the reach
    }
    new.short_range < now.short_range
}

/// Stable controller↔tower pairings. The cache key includes every living
/// tower/controller and whether the pioneer is occupied by a task. This keeps
/// assignments stable while all members remain usable, but replaces a dead or
/// newly freed controller in the very next round.
pub fn stable_pairs(turn: &Turn, state: &mut BotState) -> Vec<(i64, i64)> {
    let tower_ids: Vec<i64> = turn.towers().iter().map(|tower| tower.id).collect();
    let controller_ids: Vec<i64> = turn.controllable().iter().map(|role| role.id).collect();
    let task_busy = state.task.active;
    // Who is too wounded to hold a gun this round. It belongs in the cache key
    // with the other membership facts: the pairing now depends on it, and a
    // cached pairing that outlives a controller's wound is exactly how the gun
    // stays silent (issue #22).
    let mut withdrawing_ids: Vec<i64> = turn
        .controllable()
        .iter()
        .filter(|role| withdrawing(turn, role) || state.withdraw_holdout.contains(&role.id))
        .map(|role| role.id)
        .collect();
    withdrawing_ids.sort_unstable();

    let needs_recompute = state.night_pairs.is_empty()
        || state.night_pair_day != turn.day
        || state.night_pair_tower_ids != tower_ids
        || state.night_pair_controller_ids != controller_ids
        || state.night_pair_task_busy != task_busy
        || state.night_pair_withdrawing != withdrawing_ids;

    if needs_recompute {
        let reason = if state.night_pairs.is_empty() {
            "empty"
        } else if state.night_pair_day != turn.day {
            "day"
        } else if state.night_pair_tower_ids != tower_ids {
            "towers"
        } else if state.night_pair_controller_ids != controller_ids {
            "controllers"
        } else if state.night_pair_withdrawing != withdrawing_ids {
            "withdrawing"
        } else {
            "task_occupancy"
        };
        state.night_pairs = pairing(turn, state);
        state.night_pair_day = turn.day;
        state.night_pair_tower_ids = tower_ids;
        state.night_pair_controller_ids = controller_ids;
        state.night_pair_task_busy = task_busy;
        state.night_pair_withdrawing = withdrawing_ids;
        crate::log::event(
            "pair_recomputed",
            serde_json::json!({"round": turn.round_no, "reason": reason, "pairs": state.night_pairs}),
        );
    }
    state.night_pairs.clone()
}

pub fn operator_shortage(turn: &Turn) -> bool {
    let towers = turn.towers().len();
    let free_controllers = turn
        .controllable()
        .iter()
        .filter(|role| role.kind != UnitKind::Pioneer)
        .count();
    towers > free_controllers
}

pub fn defense_needs_pioneer(turn: &Turn) -> bool {
    let shortage = operator_shortage(turn);
    let station = turn.station();
    let station_damaged = station.map(|unit| unit.health < 1500).unwrap_or(false);
    let close_threat = station
        .map(|unit| {
            let footprint = unit.footprint();
            turn.robots.iter().any(|robot| {
                robot.health > 0
                    && robot.target_team == turn.team_type
                    && footprint_distance(robot.pos, &footprint) <= 8
            })
        })
        .unwrap_or(false);
    shortage && (station_damaged || close_threat)
}

pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let mut plan = Plan::default();
    let mut claimed: HashSet<Pos> = HashSet::new();

    // A task must never cost an operated tower while robots are actively
    // threatening our side. Release the pioneer before pairing so it can be
    // recalled in this same round. In quiet nights, two operators may cover two
    // towers while a task continues.
    if state.task.active && defense_needs_pioneer(turn) {
        crate::log::event(
            "task_defense_abort",
            serde_json::json!({"round": turn.round_no, "session": state.task.session_id, "task_kind": state.task.kind.as_str(), "reason": "unmanned_tower_under_threat"}),
        );
        state.finish_task(false, "night_defense");
    }

    // P1-4 hysteresis: settle who is in withdrawal holdout BEFORE pairing, so
    // the pairing never hands a gun to a controller the withdrawal rule owns.
    update_withdraw_holdout(turn, state);

    let mut pairs = stable_pairs(turn, state);
    // Fire the towers under the heaviest pressure first: they get first pick
    // of the shared per-round damage simulation (avoids cross-tower overkill).
    pairs.sort_by_cached_key(|(_controller_id, tower_id)| {
        let load = turn
            .role_by_id(*tower_id)
            .map(|tower| combat::threat_load(turn, tower))
            .unwrap_or(0);
        std::cmp::Reverse(load)
    });
    let mut paired: HashSet<i64> = HashSet::new();
    // The unpaired report is derived, not re-counted: `unpaired_of` asks the
    // same question the pairing does, so the reason a tower is dark cannot
    // disagree with the reason the pairing refused to hand it out (see
    // `pairing_controllers`).
    for (tower, reason) in unpaired_of(turn, state, &pairs) {
        crate::log::event(
            "tower_unpaired",
            serde_json::json!({"round": turn.round_no, "tower": tower, "reason": reason}),
        );
    }
    let mut sim = combat::init_sim(turn);
    // Snapshot the coach's policy once: the tower loop below borrows `state`
    // mutably (to report enemy fire back to the coach), and the stance must not
    // change halfway through one round's firing sequence.
    let coach_policy = state.coach.policy();
    // THE ROUND'S ASSIGNMENT (issue #206 §3b). Every tower that will pull a
    // trigger tonight, in the order it will pull it — the same filter, in the
    // same order, as the loop below. `plan_round` answers "who does each gun
    // shoot at" for all of them at once; each tower is then fired with the cell
    // the round gave it, and one the plan did not name fires exactly as it
    // always has. Passing the list rather than the loop means the plan is
    // decided before the first tower has changed the simulation — which is the
    // whole point of an assignment.
    let firing: Vec<(i64, i64)> = pairs
        .iter()
        .filter(|(controller_id, tower_id)| {
            let (Some(tower), Some(controller)) =
                (turn.role_by_id(*tower_id), turn.role_by_id(*controller_id))
            else {
                return false;
            };
            // `night_medicine` and the distance are what the loop itself tests
            // before it reaches the trigger; anything that fails them is a
            // tower that will not fire, and the plan must not spend a shot on
            // it (`simulate_round` would otherwise score a round nobody has).
            tower.alive()
                && controller.alive()
                && tower.cooldown == 0
                && chebyshev(controller.pos, tower.pos) <= 1
                && night_medicine(turn, controller).is_none()
        })
        .map(|(controller_id, tower_id)| (*tower_id, *controller_id))
        .collect();
    let aims = combat::plan_round(&coach_policy, turn, &firing);

    // Per-pair diagnostics, folded into one record after the loop — see the
    // note at the push site.
    let mut night_rows: Vec<serde_json::Value> = Vec::new();

    for (controller_id, tower_id) in &pairs {
        let (Some(tower), Some(controller)) =
            (turn.role_by_id(*tower_id), turn.role_by_id(*controller_id))
        else {
            continue;
        };
        if !tower.alive() || !controller.alive() {
            continue;
        }
        // Claim only a valid, able controller — an unusable one falls through
        // to spare duties instead of idling next to a dead tower all night.
        paired.insert(*controller_id);
        let dist = chebyshev(controller.pos, tower.pos);
        // At night "in position" is the game's own rule — `chebyshev <= 1`,
        // which is also what `validate.rs` requires of an attack keyed to this
        // tower. The recall below exists to MAN the gun, so a controller
        // already able to fire must fire rather than walk: it is the day lock
        // that insists on the inner operating cell, because only there can the
        // ring be sealed around it.
        let adjacent = dist <= 1;
        let mut targets_count: usize = 0;
        let mut fired = false;
        let mut enemy_fire = false;
        // Why a ready weapon stayed silent. "could fire but did not" was the
        // single hardest failure to diagnose from the logs (battles pk575060 /
        // pk575098 / pk575557), so each idle tower now carries its own reason.
        let mut idle_reason = "fired";
        // Set when the round's command is a demolition rather than a step: the
        // row then reads `controller_digging`, which keeps "cutting a way in"
        // distinguishable from "walking in" in table 6b.
        let mut digging = false;
        // `(pos, operating cells, adjacent walls of ours)` when the recall
        // could not move the controller at all — see the `controller_stuck`
        // branch below for what the three numbers answer.
        let mut recall_site: Option<(Pos, usize, usize)> = None;

        // SURVIVAL OUTRANKS THE POST. A controller that is about to die on its
        // own operating cell (or on the way to it) mans nothing: the gun goes
        // silent either way, and it takes a surviving role — and the score that
        // comes with it — down with the tower. Breaking contact is checked
        // before both the recall and the trigger so the two never alternate:
        // the predicate is about the wound and the robots, not about where the
        // controller is standing, so a withdrawn controller is never walked
        // back out and a controller whose wound has stopped being an emergency
        // is recalled again the same round it becomes one.
        if night_withdraw(turn, controller, &mut claimed, &mut plan) {
            // The wound that triggered it rides on the pair's row below, so the
            // `controller_withdrawn` rounds a gun spent silent and the HP it
            // was frozen at stay in one record instead of two.
            idle_reason = "controller_withdrawn";
        } else if !adjacent {
            // NIGHT RECALL (recurring defect): a controller not adjacent to its
            // tower MUST move there, outranking every other night duty (heal,
            // items, shelter, economy). Battle pk575098 / pk575557 left towers
            // idle all night because their operators were never recalled — this
            // runs every round until the controller is at an operating cell.
            let stands = tower_stand_cells(turn, tower.pos);
            let mut moved = false;
            if let Some(cmd) = walk_or_remove_wall(turn, controller, &stands, &mut claimed) {
                plan.push(controller.id, cmd);
                moved = true;
            } else {
                // Fallback: ignore claimed cells, take any reachable step so a
                // teammate's committed move never freezes the recall.
                let mut ignored = HashSet::new();
                if let Some(cmd) = walk_or_remove_wall(turn, controller, &stands, &mut ignored) {
                    plan.push(controller.id, cmd);
                    moved = true;
                } else if let Some(cmd) = break_out(turn, controller, &stands, &mut ignored) {
                    // POCKET ESCAPE (issues #126-#130). `walk_or_remove_wall`
                    // refuses to cut a wall unless that one cut reopens the
                    // route, so a controller two walls deep — or in a pocket
                    // whose single adjacent wall leads nowhere — issues NO
                    // command at all and stands on the same cell until dawn:
                    // 126's 20040 for 11 rounds at (27,13), 129's 20020 for 14
                    // at (32,10). Both guns were silent for the whole night —
                    // and 126's role is the one `wall_gate_open` named on all 11
                    // dusk rounds of that day, so the ring only closed on the
                    // hard deadline. Cutting and stepping toward the post are
                    // the only moves that change anything; see `break_out`.
                    crate::log::event(
                        "break_out",
                        serde_json::json!({
                            "round": turn.round_no,
                            "role": controller.id,
                            "tower": *tower_id,
                            "action": cmd.action,
                            "from": [controller.pos.x, controller.pos.y],
                        }),
                    );
                    digging = cmd.action == "remove";
                    plan.push(controller.id, cmd);
                    moved = true;
                }
            }
            // WHY IT COULD NOT MOVE (issues #121-#125). `controller_stuck` is
            // the second-largest silence reason in the batch after `cooldown`
            // — 11 rounds for tower 20020 in #125, 10 in #122, 3-4 elsewhere —
            // and #125 has a controller frozen on the SAME cell (30,13) for all
            // fifteen rounds of the night, which is a whole gun silent for a
            // whole night. From the log alone the cause was unreadable: the row
            // carried the tower, the controller and the reason, and none of the
            // three things the answer needs. So the failing rounds now carry
            // where the controller stood, how many operating cells it was
            // walking to, and whether an adjacent wall of ours was even
            // available to cut — the three inputs `walk_or_remove_wall`
            // decides on.
            if !moved {
                recall_site = Some((
                    controller.pos,
                    stands.len(),
                    turn.walls()
                        .into_iter()
                        .filter(|wall| chebyshev(controller.pos, wall.pos) == 1)
                        .count(),
                ));
            }
            // No third fallback onto plain `stand_cells`. The inner cells are
            // the ones behind the wall line, and everything `stand_cells` adds
            // beyond them is the wall ring itself — a controller parked there
            // is outside the wall it should be behind (issue #15), and the cell
            // under its feet can never be walled. `walk_or_remove_wall` above
            // already demolishes our own wall when the inner cells are sealed
            // off, so the gun is still reached the honest way. A controller
            // that cannot get there at all reports `controller_stuck` instead
            // of standing in the open all night.
            idle_reason = if digging {
                "controller_digging"
            } else if moved {
                "controller_walking"
            } else {
                "controller_stuck"
            };
        } else {
            // Adjacent: a badly hurt operator heals first (a dead one mans
            // nothing), otherwise man the tower.
            if let Some(cmd) = night_medicine(turn, controller) {
                idle_reason = "controller_healing";
                plan.push(controller.id, cmd);
            } else if tower.cooldown == 0 {
                if let Some((targets, kind)) = combat::choose_attack_kind_aimed(
                    &coach_policy,
                    turn,
                    tower,
                    &mut sim,
                    aims.get(&tower.id).copied(),
                ) {
                    targets_count = targets.len();
                    fired = true;
                    enemy_fire = kind == combat::TargetKind::EnemyAssets;
                    if enemy_fire {
                        // Tell the coach our own guns hit their buildings this
                        // night: that damage is not evidence about the summons.
                        state.coach.note_enemy_fire();
                    }
                    plan.push(tower.id, RoleCommand::attack(controller.id, targets));
                } else if !combat::spare_firepower(turn) {
                    // Robots hunting us are outside every ready tower's reach:
                    // hold the opportunistic shot (issue #7's "有余力时") — and
                    // spend the round on the wall instead, for the same reason
                    // the reload window is mason time below. There is no shot to
                    // trade here: the trigger already came up empty for this
                    // tower this round, and this is the second-largest silence
                    // bucket in the batch (表 6a: 42/41/30/27/18 rounds a match).
                    idle_reason = "no_target_reserved_for_robots";
                    if let Some(cmd) = cooldown_repair(turn, controller) {
                        idle_reason = "mending";
                        plan.push(controller.id, cmd);
                    }
                } else {
                    idle_reason = "no_target_in_range";
                }
            } else {
                idle_reason = "cooldown";
                // THE RELOAD IS MASON TIME (issues #126-#130).
                //
                // A rocket pad fires once every three rounds
                // (`choose_rocket` + the 3-round cooldown of 任务书 4.5), so it
                // spends most of the night reloading: `cooldown` is 36 rounds
                // for 129's 20040, 33 for 127's 10040, **51 for 126's 20040** —
                // the single largest idle bucket in the batch after `fired`.
                // The operator stands next to the gun doing nothing, because
                // every spare duty below belongs to an UNPAIRED role and every
                // controller is paired during an assault.
                //
                // Those are exactly the rounds the wall needs. The wall is what
                // the night is actually spent on — `ourWallLost` ran 2000-14335
                // a night in these five matches, and the station behind it fell
                // on night 1-3 in four of them — and `WallFixer` restores its
                // target to FULL HP for 10 gold (任务书 5.x: "目标坐标所在围墙
                // 回满血"), which is the cheapest HP on the board by a wide
                // margin. A round the gun cannot fire is a free round of it.
                //
                // Gated on `tower.cooldown != 0` so a shot is never traded for a
                // mend, and on no robot being ADJACENT to the operator: a robot
                // within one cell means it is being meleed, and that is the
                // withdraw/heal case, not the mason's.
                if let Some(cmd) = cooldown_repair(turn, controller) {
                    idle_reason = "mending";
                    plan.push(controller.id, cmd);
                }
            }
            // The controller holds position (no command) to stay adjacent.
        }
        // One row per pair, folded into a single record after the loop. This
        // used to be a `night_debug` line AND a `night_recall` line per pair
        // per round — 3 pairs x 2 records x 60 night rounds x 10 days, about
        // 250 KB a match at ~400 bytes each, most of it the same
        // `controller_pos`/`tower_pos`/`pairs_count` restated every round.
        //
        // `reason` is the whole diagnosis: `controller_walking` and
        // `controller_stuck` are the recall's two outcomes, `cooldown` is the
        // gun recharging, and `no_target_in_range` against
        // `no_target_reserved_for_robots` says whether the silence was the map
        // or our own trigger discipline. That is why `dist`, `cooldown` and
        // the two positions do not need their own keys.
        let mut row = serde_json::json!({
            "tower": *tower_id,
            "controller": *controller_id,
            "reason": idle_reason,
        });
        if fired {
            row["fired"] = serde_json::json!(targets_count);
            if enemy_fire {
                row["enemyAssets"] = serde_json::json!(true);
            }
        }
        if idle_reason == "controller_withdrawn" {
            row["hp"] = serde_json::json!(controller.health);
        }
        // `stuck`: where the controller is, how many operating cells it was
        // walking to, and how many of our own walls it could have cut. A zero
        // in the third slot with a non-zero second says the pocket has no
        // adjacent wall left to open — the one case `walk_or_remove_wall`
        // cannot answer, and the one the next batch needs to size before
        // widening the hatch.
        if let Some((pos, stands, walls)) = recall_site {
            row["stuck"] = serde_json::json!([pos.x, pos.y, stands, walls]);
        }
        night_rows.push(row);
    }
    // Still one record every night round, deliberately: the count of rounds a
    // tower spent `controller_withdrawn` IS the finding (35 次
    // controller_withdrawn, 塔全程沉默), so a record that only appeared
    // when the reason changed would delete the duration. What it no longer does
    // is repeat the positions and the pair count, which were the same every
    // round and were two thirds of the bytes.
    crate::log::event(
        "night_debug",
        serde_json::json!({"round": turn.round_no, "robots": turn.robots.len(), "pairs": night_rows}),
    );

    // Spare controllers.
    let controllers: Vec<&Unit> = turn.controllable();
    for role in controllers {
        if paired.contains(&role.id) {
            continue;
        }
        spare_night(turn, state, role, &mut claimed, &mut plan);
    }

    plan
}

fn spare_night(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    // Pioneer keeps working an active task through the night.
    if role.kind == UnitKind::Pioneer && state.task.active {
        if let Some(cmd) = task::plan_pioneer(turn, state, role, plan) {
            plan.push(role.id, cmd);
        }
        return;
    }
    // Self-heal — survival outranks every other spare duty.
    if let Some(cmd) = night_medicine(turn, role) {
        plan.push(role.id, cmd);
        return;
    }
    // Retreat inside the wall ring next to the station BEFORE anything that
    // keeps the role out in the open — a lone spare next to a mine or wall is
    // easy to focus down. Once inside we fall through to item usage.
    if shelter(turn, role, claimed, plan) {
        return;
    }
    // THE SPARE'S MASON ROUND. The paired controller only gets a mend on a
    // reload round (`cooldown_repair`), and issues #131-#135 measured the ring
    // losing 2465-15135 HP a night with the base behind it falling on night 1-3.
    // A spare standing on the inner band — which is where `shelter` just put it,
    // and the band is adjacent to the ring — is the second pair of hands the
    // night never used: `repair_target(.., 3)` below is false for every wall
    // under attack (see `combat::night_mend_target`). Deliberately placed AFTER
    // the shelter move, so the no-night-mining rule still owns the walk home,
    // and BEFORE the items, because a wall restored to full is worth more than
    // a Bomb that may not land.
    if let Some(wall_pos) = combat::night_mend_target(turn, role) {
        crate::log::event(
            "wall_mend",
            serde_json::json!({
                "round": turn.round_no,
                "role": role.id,
                "target": [wall_pos.x, wall_pos.y],
                "duty": "spare",
            }),
        );
        plan.push(role.id, RoleCommand::use_item_at("WallFixer", wall_pos));
        return;
    }
    // Burn summon orders (harassment works at night too).
    const ORDERS: [&str; 4] = [
        "BossRobotSummonOrder",
        "LargeRobotSummonOrder",
        "MiddleRobotSummonOrder",
        "SmallRobotSummonOrder",
    ];
    for order in ORDERS {
        if role.count_item(order) > 0 && state.summon_orders_today < 10 {
            state.consume_summon_order();
            state.harass_done_today = true;
            // P2-3 压制窗口: the boss order is first in ORDERS, so a spare that
            // carries one already fires it ahead of the smaller waves. What
            // this records is WHY it was worth buying today — a wave aimed at
            // a base that is nearly down or at towers inside one blast radius
            // is the shot the window opened for, and the round it lands is the
            // only place that can be confirmed from the log.
            if order == "BossRobotSummonOrder" && economy::boss_suppression_window(turn) {
                crate::log::event(
                    "boss_suppression",
                    serde_json::json!({
                        "round": turn.round_no,
                        "role": role.id,
                        "enemyStationHp": turn.enemy_station().map(|station| station.health),
                        "enemyTowers": turn
                            .enemy
                            .iter()
                            .filter(|unit| unit.kind.is_tower() && unit.alive())
                            .count(),
                    }),
                );
            }
            plan.push(role.id, RoleCommand::use_item(order));
            return;
        }
    }
    // (The wall patch used to sit here as `combat::repair_target(turn, role, 3)`.
    // It can never fire: its `safe_radius` is measured from the WALL, so it is
    // false for every wall with a robot within two cells — i.e. every wall the
    // night is losing — and it is a strict subset of the `night_mend_target`
    // call above, which has already declined by the time control reaches here.
    // Mending moved up, before the items.)
    // Bomb: worth it against clusters (>= 2 robots) or big targets.
    if role.count_item("Bomb") > 0 {
        if let Some(impact) = combat::bomb_impact(turn) {
            let clustered = turn
                .robots
                .iter()
                .filter(|robot| robot.health > 0 && chebyshev(impact, robot.pos) <= 1)
                .count();
            let big = turn.robots.iter().any(|robot| {
                robot.health > 0
                    && chebyshev(impact, robot.pos) <= 1
                    && combat::is_big_threat(robot.kind)
            });
            if clustered >= 2 || big {
                plan.push(role.id, RoleCommand::use_item_at("Bomb", impact));
                return;
            }
        }
    }
    // Dizzy: stall a wave pressing our base.
    if role.count_item("DizzyWeapon") > 0 {
        if let Some(impact) = combat::dizzy_impact(turn) {
            plan.push(role.id, RoleCommand::use_item_at("DizzyWeapon", impact));
            return;
        }
    }
    // (Shelter already ran above; reaching here means the role is inside the
    // ring and has nothing else to throw at the robots.)
}

/// Heal at night, while the role is still worth saving.
///
/// `day::use_medicine` waits for 30% health, which is tuned for the day: a role
/// that takes a hit at noon has hours to walk it off, and a potion spent early
/// is 10 gold never coming back. At night there is no walking it off — a
/// focused controller goes from 30% to dead inside the round it is shot in, and
/// the tower it was manning goes silent with it. Issue #18 lost 20011 in six
/// rounds (HP 200→0) and 20010 in TWO (HP 220→0); issue #19 lost all three on
/// D2 night and the base fell from 1500 to 105 HP behind them. Medicine
/// restores FULL health, so a potion spent at 60% buys a gun that fires all
/// night and a survival score that keeps paying — the potion saved buys
/// nothing.
///
/// Below the threat radius (no robot close enough to finish the job this
/// round) the day threshold still applies: a scratch at 3 a.m. can wait.
fn night_medicine(turn: &Turn, role: &Unit) -> Option<RoleCommand> {
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
        return Some(RoleCommand::use_item("Medicine"));
    }
    None
}

/// Mend the weakest wall this operator is standing beside, if the gun it mans
/// cannot fire this round anyway.
///
/// See the call site for why a round the gun cannot fire is a round the wall
/// gets. There are two such windows and both call this: the reload
/// (`tower.cooldown != 0`), and a ready gun whose trigger came up empty because
/// every robot hunting us is outside every tower's reach
/// (`no_target_reserved_for_robots` — 表 6a's second-largest silence bucket in
/// issues #131-#135, 18-42 rounds a match). Neither trades a shot for a mend.
///
/// The predicates live in [`combat::night_mend_target`]:
///
///   * it carries a `WallFixer` — the item is the whole errand, and a role
///     without one has nothing to mend with;
///   * no live robot is ADJACENT to the operator (chebyshev <= 1). Robots shoot
///     from three cells (任务书 4.7.2), so a wall being chewed from two cells
///     out is still repairable; a robot on the operator's own cell means it is
///     being meleed and the heal/withdraw rules own that round;
///   * an own wall of ours is ADJACENT and below its level's maximum, taken
///     weakest first — the same ranking the day's `repair_target` uses, and the
///     wall the night is closest to losing.
///
/// Bounded to one mend per round, which is what one action per role allows.
fn cooldown_repair(turn: &Turn, role: &Unit) -> Option<RoleCommand> {
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
    Some(RoleCommand::use_item_at("WallFixer", target))
}

/// Break contact with a controller that has run out of ways to survive.
///
/// Issue #20: 20010 manned tower 20020 through D1 night while robots that had
/// already breached the ring hit it round after round — HP 220 → 30 with no
/// move command anywhere in the log. It was still firing on the round it died,
/// and the tower went silent for the rest of the night regardless. The recall
/// below is unconditional by design (an unmanned gun is the defect it exists to
/// prevent), but a recall that walks a dying controller back onto the cell it
/// is being shot on is not manning the gun, it is feeding the robots: the same
/// gun falls silent one round later, minus the operator and its score.
///
/// So: below [`WITHDRAW_HEALTH_TENTHS`] of max HP with no Medicine to undo it
/// and robots inside the threat radius, the controller goes behind the ring
/// instead of holding or taking its post. Medicine is checked first because a
/// potion is strictly better than a retreat — it restores FULL health, which
/// puts the gun back in action instead of losing it. Everything else about the
/// night is unchanged: this only ever fires on a controller that is one volley
/// from death.
///
/// Returns true when the survival rule owns this controller's round.
fn night_withdraw(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>, plan: &mut Plan) -> bool {
    if !withdrawing(turn, role) {
        // Unwounded, carrying a potion, or merely hurt with nobody in reach:
        // the post is still the best place for it.
        return false;
    }
    if crate::brain::interior_cells(turn).contains(&role.pos) {
        return true; // already behind the ring: hold, do not walk back out
    }
    // `shelter` moves it one step inside; if it cannot move at all it returns
    // false and the controller falls through to the normal night duty rather
    // than standing frozen — a role that cannot retreat should still shoot.
    shelter(turn, role, claimed, plan)
}

/// Move a spare role inside the wall ring, right next to the station. Returns
/// true when a movement command was issued (the caller should stop planning
/// this round). Uses only the cells at footprint distance <= 1 — hugging the
/// station — so the role never stops on the wall line or out near the mines.
fn shelter(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>, plan: &mut Plan) -> bool {
    // The same set the day planner retreats to and the wall gate measures
    // "everyone is inside" against; sharing it keeps the three in step.
    let mut stands: Vec<Pos> = crate::brain::interior_cells(turn);
    if stands.is_empty() {
        return false;
    }
    if stands.iter().any(|stand| *stand == role.pos) {
        return false; // already hugging the station
    }
    // Prefer the corner furthest from the nearest robot.
    stands.sort_by_cached_key(|stand| {
        let nearest = turn
            .robots
            .iter()
            .filter(|robot| robot.health > 0)
            .map(|robot| chebyshev(*stand, robot.pos))
            .min()
            .unwrap_or(i32::MAX);
        std::cmp::Reverse(nearest)
    });
    if let Some(cmd) = walk_toward(turn, role, &stands, claimed) {
        plan.push(role.id, cmd);
        return true;
    }
    // A SPARE ROLE THE RING CLOSED ON HAS NO WAY BACK IN (issues #176-#185).
    //
    // This walk was plain `walk_toward`, and it is the last resort of a role
    // that holds no tower. The paired controller has a three-rung ladder for
    // exactly this (the recall above: claims, then no claims, then `break_out`),
    // but a spare has nothing — `night_goal` returns None for it, so it is not
    // in `pairs`, so it never reaches that block. `gate_open_record` is just as
    // strict about it: a spare counts as home only by standing within one cell
    // of the station footprint, with no `interior_cells` substitute.
    //
    // 178 day 2 is the case: `wall_gate_forced` seals the ring with 20011 on
    // (24,16) and 20012 on (28,6), both outside, and that match carries the
    // batch's worst `tower_unpaired` count (78) — two of three guns lost on the
    // night the base started dying. Being behind the ring is the whole of a
    // spare's night duty (任务书: the three valid night duties are operate,
    // heal, retreat), so a spare that cannot walk in must cut its way in, the
    // same as the controller next to it.
    let mut ignored = HashSet::new();
    if let Some(cmd) = walk_or_remove_wall(turn, role, &stands, &mut ignored) {
        plan.push(role.id, cmd);
        return true;
    }
    if let Some(cmd) = break_out(turn, role, &stands, &mut ignored) {
        plan.push(role.id, cmd);
        return true;
    }
    false
}
