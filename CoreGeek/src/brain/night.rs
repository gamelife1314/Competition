//! Nighttime planning (60 rounds): pair controllers with towers, man them,
//! and shoot; spare controllers use items or shelter near the base.

use std::collections::HashSet;

use crate::brain::{
    combat, economy, task, tower_stand_cells, walk_or_remove_wall, walk_toward, Plan,
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
pub fn pairing(turn: &Turn, state: &BotState) -> Vec<(i64, i64)> {
    let mut towers = turn.towers();
    towers.sort_by_cached_key(|tower| std::cmp::Reverse(combat::threat_load(turn, tower)));
    let mut controllers: Vec<&Unit> = turn
        .controllable()
        .into_iter()
        .filter(|role| !(state.task.active && role.kind == UnitKind::Pioneer))
        // P1-4 hysteresis: a controller in withdrawal holdout mans nothing —
        // it shelters and heals as a spare instead of pacing between the gun
        // and the threat radius all night.
        .filter(|role| !state.withdraw_holdout.contains(&role.id))
        .collect();
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
    pairs
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
            serde_json::json!({"round": turn.round_no, "session": state.task.session_id, "reason": "unmanned_tower_under_threat"}),
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
    let paired_towers: HashSet<i64> = pairs.iter().map(|(_, tower)| *tower).collect();
    let available = turn
        .controllable()
        .iter()
        .filter(|role| !(state.task.active && role.kind == UnitKind::Pioneer))
        .count();
    for tower in turn.towers() {
        if !paired_towers.contains(&tower.id) {
            let reason = if state.task.active && turn.pioneer().is_some() {
                "pioneer_task_occupied"
            } else if available < turn.towers().len() {
                "no_live_controller"
            } else {
                "pairing_invariant"
            };
            crate::log::event(
                "tower_unpaired",
                serde_json::json!({"round": turn.round_no, "tower": tower.id, "reason": reason}),
            );
        }
    }
    let mut sim = combat::init_sim(turn);
    // Snapshot the coach's policy once: the tower loop below borrows `state`
    // mutably (to report enemy fire back to the coach), and the stance must not
    // change halfway through one round's firing sequence.
    let coach_policy = state.coach.policy();

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
                }
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
            idle_reason = if moved {
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
                if let Some((targets, kind)) =
                    combat::choose_attack_kind_with(&coach_policy, turn, tower, &mut sim)
                {
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
                    // hold the opportunistic shot (issue #7's "有余力时").
                    idle_reason = "no_target_reserved_for_robots";
                } else {
                    idle_reason = "no_target_in_range";
                }
            } else {
                idle_reason = "cooldown";
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
        night_rows.push(row);
    }
    // Still one record every night round, deliberately: the count of rounds a
    // tower spent `controller_withdrawn` IS the finding (Improve.kimi.md reads
    // "35 次 controller_withdrawn, 塔全程沉默"), so a record that only appeared
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
    // Patch damaged walls when no robot is breathing down our neck. Only walls
    // adjacent to the role are ever targeted, so this is safe from inside.
    if let Some(wall_pos) = combat::repair_target(turn, role, 3) {
        plan.push(role.id, RoleCommand::use_item_at("WallFixer", wall_pos));
        return;
    }
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
    false
}
