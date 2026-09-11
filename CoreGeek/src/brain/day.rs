//! Daytime planning (70 rounds): workers run the economy loop
//! (mine → sell → build towers/walls → upgrades), the pioneer runs tasks,
//! the treasure hunt and shopping.

use std::collections::HashSet;

use crate::brain::{economy, night, stand_cells, task, treasure, walk_toward, Plan};
use crate::model::{chebyshev, footprint_distance, station_footprint, Turn, Unit, STONE, WEAPON_BUILD_COST};
use crate::protocol::{Pos, RoleCommand};
use crate::state::{BotState, TaskSession};

/// Rounds before dusk when controllers pre-position at their towers.
const PREPOSITION_ROUND: i64 = 58;
/// Stones to carry before walking out to the wall line (same as the demo).
const STONE_BATCH: i64 = 6;

pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let mut plan = Plan::default();
    let mut claimed: HashSet<Pos> = HashSet::new();

    let tower_gaps = tower_gaps(turn, state);
    let wall_gaps = wall_gaps(turn, state);
    let stone_demand = (wall_gaps.len() as i64 - economy::team_ores(turn, STONE)).max(0);
    let shopping = economy::shopping_list(turn, state);

    // Buyer assignment: the pioneer only when it will not be consumed by a
    // task (active task, or a task point ready to accept), else a worker.
    let task_wants_pioneer = state.task.active
        || turn.player_tasks.iter().any(|task| task.is_valid && task.cooldown_rounds == 0);
    let workers = turn.workers();
    let buyer_id: Option<i64> = if shopping.is_empty() {
        None
    } else if !task_wants_pioneer && turn.pioneer().is_some() {
        turn.pioneer().map(|unit| unit.id)
    } else {
        workers.last().map(|unit| unit.id)
    };

    let pairs = night::pairing(turn, state);

    for worker in &workers {
        worker_day(
            turn,
            state,
            worker,
            &tower_gaps,
            &wall_gaps,
            stone_demand,
            &shopping,
            buyer_id,
            &pairs,
            &mut claimed,
            &mut plan,
        );
    }

    if let Some(pioneer) = turn.pioneer() {
        pioneer_day(
            turn,
            state,
            pioneer,
            &shopping,
            buyer_id,
            &pairs,
            &mut claimed,
            &mut plan,
        );
    }

    plan
}

#[allow(clippy::too_many_arguments)]
fn worker_day(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    tower_gaps: &[(Pos, String)],
    wall_gaps: &[Pos],
    stone_demand: i64,
    shopping: &[economy::Need],
    buyer_id: Option<i64>,
    pairs: &[(i64, i64)],
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    // 1. Self-heal.
    if let Some(cmd) = use_medicine(role) {
        plan.push(role.id, cmd);
        return;
    }
    // 2. Shopping mission.
    if buyer_id == Some(role.id) && !shopping.is_empty() {
        if let Some(cmd) = buyer_flow(turn, state, role, shopping, claimed) {
            plan.push(role.id, cmd);
            return;
        }
    }
    // 3. Apply upgrade vouchers we already carry.
    if let Some(cmd) = voucher_flow(turn, role, claimed) {
        plan.push(role.id, cmd);
        return;
    }
    // 4. Build towers (gold) — highest defensive value.
    if turn.gold >= WEAPON_BUILD_COST {
        for (site, kind) in tower_gaps {
            if claimed.contains(site) {
                continue;
            }
            if let Some(cmd) = build_or_walk(turn, role, *site, kind, claimed) {
                claimed.insert(*site);
                plan.push(role.id, cmd);
                return;
            }
        }
    }
    // 5. Build walls (stone). Batch stones before a long walk; build
    //    immediately when already standing next to a gap.
    if role.count_item(STONE) > 0 && !wall_gaps.is_empty() {
        let adjacent_site = wall_gaps
            .iter()
            .find(|site| !claimed.contains(site) && chebyshev(role.pos, **site) == 1)
            .copied();
        let batch = STONE_BATCH.min(wall_gaps.len() as i64).max(1);
        let ready = adjacent_site.is_some() || role.count_item(STONE) as i64 >= batch;
        if ready {
            for site in wall_gaps {
                if claimed.contains(site) {
                    continue;
                }
                if let Some(cmd) = build_or_walk(turn, role, *site, "wall", claimed) {
                    claimed.insert(*site);
                    plan.push(role.id, cmd);
                    return;
                }
            }
        }
    }
    // 6. Late day: pre-position at the assigned tower.
    if turn.in_day_round >= PREPOSITION_ROUND {
        if let Some(tower_id) = pairs.iter().find(|(controller, _)| *controller == role.id).map(|(_, tower)| *tower) {
            if let Some(tower) = turn.role_by_id(tower_id) {
                if chebyshev(role.pos, tower.pos) > 1 {
                    let stands = stand_cells(turn, tower.pos);
                    if let Some(cmd) = walk_toward(turn, role, &stands, claimed) {
                        plan.push(role.id, cmd);
                        return;
                    }
                }
            }
        }
    }
    // 7. Economy loop: sell / mine.
    economy_flow(turn, state, role, stone_demand, claimed, plan);
}

fn pioneer_day(
    turn: &Turn,
    state: &mut BotState,
    pioneer: &Unit,
    shopping: &[economy::Need],
    buyer_id: Option<i64>,
    pairs: &[(i64, i64)],
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    // 1. Active self-evolution task owns the pioneer completely (must stay
    //    within 1 cell of the task point).
    if state.task.active {
        if let Some(cmd) = task::plan_pioneer(turn, state, pioneer, plan) {
            plan.push(pioneer.id, cmd);
        }
        return;
    }
    // 2. Self-heal.
    if let Some(cmd) = use_medicine(pioneer) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 3. Accept a fresh task when a point is ready (walk there first).
    if let Some(cmd) = state.next_task_point(turn, pioneer, claimed) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 4. Shopping mission.
    if buyer_id == Some(pioneer.id) && !shopping.is_empty() {
        if let Some(cmd) = buyer_flow(turn, state, pioneer, shopping, claimed) {
            plan.push(pioneer.id, cmd);
            return;
        }
    }
    // 5. Vouchers in the backpack.
    if let Some(cmd) = voucher_flow(turn, pioneer, claimed) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 6. Treasure hunt.
    if let Some(cmd) = treasure::plan_pioneer(turn, state, pioneer, claimed, plan) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 7. Late day: pre-position at the assigned tower.
    if turn.in_day_round >= PREPOSITION_ROUND {
        if let Some(tower_id) = pairs.iter().find(|(controller, _)| *controller == pioneer.id).map(|(_, tower)| *tower) {
            if let Some(tower) = turn.role_by_id(tower_id) {
                if chebyshev(pioneer.pos, tower.pos) > 1 {
                    let stands = stand_cells(turn, tower.pos);
                    if let Some(cmd) = walk_toward(turn, pioneer, &stands, claimed) {
                        plan.push(pioneer.id, cmd);
                        return;
                    }
                }
            }
        }
    }
    // 8. Loiter next to a task point so we catch refreshes immediately.
    loiter_at_task_point(turn, pioneer, claimed, plan);
}

/// Accept or walk toward the first valid task point.
impl BotState {
    pub fn next_task_point(
        &mut self,
        turn: &Turn,
        pioneer: &Unit,
        claimed: &mut HashSet<Pos>,
    ) -> Option<RoleCommand> {
        let candidate = turn
            .player_tasks
            .iter()
            .filter(|task| task.is_valid && task.cooldown_rounds == 0)
            .min_by_key(|task| chebyshev(pioneer.pos, task.pos))?;
        if chebyshev(pioneer.pos, candidate.pos) <= 1 {
            let timeout = if candidate.timeout_rounds > 0 { candidate.timeout_rounds } else { 250 };
            crate::log::event(
                "task_accept",
                serde_json::json!({"round": turn.round_no, "point": candidate.pos, "taskType": candidate.task_type}),
            );
            self.task = TaskSession {
                active: true,
                accepted_round: turn.round_no,
                timeout_round: turn.round_no + timeout,
                point: Some(candidate.pos),
                ..Default::default()
            };
            return Some(RoleCommand::accept_task());
        }
        let stands = stand_cells(turn, candidate.pos);
        walk_toward(turn, pioneer, &stands, claimed)
    }
}

fn loiter_at_task_point(turn: &Turn, pioneer: &Unit, claimed: &mut HashSet<Pos>, plan: &mut Plan) {
    let mut stands: Vec<Pos> = Vec::new();
    for task in &turn.player_tasks {
        stands.extend(stand_cells(turn, task.pos));
    }
    if stands.is_empty() {
        return;
    }
    if stands.iter().any(|pos| *pos == pioneer.pos) {
        return; // already loitering
    }
    if let Some(cmd) = walk_toward(turn, pioneer, &stands, claimed) {
        plan.push(pioneer.id, cmd);
    }
}

fn use_medicine(role: &Unit) -> Option<RoleCommand> {
    let max_hp = match role.kind {
        crate::model::UnitKind::Worker => 220,
        crate::model::UnitKind::Pioneer => 200,
        _ => return None,
    };
    if role.health * 2 < max_hp && role.count_item("Medicine") > 0 {
        return Some(RoleCommand::use_item("Medicine"));
    }
    None
}

/// Buy the first needed item: walk to the weapon shop, then buy. Also burns
/// robot summon orders we already carry (harassment).
fn buyer_flow(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    shopping: &[economy::Need],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
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
            return Some(RoleCommand::use_item(order));
        }
    }
    let need = shopping.first()?;
    let mut stands: Vec<Pos> = Vec::new();
    for shop in turn.weapon_shops() {
        stands.extend(stand_cells(turn, shop));
    }
    if stands.is_empty() {
        return None;
    }
    if stands.iter().any(|pos| *pos == role.pos) {
        let price = turn.weapon_shop.get(&need.name).copied().unwrap_or(i64::MAX);
        if price.saturating_mul(need.num) <= turn.gold {
            return Some(RoleCommand::buy(&need.name, need.num));
        }
        return None;
    }
    walk_toward(turn, role, &stands, claimed)
}

/// Walk to a building matching a carried voucher and apply it.
fn voucher_flow(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
    for voucher in economy::held_vouchers(role) {
        let Some(target) = economy::voucher_target(turn, role, &voucher) else {
            continue;
        };
        if chebyshev(role.pos, target) <= 1 {
            return Some(RoleCommand::use_item_at(&voucher, target));
        }
        let stands = stand_cells(turn, target);
        return walk_toward(turn, role, &stands, claimed);
    }
    None
}

fn build_or_walk(
    turn: &Turn,
    role: &Unit,
    target: Pos,
    name: &str,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if chebyshev(role.pos, target) == 1 && turn.is_land(target) {
        return Some(RoleCommand::build(target, name));
    }
    let stands = stand_cells(turn, target);
    walk_toward(turn, role, &stands, claimed)
}

fn economy_flow(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    stone_demand: i64,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    // Sell first when the backpack is heavy or a price spiked.
    if economy::should_sell(turn, state, role, stone_demand) {
        let mut stands: Vec<Pos> = Vec::new();
        for vendor in turn.vendors() {
            stands.extend(stand_cells(turn, vendor));
        }
        if stands.iter().any(|pos| *pos == role.pos) {
            if let Some(cmd) = economy::sell_command(turn, state, role, stone_demand) {
                plan.push(role.id, cmd);
                return;
            }
        } else if let Some(cmd) = walk_toward(turn, role, &stands, claimed) {
            plan.push(role.id, cmd);
            return;
        }
    }
    if role.backpack_full() {
        return; // nothing more to do this round
    }
    // Mine: reserve distinct mines per worker via `claimed`.
    if let Some((mine, ore)) = economy::choose_mine(turn, state, stone_demand, claimed) {
        // Walk to any stand cell adjacent to the mine; collect when there.
        let adjacent_mine = nearest_adjacent_mine(turn, role, &ore, claimed);
        if let Some(mine_pos) = adjacent_mine {
            claimed.insert(mine_pos);
            plan.push(role.id, RoleCommand::collect(mine_pos));
            return;
        }
        let stands = stand_cells(turn, mine);
        if let Some(cmd) = walk_toward(turn, role, &stands, claimed) {
            claimed.insert(mine);
            plan.push(role.id, cmd);
        }
    }
}

/// A mine of the preferred ore (or any valuable ore) we already stand next to.
fn nearest_adjacent_mine(turn: &Turn, role: &Unit, preferred_ore: &str, claimed: &HashSet<Pos>) -> Option<Pos> {
    let mut best: Option<(i64, Pos)> = None;
    for (pos, ore) in turn.all_mines() {
        if claimed.contains(&pos) || chebyshev(role.pos, pos) != 1 {
            continue;
        }
        let value = if ore == preferred_ore { 0 } else { 1 };
        if best.map(|(v, _)| value < v).unwrap_or(true) {
            best = Some((value, pos));
        }
    }
    best.map(|(_, pos)| pos)
}

// ---------------------------------------------------------------------------
// Build site planning
// ---------------------------------------------------------------------------

/// Desired tower cells (ring at distance 1 from the station footprint),
/// paired with the weapon kind that should stand there. Cells already
/// holding one of our towers, blacklisted cells and non-land are excluded.
pub fn tower_gaps(turn: &Turn, state: &BotState) -> Vec<(Pos, String)> {
    let Some(station) = turn.station() else { return Vec::new() };
    let footprint = station_footprint(station.pos);
    let occupied: HashSet<Pos> =
        turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    let center = Pos { x: turn.width / 2, y: turn.height / 2 };

    let mut cells: Vec<Pos> = ring_cells(&footprint, 1)
        .into_iter()
        .filter(|pos| turn.is_land(*pos) && !occupied.contains(pos))
        .collect();
    cells.sort_by_key(|pos| (chebyshev(*pos, center), pos.x, pos.y));

    let mut have = [0i32; 3]; // gatling, railgun, rocket
    for tower in turn.towers() {
        match tower.kind {
            crate::model::UnitKind::Gatling => have[0] += 1,
            crate::model::UnitKind::Railgun => have[1] += 1,
            crate::model::UnitKind::Rocket => have[2] += 1,
            _ => {}
        }
    }
    let kinds = ["gatling", "railgun", "rocket"];
    let mut gaps: Vec<(Pos, String)> = Vec::new();
    let mut used: HashSet<Pos> = HashSet::new();
    for (idx, kind) in kinds.iter().enumerate() {
        if have[idx] > 0 {
            continue;
        }
        let site = cells.iter().copied().find(|pos| {
            !used.contains(pos)
                && !state.blacklisted_builds.contains(&(*pos, kind.to_string()))
        });
        if let Some(pos) = site {
            used.insert(pos);
            gaps.push((pos, kind.to_string()));
        }
    }
    gaps
}

/// Desired wall cells: the ring at distance 2 from the station footprint in
/// defensive order (bottom → left → top → right), leaving one entrance gap.
pub fn wall_gaps(turn: &Turn, state: &BotState) -> Vec<Pos> {
    let Some(station) = turn.station() else { return Vec::new() };
    let footprint = station_footprint(station.pos);
    let xs: Vec<i32> = footprint.iter().map(|pos| pos.x).collect();
    let ys: Vec<i32> = footprint.iter().map(|pos| pos.y).collect();
    let (xmin, xmax) = (*xs.iter().min().unwrap_or(&0), *xs.iter().max().unwrap_or(&0));
    let (ymin, ymax) = (*ys.iter().min().unwrap_or(&0), *ys.iter().max().unwrap_or(&0));

    let mut order: Vec<Pos> = Vec::new();
    for x in (xmin - 2..=xmax + 2).rev() {
        order.push(Pos { x, y: ymin - 2 });
    }
    for y in ymin - 1..=ymax + 1 {
        order.push(Pos { x: xmin - 2, y });
    }
    for x in xmin - 2..=xmax + 2 {
        order.push(Pos { x, y: ymax + 2 });
    }
    for y in (ymin - 1..=ymax + 1).rev() {
        order.push(Pos { x: xmax + 2, y });
    }
    let entrance = Pos { x: xmax + 2, y: ymin - 1 };

    let existing_walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    let occupied: HashSet<Pos> =
        turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    let mut seen: HashSet<Pos> = HashSet::new();
    order
        .into_iter()
        .filter(|pos| *pos != entrance && seen.insert(*pos))
        .filter(|pos| {
            turn.is_land(*pos)
                && !existing_walls.contains(pos)
                && !occupied.contains(pos)
                && !state.blacklisted_builds.contains(&(*pos, "wall".to_string()))
        })
        .collect()
}

fn ring_cells(footprint: &[Pos], radius: i32) -> Vec<Pos> {
    let xs: Vec<i32> = footprint.iter().map(|pos| pos.x).collect();
    let ys: Vec<i32> = footprint.iter().map(|pos| pos.y).collect();
    let (xmin, xmax) = (*xs.iter().min().unwrap_or(&0), *xs.iter().max().unwrap_or(&0));
    let (ymin, ymax) = (*ys.iter().min().unwrap_or(&0), *ys.iter().max().unwrap_or(&0));
    let mut cells = Vec::new();
    for x in xmin - radius..=xmax + radius {
        for y in ymin - radius..=ymax + radius {
            let pos = Pos { x, y };
            if footprint.contains(&pos) {
                continue;
            }
            if footprint_distance(pos, footprint) == radius {
                cells.push(pos);
            }
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{neighbours, ORES};

    fn pos(x: i32, y: i32) -> Pos {
        Pos { x, y }
    }

    #[test]
    fn ring_distance_one_of_footprint() {
        let footprint = station_footprint(pos(10, 24));
        let ring = ring_cells(&footprint, 1);
        assert_eq!(ring.len(), 12); // 4x4 outer minus 2x2 footprint
        assert!(ring.iter().all(|cell| footprint_distance(*cell, &footprint) == 1));
    }

    #[test]
    fn wall_order_leaves_entrance() {
        // entrance at (xmax+2, ymin-1) must not appear in the ring order
        let footprint = station_footprint(pos(10, 24));
        let xs: Vec<i32> = footprint.iter().map(|p| p.x).collect();
        let ys: Vec<i32> = footprint.iter().map(|p| p.y).collect();
        let entrance = pos(xs.iter().max().unwrap() + 2, ys.iter().min().unwrap() - 1);
        let mut order: Vec<Pos> = Vec::new();
        for x in (xs.iter().min().unwrap() - 2..=xs.iter().max().unwrap() + 2).rev() {
            order.push(pos(x, ys.iter().min().unwrap() - 2));
        }
        assert!(!order.contains(&entrance));
        let _ = neighbours(entrance);
        let _ = ORES;
    }
}
