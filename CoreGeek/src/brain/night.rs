//! Nighttime planning (60 rounds): pair controllers with towers, man them,
//! and shoot; spare controllers use items or shelter near the base.

use std::collections::HashSet;

use crate::brain::{combat, task, tower_stand_cells, walk_or_remove_wall, walk_toward, Plan};
use crate::model::{chebyshev, footprint_distance, Turn, Unit, UnitKind};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

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
        let index = nearest(&reachable)
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

    let needs_recompute = state.night_pairs.is_empty()
        || state.night_pair_day != turn.day
        || state.night_pair_tower_ids != tower_ids
        || state.night_pair_controller_ids != controller_ids
        || state.night_pair_task_busy != task_busy;

    if needs_recompute {
        let reason = if state.night_pairs.is_empty() {
            "empty"
        } else if state.night_pair_day != turn.day {
            "day"
        } else if state.night_pair_tower_ids != tower_ids {
            "towers"
        } else if state.night_pair_controller_ids != controller_ids {
            "controllers"
        } else {
            "task_occupancy"
        };
        state.night_pairs = pairing(turn, state);
        state.night_pair_day = turn.day;
        state.night_pair_tower_ids = tower_ids;
        state.night_pair_controller_ids = controller_ids;
        state.night_pair_task_busy = task_busy;
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

        if !adjacent {
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
            crate::log::event(
                "night_recall",
                serde_json::json!({
                    "round": turn.round_no,
                    "controller": controller.id,
                    "tower": tower.id,
                    "dist": dist,
                    "moved": moved,
                }),
            );
        } else {
            // Adjacent: a badly hurt operator heals first (a dead one mans
            // nothing), otherwise man the tower.
            if let Some(cmd) = night_medicine(turn, controller) {
                idle_reason = "controller_healing";
                plan.push(controller.id, cmd);
            } else if tower.cooldown == 0 {
                if let Some((targets, kind)) = combat::choose_attack_kind(turn, tower, &mut sim) {
                    targets_count = targets.len();
                    fired = true;
                    enemy_fire = kind == combat::TargetKind::EnemyAssets;
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
        // One diagnostic line per pair per round: makes "weapon unoperated"
        // failures visible in the stdout JSONL log without a debugger.
        crate::log::event(
            "night_debug",
            serde_json::json!({
                "round": turn.round_no,
                "pairs_count": pairs.len(),
                "controller_adjacent": adjacent,
                "dist": dist,
                "cooldown": tower.cooldown,
                "attack_targets": targets_count,
                "tower_id": *tower_id,
                "controller_id": *controller_id,
                "controller_pos": controller.pos,
                "tower_pos": tower.pos,
                "fired": fired,
                "enemyFire": enemy_fire,
                "reason": idle_reason,
            }),
        );
    }

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
        .any(|robot| robot.health > 0 && chebyshev(robot.pos, role.pos) <= 3);
    let threshold = if threatened { 7 } else { 3 };
    if role.health * 10 < max_hp * threshold {
        return Some(RoleCommand::use_item("Medicine"));
    }
    None
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
