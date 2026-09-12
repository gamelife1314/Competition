//! Nighttime planning (60 rounds): pair controllers with towers, man them,
//! and shoot; spare controllers use items or shelter near the base.

use std::collections::HashSet;

use crate::brain::{combat, task, tower_stand_cells, walk_or_remove_wall, walk_toward, Plan};
use crate::model::{chebyshev, Turn, Unit, UnitKind};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// Greedy pairing: every living tower gets the closest free controller.
/// A pioneer busy with a self-evolution task must stay at the task point and
/// is therefore excluded. Towers under the heaviest pressure pair first so a
/// long walk never starves the position that matters most.
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
        let mut best_index = 0usize;
        let mut best_dist = i32::MAX;
        for (index, role) in controllers.iter().enumerate() {
            let dist = chebyshev(role.pos, tower.pos);
            if dist < best_dist {
                best_dist = dist;
                best_index = index;
            }
        }
        let role = controllers.remove(best_index);
        pairs.push((role.id, tower.id));
    }
    pairs
}

/// Stable controller↔tower pairings: computed once per night (or when
/// towers change), reused across rounds so controllers actually reach
/// their assigned weapon instead of oscillating between targets.
pub fn stable_pairs(turn: &Turn, state: &mut BotState) -> Vec<(i64, i64)> {
    let tower_ids: Vec<i64> = turn
        .towers()
        .iter()
        .filter(|tower| tower.alive())
        .map(|tower| tower.id)
        .collect();

    // Recompute if: first night, day changed, or tower set changed.
    let needs_recompute = state.night_pairs.is_empty()
        || state.night_pair_day != turn.day
        || state.night_pair_tower_ids != tower_ids;

    if needs_recompute {
        let towers = turn.towers();
        let mut controllers: Vec<&Unit> = turn
            .controllable()
            .into_iter()
            .filter(|role| !(state.task.active && role.kind == UnitKind::Pioneer))
            .collect();
        let mut pairs: Vec<(i64, i64)> = Vec::new();
        for tower in towers {
            if controllers.is_empty() || !tower.alive() {
                break;
            }
            let mut best_index = 0usize;
            let mut best_dist = i32::MAX;
            for (index, role) in controllers.iter().enumerate() {
                let dist = chebyshev(role.pos, tower.pos);
                if dist < best_dist {
                    best_dist = dist;
                    best_index = index;
                }
            }
            let role = controllers.remove(best_index);
            pairs.push((role.id, tower.id));
        }
        state.night_pairs = pairs;
        state.night_pair_day = turn.day;
        state.night_pair_tower_ids = tower_ids;
    }
    state.night_pairs.clone()
}

pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let mut plan = Plan::default();
    let mut claimed: HashSet<Pos> = HashSet::new();

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
    let mut sim = combat::init_sim(turn);

    for (controller_id, tower_id) in &pairs {
        let (Some(tower), Some(controller)) = (turn.role_by_id(*tower_id), turn.role_by_id(*controller_id))
        else { continue };
        if !tower.alive() || !controller.alive() {
            continue;
        }
        // Claim only a valid, able controller — an unusable one falls through
        // to spare duties instead of idling next to a dead tower all night.
        paired.insert(*controller_id);
        let dist = chebyshev(controller.pos, tower.pos);
        let adjacent = dist <= 1;
        let mut targets_count: usize = 0;
        let mut fired = false;
        // Survival first: a badly hurt operator heals instead of firing or
        // walking this round — a dead controller mans nothing.
        if let Some(cmd) = crate::brain::day::use_medicine(controller) {
            plan.push(controller.id, cmd);
        } else if adjacent {
            // Man the tower: attack commands are keyed by the TOWER's id.
            if tower.cooldown == 0 {
                if let Some(targets) = combat::choose_attack(turn, tower, &mut sim) {
                    targets_count = targets.len();
                    fired = true;
                    plan.push(tower.id, RoleCommand::attack(controller.id, targets));
                }
            }
            // The controller holds position (no command) to stay adjacent.
        } else {
            let stands = tower_stand_cells(turn, tower.pos);
            // Try walking while respecting claimed cells (soft preference).
            if let Some(cmd) = walk_or_remove_wall(turn, controller, &stands, &mut claimed) {
                plan.push(controller.id, cmd);
            } else {
                // Fallback: ignore claimed cells, take any reachable step.
                // This prevents controllers getting stuck when a teammate's
                // committed step blocks the only available path.
                let mut ignored = HashSet::new();
                if let Some(cmd) = walk_or_remove_wall(turn, controller, &stands, &mut ignored) {
                    plan.push(controller.id, cmd);
                }
            }
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
    if let Some(cmd) = crate::brain::day::use_medicine(role) {
        plan.push(role.id, cmd);
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
    // Patch damaged walls when no robot is breathing down our neck.
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
                robot.health > 0 && chebyshev(impact, robot.pos) <= 1 && combat::is_big_threat(robot.kind)
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
    // No night mining: every spare role shelters next to the base. Mining at
    // night drags a worker outside the walls where it can die.
    shelter(turn, role, claimed, plan);
}

fn shelter(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>, plan: &mut Plan) {
    let Some(station) = turn.station() else { return };
    let footprint = station.footprint();
    let robot_positions: Vec<Pos> =
        turn.robots.iter().filter(|robot| robot.health > 0).map(|robot| robot.pos).collect();
    let already_safe = footprint_distance_to(role.pos, &footprint) <= 2
        && robot_positions.iter().all(|robot| chebyshev(*robot, role.pos) >= 4);
    if already_safe {
        return;
    }
    let mut stands: Vec<Pos> = Vec::new();
    for cell in &footprint {
        for around in crate::model::neighbours(*cell) {
            if turn.is_land(around)
                && footprint_distance_to(around, &footprint) <= 2
                && robot_positions.iter().all(|robot| chebyshev(*robot, around) >= 3)
            {
                stands.push(around);
            }
        }
    }
    if stands.is_empty() {
        return;
    }
    if let Some(cmd) = walk_toward(turn, role, &stands, claimed) {
        plan.push(role.id, cmd);
    }
}

fn footprint_distance_to(pos: Pos, footprint: &[Pos]) -> i32 {
    crate::model::footprint_distance(pos, footprint)
}
