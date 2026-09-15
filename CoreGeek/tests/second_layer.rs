//! P2-1: the second wall layer — ring 3, on the arc that is actually taking
//! damage, and nowhere else (docs/FAILURE-ANALYSIS-2026-09-14.md §4 P2).
//!
//! The analysis rules out a full-map second ring: it is stone the economy does
//! not have and walking the day cannot spare. What it asks for instead is a
//! layer that exists only where the robots demonstrably come through, bounded
//! so that it can never close into a ring of its own. These tests pin all four
//! of those properties: the arc is chosen from damage statistics, it is capped,
//! it keeps the gate's doorway clear, and with no evidence it is not built.

use std::collections::HashSet;

use serde_json::{json, Value};

use coregeek::brain::day::{primary_wall_gaps, tower_gaps, wall_gaps};
use coregeek::model::{footprint_distance, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::{arc_sector, BotState, CENTRE_SECTOR, MAX_THREAT_SECTORS};

const BASE: (i32, i32) = (18, 16);

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn station() -> Value {
    json!({
        "id": 10001, "pos": {"x": BASE.0, "y": BASE.1}, "roleType": "station",
        "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn tower(id: i64, kind: &str, dx: i32, dy: i32) -> Value {
    tower_with_level(id, kind, dx, dy, 2)
}

fn tower_with_level(id: i64, kind: &str, dx: i32, dy: i32, level: i64) -> Value {
    json!({
        "id": id, "pos": {"x": BASE.0 + dx, "y": BASE.1 + dy}, "roleType": kind,
        "health": 1000, "attackPower": 10, "attackRange": 3,
        "level": level, "backPackCapability": 0, "backpack": []
    })
}

fn worker(id: i64, dx: i32, dy: i32) -> Value {
    json!({
        "id": id, "pos": {"x": BASE.0 + dx, "y": BASE.1 + dy}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 100, "backpack": []
    })
}

fn wall(id: i64, pos: Pos) -> Value {
    json!({
        "id": id, "pos": {"x": pos.x, "y": pos.y}, "roleType": "wall",
        "health": 1000, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn footprint() -> Vec<Pos> {
    station_footprint(Pos {
        x: BASE.0,
        y: BASE.1,
    })
}

/// On-board cells of the ring at `radius` from the station footprint.
fn ring(radius: i32) -> Vec<Pos> {
    let footprint = footprint();
    let mut cells = Vec::new();
    for x in BASE.0 - 6..=BASE.0 + 7 {
        for y in BASE.1 - 6..=BASE.1 + 7 {
            let pos = Pos { x, y };
            if pos.x < 0 || pos.y < 0 || pos.x >= 41 || pos.y >= 32 {
                continue;
            }
            if footprint_distance(pos, &footprint) == radius {
                cells.push(pos);
            }
        }
    }
    cells.sort_by_key(|pos| (pos.x, pos.y));
    cells
}

/// The station's gate cell, as `day::wall_gate` computes it: two cells past the
/// footprint's east edge, one below its south edge.
fn gate() -> Pos {
    Pos {
        x: BASE.0 + 3,
        y: BASE.1 - 2,
    }
}

/// Every ring-2 cell but the gate — a ring the day considers closed.
fn closed_ring() -> Vec<Value> {
    ring(2)
        .into_iter()
        .filter(|pos| *pos != gate())
        .enumerate()
        .map(|(index, pos)| wall(20000 + index as i64, pos))
        .collect()
}

fn board(roles: Vec<Value>, round_no: i64) -> Value {
    json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 100, "totalScore": 0,
            "playerTasks": [], "roles": roles
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    })
}

/// Round 131 = day 2 round 1 (70 day rounds per day).
const DAY_TWO: i64 = 131;

/// Three towers, a worker, and the closed ring: the state in which the second
/// layer is eligible at all.
fn ready_day_two() -> Vec<Value> {
    let mut roles = vec![
        station(),
        tower(10020, "gatling", 0, 3),
        tower(10030, "railgun", 2, 3),
        tower(10040, "rocket", -2, 3),
        worker(10011, 0, -1),
    ];
    roles.extend(closed_ring());
    roles
}

/// Charge `sector` with threat points by damaging a wall that sits in it.
fn state_with_damage_in(sector: usize, points: i64) -> BotState {
    let mut state = BotState::default();
    state.threat_sectors[sector] = points;
    state
}

/// Ring-3 cells that fall in `sector`.
fn ring_three_in(sector: usize) -> Vec<Pos> {
    let center = Pos {
        x: BASE.0,
        y: BASE.1,
    };
    ring(3)
        .into_iter()
        .filter(|pos| arc_sector(center, *pos) == sector)
        .collect()
}

// ---------------------------------------------------------------------------
// Where the layer may stand.
// ---------------------------------------------------------------------------

/// Sector of the base's north-east quadrant.
fn north_east() -> usize {
    arc_sector(
        Pos {
            x: BASE.0,
            y: BASE.1,
        },
        Pos {
            x: BASE.0 + 4,
            y: BASE.1 + 4,
        },
    )
}

#[test]
fn the_second_layer_stands_on_the_damaged_arc_and_nowhere_else() {
    // Threat evidence only in the north-east quadrant. The whole second layer
    // must fall inside it: a ring-3 cell anywhere else is stone spent on an arc
    // the robots have never touched.
    let sector = north_east();
    assert_ne!(sector, CENTRE_SECTOR);
    let state = state_with_damage_in(sector, 500);
    let turn = turn_from(board(ready_day_two(), DAY_TWO));

    let gaps = wall_gaps(&turn, &state);
    assert!(!gaps.is_empty(), "the threatened arc is worth walling");

    let center = Pos {
        x: BASE.0,
        y: BASE.1,
    };
    let footprint = footprint();
    for site in &gaps {
        assert_eq!(
            footprint_distance(*site, &footprint),
            3,
            "{site:?} is not a second-layer cell"
        );
        assert_eq!(
            arc_sector(center, *site),
            sector,
            "{site:?} is outside the damaged arc"
        );
    }
    // And it is a real share of that arc, not one token cell.
    assert!(gaps.len() >= 2, "only {} cells of the arc: {gaps:?}", gaps.len());
}

#[test]
fn an_undamaged_base_builds_no_second_layer_at_all() {
    // No threat statistics: nothing has ever hit the base, and the analysis is
    // explicit that the outer layer is only worth its stone where it has.
    let turn = turn_from(board(ready_day_two(), DAY_TWO));
    assert!(
        wall_gaps(&turn, &BotState::default()).is_empty(),
        "a base that has taken no damage gets no second layer"
    );
}

#[test]
fn the_arc_is_capped_so_a_second_ring_can_never_close() {
    // Damage everywhere: every one of the eight sectors qualifies. The layer
    // must still stop at MAX_THREAT_SECTORS, because a closed second ring is
    // exactly what the analysis rules out — it would enclose the corridor
    // between the layers and trap whoever is in it.
    let mut state = BotState::default();
    for sector in 0..9 {
        state.threat_sectors[sector] = 400;
    }
    let sectors = state.threatened_sectors();
    assert_eq!(sectors.len(), MAX_THREAT_SECTORS, "the arc cap holds");
    assert!(
        sectors.len() < 8,
        "a full ring of sectors would close the second layer"
    );

    // The guarantee is structural, not a per-day budget that a long match
    // could spend its way past: three sectors of eight is the most the arc can
    // ever cover, so five sectors are never walled at all — and every sector of
    // this base's ring 3 holds at least one cell. A ring with an unwalled cell
    // is not a ring, so the outer layer can never close into one, however many
    // days it is given.
    let open_sectors: Vec<usize> = (0..9)
        .filter(|sector| *sector != CENTRE_SECTOR && !sectors.contains(sector))
        .filter(|sector| !ring_three_in(*sector).is_empty())
        .collect();
    assert_eq!(open_sectors.len(), 5, "five sectors stay unwalled");

    let turn = turn_from(board(ready_day_two(), DAY_TWO));
    let ring_three = ring(3);
    for sector in open_sectors {
        let cells = ring_three_in(sector);
        assert!(
            cells.iter().all(|cell| !wall_gaps(&turn, &state).contains(cell)),
            "sector {sector} is meant to stay open"
        );
    }
    assert!(
        wall_gaps(&turn, &state).len() < ring_three.len(),
        "the second layer is a strict subset of ring 3, never all of it"
    );
}

#[test]
fn the_layer_never_walls_the_gate_into_a_pocket() {
    // The gate is the only way out of the ring for the ore, the vendor and the
    // shop, and `open_door` only ever cuts through the RADIUS-2 ring. A ring-3
    // wall across its mouth would seal the base's own doorway with no way to
    // reopen it, so a clearance around the gate is kept clear.
    let mut state = BotState::default();
    // Threat from the east, which is where the gate is.
    let center = Pos {
        x: BASE.0,
        y: BASE.1,
    };
    let east = arc_sector(
        center,
        Pos {
            x: BASE.0 + 5,
            y: BASE.1 - 2,
        },
    );
    state.threat_sectors[east] = 800;
    let turn = turn_from(board(ready_day_two(), DAY_TWO));

    let open: HashSet<Pos> = ring(3)
        .into_iter()
        .filter(|pos| coregeek::model::chebyshev(*pos, gate()) <= 2)
        .collect();
    assert!(!open.is_empty(), "the clearance set is not empty");
    for site in wall_gaps(&turn, &state) {
        assert!(
            !open.contains(&site),
            "{site:?} walls the gate's own doorway"
        );
    }
}

#[test]
fn the_batch_is_bounded_so_the_economy_is_not_starved() {
    // At most four cells a day. The second layer is bought with surplus: the
    // six-cell maintenance budget still has to cover whatever the night did to
    // the ring that actually holds.
    let mut state = BotState::default();
    for sector in 0..9 {
        state.threat_sectors[sector] = 400;
    }
    let turn = turn_from(board(ready_day_two(), DAY_TWO));
    assert!(
        wall_gaps(&turn, &state).len() <= 4,
        "more than a mine trip's worth of outer wall in one day"
    );
}

// ---------------------------------------------------------------------------
// When the layer may be built at all.
// ---------------------------------------------------------------------------

#[test]
fn the_second_layer_waits_for_the_first_ring_that_holds_the_night() {
    // One ring-2 cell missing: the day's stone belongs to the ring that stands
    // between the robots and the station, not to a layer outside it.
    let hole = ring(2)
        .into_iter()
        .find(|pos| *pos != gate())
        .expect("the ring has cells");
    let mut roles = ready_day_two();
    roles.retain(|unit| {
        unit["roleType"] != "wall"
            || unit["pos"]["x"] != json!(hole.x)
            || unit["pos"]["y"] != json!(hole.y)
    });
    let turn = turn_from(board(roles, DAY_TWO));
    let state = state_with_damage_in(north_east(), 900);

    assert!(
        !primary_wall_gaps(&turn, &state).is_empty(),
        "the primary ring is still open"
    );
    for site in wall_gaps(&turn, &state) {
        assert_eq!(
            footprint_distance(site, &footprint()),
            2,
            "with the ring open only ring-2 cells are offered"
        );
    }
}

#[test]
fn the_second_layer_waits_for_the_third_gun() {
    // Two towers: the build-out is not finished and 25 gold is still owed to
    // the defence. The outer layer is a luxury bought after it.
    let mut roles: Vec<Value> = vec![
        station(),
        tower(10020, "gatling", 0, 3),
        tower(10030, "railgun", 2, 3),
        worker(10011, 0, -1),
    ];
    roles.extend(closed_ring());
    let turn = turn_from(board(roles, DAY_TWO));
    let state = state_with_damage_in(0, 900);
    assert!(
        wall_gaps(&turn, &state).is_empty(),
        "two towers: no second layer yet"
    );
}

#[test]
fn day_one_belongs_to_the_first_ring() {
    // A ring-3 wall on day 1 is a hole in the ring that is actually holding the
    // night. Even with the ring closed and three towers standing.
    let turn = turn_from(board(ready_day_two(), 60));
    let mut state = state_with_damage_in(0, 900);
    state.threat_sectors[1] = 900;
    assert!(turn.day == 1);
    assert!(
        wall_gaps(&turn, &state).is_empty(),
        "day 1 spends its stone on the first ring"
    );
}

// ---------------------------------------------------------------------------
// Where the threat statistics come from.
// ---------------------------------------------------------------------------

#[test]
fn wall_damage_is_charged_to_the_sector_it_landed_in() {
    // The evidence the arc is chosen from is damage actually taken, not
    // proximity. A wall cell that loses HP this round charges its sector.
    let target = ring(2)
        .into_iter()
        .find(|pos| *pos != gate())
        .expect("the ring has cells");
    let center = Pos {
        x: BASE.0,
        y: BASE.1,
    };
    let sector = arc_sector(center, target);

    let mut roles = vec![station(), worker(10011, 0, -1), wall(20001, target)];
    roles.extend(
        ring(2)
            .into_iter()
            .filter(|pos| *pos != target && *pos != gate())
            .enumerate()
            .map(|(index, pos)| wall(21000 + index as i64, pos)),
    );
    let turn = turn_from(board(roles, DAY_TWO));

    let mut state = BotState::default();
    state.observe(&turn); // first sighting: nothing to attribute
    assert_eq!(
        state.threat_sectors[sector], 0,
        "an undamaged wall is not evidence"
    );

    // The same cell, 400 HP down.
    let mut roles = vec![station(), worker(10011, 0, -1)];
    roles.push(json!({
        "id": 20001, "pos": {"x": target.x, "y": target.y}, "roleType": "wall",
        "health": 600, "level": 1, "backPackCapability": 0, "backpack": []
    }));
    roles.extend(
        ring(2)
            .into_iter()
            .filter(|pos| *pos != target && *pos != gate())
            .enumerate()
            .map(|(index, pos)| wall(21000 + index as i64, pos)),
    );
    let turn = turn_from(board(roles, DAY_TWO + 1));
    state.observe(&turn);
    assert!(
        state.threat_sectors[sector] > 400,
        "400 HP lost must charge the sector it was lost in: {:?}",
        state.threat_sectors
    );
    assert_eq!(
        state.threatened_sectors(),
        vec![sector],
        "the damaged sector is the threatened one"
    );
}

#[test]
fn robots_closing_on_the_base_are_a_weaker_signal_than_damage() {
    // A robot within the threat radius scores, so an arc can be identified
    // before the first wall falls — but far below a real hit, so a sector that
    // has merely been walked past never outranks one that has been breached.
    let robot = |id: i64, x: i32, y: i32| {
        json!({
            "id": id, "pos": {"x": x, "y": y}, "roleType": "smallRobot",
            "health": 100, "attackPower": 5, "attackRange": 1,
            "level": 1, "backPackCapability": 0, "backpack": []
        })
    };
    let center = Pos {
        x: BASE.0,
        y: BASE.1,
    };
    let sector = north_east();
    let mut payload = board(vec![station(), worker(10011, 0, -1)], DAY_TWO);
    payload["robot"] = json!({"roles": [
        robot(30001, BASE.0 + 4, BASE.1 + 4),
        robot(30002, BASE.0 + 5, BASE.1 + 4),
    ]});
    let turn = turn_from(payload);
    let mut state = BotState::default();
    state.observe(&turn);

    assert_eq!(state.threat_sectors[sector], 2, "one point per sighting");
    assert_eq!(state.threatened_sectors(), vec![sector]);

    // A robot outside the radius is not a threat to any sector.
    let mut payload = board(vec![station(), worker(10011, 0, -1)], DAY_TWO);
    payload["robot"] = json!({"roles": [robot(30003, BASE.0 + 20, BASE.1 + 4)]});
    let far = turn_from(payload);
    let mut distant = BotState::default();
    distant.observe(&far);
    assert_eq!(distant.threatened_sectors(), Vec::<usize>::new());

    // One wall hit outweighs a whole night of sightings.
    let breached = arc_sector(center, ring(2)[0]);
    assert_ne!(breached, sector);
    state.threat_sectors[breached] += 525;
    assert_eq!(
        state.threatened_sectors().first().copied(),
        Some(breached),
        "damage outranks proximity"
    );
}

#[test]
fn damage_arcs_reach_the_wall_blueprint_through_the_planner() {
    // End to end: the day planner reads the statistics `observe` filled and
    // puts the outer layer on that arc.
    let target = ring(2)
        .into_iter()
        .find(|pos| *pos != gate())
        .expect("the ring has cells");
    let center = Pos {
        x: BASE.0,
        y: BASE.1,
    };
    let sector = arc_sector(center, target);

    let mut roles = vec![
        station(),
        tower(10020, "gatling", 0, 3),
        tower(10030, "railgun", 2, 3),
        tower(10040, "rocket", -2, 3),
        worker(10011, 0, -1),
    ];
    roles.push(json!({
        "id": 20001, "pos": {"x": target.x, "y": target.y}, "roleType": "wall",
        "health": 1000, "level": 1, "backPackCapability": 0, "backpack": []
    }));
    roles.extend(
        ring(2)
            .into_iter()
            .filter(|pos| *pos != target && *pos != gate())
            .enumerate()
            .map(|(index, pos)| wall(21000 + index as i64, pos)),
    );
    let turn = turn_from(board(roles, DAY_TWO));

    let mut state = BotState::default();
    state.observe(&turn);
    // Second round: the same wall, now breached.
    let mut roles = vec![
        station(),
        tower(10020, "gatling", 0, 3),
        tower(10030, "railgun", 2, 3),
        tower(10040, "rocket", -2, 3),
        worker(10011, 0, -1),
    ];
    roles.push(json!({
        "id": 20001, "pos": {"x": target.x, "y": target.y}, "roleType": "wall",
        "health": 200, "level": 1, "backPackCapability": 0, "backpack": []
    }));
    roles.extend(
        ring(2)
            .into_iter()
            .filter(|pos| *pos != target && *pos != gate())
            .enumerate()
            .map(|(index, pos)| wall(21000 + index as i64, pos)),
    );
    let turn = turn_from(board(roles, DAY_TWO + 1));
    state.observe(&turn);

    // The towers the plan wants are already up, so the wall blueprint is the
    // only thing `plan` still has to say about building.
    assert!(tower_gaps(&turn, &state).is_empty());
    let planned: HashSet<Pos> = wall_gaps(&turn, &state).into_iter().collect();
    assert!(!planned.is_empty(), "the breached arc gets its outer layer");
    let footprint = footprint();
    for site in &planned {
        assert_eq!(footprint_distance(*site, &footprint), 3);
        assert_eq!(arc_sector(center, *site), sector);
    }
}

#[test]
fn the_sector_map_partitions_the_compass() {
    // The two layers disagreeing about which sector a cell is in would put the
    // outer wall somewhere the damage never was, so the mapping is pinned.
    let center = Pos { x: 10, y: 10 };
    assert_eq!(centred(center), CENTRE_SECTOR);
    assert_eq!(arc_sector(center, Pos { x: 11, y: 10 }), 5, "east");
    assert_eq!(arc_sector(center, Pos { x: 9, y: 10 }), 3, "west");
    assert_eq!(arc_sector(center, Pos { x: 10, y: 11 }), 7, "north");
    assert_eq!(arc_sector(center, Pos { x: 10, y: 9 }), 1, "south");
    assert_eq!(arc_sector(center, Pos { x: 11, y: 11 }), 8, "north-east");
    assert_eq!(arc_sector(center, Pos { x: 9, y: 9 }), 0, "south-west");
}

fn centred(center: Pos) -> usize {
    arc_sector(center, center)
}
