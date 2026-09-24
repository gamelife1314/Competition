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
use crate::model::{chebyshev, footprint_distance, station_footprint, Turn, WEAPON_BUILD_COST};
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
}
