//! Wall geometry: the ring's gap lists, layers, budgets and sector math
//! (issue #221 phase 2c). Pure queries — every function here answers "which
//! cells does the wall line want", never emits a command.
//!
//! Pure move out of `day.rs`, unchanged except visibility. Phase 4 replaces
//! the far-shoulder/gate-aware ordering with the comment-1 single-direction
//! sweep and the fixed back-4 entrance.

use std::collections::HashSet;

use crate::brain::{economy, route};

use super::build::wall_gate;
use crate::model::{chebyshev, footprint_distance, station_footprint, Turn, WEAPON_BUILD_COST};
use crate::protocol::Pos;
use crate::state::BotState;

/// The radius-2 station ring has 20 cells. Day 1 is allowed to complete that
/// entire single-layer shell; later days retain the conservative repair/expand
/// budget so fortification cannot permanently starve the economy.
pub const D1_WALL_CAP: i64 = 20;
pub const LATER_WALL_CAP: i64 = 6;

/// First day the second wall layer may be paid for (P2-1). Day 1 belongs to the
/// first ring: a stone spent on ring 3 that day is a hole in the ring that is
/// actually holding the night.
const SECOND_LAYER_MIN_DAY: i64 = 2;
/// Ring-3 cells the day may ask for at most. The outer layer is bought with
/// surplus — four cells is one mine trip, and the six-cell maintenance budget
/// still leaves room for the breach repair the ring itself may need.
const SECOND_LAYER_BATCH: usize = 4;
/// Cells kept clear around the gate before the second layer may stand. The gate
/// is how everything inside reaches the ore, the vendor and the shop; an outer
/// wall built across its mouth would seal the base's own doorway into a pocket,
/// and `open_door` only ever cuts through the radius-2 ring.
const SECOND_LAYER_GATE_CLEARANCE: i32 = 2;

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
/// `primary_open` is the second exit from the maintenance budget, and it is the
/// one that does not depend on history. `ring_ever_complete` asks "was the ring
/// EVER whole" — so a ring that was never whole at all (day 1 lost to a distant
/// stone vein, a night that emptied the shell before it closed) had no exit: it
/// was repaired on 6 cells a day forever, and 6 cells a day cannot out-build a
/// night that takes 10 walls out. Six in, ten out is a ratchet, and the ring it
/// ratchets down is the one holding the base. A ring that is OPEN NOW gets the
/// build-out budget whether it has ever been whole or not, whatever the day.
///
/// The anti-starvation intent is untouched, and it is the third case: a ring
/// that is whole (no primary gaps) and wants MORE than it has — the ring-3
/// second layer, extra tidying — is still on the 6-cell maintenance budget. The
/// cap is released to REPAIR a ring, never to expand one.
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

/// Every wall cell the day wants filled: the primary ring, and — once that
/// ring is complete — the second layer on the damaged arc (P2-1).
///
/// The order matters and is deliberate, and it is a two-step one:
///
///   1. **the ring, as [`ring_gap_split`] divides it** — 「朝向敌人的三个方向城墙
///      一定是完整的」 first, and the far shoulder only once that arc is
///      finished. Nothing outranks a cell a robot can walk in through, and the
///      shoulder is what the day does with the stone left over after the arc
///      (「有条件全部建造好」). Past [`far_edge_cutoff`] the split puts the
///      shoulder back in the first half by itself, because by then the ring is
///      what holds the night.
///   2. **the second layer** — it guards the arc the robots demonstrably come
///      through, and it is a luxury: it is reached only on a day whose primary
///      ring is whole, which is exactly the day that has earned it.
///
/// The important consequence is the one `plan`'s `stone_demand` is built on: a
/// far cell is never on this list while the arc that faces the enemy is still
/// open, so no stone is ever mined for a cell the day has been told it may skip.
pub fn wall_gaps(turn: &Turn, state: &BotState) -> Vec<Pos> {
    let (owed, shoulder) = ring_gap_split(turn, state);
    if !owed.is_empty() {
        return owed;
    }
    if !shoulder.is_empty() {
        return shoulder;
    }
    second_layer_gaps(turn, state)
}

/// Ring cells the day still owes a wall: the primary ring minus the walls that
/// are standing in it.
///
/// This is the DEBT, not the work list. [`primary_wall_gaps`] is what the sweep
/// can be sent to — it drops the gate until the seal, the door the economy cut,
/// any cell a teammate's footprint covers, any cell that already failed to
/// build — and every one of those drops is a cell the night still finds open.
/// The difference is what a day that is running out of time looks like from
/// inside the planner: on the day-1 board the sweep's list emptied at R46 with
/// four cells of the ring's south row still bare, and the crew — one carrier
/// outside the ring, the other idle inside it — could not touch them until the
/// dusk seal released them at R61 (「第一天只挖石头」's ring, closed at R65 with
/// dusk at 55). See `plan`'s `ring_at_risk`, which is the deadline this count
/// exists for.
pub(crate) fn ring_open_cells(turn: &Turn, state: &BotState) -> usize {
    let Some(station) = turn.station() else {
        return 0;
    };
    let footprint = station_footprint(station.pos);
    let walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    ring_cells(&footprint, 2)
        .into_iter()
        .filter(|pos| {
            turn.is_land(*pos)
                && !walls.contains(pos)
                && !state
                    .blacklisted_builds
                    .contains(&(*pos, "wall".to_string()))
        })
        // ...and only the cells the day OWES. The far shoulder is not a debt
        // (issue #206 §6): counting it kept `plan`'s `ring_at_risk` true from
        // the moment the enemy-facing arc closed, which pulls the economy
        // worker back onto the wall line for a cell the day does not owe —
        // the freeze the whole of `ring_at_risk` exists to prevent, rebuilt
        // out of the cell the owner just made optional.
        .filter(|pos| !far_shoulder(turn, state, *pos))
        .count()
}

/// The second wall layer: ring-3 cells on the arc that is actually taking
/// damage, and nowhere else (P2-1).
///
/// Three limits keep it from becoming the full-map second ring the analysis
/// rules out. It exists only on the sectors [`BotState::threatened_sectors`]
/// ranks highest — at most three of the eight — so the arc is open at both ends
/// and can never trap a role the way a second ring would; only
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
    let gate = wall_gate(turn, state);
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
        .filter(|pos| {
            gate.map_or(true, |gate| chebyshev(*pos, gate) > SECOND_LAYER_GATE_CLEARANCE)
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

/// Which wall layer a cell belongs to: 2 is the ring that holds the night, 3 is
/// the second layer on the damaged arc. Written into `wall_build` so the two
/// are told apart in the log — a wall count that does not say which layer it
/// came from cannot answer whether P2-1 built anything.
pub(crate) fn wall_layer(turn: &Turn, site: Pos) -> i64 {
    match turn.station() {
        Some(station) => footprint_distance(site, &station.footprint()) as i64,
        None => 0,
    }
}

/// Desired D1 wall cells: one radius-2 shell around the station. One gate cell
/// remains omitted while any controller is outside; after the dusk retreat
/// checkpoint `update_wall_gate` explicitly admits that final seal cell.
///
/// This is the ring the day OWES — [`ring_gap_split`]'s first half. The far
/// shoulder is not in it (issue #206 §6).
pub fn primary_wall_gaps(turn: &Turn, state: &BotState) -> Vec<Pos> {
    ring_gap_split(turn, state).0
}

/// The compass ring of the eight non-centre sectors, in order round the base.
///
/// `arc_sector` numbers the 3×3 compass row-major (0 is south-west, 4 is the
/// centre, 8 is north-east), so two sectors that touch on the map are NOT
/// neighbours in the numbers: 2 (south-east) touches 5 (east), not 3. Walking
/// the compass therefore needs this list, and it is the same walk the ring
/// cells themselves take.
const SECTOR_RING: [usize; 8] = [0, 1, 2, 5, 8, 7, 6, 3];

/// The three sectors that face `enemy_sector`: the enemy's own and the two
/// beside it. The owner's 「朝向敌人的三个方向城墙一定是完整的」 — three of the
/// eight is the arc a robot walks in through, and it is why the far shoulder can
/// be left for last without opening a way in.
///
/// A centre sector (the enemy standing on top of our station) has no bearing to
/// spread, so nothing at all is optional there.
fn enemy_facing_sectors(enemy_sector: usize) -> [usize; 3] {
    match SECTOR_RING.iter().position(|sector| *sector == enemy_sector) {
        Some(at) => {
            let last = SECTOR_RING.len() - 1;
            [
                SECTOR_RING[if at == 0 { last } else { at - 1 }],
                enemy_sector,
                SECTOR_RING[if at == last { 0 } else { at + 1 }],
            ]
        }
        None => [enemy_sector; 3],
    }
}

/// How close a robot has to be for the ring to stop treating any cell as
/// optional.
///
/// The shell sits two cells out from the station's footprint, so a robot inside
/// the ring or standing on its far shoulder is within a few cells of the
/// centre. That is the case the owner's 「后边的门可以开着」 does not cover: a
/// door is a door while nothing is at it, and a way in the moment something is.
/// Deliberately generous — the cheap mistake here is a wall the day did not
/// strictly need.
const ROBOT_AT_THE_RING: i32 = 6;

/// The round the far shoulder stops being optional.
///
/// 「可以选择性缺口」 is a licence to leave the far side for last, not to leave it
/// for the night: past this round the ring is again the thing standing between
/// the base and the dark, and every cell of it is owed. Measured the way the
/// rest of the afternoon is — back from `DUSK_ROUND` by the walk home — so the
/// crew that has to close it still has the rounds to.
pub fn far_edge_cutoff() -> i64 {
    economy::DUSK_ROUND - economy::DUSK_TRIP_MARGIN
}

/// Is this ring cell on the shoulder that faces AWAY from the enemy — the one
/// the owner made optional (issue #206 §6)?
///
/// Four things make a cell required instead, and the first three are the ones
/// that decide an ordinary round:
///
///   * it is on the enemy's arc (or the enemy is standing on the base, which
///     leaves no bearing to be far from);
///   * robots have already come through its sector — [`BotState::
///     threatened_sectors`] is evidence, and evidence beats the compass;
///   * it is late: see [`far_edge_cutoff`];
///   * a robot is close enough to be at the ring at all.
///
/// The geometry is the one already in the codebase — `state::arc_sector` around
/// the station, the enemy's own bearing from `turn.enemy_station`, and the
/// damage-ranked sectors P2-1 built — so the wall line and the second layer
/// cannot disagree about which side of the base a cell is on.
pub fn far_shoulder(turn: &Turn, state: &BotState, cell: Pos) -> bool {
    if turn.in_day_round >= far_edge_cutoff() {
        return false;
    }
    let Some(station) = turn.station() else {
        return false;
    };
    let Some(enemy) = turn.enemy_station() else {
        // No enemy base on the board is no bearing to be far from: with nothing
        // to face, every cell faces it.
        return false;
    };
    let center = station.pos;
    let sector = crate::state::arc_sector(center, cell);
    if enemy_facing_sectors(crate::state::arc_sector(center, enemy.pos)).contains(&sector) {
        return false;
    }
    if state.threatened_sectors().contains(&sector) {
        return false;
    }
    !turn
        .robots
        .iter()
        .any(|robot| chebyshev(robot.pos, center) <= ROBOT_AT_THE_RING)
}

/// The primary ring's open cells, split by whether the day OWES them.
///
/// `.0` is the ring the owner requires — 「朝向敌人的三个方向城墙一定是完整的」 —
/// and `.1` is the far shoulder — 「0 的位置可以选择性缺口，有条件全部建造好」.
/// The split changes what the day owes, never what it may build: the same cells
/// are on the list, and a day with the stone to spare builds all of them.
pub fn ring_gap_split(turn: &Turn, state: &BotState) -> (Vec<Pos>, Vec<Pos>) {
    let Some(station) = turn.station() else {
        return (Vec::new(), Vec::new());
    };
    let footprint = station_footprint(station.pos);
    let Some(gate) = wall_gate(turn, state) else {
        return (Vec::new(), Vec::new());
    };

    // THE BUILD ORDER IS THE RING WALKED FROM THE ENTRANCE (see
    // `route::build_order`): the crew steps out of the door it will use all day
    // and lays stone round the shell, so consecutive placements are adjacent —
    // one step each — and the last cell placed is the entrance's far shoulder,
    // where the crew is standing when the ring closes. The order this replaced
    // sorted on Chebyshev distance from the fixed corner gate with an `(x, y)`
    // tiebreak, which is a compass sweep: the crew started on the far side of
    // the base and crossed its own finished wall on the way back.
    let order = route::build_order(turn, state, gate);
    let mut cells = ring_cells(&footprint, 2);
    cells.sort_by_key(|pos| (route::build_rank(&order, *pos), pos.x, pos.y));

    let existing_walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    let occupied: HashSet<Pos> = turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    cells
        .into_iter()
        .filter(|pos| state.wall_gate_sealed || *pos != gate)
        // A cell the economy cut open this morning is a door, not a hole: the
        // gap list must not send the wall crew to re-seal it under the digger's
        // feet all day. From dusk the door stops being honoured and the cell is
        // an ordinary gap again, so the ring is closed before the robots come.
        .filter(|pos| turn.in_day_round >= economy::DUSK_ROUND || !state.door_cells.contains(pos))
        .filter(|pos| {
            turn.is_land(*pos)
                && !existing_walls.contains(pos)
                && !occupied.contains(pos)
                && !state
                    .blacklisted_builds
                    .contains(&(*pos, "wall".to_string()))
        })
        // The split, last, so it sees the cells the sweep would actually be
        // sent to: a cell that is already walled or that a teammate is standing
        // on is not a gap in either half.
        .partition(|pos| !far_shoulder(turn, state, *pos))
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
