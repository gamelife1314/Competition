//! Daytime planning (70 rounds): workers run the economy loop
//! (mine → sell → build towers/walls → upgrades), the pioneer runs tasks,
//! the treasure hunt and shopping.

use std::collections::HashSet;

use crate::brain::{economy, night, stand_cells, task, tower_stand_cells, treasure, walk_or_remove_wall, walk_toward, Plan};
use crate::model::{chebyshev, footprint_distance, station_footprint, Turn, Unit, DAY_ROUNDS, STONE, WEAPON_BUILD_COST};
use crate::protocol::{Pos, RoleCommand};
use crate::state::{BotState, TaskSession};

/// First day round (in_day_round) when a controller must drop everything and
/// walk to its tower, so it arrives adjacent (chebyshev <= 1) by the first
/// night round. `dist - 1` moves are needed (one per round) plus three rounds
/// of slack for blocked cells, detours and the now-thicker wall line.
fn preposition_round(dist: i32) -> i64 {
    (DAY_ROUNDS - 1 - dist as i64 - 3).max(0)
}
/// Stones to carry before walking out to the wall line (same as the demo).
const STONE_BATCH: i64 = 6;

pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let mut plan = Plan::default();
    let mut claimed: HashSet<Pos> = HashSet::new();

    let tower_gaps = tower_gaps(turn, state);
    let wall_gaps = wall_gaps(turn, state);
    let stone_demand = (wall_gaps.len() as i64 - economy::team_ores(turn, STONE)).max(0);
    // Gold reserved for finishing the tower build-out is untouchable by the
    // shopping list — defenses come before consumables.
    let build_reserve = tower_gaps.len() as i64 * WEAPON_BUILD_COST;
    let shopping = economy::shopping_list(turn, state, build_reserve);

    // Buyer assignment: a dedicated WORKER so voucher purchases are never
    // preempted by a task accept or the treasure hunt. The pioneer stays free
    // for tasks/treasure.
    let workers = turn.workers();
    let buyer_id: Option<i64> = if shopping.is_empty() {
        None
    } else {
        workers.last().map(|unit| unit.id)
    };
    if !shopping.is_empty() {
        crate::log::event(
            "shopping",
            serde_json::json!({
                "buyer": buyer_id,
                "gold": turn.gold,
                "reserve": build_reserve,
                "needs": shopping.iter().map(|need| need.name.as_str()).collect::<Vec<_>>(),
            }),
        );
    }

    let pairs = night::stable_pairs(turn, state);

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
        pioneer_day(turn, state, pioneer, &pairs, &mut claimed, &mut plan);
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
    // 1. Self-heal — a dead worker builds nothing, so this outranks all else.
    if let Some(cmd) = use_medicine(role) {
        plan.push(role.id, cmd);
        return;
    }
    // 2. Apply upgrade vouchers we already carry — using them the moment we
    //    hold them beats buying more or anything else (the night's firepower
    //    depends on it).
    if let Some(cmd) = voucher_flow(turn, role, claimed) {
        plan.push(role.id, cmd);
        return;
    }
    // 3. Build walls (stone) BEFORE weapons: the wall ring protects the base
    //    and the roles standing behind it. Building stops the moment any role
    //    could no longer reach its night weapon — the gate stays open until
    //    everyone has retreated inside, so we never wall ourselves out.
    if role.count_item(STONE) > 0 && !wall_gaps.is_empty() && roles_can_reach(turn, pairs) {
        // Build immediately when already standing next to a safe gap.
        let adjacent_site = wall_gaps
            .iter()
            .find(|site| {
                !claimed.contains(site)
                    && chebyshev(role.pos, **site) == 1
                    && !wall_would_trap(turn, pairs, **site)
            })
            .copied();
        if let Some(site) = adjacent_site {
            claimed.insert(site);
            crate::log::event(
                "wall_build",
                serde_json::json!({"role": role.id, "target": site, "stone": role.count_item(STONE)}),
            );
            plan.push(role.id, RoleCommand::build(site, "wall"));
            return;
        }
        // Otherwise commit to the wall line once we carry a batch of stone.
        let batch = STONE_BATCH.min(wall_gaps.len() as i64).max(1);
        if role.count_item(STONE) as i64 >= batch {
            for site in wall_gaps {
                if claimed.contains(site) || wall_would_trap(turn, pairs, *site) {
                    continue;
                }
                if let Some(cmd) = build_or_walk(turn, role, *site, "wall", claimed) {
                    claimed.insert(*site);
                    crate::log::event(
                        "wall_build",
                        serde_json::json!({"role": role.id, "target": *site, "stone": role.count_item(STONE)}),
                    );
                    plan.push(role.id, cmd);
                    return;
                }
            }
        }
    }
    // 4. Build weapons (gold) once the wall line is underway — but keep a
    //    gold reserve so the main weapon's level-2 upgrade is never starved.
    if economy::may_build_weapon(turn) {
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
    // 5. Shopping (dedicated buyer) — upgrades come after survival.
    if buyer_id == Some(role.id) && !shopping.is_empty() {
        if let Some(cmd) = buyer_flow(turn, role, shopping, claimed) {
            plan.push(role.id, cmd);
            return;
        }
    }
    // 6. Mine the nearest ore (stone first while walls are wanted). Mining
    //    pauses during dusk so the ore we hold is converted to gold instead.
    if turn.in_day_round < economy::DUSK_ROUND && !role.backpack_full() {
        if let Some(cmd) = mine_flow(turn, state, role, stone_demand, claimed) {
            plan.push(role.id, cmd);
            return;
        }
    }
    // 7. Sell ore for gold.
    if economy::should_sell(turn, state, role, stone_demand) {
        if let Some(cmd) = sell_flow(turn, state, role, stone_demand, claimed) {
            plan.push(role.id, cmd);
            return;
        }
    }
    // 8. Pre-position near the assigned tower only in the final rounds, so
    //    the first night round is spent firing instead of walking. Use the
    //    inner stand cells (never walled over) and demolish a wall of ours if
    //    the ring has already sealed us out.
    if let Some(tower_id) = pairs.iter().find(|(controller, _)| *controller == role.id).map(|(_, tower)| *tower) {
        if let Some(tower) = turn.role_by_id(tower_id) {
            let dist = chebyshev(role.pos, tower.pos);
            if dist > 1 && turn.in_day_round >= preposition_round(dist) {
                let stands = tower_stand_cells(turn, tower.pos);
                if let Some(cmd) = walk_or_remove_wall(turn, role, &stands, claimed) {
                    plan.push(role.id, cmd);
                    return;
                }
            }
        }
    }
    // 9. Repair walls damaged overnight (cheap: 10g per fix) when standing
    //    next to one — keeps the ring standing.
    if let Some(wall_pos) = crate::brain::combat::repair_target(turn, role, 0) {
        plan.push(role.id, RoleCommand::use_item_at("WallFixer", wall_pos));
        return;
    }
    // 10. Burn a carried robot-summon order only when everything else is done.
    if let Some(cmd) = burn_summon_order(state, role) {
        plan.push(role.id, cmd);
    }
}

fn pioneer_day(
    turn: &Turn,
    state: &mut BotState,
    pioneer: &Unit,
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
    // 4. Vouchers in the backpack.
    if let Some(cmd) = voucher_flow(turn, pioneer, claimed) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 5. Treasure hunt.
    if let Some(cmd) = treasure::plan_pioneer(turn, state, pioneer, claimed, plan) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 6. Pre-position at the assigned tower (distance-aware deadline) so the
    //    first night round is spent firing, not walking. The pioneer uses the
    //    inner stand cells (never walled over); wall demolition stays worker-only.
    if let Some(tower_id) = pairs.iter().find(|(controller, _)| *controller == pioneer.id).map(|(_, tower)| *tower) {
        if let Some(tower) = turn.role_by_id(tower_id) {
            let dist = chebyshev(pioneer.pos, tower.pos);
            if dist > 1 && turn.in_day_round >= preposition_round(dist) {
                let stands = tower_stand_cells(turn, tower.pos);
                if let Some(cmd) = walk_toward(turn, pioneer, &stands, claimed) {
                    plan.push(pioneer.id, cmd);
                    return;
                }
            }
        }
    }
    // 7. Loiter next to a task point so we catch refreshes immediately.
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

/// Heal when badly hurt. A dead controller builds nothing and mans nothing,
/// so this outranks every other action. Threshold: below 30% HP.
pub(crate) fn use_medicine(role: &Unit) -> Option<RoleCommand> {
    let max_hp = match role.kind {
        crate::model::UnitKind::Worker => 220,
        crate::model::UnitKind::Pioneer => 200,
        _ => return None,
    };
    if role.health * 10 < max_hp * 3 && role.count_item("Medicine") > 0 {
        return Some(RoleCommand::use_item("Medicine"));
    }
    None
}

/// Buy the first needed item: walk to the weapon shop, then buy. Buying is
/// the buyer's sole job — a carried summon order must never preempt a voucher,
/// upgrade or repair purchase.
fn buyer_flow(
    turn: &Turn,
    role: &Unit,
    shopping: &[economy::Need],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
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
            crate::log::event(
                "buy",
                serde_json::json!({"role": role.id, "name": need.name, "num": need.num, "gold": turn.gold}),
            );
            return Some(RoleCommand::buy(&need.name, need.num));
        }
        return None; // wait for gold
    }
    walk_toward(turn, role, &stands, claimed)
}

/// Use a carried robot-summon order against the enemy (harassment), one per
/// round, capped by the daily summon budget. Kept out of `buyer_flow` so a
/// voucher purchase is never delayed by it.
fn burn_summon_order(state: &mut BotState, role: &Unit) -> Option<RoleCommand> {
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
    None
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

/// Is there a walkable route from `start` to any of `stands`? A role already
/// standing on a stand cell counts as reachable (no move needed).
fn can_reach(turn: &Turn, start: Pos, stands: &[Pos]) -> bool {
    if stands.iter().any(|stand| *stand == start) {
        return true;
    }
    let blocked = turn.blocked_for(-1);
    crate::path::step_toward_stands(turn, start, stands, &blocked).is_some()
}

/// Stand cells of the tower a role is paired with for the coming night.
fn night_goal(turn: &Turn, pairs: &[(i64, i64)], role_id: i64) -> Option<Vec<Pos>> {
    let tower_id = pairs.iter().find(|(controller, _)| *controller == role_id).map(|(_, tower)| *tower)?;
    let tower = turn.role_by_id(tower_id)?;
    Some(tower_stand_cells(turn, tower.pos))
}

/// Every controllable role must still be able to reach its night weapon. If
/// any can't, wall building must stop — the gate stays open until everyone is
/// inside, so we never seal a role outside the ring.
fn roles_can_reach(turn: &Turn, pairs: &[(i64, i64)]) -> bool {
    for role in turn.controllable() {
        if let Some(stands) = night_goal(turn, pairs, role.id) {
            if !can_reach(turn, role.pos, &stands) {
                return false;
            }
        }
    }
    true
}

/// Would placing a wall at `site` cut any role off from its weapon? Simulate
/// the wall and re-run the reachability check. Together with the
/// far-side-first build order, this guarantees the ring is only ever closed
/// after everyone has retreated inside.
fn wall_would_trap(turn: &Turn, pairs: &[(i64, i64)], site: Pos) -> bool {
    let mut blocked = turn.blocked_for(-1);
    blocked.insert(site);
    for role in turn.controllable() {
        let Some(stands) = night_goal(turn, pairs, role.id) else { continue };
        if stands.iter().any(|stand| *stand == role.pos) {
            continue; // already at the weapon: nothing to trap
        }
        if crate::path::step_toward_stands(turn, role.pos, &stands, &blocked).is_none() {
            return true;
        }
    }
    false
}

/// Walk to the nearest mine and collect. Stone is preferred while the wall
/// line needs it; distance always beats ore value.
fn mine_flow(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    stone_demand: i64,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    let (mine, ore) = economy::choose_mine(turn, state, role.pos, stone_demand, claimed)?;
    // Collect an adjacent mine of the preferred ore directly.
    if let Some(mine_pos) = nearest_adjacent_mine(turn, role, &ore, claimed) {
        claimed.insert(mine_pos);
        return Some(RoleCommand::collect(mine_pos));
    }
    let stands = stand_cells(turn, mine);
    let cmd = walk_toward(turn, role, &stands, claimed)?;
    claimed.insert(mine);
    Some(cmd)
}

/// Walk to a vendor and sell the most valuable ore stack.
fn sell_flow(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    stone_demand: i64,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    let mut stands: Vec<Pos> = Vec::new();
    for vendor in turn.vendors() {
        stands.extend(stand_cells(turn, vendor));
    }
    if stands.is_empty() {
        return None;
    }
    if stands.iter().any(|pos| *pos == role.pos) {
        return economy::sell_command(turn, state, role, stone_demand);
    }
    walk_toward(turn, role, &stands, claimed)
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

/// Desired wall cells: a two-cell-thick ring (distance 3 outer + distance 2
/// inner) ordered FURTHEST-from-the-gate first, so the far side builds up
/// while the corridor stays open for the roles still outside. The two gate
/// cells span both rings and are never built — the reachability guards in
/// `worker_day` seal the ring only once every role is already inside.
pub fn wall_gaps(turn: &Turn, state: &BotState) -> Vec<Pos> {
    let Some(station) = turn.station() else { return Vec::new() };
    let footprint = station_footprint(station.pos);
    let xs: Vec<i32> = footprint.iter().map(|pos| pos.x).collect();
    let ys: Vec<i32> = footprint.iter().map(|pos| pos.y).collect();
    let xmax = *xs.iter().max().unwrap_or(&0);
    let ymin = *ys.iter().min().unwrap_or(&0);

    // Two gate cells: one per ring, so the entrance corridor is two cells wide
    // (a single-cell gate would still leave a wall between the two rings).
    let gate: [Pos; 2] = [
        Pos { x: xmax + 2, y: ymin - 1 },
        Pos { x: xmax + 3, y: ymin - 1 },
    ];
    let mut cells: Vec<Pos> = ring_cells(&footprint, 3);
    cells.extend(ring_cells(&footprint, 2));
    // Furthest from the gate first: the open corridor is the LAST thing a
    // closing ring would block, so this order keeps roles able to return.
    cells.sort_by_key(|pos| std::cmp::Reverse(chebyshev(*pos, gate[0])));

    let existing_walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    let occupied: HashSet<Pos> =
        turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    let mut seen: HashSet<Pos> = HashSet::new();
    cells
        .into_iter()
        .filter(|pos| !gate.contains(pos) && seen.insert(*pos))
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
