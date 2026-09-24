//! The economy worker (Worker B) mainline — the team's full-time money loop
//! (issue #221 phase 3, spec: comment 1 §5/§6).
//!
//! ONE priority chain, walked every round, day and night alike — `is_day` is
//! a threat parameter (robot clearance) and `day == 1` a help obligation, never
//! a control-flow fork:
//!
//! 0. **HP first** — drink Medicine; hurt and out of stock, buy one (no gold:
//!    the chain falls through to mining, which is how the money comes back);
//! 1. **day 1 only** — help Worker A finish ONE weapon tower, then go out;
//! 2. **the loop** — ROI-pick a vein (price × free slots ÷ trip rounds), latch
//!    it, mine it out (the game gives 10 collects per vein), sell when a
//!    trigger fires, repeat;
//! 3. **sell triggers** (comment 1 §6) — T4 pack full (must), T1 the pioneer's
//!    buy deadline, T2 a hurt teammate the purse cannot medicate, T3 the
//!    peddler within 3 rounds, T5 a treasure plan short of its 45-gold floor;
//! 4. **night** — B does NOT come home: it keeps working, but holds ≥ 4 cells
//!    of clearance from every living robot and refuses veins inside that ring.
//!
//! The legacy `should_sell` trigger pile (stone surplus ≥ 8, dusk deadline,
//! sell_batch, price spike, force_sell ≥ 15 …) is deliberately NOT used here —
//! comment 1 replaces it with the five triggers above. Since phase 4b neither
//! worker mainline calls it; the set survives in `economy.rs` only for the
//! legacy integration tests, which phase 5c leaves as accepted drift — the
//! scheduler's acceptance is `cargo test --lib`.

use std::collections::HashSet;

use super::super::action::build::build_or_walk;
use super::super::action::geometry::{
    stone_demand_of, wall_gaps, wall_would_trap, HARD_SEAL_ROUND, SEAL_GRACE,
};
use super::super::action::mine::stone_covered;
use super::super::action::sell::sell_flow;
use super::super::action::shop::{max_hp, self_provision, use_medicine};
use super::super::action::tower_site::tower_gaps;
use super::super::route::trip_rounds;
use super::super::{economy, stand_cells, treasure, walk_toward, Plan};
use crate::model::{chebyshev, Turn, Unit, STONE};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// Night clearance from robots (comment 1 §5): B keeps at least this many
/// cells (chebyshev) between itself — and the vein it is walking to — and any
/// living robot once dark.
pub(crate) const NIGHT_ROBOT_CLEARANCE: i32 = 4;

/// Game rule: a vein yields 10 collects, then it is ours no more. The server
/// does not report the count, so `BotState::vein_hits` tracks issued collects
/// and the latch releases at this bound.
pub(crate) const VEIN_COLLECT_LIMIT: i64 = 10;

/// Sell trigger T3 (comment 1 §6): the peddler counts as "on the way" within
/// this many rounds of travel.
pub(crate) const NEAR_VENDOR_ROUNDS: i64 = 3;

/// Sell trigger T5: the pioneer's treasure buy needs at least this much gold
/// in the purse to be worth converting ore for.
pub(crate) const TREASURE_GOLD_FLOOR: i64 = 45;

/// Rounds one carrier alone needs per ring wall (fetch + walk + lay), for the
/// `ring_fits_without_me` release. The legacy pair-rate constant (= 2,
/// deleted with `worker_day`) assumed two carriers working the line.
pub(crate) const SOLO_RING_CELL_ROUNDS: i64 = 4;

/// What one stone is worth to the ROI metric while the ring is still short:
/// not a vendor price (the peddler refuses stone) but a wall — the same order
/// of magnitude as an ore unit, so D1 morning B digs stone like the legacy
/// shared wall duty did, and stops the moment the team pool covers demand.
pub(crate) const STONE_WALL_VALUE: i64 = 10;

/// Plan one round for the economy worker and merge its command into `plan`.
/// `claimed` is shared with the legacy planner (which runs FIRST, so Worker A
/// keeps claim priority) — a vein or site claimed there is invisible here.
pub(crate) fn plan(
    turn: &Turn,
    state: &mut BotState,
    id: i64,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    let Some(role) = turn.role_by_id(id) else {
        return;
    };
    if !role.alive() {
        return;
    }
    if let Some(cmd) = mainline(turn, state, role, claimed) {
        plan.push(id, cmd);
    }
}

/// The single priority chain. Returns at most one command — the first guard
/// that fires owns the round.
fn mainline(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    // Threat parameter: robots get a 4-cell ring after dark, none by day.
    let clearance = if turn.is_day {
        0
    } else {
        NIGHT_ROBOT_CLEARANCE
    };
    // Being cornered outranks every errand: step away before anything else.
    if let Some(cmd) = robot_evasion(turn, role, clearance) {
        return Some(cmd);
    }
    // Step 0 — HP first (comment 1 §5).
    if let Some(cmd) = use_medicine(role) {
        return Some(cmd);
    }
    if hurt(role) {
        // Hurt with no medicine in the pack: buy one. No gold → `None`, and
        // the chain falls through to the mine, which is the spec's "先挖矿攒钱".
        if let Some(cmd) = self_provision(turn, role, claimed, true) {
            return Some(cmd);
        }
    }
    // Step 1 — day 1: help A finish ONE weapon, then go out.
    if let Some(cmd) = d1_weapon_help(turn, state, role, claimed) {
        return Some(cmd);
    }
    // Steps 2/3 — the money loop. A sell trigger owns the round before the
    // next collect does: gold deadlines are why the ore exists.
    let stone_demand = stone_demand_of(turn, state);
    // Day-1 wall hunger (issues #12/#13/#14): stone in B's pack while the ring
    // is short is a HOLE IN THE RING, not merchandise. Hungry until the gaps
    // are gone OR Worker A alone still has the ROUNDS to close them — the
    // legacy `ring_fits_without_me` release, which is what let the earn tests
    // and the ring tests both pass. Being "covered" in the team pool is NOT a
    // release: stone in B's own pack is unavailable to A, and a covered pool
    // with an open ring and no rounds left is exactly the frozen-evening board
    // the legacy duty existed to prevent.
    let wall_hunger = day1_wall_hunger(turn, state, stone_demand, role);
    // Delivery outranks every money errand while the team still wants stone
    // for walls — on ANY day, in daylight only (after dark B keeps its
    // distance from the ring: comment 1 §6's night is the robots'). The
    // permanent entrance (design D17) is never in `wall_gaps`, so every cell
    // the delivery walks to is a cell the day intends to wall.
    let stone_in_pack = role.count_item(STONE);
    if turn.is_day && stone_in_pack > 0 && stone_wanted(turn, stone_demand, wall_hunger) {
        // Batch the errand: the row is walked once per haul, not once per
        // stone. B sets out when the pack covers the whole visible debt, when
        // it is full (more digging cannot fit), or when no reachable stone is
        // left to dig — a partial load in hand is still a load delivered.
        let debt = wall_gaps(turn, state).len();
        if stone_in_pack >= debt
            || economy::free_slots(role) <= 0
            || !stone_reachable(turn, state, role)
        {
            if let Some(cmd) = deliver_stone(turn, state, role, claimed) {
                return Some(cmd);
            }
            // Every gap was vetoed (or its stands unreachable) this round —
            // but the stone is in the pack and the debt is visible. Do not
            // release B to the money loop here: see `camp_at_gap`. The hold
            // must RETURN, not fall through — falling through ends in the
            // mine walk, whose latched vein is the other direction, and the
            // oscillation is back with B pacing between the row and the vein.
            match camp_at_gap(turn, state, role, claimed) {
                Some(Some(cmd)) => return Some(cmd),
                Some(None) => return None, // at the mouth: hold
                None => {}                 // no actionable gap: money loop
            }
        }
    }
    // The legacy day-1 gate watch — B camping at the pending gate's mouth so
    // the seal round finds its stone — is deleted with the gate (design D17).
    // The fixed build order has no cell that is owed but hidden: every gap B
    // can see is a gap B may build, so the delivery step above is the whole
    // of B's wall duty and the permanent entrance needs no watch at all.
    if let Some(trigger) = sell_trigger(turn, state, role, stone_demand) {
        // While the day-1 ring is hungry, only the structural triggers keep
        // their rank: a full pack (T4 — no free slot to carry stone with) and
        // a teammate who needs Medicine money (T2). The money errands (T1/T3/
        // T5) wait for the release — a sell walk across the map is the ring's
        // afternoon lost (issues #12/#13/#14 were all "the earners left").
        // A full pack under hunger drops its cheapest ore for stone room:
        // the ring is worth more than the iron, and the vendor is still there
        // tomorrow (this is the legacy `stone_trip_fits` verdict — "an
        // unwinnable walk is not a stone errand" — applied to the pack).
        let ring_first = wall_hunger
            && !matches!(trigger, "backpack_full" | "medicine_gold")
            && (role.count_item(STONE) > 0 || stone_reachable(turn, state, role));
        if !ring_first {
            crate::log::event(
                "economy_trigger",
                serde_json::json!({
                    "round": turn.round_no,
                    "role": role.id,
                    "trigger": trigger,
                }),
            );
            let sellable = economy::sellable_ores(turn, role, stone_demand);
            if sellable <= 0 || (wall_hunger && trigger == "backpack_full") {
                // No sellable ore (or a full pack the ring outranks): dump
                // the cheapest droppable and keep the loop turning.
                return economy::discard_command(turn, state, role, stone_demand)
                    .or_else(|| sell_flow(turn, state, role, stone_demand, claimed));
            }
            return sell_flow(turn, state, role, stone_demand, claimed);
        }
    }
    mine(turn, state, role, stone_demand, wall_hunger, clearance, claimed)
}

/// Is the day-1 ring still B's problem? True while gaps remain and either the
/// team pool is short of stone or Worker A cannot close the remaining gaps by
/// dusk alone (`gaps × SOLO_RING_CELL_ROUNDS` against the rounds left).
fn day1_wall_hunger(turn: &Turn, state: &BotState, stone_demand: i64, role: &Unit) -> bool {
    if turn.day != 1 || !turn.is_day {
        return false;
    }
    // The WHOLE debt, in one list: the fixed build order (comment 1 §4) has
    // no hidden shoulder and no pending gate — the permanent entrance is
    // never owed — so `wall_gaps` IS the day's wall work, front column to
    // far corner, and a release priced on it cannot strand a cell nobody
    // staffs.
    let gaps = wall_gaps(turn, state);
    let n = gaps.len() as i64;
    if n <= 0 {
        return false;
    }
    let remaining = (economy::DUSK_ROUND - turn.in_day_round).max(0);
    // What A can still do about it: walk from wherever A stands to the
    // nearest gap, then lay at the solo rate. The legacy pair-rate release
    // assumed a second carrier already ON the wall line; an A halfway to a
    // copper run — or already in the dusk retreat — contributes nothing, and
    // a B released on that arithmetic leaves a ring nobody closes (the
    // control board ended 19/20 with the last cell waiting for a carrier the
    // dusk had already recalled).
    let a_walk = turn
        .workers()
        .into_iter()
        .find(|mate| mate.id != role.id && mate.alive())
        .map(|mate| {
            gaps.iter()
                .map(|gap| chebyshev(mate.pos, *gap) as i64)
                .min()
                .unwrap_or(0)
        })
        .unwrap_or(i64::MAX / 4);
    let ring_fits_without_me = a_walk.saturating_add(n * SOLO_RING_CELL_ROUNDS) <= remaining;
    !stone_covered(turn, stone_demand) || !ring_fits_without_me
}

/// Can B still contribute to the ring at all? A hunger B cannot feed — no
/// stone in the pack and no stone vein reachable before dusk — releases the
/// sell deferral: earning is then the most B can do for the team (issue #17's
/// "stone out of reach becomes a day of ore that sells").
fn stone_reachable(turn: &Turn, state: &BotState, role: &Unit) -> bool {
    turn.all_mines().iter().any(|(pos, ore)| {
        ore == STONE
            && !state.ore_on_outage(ore, turn.day)
            && stone_trip_deliverable(
                turn,
                state,
                role,
                *pos,
                trip_rounds(turn, role.pos, *pos).max(1),
            )
    })
}

/// Can a stone trip still land as a WALL before dusk? Walk out (half the
/// round trip), dig one stone, walk to the nearest open gap, build — the
/// legacy `stone_trip_fits` arithmetic with B's deadline: B does not go home,
/// but the ring it feeds closes by dusk, and a stone laid after dark walls
/// nothing in.
fn stone_trip_deliverable(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    vein: Pos,
    trip: i64,
) -> bool {
    let out = trip / 2;
    let gaps = wall_gaps(turn, state);
    let back = gaps
        .iter()
        .map(|gap| chebyshev(vein, *gap) as i64)
        .min()
        .unwrap_or(0);
    // The batch haul: dig one stone a round until the pack (or the debt) is
    // full, haul it to the line, lay a wall a round — and finish before dusk,
    // with the walk back out as slack (that slack is the next errand's
    // start). The slack is also what refuses cross-map expeditions: a
    // 23-cell walk out is not a wall errand, it is an expedition, and issue
    // #17's zero-wall day began as a "reachable" vein 22 cells away.
    let load = economy::free_slots(role)
        .max(1)
        .min(gaps.len().max(1) as i64);
    if out + 2 * load + back + out <= (economy::DUSK_ROUND - turn.in_day_round).max(0) {
        return true;
    }
    // The single last stone is the one errand `SEAL_GRACE` exists for (the
    // ring test measures against DUSK + grace): inside the grace window, one
    // dig and one lay just have to land before the grace ends.
    turn.in_day_round >= economy::DUSK_ROUND - SEAL_GRACE
        && out + 2 + back <= (economy::DUSK_ROUND + SEAL_GRACE - turn.in_day_round).max(0)
}

/// Below 80% HP counts as hurt — `self_provision`'s own threshold; the chain
/// only asks the question, the executor decides what is affordable.
fn hurt(role: &Unit) -> bool {
    max_hp(role).is_some_and(|hp| role.health * 10 < hp * 8)
}

/// Sell triggers of comment 1 §6, in the order the spec ranks their urgency.
/// Returns the trigger name for the log, `None` when the ore stays in the pack.
fn sell_trigger(turn: &Turn, state: &BotState, role: &Unit, stone_demand: i64) -> Option<&'static str> {
    // T4 — backpack full: MUST sell (or dump what cannot be sold).
    if economy::free_slots(role) <= 0 {
        return Some("backpack_full");
    }
    if economy::sellable_ores(turn, role, stone_demand) <= 0 {
        return None;
    }
    // T1 — the pioneer's synced buy deadline: the ore must be gold before the
    // buyer stands at the counter, or the purchase waits a whole loop.
    if let Some(deadline) = state.buy_deadline {
        if turn.round_no + economy::vendor_travel(turn, role.pos) >= deadline {
            return Some("buy_deadline");
        }
    }
    // T2 — a teammate below 30% HP and a purse that cannot buy the Medicine.
    let medicine_price = turn
        .weapon_shop
        .get("Medicine")
        .copied()
        .unwrap_or(i64::MAX);
    let teammate_hurt = turn
        .controllable()
        .iter()
        .any(|mate| max_hp(mate).is_some_and(|hp| mate.health * 10 < hp * 3));
    if teammate_hurt && turn.gold < medicine_price {
        return Some("medicine_gold");
    }
    // T3 — the peddler is already within 3 rounds: sell on the way past.
    if economy::vendor_travel(turn, role.pos) <= NEAR_VENDOR_ROUNDS {
        return Some("near_vendor");
    }
    // T5 — a treasure plan is live and the purse is under its 45-gold floor.
    // `plan_cost`, not `gold_reserve`: the reserve is wallet-gated (it only
    // reports gold the buyer can actually set aside), and a purse below the
    // floor is the very case the gate zeroes — this trigger exists for it.
    if treasure::plan_cost(turn, state) > 0 && turn.gold < TREASURE_GOLD_FLOOR {
        return Some("treasure_floor");
    }
    None
}

/// Day-1 obligation (comment 1 §5 step 1): help Worker A finish ONE weapon
/// tower, then the loop takes over. The help flag latches on the round the
/// build command is issued, so B never builds a second one.
fn d1_weapon_help(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if turn.day != 1 || !turn.is_day || state.d1_weapon_helped {
        return None;
    }
    if !economy::may_build_weapon(turn, state) {
        return None;
    }
    let mut sites: Vec<(Pos, String)> = tower_gaps(turn, state)
        .into_iter()
        .filter(|(pos, _)| !claimed.contains(pos))
        .collect();
    sites.sort_by_key(|(pos, _)| (pos.x, pos.y));
    let (site, name) = sites.first()?;
    let (site, name) = (*site, name.clone());
    // Build issues exactly at adjacency — that round, the help is counted.
    if chebyshev(role.pos, site) == 1 && turn.is_land(site) {
        state.d1_weapon_helped = true;
    }
    claimed.insert(site);
    build_or_walk(turn, role, site, &name, claimed)
}

/// The mine half of the loop: keep the latched vein while it is valid, ROI-pick
/// a new one when it is not, and unload stone into the D1 ring on the way.
fn mine(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    stone_demand: i64,
    wall_hunger: bool,
    clearance: i32,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    let vein = latched_vein(turn, state, stone_demand, wall_hunger, clearance, claimed)
        .or_else(|| {
            choose_vein(turn, state, role, stone_demand, wall_hunger, clearance, claimed)
                .map(|(pos, ore)| {
                    state.vein_latch = Some(pos);
                    (pos, ore)
                })
        })?;
    if chebyshev(role.pos, vein.0) == 1 {
        *state.vein_hits.entry(vein.0).or_insert(0) += 1;
        return Some(RoleCommand::collect(vein.0));
    }
    let stands = stand_cells(turn, vein.0);
    let cmd = walk_toward(turn, role, &stands, claimed)?;
    claimed.insert(vein.0);
    Some(cmd)
}

/// The latched vein, revalidated every round: still on the map, not mined out,
/// not blacked out, not claimed by a teammate this round, not inside the robot
/// ring at night — and stone only while the team pool is still short of demand.
fn latched_vein(
    turn: &Turn,
    state: &BotState,
    stone_demand: i64,
    wall_hunger: bool,
    clearance: i32,
    claimed: &HashSet<Pos>,
) -> Option<(Pos, String)> {
    let pos = state.vein_latch?;
    if state.vein_hits.get(&pos).copied().unwrap_or(0) >= VEIN_COLLECT_LIMIT {
        return None;
    }
    if claimed.contains(&pos) {
        return None;
    }
    let (_, ore) = turn.all_mines().into_iter().find(|(p, _)| *p == pos)?;
    if state.ore_on_outage(&ore, turn.day) {
        return None;
    }
    if ore == STONE {
        if !stone_wanted(turn, stone_demand, wall_hunger) {
            return None;
        }
    } else if wall_hunger {
        // The ring is B's problem again: an ore latch does not survive the
        // hunger, so the next pick is the stone the walls are waiting for.
        return None;
    }
    if !night_safe(turn, pos, clearance) {
        return None;
    }
    Some((pos, ore))
}

/// ROI vein pick (comment 1 §5 step 2): `price × free slots ÷ trip rounds`,
/// stone priced as a wall while the ring is short and skipped once it is not
/// (the peddler refuses stone). Unlike the legacy `choose_sellable_mine` there
/// is NO dusk-budget filter — B does not come home, so a vein is worth exactly
/// what it earns, whenever the round is. Total-order tie-break: ROI desc, trip
/// asc, then coordinates.
fn choose_vein(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    stone_demand: i64,
    wall_hunger: bool,
    clearance: i32,
    claimed: &HashSet<Pos>,
) -> Option<(Pos, String)> {
    let stone_short = stone_wanted(turn, stone_demand, wall_hunger);
    // Ring-first (comment 1 §5): while B is hungry, deliverable stone
    // outranks every ore. No ROI comparison can express that — a wall is not
    // merchandise, and the ore B is standing ON always outbids the stone
    // three cells away. Hunger must not strand B, though: when no stone
    // candidate survives the filters (exhausted, claimed, outside the grace
    // window), the second pass is the earn the `stone_reachable` release
    // already agreed to.
    if wall_hunger && stone_short {
        let stone_pick = pick_vein(turn, state, role, stone_short, clearance, claimed, true);
        if stone_pick.is_some() {
            return stone_pick;
        }
    }
    pick_vein(turn, state, role, stone_short, clearance, claimed, false)
}

/// One ROI pass over the board's veins. `stone_only` is the ring-first filter:
/// ore is invisible to it. Total-order tie-break: ROI desc, trip asc, then
/// coordinates — a consistent comparison is what keeps the pick independent
/// of the zone HashMap's iteration order. Shared with Worker A's quota mine
/// (phase 4b): A passes `stone_only = true` for the day's wall stone and
/// `stone_short = false` for its surplus ore, with its own latch field.
pub(crate) fn pick_vein(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    stone_short: bool,
    clearance: i32,
    claimed: &HashSet<Pos>,
    stone_only: bool,
) -> Option<(Pos, String)> {
    let free = economy::free_slots(role).max(1);
    let mut best: Option<(f64, i64, i32, i32, Pos, String)> = None;
    for (pos, ore) in turn.all_mines() {
        if stone_only && ore != STONE {
            continue;
        }
        if state.ore_on_outage(&ore, turn.day) || claimed.contains(&pos) {
            continue;
        }
        if state.vein_hits.get(&pos).copied().unwrap_or(0) >= VEIN_COLLECT_LIMIT {
            continue;
        }
        if !night_safe(turn, pos, clearance) {
            continue;
        }
        let trip = trip_rounds(turn, role.pos, pos).max(1);
        let value = if ore == STONE {
            if stone_short && stone_trip_deliverable(turn, state, role, pos, trip) {
                STONE_WALL_VALUE
            } else {
                continue; // stone the ring does not want — or cannot get in time
            }
        } else {
            economy::priced(turn, state, &ore)
        };
        let roi = (value as f64 * free as f64) / trip as f64;
        let is_better = best.as_ref().map(|current| {
            roi.partial_cmp(&current.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| current.1.cmp(&trip)) // nearer wins on tie
                .then_with(|| current.2.cmp(&pos.x))
                .then_with(|| current.3.cmp(&pos.y))
                .is_gt()
        });
        if is_better.unwrap_or(true) {
            best = Some((roi, trip, pos.x, pos.y, pos, ore));
        }
    }
    best.map(|(_, _, _, _, pos, ore)| (pos, ore))
}

/// True when no living robot is within `clearance` cells of `pos`. With
/// `clearance == 0` (day) everything is safe — the filter is a night parameter.
/// Shared with A's swept-night stone picker (phase 4b-3, design D15).
pub(crate) fn night_safe(turn: &Turn, pos: Pos, clearance: i32) -> bool {
    clearance <= 0
        || !turn
            .robots
            .iter()
            .any(|robot| robot.health > 0 && chebyshev(robot.pos, pos) < clearance)
}

/// Step away from the robots (comment 1 §5: B avoids them, it does not fight
/// and does not run home). One adjacent walkable cell that strictly increases
/// the nearest-robot distance; deterministic tie-break on coordinates. `None`
/// by day (clearance 0), when no robot is near, or when cornered — a cornered
/// B falls through to its errand instead of freezing on a worse cell.
fn robot_evasion(turn: &Turn, role: &Unit, clearance: i32) -> Option<RoleCommand> {
    if clearance <= 0 {
        return None;
    }
    let threats: Vec<Pos> = turn
        .robots
        .iter()
        .filter(|robot| robot.health > 0)
        .map(|robot| robot.pos)
        .collect();
    if threats.is_empty() {
        return None;
    }
    let nearest = threats
        .iter()
        .map(|pos| chebyshev(role.pos, *pos))
        .min()
        .unwrap_or(i32::MAX);
    if nearest >= clearance {
        return None;
    }
    let blocked = turn.blocked_for(role.id);
    let mut best: Option<(i32, Pos)> = None;
    for dx in [-1i32, 0, 1] {
        for dy in [-1i32, 0, 1] {
            if dx == 0 && dy == 0 {
                continue;
            }
            let pos = Pos {
                x: role.pos.x + dx,
                y: role.pos.y + dy,
            };
            if !turn.is_land(pos) || blocked.contains(&pos) {
                continue;
            }
            let gain = threats
                .iter()
                .map(|threat| chebyshev(pos, *threat))
                .min()
                .unwrap_or(0);
            if gain <= nearest {
                continue;
            }
            let better = best.map_or(true, |(old_gain, old): (i32, Pos)| {
                gain > old_gain || (gain == old_gain && (pos.x, pos.y) < (old.x, old.y))
            });
            if better {
                best = Some((gain, pos));
            }
        }
    }
    best.map(|(_, pos)| RoleCommand::move_to(pos))
}

/// Day-1 stone delivery: while [`day1_wall_hunger`] holds, every stone B is
/// carrying becomes the NEAREST open gap's wall — walked to and built, not
/// merely dropped when B happens to pass (the legacy `economy_unload_stone` +
/// the D1 half of the shared wall duty). `wall_would_trap` keeps the delivery
/// from sealing somebody in.
fn deliver_stone(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    // B's own id is the veto's `owned` skip list: B sleeps outside the ring by
    // design (comment 1 §6), so B standing beyond a gap must not veto B's own
    // seal — the ring closing outranks a worker who is outside on purpose.
    let mut gaps: Vec<Pos> = wall_gaps(turn, state)
        .into_iter()
        .filter(|gap| !claimed.contains(gap))
        .collect();
    gaps.sort_by_key(|gap| (chebyshev(role.pos, *gap), gap.x, gap.y));
    for gap in gaps {
        // Past `HARD_SEAL_ROUND` the night outranks the trap check (the same
        // override the legacy dusk seal runs): a straggler's way home is worth
        // less than a ring the robots walk into.
        if wall_would_trap(turn, state, gap, &[role.id]) && turn.in_day_round < HARD_SEAL_ROUND {
            continue;
        }
        // Claim only what the walk actually committed to (the legacy pattern
        // in `worker_day`): a claim left behind by a failed walk hides the
        // gap from every later step this round, including the camp below.
        if let Some(cmd) = build_or_walk(turn, role, gap, "wall", claimed) {
            claimed.insert(gap);
            return Some(cmd);
        }
        // This gap's stands are unreachable this round — try the next one
        // instead of giving the whole delivery up.
    }
    None
}

/// Wait at the mouth of the nearest gap B owes stone to.
///
/// The waiting posture of `deliver_stone` for the rounds when
/// `wall_would_trap` holds every gap: a veto lifted or re-landed round to
/// round on where a teammate's BODY happens to stand (the spare board: worker
/// A idling in the pioneer's only corridor made the last two ring cells
/// load-bearing for the pioneer's walk to its guns), and a B released to the
/// money loop in between walks one step ring-ward per clear round and turns
/// back to its latched vein on every vetoed one — an oscillation that ends
/// the day with a full pack, an open ring, and holes at both ends of the
/// walk. Camping at the nearest actionable gap's mouth makes the build a
/// one-command affair the moment the veto lifts or `HARD_SEAL_ROUND`
/// overrides it. Gaps a teammate claimed this round are skipped, so B camps
/// on the cell that is actually B's.
/// `Some(Some(cmd))` walks toward the camp, `Some(None)` holds at the mouth
/// (the caller must return — a fall-through releases B to the money loop and
/// re-opens the oscillation this camp exists to end), `None` means no
/// actionable gap at all and the mainline carries on.
fn camp_at_gap(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
) -> Option<Option<RoleCommand>> {
    let mut gaps: Vec<Pos> = wall_gaps(turn, state)
        .into_iter()
        .filter(|gap| !claimed.contains(gap))
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by_key(|gap| (chebyshev(role.pos, *gap), gap.x, gap.y));
    let gap = gaps[0];
    if chebyshev(role.pos, gap) == 1 {
        // At the mouth already: holding IS the camp. No command — stepping
        // off would only re-walk the stand next round.
        return Some(None);
    }
    let stands = stand_cells(turn, gap);
    Some(walk_toward(turn, role, &stands, claimed))
}

/// Is stone worth B's rounds right now? The team pool being short is the
/// always-true reason; the day-1 wall hunger is the other — a covered pool
/// whose stone sits in the wrong pack still needs B to carry and lay its own.
fn stone_wanted(turn: &Turn, stone_demand: i64, wall_hunger: bool) -> bool {
    wall_hunger || !stone_covered(turn, stone_demand)
}
