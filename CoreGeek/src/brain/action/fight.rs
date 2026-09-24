//! Fight: controller↔tower pairing, the pioneer's post choice, and the
//! withdrawal rule that keeps a wounded controller off a gun it cannot hold.
//!
//! Moved out of `night.rs` by issue #221's phase 2: these are the shared
//! executors the role mainlines call — the scheduler's `Context` pairing is
//! `stable_pairs`, and the wall worker's fight step is what the pairing
//! produces. Pure move: the code below is the block that lived in
//! `night.rs`, unchanged except for visibility.

use std::collections::HashSet;

use crate::brain::{combat, tower_stand_cells};
use crate::model::{chebyshev, footprint_distance, Turn, Unit, UnitKind};
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
    pairing_controllers_owned(turn, state, &[])
}

/// [`pairing_controllers`] minus the units a role mainline owns (issue #221):
/// an owned controller never takes a legacy gun, and — this is the point of
/// the variant — never consumes one in the greedy pass either, so its tower
/// goes to the next controller instead of going dark with B standing beside it.
pub(crate) fn pairing_controllers_owned<'a>(
    turn: &'a Turn,
    state: &BotState,
    owned: &[i64],
) -> Vec<&'a Unit> {
    turn.controllable()
        .into_iter()
        .filter(|role| !(state.task.active && role.kind == UnitKind::Pioneer))
        // P1-4 hysteresis: a controller in withdrawal holdout mans nothing —
        // it shelters and heals as a spare instead of pacing between the gun
        // and the threat radius all night.
        .filter(|role| !state.withdraw_holdout.contains(&role.id))
        .filter(|role| !owned.contains(&role.id))
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
    pairing_owned(turn, state, &[])
}

/// [`pairing`] with the issue-#221 roster seam: controllers in `owned` are
/// invisible to the greedy pass, so their towers pair with whoever is left.
pub(crate) fn pairing_owned(turn: &Turn, state: &BotState, owned: &[i64]) -> Vec<(i64, i64)> {
    let mut towers = turn.towers();
    towers.sort_by_cached_key(|tower| std::cmp::Reverse(combat::threat_load(turn, tower)));
    let mut controllers: Vec<&Unit> = pairing_controllers_owned(turn, state, owned);
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
    if new.exposure >= now.exposure.saturating_add(PIONEER_SAFE_MARGIN) {
        return true; // materially further from the enemy bearing
    }
    if new.exposure.saturating_add(PIONEER_SAFE_MARGIN) <= now.exposure {
        return false; // materially closer to it: not worth the reach
    }
    new.short_range < now.short_range
}

/// Stable controller↔tower pairings. The cache key includes every living
/// tower/controller and whether the pioneer is occupied by a task. This keeps
/// assignments stable while all members remain usable, but replaces a dead or
/// newly freed controller in the very next round.
pub fn stable_pairs(turn: &Turn, state: &mut BotState) -> Vec<(i64, i64)> {
    stable_pairs_owned(turn, state, &[])
}

/// [`stable_pairs`] with the issue-#221 roster seam. The owned set is folded
/// into the cached controller list, so a caller that owns different units
/// (the night planner vs the day planner's gun-deadline query) can never read
/// the other's pairing out of the cache.
pub(crate) fn stable_pairs_owned(
    turn: &Turn,
    state: &mut BotState,
    owned: &[i64],
) -> Vec<(i64, i64)> {
    let tower_ids: Vec<i64> = turn.towers().iter().map(|tower| tower.id).collect();
    let controller_ids: Vec<i64> = turn
        .controllable()
        .iter()
        .map(|role| role.id)
        .filter(|id| !owned.contains(id))
        .collect();
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
        state.night_pairs = pairing_owned(turn, state, owned);
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
