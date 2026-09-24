//! Spawn-relative planning: a match is two halves and the sides swap, so every
//! decision has to hold from any base position the judger hands us.
//!
//! `station_footprint` grows up and to the right from `pos`, so the extreme
//! placements are (0,1) hugging the top-left and (39,31) hugging the bottom
//! right. There the radius-2 shell runs off the board: those cells need no wall
//! because the map edge itself is the seal, and a ring that quietly dropped
//! ON-board cells instead would leave the base open at exactly one corner.
//! Since issue #221 phase 4b the invariant is set equality against the
//! in-bounds shell MINUS the four permanent entrance cells (comment 1 §4) —
//! no missing wall where a robot could walk, no phantom wall off the board,
//! and never a wall across the crew's own door.

use std::collections::HashSet;

use serde_json::{json, Value};

use coregeek::brain::action::base_layout;
use coregeek::brain::day::{plan as day_plan, tower_gaps, wall_gaps};
use coregeek::model::{footprint_distance, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn station(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "station",
        "health": 1500, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn worker(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "worker",
        "health": 100, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 4, "backpack": []
    })
}

fn pioneer(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "pioneer",
        "health": 100, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 4, "backpack": []
    })
}

/// Day 1, first round. The roles sit mid-map so they occupy no ring cell.
fn world(station_pos: (i32, i32)) -> Value {
    json!({
        "roundNo": 1,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 25, "totalScore": 0, "playerTasks": [],
            "roles": [
                station(10001, station_pos.0, station_pos.1),
                worker(10002, 20, 15),
                worker(10003, 21, 15),
                pioneer(10004, 22, 15),
            ]
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    })
}

/// Top-left, bottom-right and mid-map: the two placements where the ring runs
/// into the board edge, and one where it does not.
const BASES: [(&str, (i32, i32)); 3] = [
    ("top_left", (0, 1)),
    ("bottom_right", (39, 31)),
    ("mid_map", (10, 24)),
];

fn footprint_of(base: (i32, i32)) -> Vec<Pos> {
    station_footprint(Pos {
        x: base.0,
        y: base.1,
    })
}

/// Every cell of the radius-`radius` shell around the station footprint, on or
/// off the board — the ring `wall_gaps` has to account for completely.
fn shell(station_pos: (i32, i32), radius: i32) -> Vec<Pos> {
    let footprint = footprint_of(station_pos);
    let xs: Vec<i32> = footprint.iter().map(|cell| cell.x).collect();
    let ys: Vec<i32> = footprint.iter().map(|cell| cell.y).collect();
    let mut cells = Vec::new();
    for x in xs.iter().min().unwrap() - radius..=xs.iter().max().unwrap() + radius {
        for y in ys.iter().min().unwrap() - radius..=ys.iter().max().unwrap() + radius {
            let pos = Pos { x, y };
            if !footprint.contains(&pos) && footprint_distance(pos, &footprint) == radius {
                cells.push(pos);
            }
        }
    }
    cells
}

#[test]
fn the_day_one_ring_is_the_shell_minus_the_permanent_entrance() {
    for (name, base) in BASES {
        let turn = turn_from(world(base));
        let state = BotState::default();

        let entrance: HashSet<Pos> = base_layout::entrance_cells(&turn)
            .into_iter()
            .filter(|cell| turn.in_bounds(*cell))
            .collect();
        let expected: HashSet<Pos> = shell(base, 2)
            .into_iter()
            .filter(|cell| turn.in_bounds(*cell))
            .filter(|cell| !entrance.contains(cell))
            .collect();
        let ring = wall_gaps(&turn, &state);
        let actual: HashSet<Pos> = ring.iter().copied().collect();

        assert_eq!(
            actual,
            expected,
            "{name} at {base:?}: missing {:?}, phantom {:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        );
        assert_eq!(ring.len(), actual.len(), "{name} repeated a ring cell");
        // …and in comment 1 §4's fixed one-direction order, not merely as a
        // set: front column, top row, bottom row.
        let order: Vec<Pos> = base_layout::wall_build_order(&turn)
            .into_iter()
            .filter(|cell| turn.is_land(*cell))
            .collect();
        assert_eq!(ring, order, "{name}: the gap list is not the fixed order");
        let footprint = footprint_of(base);
        for cell in &ring {
            assert_eq!(
                footprint_distance(*cell, &footprint),
                2,
                "{name}: {cell:?} is not on the shell"
            );
        }
    }
}

#[test]
fn only_the_map_edge_or_the_entrance_may_leave_a_shell_cell_unwalled() {
    for (name, base) in BASES {
        let turn = turn_from(world(base));
        let state = BotState::default();
        let entrance: HashSet<Pos> = base_layout::entrance_cells(&turn).into_iter().collect();

        let actual: HashSet<Pos> = wall_gaps(&turn, &state).into_iter().collect();
        for cell in shell(base, 2) {
            if actual.contains(&cell) || entrance.contains(&cell) {
                continue;
            }
            assert!(
                !turn.in_bounds(cell),
                "{name}: {cell:?} is walkable land with no wall in front of it"
            );
        }
    }
}

/// Design D17 in one assert: the back four cells are the crew's permanent way
/// out — never offered to the wall line on any day, sealed or not, so no gate
/// latch, dusk seal or morning door-cut can ever wall the economy in (issue
/// #14's structural answer).
#[test]
fn the_permanent_entrance_is_never_offered() {
    for (name, base) in BASES {
        let turn = turn_from(world(base));
        let state = BotState::default();
        let entrance = base_layout::entrance_cells(&turn);
        let gaps: HashSet<Pos> = wall_gaps(&turn, &state).into_iter().collect();
        assert!(
            entrance.iter().all(|cell| !gaps.contains(cell)),
            "{name}: the entrance must never be a wall gap"
        );
        if base == (10, 24) {
            // Mid-map nothing runs off the board: 20 shell cells = 16 walls
            // + 4 entrance, exactly.
            assert_eq!(gaps.len(), 16, "{name}: the ring is not 16 cells");
            assert_eq!(
                entrance,
                vec![
                    Pos { x: 8, y: 22 },
                    Pos { x: 8, y: 23 },
                    Pos { x: 8, y: 24 },
                    Pos { x: 8, y: 25 }
                ],
                "{name}: the entrance is not the back column"
            );
        }
    }
}

#[test]
fn the_tower_sites_are_the_l_cells_that_fit_on_the_board() {
    for (name, base) in BASES {
        let turn = turn_from(world(base));
        let footprint = footprint_of(base);
        let state = BotState::default();

        let gaps = tower_gaps(&turn, &state);
        // Comment 1 §1: the sites are the FIXED L cells around the operator
        // stand — every one of them that is actually on the board, in layout
        // order. A base pressed so far into a corner that the L runs off the
        // map simply has fewer (here: zero) sites; nothing is invented.
        let expected: Vec<Pos> = base_layout::weapon_sites(&turn)
            .into_iter()
            .filter(|pos| turn.is_land(*pos))
            .collect();
        let actual: Vec<Pos> = gaps.iter().map(|(pos, _)| *pos).collect();
        assert_eq!(actual, expected, "{name}: the sites are not the L");
        // Issue #206 §5's positional line, read off the config: slot i builds
        // `TOWER_BUILD_ORDER[i]` (ABSOLUTE indexing — the pk616181/pk616182
        // second-rocket bug is what this pins).
        let kinds: Vec<&str> = gaps.iter().map(|(_, kind)| kind.as_str()).collect();
        let expected_kinds: Vec<&str> = coregeek::config::TOWER_BUILD_ORDER
            .iter()
            .take(kinds.len())
            .map(|kind| kind.as_str())
            .collect();
        assert_eq!(
            kinds, expected_kinds,
            "{name}: the tower line is not the configured one"
        );
        if base == (10, 24) {
            assert_eq!(
                actual,
                vec![
                    Pos { x: 9, y: 22 },
                    Pos { x: 10, y: 22 },
                    Pos { x: 9, y: 24 }
                ],
                "{name}: the L does not sit where the spec puts it"
            );
        }
        for (pos, _) in &gaps {
            assert_eq!(
                footprint_distance(*pos, &footprint),
                1,
                "{name} tower site is not hugging the station: {pos:?}"
            );
        }
    }
}

#[test]
fn a_full_day_of_planning_runs_from_every_base() {
    for (name, base) in BASES {
        let turn = turn_from(world(base));
        let mut state = BotState::default();
        let plan = day_plan(&turn, &mut state);
        // Nothing here asserts a specific opening move — the point is that the
        // whole day planner, wall order and economy included, runs with the
        // base pressed into either board edge.
        assert!(
            plan.commands.len() <= turn.ours.len(),
            "{name}: at most one command per role"
        );
    }
}
