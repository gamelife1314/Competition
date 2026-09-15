//! Issue #206 §6: the wall line is built toward the enemy.
//!
//! The ring has four edges and the crew walks it in one direction, so the order
//! is a defence decision before it is an economic one: robots come from the
//! enemy base, and a day cut short by dusk or by stone has to have its walls
//! between the base and the enemy. The direction used to be chosen by the
//! day's errands alone, which on a board whose stone sits away from the enemy
//! builds the far side first and leaves the enemy-facing side for whatever
//! rounds are left over.
//!
//! The board here is the canonical one from the task book (§4.1: the two bases
//! sit in opposite corners, and the sides swap between halves): our station in
//! one corner, the enemy station diagonally across the map, and the stone the
//! day needs on the side AWAY from the enemy — so the two keys disagree and the
//! test says which one wins.
//!
//! The entrance is deliberately left where the economy puts it. The plan's
//! second half is exactly that: the DOOR stays on the economic side, because a
//! door facing the enemy is a highway into the station, and it is the walk that
//! turns to face the threat.

use std::collections::HashMap;

use serde_json::{json, Value};

use coregeek::brain::route::{build_order, entrance, ring_cells};
use coregeek::model::{chebyshev, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

/// Our station, pressed into a corner the way the task book places it.
const BASE: (i32, i32) = (2, 3);
/// The enemy station, in the opposite corner.
const ENEMY: (i32, i32) = (38, 20);
/// The stone the ring owes, put on the far side of the base from the enemy.
const STONE: (i32, i32) = (0, 7);
/// The entrance the direction test starts from: on the ring's north edge, so
/// its two shoulders run east (toward the enemy) and west (away from it).
const ENTRANCE: (i32, i32) = (2, 5);

fn role(id: i64, kind: &str, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": kind,
        "health": if kind == "station" { 1500 } else { 100 },
        "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": if kind == "station" { 0 } else { 4 },
        "backpack": []
    })
}

/// The board: our corner, the enemy's corner, a stone vein away from the
/// enemy, and one worker so the errand list is not empty.
///
/// `enemy` is the ONLY thing that varies between the two boards — same crew,
/// same stone, same round. Anything else and the comparison would be measuring
/// the errand list instead of the bearing.
fn board(enemy: bool) -> Turn {
    let roles = vec![
        role(10001, "station", BASE.0, BASE.1),
        role(10002, "worker", 6, 6),
        role(10003, "pioneer", 7, 6),
    ];
    let mut enemy_roles = Vec::new();
    if enemy {
        enemy_roles.push(role(20001, "station", ENEMY.0, ENEMY.1));
    }
    let payload = json!({
        "roundNo": 5,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": STONE.0, "y": STONE.1}, "neutralType": "stone"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": 75, "totalScore": 0, "playerTasks": [],
            "roles": roles,
        },
        "teamEnemy": {"roles": enemy_roles},
        "robot": {"roles": []},
    });
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// The ring split into its four edges, each cell belonging to exactly one of
/// them (corners go to the north/south edge, which is where the walk meets
/// them first). Read off the shell rather than restated, so the test cannot
/// drift from the geometry the planner actually uses.
fn edges(turn: &Turn) -> HashMap<&'static str, Vec<Pos>> {
    let footprint = station_footprint(turn.station().expect("station").pos);
    let xs: Vec<i32> = footprint.iter().map(|cell| cell.x).collect();
    let ys: Vec<i32> = footprint.iter().map(|cell| cell.y).collect();
    let north = ys.iter().max().unwrap() + 2;
    let south = ys.iter().min().unwrap() - 2;
    let west = xs.iter().min().unwrap() - 2;
    let mut out: HashMap<&'static str, Vec<Pos>> = HashMap::new();
    for cell in ring_cells(turn, 2) {
        let side = if cell.y == north {
            "north"
        } else if cell.y == south {
            "south"
        } else if cell.x == west {
            "west"
        } else {
            "east"
        };
        out.entry(side).or_default().push(cell);
    }
    out
}

/// The edge whose cells sit furthest from the enemy on average — the one the
/// crew can afford to leave for last.
fn far_edge(turn: &Turn, edges: &HashMap<&'static str, Vec<Pos>>) -> &'static str {
    let enemy = turn.enemy_station().expect("enemy station").pos;
    let mean = |cells: &Vec<Pos>| -> i64 {
        cells.iter().map(|cell| chebyshev(*cell, enemy) as i64).sum::<i64>() / cells.len() as i64
    };
    let mut ranked: Vec<(&'static str, i64)> =
        edges.iter().map(|(name, cells)| (*name, mean(cells))).collect();
    ranked.sort_by_key(|(_, distance)| -*distance);
    assert!(
        ranked[0].1 > ranked[1].1,
        "test setup: the board does not separate a far edge ({ranked:?})"
    );
    ranked[0].0
}

#[test]
fn the_walk_turns_toward_the_enemy_even_though_the_stone_is_the_other_way() {
    // The whole of P0-3 in one board. The stone vein is west, so the errand key
    // — the only key there used to be — walks the ring the other way and lays
    // the west edge first. The enemy is east, so the surviving walls of a day
    // that ran out of rounds are the ones on the wrong side.
    let turn = board(true);
    let state = BotState::default();
    let entrance = Pos {
        x: ENTRANCE.0,
        y: ENTRANCE.1,
    };
    let order = build_order(&turn, &state, entrance);
    let edges = edges(&turn);
    let index: HashMap<Pos, usize> = order
        .iter()
        .enumerate()
        .map(|(index, cell)| (*cell, index))
        .collect();

    let far = far_edge(&turn, &edges);
    assert_eq!(far, "west", "test setup: the enemy is not east of the base");

    // The two shoulders of the entrance: one steps toward the enemy, one away.
    let enemy = turn.enemy_station().expect("enemy station").pos;
    let toward = Pos {
        x: entrance.x + 1,
        y: entrance.y,
    };
    let away = Pos {
        x: entrance.x - 1,
        y: entrance.y,
    };
    assert!(
        index[&toward] < index[&away],
        "the first step went {away:?} (index {}), away from the enemy at {enemy:?}, \
         instead of {toward:?} (index {})",
        index[&away],
        index[&toward]
    );

    // Issue #206 §6 in full: the three edges that face the enemy are up before
    // the far edge is started. The entrance's own edge is exempt — the walk is
    // built to END on the entrance's other shoulder, so its last cells are the
    // ring's last cells by construction. That exception is the only one.
    let entrance_edge = edges
        .iter()
        .find(|(_, cells)| cells.contains(&entrance))
        .map(|(name, _)| *name)
        .expect("the entrance is on the ring");
    let far_start = edges[far]
        .iter()
        .filter_map(|cell| index.get(cell))
        .min()
        .copied()
        .unwrap_or(0);
    for (name, cells) in &edges {
        if *name == far || *name == entrance_edge {
            continue;
        }
        let done = cells
            .iter()
            .filter_map(|cell| index.get(cell))
            .max()
            .copied()
            .unwrap_or(0);
        assert!(
            done < far_start,
            "the {name} edge (last cell at {done}) is still being built when the \
             {far} edge starts at {far_start}: the enemy side is not first"
        );
    }
}

#[test]
fn the_enemy_bearing_is_what_turns_the_walk() {
    // The same board with the enemy's roles removed. Nothing else changes, so
    // the only difference the order can show is the one the bearing makes — and
    // it has to show one, or the test above is passing on the errand key and
    // proves nothing. This is the red-on-revert evidence built into the suite.
    let with_enemy = board(true);
    let without = board(false);
    let state = BotState::default();
    let entrance = Pos {
        x: ENTRANCE.0,
        y: ENTRANCE.1,
    };
    let facing = build_order(&with_enemy, &state, entrance);
    let errands = build_order(&without, &state, entrance);
    assert_ne!(
        facing, errands,
        "the enemy bearing changed nothing: the walk is still the errand walk"
    );
    // And the enemy-free order is the one that goes the wrong way, which is
    // what makes the assertion above a fix rather than a shuffle.
    let edges = edges(&without);
    let far = far_edge(&with_enemy, &edges);
    let index: HashMap<Pos, usize> = errands
        .iter()
        .enumerate()
        .map(|(index, cell)| (*cell, index))
        .collect();
    let far_start = edges[far]
        .iter()
        .filter_map(|cell| index.get(cell))
        .min()
        .copied()
        .unwrap_or(0);
    let east_done = edges["east"]
        .iter()
        .filter_map(|cell| index.get(cell))
        .max()
        .copied()
        .unwrap_or(0);
    assert!(
        east_done > far_start,
        "test setup: the errand-only walk already builds the {far} edge last \
         (far starts at {far_start}, east ends at {east_done}), so this board \
         cannot tell the two keys apart"
    );
}

#[test]
fn the_door_stays_on_the_economic_side() {
    // P0-3's other half, and this half is a LOCK rather than a fix: the bearing
    // turns the walk, and it must not drag the door with it. A door on the
    // enemy side is a highway into the station, and the entrance is the one
    // cell the ring deliberately leaves open all afternoon.
    //
    // Stated as the two things the plan asks for. First, the bearing does not
    // move the door at all: the same board with and without an enemy station
    // gives the same entrance. Second, the door is on the side the day's work
    // is on — nearer the stone the ring owes than any cell of the edge facing
    // the enemy.
    let state = BotState::default();
    let turn = board(true);
    let door = entrance(&turn, &state, 1).expect("a board with errands has a door");
    let bare = entrance(&board(false), &state, 1).expect("the same board, no enemy");
    assert_eq!(
        door, bare,
        "the enemy bearing moved the door from {bare:?} to {door:?}"
    );

    let stone = Pos {
        x: STONE.0,
        y: STONE.1,
    };
    for cell in &edges(&turn)["east"] {
        assert!(
            chebyshev(door, stone) < chebyshev(*cell, stone),
            "the door at {door:?} is further from the stone at {stone:?} than \
             {cell:?}, a cell of the edge facing the enemy: the entrance stopped \
             being the economic one"
        );
    }
}
