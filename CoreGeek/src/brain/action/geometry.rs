//! Wall geometry: the ring's gap list, debt count, layers and budgets
//! (issue #221 phase 4b, spec: comment 1 §4). Pure queries — every function
//! here answers "which cells does the wall line want", never emits a command.
//!
//! The order is FIXED and one-directional ([`super::base_layout`]): the
//! enemy-facing front column first, then the top row, then the bottom row; the
//! back four cells are the permanent entrance and are never offered. A day
//! that runs out of stone or rounds leaves the order's TAIL open — the far
//! side, away from the robots. The legacy far-shoulder split (issue #206 §6),
//! the errand-chosen gate and the dusk seal that closed it are deleted
//! (design D17): with a permanent entrance there is no cell the day "leaves
//! open on purpose but owes by nightfall", so every listed cell is owed and
//! the debt needs no split.

use std::collections::HashSet;

use super::base_layout;
use crate::model::{
    chebyshev, footprint_distance, station_footprint, Turn, Unit, WEAPON_BUILD_COST,
};
use crate::protocol::Pos;
use crate::state::BotState;

/// The radius-2 station ring has 20 cells, 16 of them wall cells (the back
/// four are the permanent entrance). Day 1 is allowed to complete that entire
/// single-layer shell; later days retain the conservative repair/expand budget
/// so fortification cannot permanently starve the economy.
pub const D1_WALL_CAP: i64 = 20;
pub const LATER_WALL_CAP: i64 = 6;

/// First day the second wall layer may be paid for (P2-1). Day 1 belongs to
/// the first ring: a stone spent on ring 3 that day is a hole in the ring that
/// is actually holding the night.
const SECOND_LAYER_MIN_DAY: i64 = 2;
/// Ring-3 cells the day may ask for at most. The outer layer is bought with
/// surplus — four cells is one mine trip, and the six-cell maintenance budget
/// still leaves room for the breach repair the ring itself may need.
const SECOND_LAYER_BATCH: usize = 4;
/// Cells kept clear around the PERMANENT ENTRANCE before the second layer may
/// stand (comment 1 §4). The entrance column is how everything inside reaches
/// the ore, the vendor and the shop; an outer wall built across its mouth
/// would seal the base's own doorway into a pocket — the failure the legacy
/// gate clearance existed for, now measured from the four fixed cells.
const SECOND_LAYER_ENTRANCE_CLEARANCE: i32 = 2;

/// How many ring cells this day's fortification budget covers.
///
/// Day 1 is the build-out: the whole shell is allowed, because an open ring is
/// what the robots walk through. Later days keep a maintenance budget so wall
/// work cannot permanently starve the economy — EXCEPT when the ring has been
/// closed before and is open now. That is a breach, not upkeep: the 6-cell
/// budget cannot even re-close a ring the night took 10 walls out of, and the
/// half-spent budget is paid for by the station (issue #21: -30 residual, base
/// 1500 → 20 HP).
///
/// `primary_open` is the second exit from the maintenance budget, and it is
/// the one that does not depend on history. `ring_ever_complete` asks "was the
/// ring EVER whole" — so a ring that was never whole at all (day 1 lost to a
/// distant stone vein, a night that emptied the shell before it closed) had no
/// exit: it was repaired on 6 cells a day forever, and 6 cells a day cannot
/// out-build a night that takes 10 walls out. Six in, ten out is a ratchet,
/// and the ring it ratchets down is the one holding the base. A ring that is
/// OPEN NOW gets the build-out budget whether it has ever been whole or not,
/// whatever the day.
///
/// The anti-starvation intent is untouched, and it is the third case: a ring
/// that is whole (no primary gaps) and wants MORE than it has — the ring-3
/// second layer, extra tidying — is still on the 6-cell maintenance budget.
/// The cap is released to REPAIR a ring, never to expand one.
pub fn wall_daily_cap(day: i64, ring_ever_complete: bool, primary_open: usize) -> i64 {
    if day == 1 || ring_ever_complete || primary_open > 0 {
        D1_WALL_CAP
    } else {
        LATER_WALL_CAP
    }
}

/// Gold reserved for tower builds. Up to 3 towers are reserved (one per empty
/// slot), so the third gatling is funded as soon as gold is available. Battle
/// analysis (pk616181/pk616182) showed the base falls on night 1 with only 2
/// towers against 0-tower rush opponents; the third tower's close-range fire
/// is worth more than the gold it costs.
pub fn tower_build_reserve(tower_count: usize, gap_count: usize) -> i64 {
    ((3 - tower_count as i64).max(0)).min(gap_count as i64) * WEAPON_BUILD_COST
}

/// Every wall cell the day wants filled: the primary ring in comment 1 §4's
/// fixed one-direction order, and — once that ring is complete — the second
/// layer on the damaged arc (P2-1).
///
/// The order matters and is deliberate:
///
///   1. **the ring, front column first** — 「先建朝向敌人的一面」. Nothing
///      outranks a cell a robot can walk in through, and the order's tail is
///      what a day short of stone or rounds leaves open: the far side, away
///      from the enemy. The permanent entrance is never in the list at all.
///   2. **the second layer** — it guards the arc the robots demonstrably come
///      through, and it is a luxury: it is reached only on a day whose primary
///      ring is whole, which is exactly the day that has earned it.
///
/// The important consequence is the one `plan`'s `stone_demand` is built on:
/// the whole debt is one list in one order, so no stone is ever mined for a
/// cell the day has been told it may skip — there are no skippable cells.
pub fn wall_gaps(turn: &Turn, state: &BotState) -> Vec<Pos> {
    let owed = primary_wall_gaps(turn, state);
    if !owed.is_empty() {
        return owed;
    }
    second_layer_gaps(turn, state)
}

/// The primary ring's still-missing wall cells, in comment 1 §4's build order:
/// front column, top row, bottom row. The entrance cells are never offered,
/// and neither are existing walls, cells under a teammate's footprint, cells
/// the judger has blacklisted, or non-land.
pub fn primary_wall_gaps(turn: &Turn, state: &BotState) -> Vec<Pos> {
    if turn.station().is_none() {
        return Vec::new();
    }
    let existing_walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    let occupied: HashSet<Pos> = turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    base_layout::wall_build_order(turn)
        .into_iter()
        .filter(|pos| {
            turn.is_land(*pos)
                && !existing_walls.contains(pos)
                && !occupied.contains(pos)
                && !state
                    .blacklisted_builds
                    .contains(&(*pos, "wall".to_string()))
        })
        .collect()
}

/// Ring cells the day still owes a wall: the primary ring minus the walls that
/// are standing in it.
///
/// This is the DEBT, not the work list. [`primary_wall_gaps`] is what the
/// sweep can be sent to — it drops any cell a teammate's footprint covers and
/// any cell that already failed to build — and every one of those drops is a
/// cell the night still finds open. The difference is what a day that is
/// running out of time looks like from inside the planner: on the day-1 board
/// the sweep's list emptied at R46 with four cells of the ring's south row
/// still bare, and the crew — one carrier outside the ring, the other idle
/// inside it — could not touch them until the dusk released them. See `plan`'s
/// `ring_at_risk`, which is the deadline this count exists for. The entrance
/// cells are not debt and never were: they are open by design (comment 1 §4).
pub(crate) fn ring_open_cells(turn: &Turn, state: &BotState) -> usize {
    if turn.station().is_none() {
        return 0;
    }
    let walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    base_layout::wall_build_order(turn)
        .into_iter()
        .filter(|pos| {
            turn.is_land(*pos)
                && !walls.contains(pos)
                && !state
                    .blacklisted_builds
                    .contains(&(*pos, "wall".to_string()))
        })
        .count()
}

/// The second wall layer: ring-3 cells on the arc that is actually taking
/// damage, and nowhere else (P2-1).
///
/// Three limits keep it from becoming the full-map second ring the analysis
/// rules out. It exists only on the sectors [`BotState::threatened_sectors`]
/// ranks highest — at most three of the eight — so the arc is open at both
/// ends and can never trap a role the way a second ring would; only
/// [`SECOND_LAYER_BATCH`] cells of it are asked for per day, so the stone and
/// the walking are bounded and cannot starve the economy; and it is offered
/// only once the first ring is complete *and* the third gun stands, because up
/// to that point every stone belongs to a defence that is not finished yet.
///
/// With no threat evidence — `threatened_sectors` empty — nothing is built.
/// That is the point: the outer layer is paid for only where the robots
/// demonstrably come through, never speculatively around the map.
pub(crate) fn second_layer_gaps(turn: &Turn, state: &BotState) -> Vec<Pos> {
    if turn.day < SECOND_LAYER_MIN_DAY || turn.towers().len() < 3 {
        return Vec::new();
    }
    // Offense-first: don't build a second wall layer until weapons are maxed.
    // Every stone and gold spent on an outer wall while the guns are still
    // level 1 is a round the crew walks away from the mine→sell→upgrade loop
    // that actually closes the firepower gap. The first ring is the minimum
    // viable defense; the second layer is a luxury we can afford once the
    // towers are level 2.
    let weapons_maxed = turn.towers().iter().all(|t| t.level >= 2);
    if !weapons_maxed {
        return Vec::new();
    }
    let sectors = state.threatened_sectors();
    if sectors.is_empty() {
        return Vec::new();
    }
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let center = station.pos;
    let entrance = base_layout::entrance_cells(turn);
    let existing: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    let occupied: HashSet<Pos> = turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    let mut cells: Vec<(usize, Pos)> = ring_cells(&footprint, 3)
        .into_iter()
        .filter(|pos| turn.is_land(*pos))
        .filter(|pos| !existing.contains(pos) && !occupied.contains(pos))
        .filter(|pos| {
            !state
                .blacklisted_builds
                .contains(&(*pos, "wall".to_string()))
        })
        // The entrance's mouth stays open at EVERY layer: an outer wall across
        // it would pocket the base's only door (the legacy rule measured this
        // from the day's gate cell; the entrance column is its fixed form).
        .filter(|pos| {
            entrance
                .iter()
                .all(|cell| chebyshev(*pos, *cell) > SECOND_LAYER_ENTRANCE_CLEARANCE)
        })
        .filter_map(|pos| {
            let sector = crate::state::arc_sector(center, pos);
            let rank = sectors.iter().position(|ranked| *ranked == sector)?;
            Some((rank, pos))
        })
        .collect();
    cells.sort_by_key(|(rank, pos)| (*rank, pos.x, pos.y));
    cells.truncate(SECOND_LAYER_BATCH);
    cells.into_iter().map(|(_, pos)| pos).collect()
}

/// Which wall layer a cell belongs to: 2 is the ring that holds the night, 3
/// is the second layer on the damaged arc. Written into `wall_build` so the
/// two are told apart in the log — a wall count that does not say which layer
/// it came from cannot answer whether P2-1 built anything.
pub(crate) fn wall_layer(turn: &Turn, site: Pos) -> i64 {
    match turn.station() {
        Some(station) => footprint_distance(site, &station.footprint()) as i64,
        None => 0,
    }
}

pub(crate) fn ring_cells(footprint: &[Pos], radius: i32) -> Vec<Pos> {
    let xs: Vec<i32> = footprint.iter().map(|pos| pos.x).collect();
    let ys: Vec<i32> = footprint.iter().map(|pos| pos.y).collect();
    let (xmin, xmax) = (
        *xs.iter().min().unwrap_or(&0),
        *xs.iter().max().unwrap_or(&0),
    );
    let (ymin, ymax) = (
        *ys.iter().min().unwrap_or(&0),
        *ys.iter().max().unwrap_or(&0),
    );
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

// ---------------------------------------------------------------------------
// Wall safety: the quota batch, the seal deadlines and the trap veto. Moved
// out of `day.rs` by issue #221 phase 5c — the wall line's geometry and the
// checks that keep it from sealing the crew out live together now. The veto is
// ROLE-based: A's night post is the operator cell of the fixed L (design
// D14-D16) and everybody else's home is the inside of the ring. The legacy
// controller↔tower pairing it used to be measured against is deleted.
// ---------------------------------------------------------------------------

/// Stones to carry before walking out to the wall line on a maintenance day.
const STONE_BATCH: i64 = 6;
/// Day 1's wall line is one complete ring, and the mine↔ring commute costs more
/// rounds than the mining does. Carrying the whole remaining demand in a single
/// trip is what closes the ring before the dusk lock-in; the one-stone dribble
/// (mine one, walk back, build one) is how the base ended up with two towers, no
/// wall and a frozen purse in issues #12/#13/#14.
const RING_BATCH: i64 = 20;

/// Rounds kept in hand on top of the walk home before a stone carrier gives up
/// on the vein — a blocked cell, a detour, a claim.
const WALL_TRIP_SLACK: i64 = 3;

/// Has the vein stopped paying for the walk home? The load already in hand is
/// what the ring gets; a carrier that keeps digging is a carrier the night
/// finds outside the wall. Measured against the wall window (dusk plus the
/// seal grace), not the gun deadline: the seal is allowed to spend the last
/// rounds of the day on the ring, and a trip that is still placing walls then
/// is a trip that worked. Read by the wall worker's batch release (phase 4b).
pub(crate) fn wall_trip_overdue(turn: &Turn, role: &Unit, gaps: &[Pos]) -> bool {
    let home = gaps
        .iter()
        .map(|gap| chebyshev(role.pos, *gap) as i64)
        .min()
        .unwrap_or(0);
    turn.in_day_round + home + 1 + WALL_TRIP_SLACK >= crate::brain::economy::DUSK_ROUND + SEAL_GRACE
}

/// Stones this role should carry before it walks out to the wall line. Read
/// by the wall worker's batch release (phase 4b).
pub(crate) fn stone_batch(turn: &Turn, gaps: usize) -> i64 {
    let want = if turn.day == 1 {
        RING_BATCH
    } else {
        STONE_BATCH
    };
    want.min(gaps as i64).max(1)
}

/// Day-rounds after dusk during which a stone carrier may still walk out to
/// close the last hole in the ring. Long enough for a round trip from any
/// tower post, short enough that the gun is manned again well before night.
pub(crate) const SEAL_GRACE: i64 = 8;
/// The day-round past which the ring is sealed whether or not the crew is home.
///
/// `SEAL_GRACE` is the window the seal is *allowed* to spend; this is the point
/// at which it stops being allowed to spend more. Across issues #121-#125 the
/// gate never sealed on any day after the first in ANY of the five matches —
/// `wall_gate_open` for 5, 7, 10, 11 and 15 of the fifteen dusk rounds, the
/// last of those being the whole window, i.e. the ring kept a robot-sized hole
/// every night of the match. The cause is that the seal waits for every role,
/// and a role fourteen to twenty cells out at dusk cannot arrive in time; the
/// wait then outlives the day, and the night planner never revisits the flag,
/// so the hole is permanent.
///
/// A straggler left outside is recoverable — the night recall's
/// `walk_or_remove_wall` hatch cuts back through our own ring — while an open
/// ring is not: the wall is the only thing between the waves and the station,
/// and `score_3` is 10×day for every day the station stands (550 over ten).
/// Sealing with three day-rounds to spare is also what leaves the stone carrier
/// time to actually place the gate cell.
pub(crate) const HARD_SEAL_ROUND: i64 = crate::brain::economy::DUSK_ROUND + 11;

/// Is there a walkable route from `start` to any of `stands`? A role already
/// standing on a stand cell counts as reachable (no move needed).
pub(crate) fn can_reach(turn: &Turn, start: Pos, stands: &[Pos]) -> bool {
    if stands.iter().any(|stand| *stand == start) {
        return true;
    }
    let blocked = turn.blocked_for(-1);
    crate::path::step_toward_stands(turn, start, stands, &blocked).is_some()
}

/// Where a role has to be able to get before the ring closes around it: the
/// OPERATOR CELL for the wall worker — the one post of the single-operator
/// night (design D14-D16), adjacent by construction to all three weapon sites
/// — and the inside of the ring for everybody else.
///
/// The second case is the whole of issue #15's "idle role walled out". With
/// guns and roles no longer paired, "no gun to man" is simply "no post": both
/// safety checks measure every role that has one against it, so a role mining
/// outside can never be sealed away from its own base by the crew closing the
/// last ring cell behind it (the gate that used to wait on "everyone inside"
/// died with design D17, but the veto that keeps everyone ABLE to get inside
/// is the same load-bearing check).
fn night_home(turn: &Turn, roles: &crate::brain::role::Roles, role_id: i64) -> Vec<Pos> {
    if roles.wall_worker == Some(role_id) {
        base_layout::operator_cell(turn).into_iter().collect()
    } else {
        crate::brain::interior_cells(turn)
    }
}

/// Every controllable role must still be able to reach its night post — the
/// operator cell for A, the inside of the ring for everybody else. If any
/// can't, wall building must stop: we never seal a role outside the ring.
pub(crate) fn roles_can_reach(turn: &Turn) -> bool {
    let roles = crate::brain::role::Roles::of(turn);
    for role in turn.controllable() {
        let home = night_home(turn, &roles, role.id);
        if home.is_empty() {
            continue; // nowhere to be: not a verdict this check can make
        }
        if !can_reach(turn, role.pos, &home) {
            return false;
        }
    }
    true
}

/// Would placing a wall at `site` cut any role off from its post, or seal a
/// gap the ring still has to fill away from the workers who can fill it?
/// Simulate the wall and re-run both reachability checks. Together with the
/// far-side-first build order, this guarantees the ring is only ever closed
/// after everyone can still get in — and that the last stone can still reach
/// the last gap.
pub(crate) fn wall_would_trap(turn: &Turn, state: &BotState, site: Pos, owned: &[i64]) -> bool {
    let roles = crate::brain::role::Roles::of(turn);
    let mut blocked = turn.blocked_for(-1);
    blocked.insert(site);
    for role in turn.controllable() {
        // A unit dispatched by its own mainline is OUTSIDE ON PURPOSE (comment
        // 1 §6: the economy worker does not come home at dusk). Standing beyond
        // this wall is that unit's plan, not an accident the wall caused, so
        // it never gets a veto — otherwise the ring's last cell waits forever
        // for a worker who is never coming back (the seal board finished at
        // 19/20 with the door open and the worker's own stones two maps away).
        if owned.contains(&role.id) {
            continue;
        }
        // A role with no gun to man is measured against the inside of the ring
        // (see `night_home`): "no tower" is not "no home", and the hole this
        // wall would cut is in ITS way home, not only in a controller's.
        let home = night_home(turn, &roles, role.id);
        if home.is_empty() {
            continue;
        }
        if home.iter().any(|stand| *stand == role.pos) {
            continue; // already at the post / already home: nothing to trap
        }
        // A role that cannot reach its post even WITHOUT this wall is not
        // what the wall would trap. Counting it anyway vetoes every remaining
        // ring cell at once — which is how day 1 ended at 19/20 with the last
        // stone sitting in a backpack (issues #12/#13/#14).
        if !crate::brain::can_reach_any(turn, role, &home) {
            continue;
        }
        if crate::path::step_toward_stands(turn, role.pos, &home, &blocked).is_none() {
            return true;
        }
    }
    // The same question for the wall line itself. A ring cell is only
    // buildable from a cell next to it, and a cell a teammate was standing on
    // when the sweep went past is exactly the one that stays open. Closing the
    // ring over the top of it leaves that hole reachable from the outside
    // only, with the stone on the wrong side of the wall — day 1 finished
    // 19/20 with two stones stuck in a backpack exactly that way.
    let can_build = |role: &Unit, gap: Pos, blocked: &HashSet<Pos>| -> bool {
        if chebyshev(role.pos, gap) == 1 {
            return true;
        }
        let stands = crate::brain::stand_cells(turn, gap);
        crate::path::step_toward_stands(turn, role.pos, &stands, blocked).is_some()
    };
    let before = turn.blocked_for(-1);
    for gap in pending_ring(turn, state, site) {
        let reachable = |blocked: &HashSet<Pos>| {
            turn.controllable()
                .iter()
                .filter(|role| !owned.contains(&role.id))
                .any(|role| can_build(role, gap, blocked))
        };
        // A gap nobody could reach even before this wall is not the wall's
        // doing, and vetoing on its account would leave the ring open forever.
        if reachable(&before) && !reachable(&blocked) {
            return true;
        }
    }
    false
}

/// Ring cells that are still to be filled, IGNORING whether a teammate happens
/// to be standing on one. `wall_gaps` drops an occupied cell so we never build
/// under a unit, but that same filter hides the cell from the build-order
/// safety check — and a ring cell a role was standing on when the sweep went
/// past is precisely the one that ends up walled off from the inside, with the
/// stone on the wrong side. Day 1 finished 19/20 exactly that way.
///
/// The list is the fixed build order (comment 1 §4), so the four permanent
/// entrance cells are not in it and never were "pending": the gate clause this
/// function used to carry died with the gate (design D17).
pub(crate) fn pending_ring(turn: &Turn, state: &BotState, ignore: Pos) -> Vec<Pos> {
    let walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    base_layout::wall_build_order(turn)
        .into_iter()
        .filter(|pos| {
            *pos != ignore
                && turn.is_land(*pos)
                && !walls.contains(pos)
                && !state
                    .blacklisted_builds
                    .contains(&(*pos, "wall".to_string()))
        })
        .collect()
}

/// The stone the day still owes: wall gaps inside today's budget, and nothing
/// else — the door and gate terms died with the gate (design D17), and the
/// permanent entrance is never walled. Recomputed from pure queries so a role
/// mainline (issue #221) can ask it without threading the round context's
/// locals. Reads the STORED `ring_ever_complete`, which the context may set a
/// round later — a one-round lag on the cap latch, never on the gap count.
pub(crate) fn stone_demand_of(turn: &Turn, state: &BotState) -> i64 {
    let gaps = wall_gaps(turn, state);
    let primary_open = ring_open_cells(turn, state);
    let cap = wall_daily_cap(turn.day, state.ring_ever_complete, primary_open);
    (gaps.len() as i64).min(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32, y: i32) -> Pos {
        Pos { x, y }
    }

    fn unit(id: i64, kind: &str, at: Pos) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": kind,
            "health": 1000, "level": 1, "backPackCapability": 100, "backpack": []
        })
    }

    /// The day-1 opening: station at (10,24) on the 41×32 map, plus whatever
    /// walls/roles the test asks for.
    fn board(extra: Vec<serde_json::Value>) -> Turn {
        let mut roles = vec![unit(10001, "station", pos(10, 24))];
        roles.extend(extra);
        let payload = serde_json::json!({
            "roundNo": 5,
            "mapInfo": {"width": 41, "height": 32, "zones": []},
            "teamOur": {
                "type": "challenger", "goldNum": 0, "totalScore": 0,
                "playerTasks": [], "roles": roles
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
        });
        let req: crate::protocol::Request =
            serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    /// Comment 1 §4's order on the live board: front column x=13 (y 21→26),
    /// top row y=26 (x 12→8), bottom row y=21 (x 8→12). The entrance column
    /// x=8 (y 22..25) is never offered — that is design D17's whole point: no
    /// gate cell, no seal, no door to re-cut every morning.
    #[test]
    fn the_gap_list_is_the_fixed_order_without_the_entrance() {
        let turn = board(vec![]);
        let state = BotState::default();
        let gaps = wall_gaps(&turn, &state);
        assert_eq!(gaps.len(), 16, "the whole ring minus the entrance four");
        assert_eq!(gaps[0], pos(13, 21), "the front column goes up first");
        assert_eq!(gaps[5], pos(13, 26));
        assert_eq!(gaps[6], pos(12, 26), "then the top row, walking back");
        assert_eq!(gaps[10], pos(8, 26));
        assert_eq!(gaps[11], pos(8, 21), "then the bottom row, walking out");
        assert_eq!(gaps[15], pos(12, 21), "the order's tail is the far corner");
        let entrance = base_layout::entrance_cells(&turn);
        assert!(
            gaps.iter().all(|gap| !entrance.contains(gap)),
            "the permanent entrance is never a gap"
        );
        assert_eq!(
            ring_open_cells(&turn, &state),
            16,
            "an empty board owes exactly the 16 wall cells"
        );
    }

    /// Built cells leave the list in order, and the debt counts cells a
    /// teammate's body hides from the work list — the two numbers answer
    /// different questions (`ring_at_risk` reads the debt).
    #[test]
    fn built_cells_leave_the_list_in_order_and_the_debt_counts_bodies() {
        let front: Vec<serde_json::Value> = (21..=26)
            .map(|y| unit(20000 + y as i64, "wall", pos(13, y)))
            .collect();
        let mut roles = front;
        roles.push(unit(10002, "worker", pos(12, 26))); // standing on the next gap
        let turn = board(roles);
        let state = BotState::default();
        let gaps = wall_gaps(&turn, &state);
        assert!(
            !gaps.contains(&pos(12, 26)),
            "the cell under the worker is not offerable this round"
        );
        assert_eq!(gaps[0], pos(11, 26), "the order resumes behind it");
        assert_eq!(gaps.len(), 9, "16 - 6 built - 1 occupied");
        assert_eq!(
            ring_open_cells(&turn, &state),
            10,
            "the debt still counts the occupied cell: the night finds it open"
        );
    }

    // -- the trap veto (moved out of `day.rs`, phase 5c) --------------------

    /// One role in a pocket of our own walls whose only opening is `(21,20)`.
    /// A pioneer, so the roster gives it no post: this is the "idle role" of
    /// P0-3, measured against the inside of the ring. `seal` closes the
    /// opening.
    fn pocketed_idle_role(seal: bool) -> Turn {
        let mut cells = vec![
            pos(19, 19),
            pos(19, 20),
            pos(19, 21),
            pos(20, 19),
            pos(20, 21),
            pos(21, 19),
            pos(21, 21),
        ];
        if seal {
            cells.push(pos(21, 20));
        }
        let mut roles: Vec<serde_json::Value> = cells
            .iter()
            .enumerate()
            .map(|(index, at)| unit(20000 + index as i64, "wall", *at))
            .collect();
        roles.push(unit(10002, "pioneer", pos(20, 20)));
        board(roles)
    }

    /// P0-3: a role with no post still has a home — the inside of the ring —
    /// and a wall that would cut it off from that home is a wall the crew may
    /// not build. Both safety checks used to `continue` past such a role,
    /// which is how an idle worker ended a day sealed outside the ring with
    /// the gate open behind it (docs/FAILURE-ANALYSIS-2026-09-14.md §3.3).
    #[test]
    fn a_wall_that_seals_an_idle_role_out_is_refused() {
        let turn = pocketed_idle_role(false);
        let state = BotState::default();
        let idle = turn.role_by_id(10002).expect("the idle role exists");
        let roles = crate::brain::role::Roles::of(&turn);
        assert_eq!(
            night_home(&turn, &roles, idle.id),
            crate::brain::interior_cells(&turn),
            "test setup: this role mans no gun — its home is the ring's inside"
        );
        // The pocket has exactly one opening and the role is not standing on
        // its home band, so today it can still get home — which is what makes
        // the wall on that opening the crew's doing and not the role's problem.
        assert!(
            crate::brain::can_reach_any(&turn, idle, &crate::brain::interior_cells(&turn)),
            "test setup: the role must be able to reach home BEFORE the wall"
        );
        assert!(
            roles_can_reach(&turn),
            "test setup: nobody is cut off yet, so the wall step is running"
        );
        assert!(
            wall_would_trap(&turn, &state, pos(21, 20), &[]),
            "the one cell that lets the idle role home was about to be walled over"
        );
    }

    /// …and once that cell IS wall — by the crew, by a robot, or by the crew's
    /// own earlier mistake — the whole wall step stands down instead of
    /// building somewhere else with a role sealed out of its own base.
    #[test]
    fn an_idle_role_already_sealed_out_stops_the_wall_line() {
        let sealed = pocketed_idle_role(true);
        assert!(
            !roles_can_reach(&sealed),
            "wall building went ahead with a role sealed out of its own base"
        );
    }

    /// The same predicate on a board where nobody is cut off: a role standing
    /// inside is not a veto, and neither is a wall far from it.
    #[test]
    fn a_wall_nobody_is_cut_off_by_is_allowed() {
        let turn = board(vec![unit(10002, "worker", pos(12, 24))]);
        let state = BotState::default();
        assert!(
            roles_can_reach(&turn),
            "a role standing inside the base is not a veto"
        );
        assert!(
            !wall_would_trap(&turn, &state, pos(20, 20), &[]),
            "a wall in the open, far from every role's way home, traps nobody"
        );
    }

    // -- ring geometry (moved out of `day.rs`, phase 5c) ---------------------

    #[test]
    fn ring_distance_one_of_footprint() {
        let footprint = station_footprint(pos(10, 24));
        let ring = ring_cells(&footprint, 1);
        assert_eq!(ring.len(), 12); // 4x4 outer minus 2x2 footprint
        assert!(ring
            .iter()
            .all(|cell| footprint_distance(*cell, &footprint) == 1));
    }

    #[test]
    fn radius_two_ring_is_one_complete_layer() {
        let footprint = station_footprint(pos(10, 24));
        let ring = ring_cells(&footprint, 2);
        assert_eq!(ring.len(), 20);
        assert!(ring
            .iter()
            .all(|cell| footprint_distance(*cell, &footprint) == 2));
        assert!(ring
            .iter()
            .all(|cell| footprint_distance(*cell, &footprint) != 3));
    }
}
