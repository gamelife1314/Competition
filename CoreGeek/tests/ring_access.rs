//! Reachability inside the base: the ring corridor, the "can I still get to my
//! gun" question, and the pairing that depends on it.
//!
//! Issues 12/13/14 all bottom out in the same shape. The station, the towers
//! and the wall line partition the base interior into cells that look adjacent
//! on the map and are unreachable in fact. A controller handed the nearest gun
//! then spends the night walking into a wall ("0 角色站桩闲置"), the tower never
//! fires, and `wall_would_trap` refuses to seal the last ring cell because THAT
//! role would still be cut off — so the ring stays open and the base is exposed.
//! These tests pin the three primitives the fix is built from.

use std::collections::HashSet;

use serde_json::{json, Value};

use coregeek::brain::{can_reach_any, interior_cells, night, tower_stand_cells};
use coregeek::model::{footprint_distance, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn station(x: i32, y: i32) -> Value {
    json!({
        "id": 10001, "pos": {"x": x, "y": y}, "roleType": "station",
        "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn role(id: i64, kind: &str, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": kind,
        "health": 220, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 100, "backpack": []
    })
}

fn gatling(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "gatling",
        "health": 1000, "attackPower": 10, "attackRange": 3,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn wall(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "wall",
        "health": 1000, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

/// A round-1 daytime board: `roundNo 1` is day 1, in-day round 0, so no
/// deadline in the day planner has fired yet.
fn world(ours: Vec<Value>) -> Value {
    json!({
        "roundNo": 1,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [], "roles": ours
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    })
}

#[test]
fn interior_cells_are_the_walkable_band_around_the_station() {
    // The band the wall gate measures "everyone is inside" against, and the
    // retreat target for a role whose tower is walled off.
    let turn = turn_from(world(vec![station(10, 24)]));
    let footprint = station_footprint(Pos { x: 10, y: 24 });
    let cells = interior_cells(&turn);
    assert!(!cells.is_empty(), "a mid-map station has an interior band");
    for cell in &cells {
        assert!(
            footprint_distance(*cell, &footprint) <= 1,
            "{cell:?} is outside the band"
        );
        assert!(turn.is_land(*cell), "{cell:?} is not walkable");
        assert!(!footprint.contains(cell), "{cell:?} is the station itself");
    }
    // The four cells orthogonally touching the 2x2 footprint are always in it.
    for cell in [Pos { x: 12, y: 24 }, Pos { x: 9, y: 23 }] {
        assert!(cells.contains(&cell), "{cell:?} missing from the band");
    }
    // No duplicates: the sweep walks every footprint cell's neighbours.
    let unique: HashSet<Pos> = cells.iter().copied().collect();
    assert_eq!(unique.len(), cells.len(), "the band repeats a cell");
}

#[test]
fn a_role_walled_off_from_its_gun_cannot_reach_it() {
    // A gun on the ring with its operating cells sealed off by our own wall
    // line. Distance says "next to it"; the pathfinder says "no route", and the
    // pairing has to believe the pathfinder.
    let mut ours = vec![station(10, 24), gatling(10020, 10, 21)];
    // Ring the gun's stand cells in, leaving the role outside the box.
    for (index, (x, y)) in [(9, 22), (10, 22), (11, 22), (9, 20), (10, 20), (11, 20)]
        .iter()
        .enumerate()
    {
        ours.push(wall(20000 + index as i64, *x, *y));
    }
    ours.push(role(10002, "worker", 10, 19));
    let turn = turn_from(world(ours));

    let controller = turn.role_by_id(10002).expect("controller exists");
    let stands = tower_stand_cells(&turn, Pos { x: 10, y: 21 });
    assert!(
        !stands.iter().any(|stand| *stand == controller.pos),
        "the controller is not already on a stand cell"
    );
    assert!(
        !can_reach_any(&turn, controller, &stands),
        "the wall line cuts every route to the gun"
    );
}

#[test]
fn standing_on_a_stand_cell_counts_as_reachable() {
    // `walk_or_remove_wall` must not tear a wall down for a role that is
    // already where it needs to be: reachable includes "already there".
    let turn = turn_from(world(vec![
        station(10, 24),
        gatling(10020, 10, 21),
        role(10002, "worker", 10, 22), // adjacent to the gun
    ]));
    let controller = turn.role_by_id(10002).expect("controller exists");
    let stands = tower_stand_cells(&turn, Pos { x: 10, y: 21 });
    assert!(stands.contains(&controller.pos));
    assert!(can_reach_any(&turn, controller, &stands));
}

#[test]
fn pairing_prefers_a_controller_that_can_reach_the_gun() {
    // One gun, two controllers. 10002 is the nearest by chebyshev — two cells
    // from the muzzle — but a ring of our own walls seals it in, so it could
    // never fire a shot. 10003 is twice as far and has a clear route. Distance
    // alone hands the gun to 10002 and the weapon stays silent all night,
    // which is the "炮塔全程无人操控" half of issues #12/#13/#14.
    let mut ours = vec![station(10, 24), gatling(10020, 10, 21)];
    for (index, (x, y)) in [
        (9, 18),
        (10, 18),
        (11, 18),
        (9, 19),
        (11, 19),
        (9, 20),
        (10, 20),
        (11, 20),
    ]
    .iter()
    .enumerate()
    {
        ours.push(wall(20000 + index as i64, *x, *y));
    }
    ours.push(role(10002, "worker", 10, 19)); // sealed in, 2 cells from the gun
    ours.push(role(10003, "worker", 10, 25)); // 4 cells away, route clear
    let turn = turn_from(world(ours));
    let state = BotState::default();

    let sealed = turn.role_by_id(10002).expect("controller exists");
    let stands = tower_stand_cells(&turn, Pos { x: 10, y: 21 });
    assert!(
        !can_reach_any(&turn, sealed, &stands),
        "the test board does not actually seal 10002 away from the gun"
    );

    let pairs = night::pairing(&turn, &state);
    assert_eq!(
        pairs,
        vec![(10003, 10020)],
        "gun 10020 went to a controller that cannot reach it"
    );
}
