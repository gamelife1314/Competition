//! Spawn-relative planning: a match is two halves and the sides swap, so every
//! decision has to hold from any base position the judger hands us.
//!
//! `station_footprint` grows up and to the right from `pos`, so the extreme
//! placements are (0,1) hugging the top-left and (39,31) hugging the bottom
//! right. There the radius-2 shell runs off the board: those cells need no wall
//! because the map edge itself is the seal, and a ring that quietly dropped
//! ON-board cells instead would leave the base open at exactly one corner.
//! So the invariant is set equality against the in-bounds shell — no missing
//! wall where a robot could walk, no phantom wall off the board.

use std::collections::HashSet;

use serde_json::{json, Value};

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
fn the_day_one_ring_is_exactly_the_on_board_shell() {
    for (name, base) in BASES {
        let turn = turn_from(world(base));
        let mut state = BotState::default();
        state.wall_gate_sealed = true;

        let expected: HashSet<Pos> = shell(base, 2)
            .into_iter()
            .filter(|cell| turn.in_bounds(*cell))
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
fn only_the_map_edge_may_leave_a_shell_cell_unwalled() {
    for (name, base) in BASES {
        let turn = turn_from(world(base));
        let mut state = BotState::default();
        state.wall_gate_sealed = true;

        let actual: HashSet<Pos> = wall_gaps(&turn, &state).into_iter().collect();
        for cell in shell(base, 2) {
            if actual.contains(&cell) {
                continue;
            }
            assert!(
                !turn.in_bounds(cell),
                "{name}: {cell:?} is walkable land with no wall in front of it"
            );
        }
    }
}

#[test]
fn the_gate_withholds_at_most_one_ring_cell_until_the_dusk_seal() {
    for (name, base) in BASES {
        let turn = turn_from(world(base));
        let mut open = BotState::default();
        open.wall_gate_sealed = false;
        let mut sealed = BotState::default();
        sealed.wall_gate_sealed = true;

        let before: HashSet<Pos> = wall_gaps(&turn, &open).into_iter().collect();
        let after: HashSet<Pos> = wall_gaps(&turn, &sealed).into_iter().collect();
        assert!(
            before.is_subset(&after),
            "{name}: sealing must not move the other walls"
        );
        assert!(
            after.len() - before.len() <= 1,
            "{name}: the seal may admit only the gate cell"
        );
        if base == (10, 24) {
            // Mid-map the gate is always a real ring cell, so the difference is
            // exactly the one cell the open ring withholds.
            assert_eq!(after.len(), before.len() + 1, "{name}: gate never opened");
        }
    }
}

#[test]
fn the_three_tower_sites_sit_on_the_inner_ring_from_every_base() {
    for (name, base) in BASES {
        let turn = turn_from(world(base));
        let footprint = footprint_of(base);
        let state = BotState::default();

        let gaps = tower_gaps(&turn, &state);
        let kinds: Vec<&str> = gaps.iter().map(|(_, kind)| kind.as_str()).collect();
        // Issue #206 §5 replaced the per-kind build order with a positional
        // line: slot i of the empty-slot list builds `TOWER_BUILD_ORDER[i]`, so
        // the kinds are the configured line, read off the config rather than
        // restated here. Reverting to per-kind `have[]` counting makes this red
        // (`gatling` reappears / repeats disappear).
        assert_eq!(
            kinds,
            coregeek::config::TOWER_BUILD_ORDER,
            "{name}: the tower line is not the configured one"
        );
        let unique: HashSet<Pos> = gaps.iter().map(|(pos, _)| *pos).collect();
        assert_eq!(
            unique.len(),
            kinds.len(),
            "{name} stacked two towers on one cell"
        );
        for (pos, _) in &gaps {
            assert!(
                turn.is_land(*pos),
                "{name} tower site off the board: {pos:?}"
            );
            assert_eq!(
                footprint_distance(*pos, &footprint),
                1,
                "{name} tower site is not hugging the station: {pos:?}"
            );
        }
    }
}

#[test]
fn a_standing_tower_does_not_consume_its_kind_from_the_line() {
    // The whole point of issue #206 §5's positional slots. Slots are counted
    // from the towers standing RIGHT NOW, so a rocket on the board frees the
    // head of the line rather than vetoing it: with one rocket up, the next
    // empty slot is slot 0 again and the day plans the head of the line plus
    // the next entry. Per-kind `have[]` counting — the behaviour this replaces
    // — skipped every kind it already had and could never repeat one, so it
    // returned just `["railgun"]` here and the owner's "2 missiles + 1 railgun"
    // was unreachable.
    let line = coregeek::config::TOWER_BUILD_ORDER;
    assert!(!line.is_empty(), "test setup: the configured line is empty");
    let base = (10, 24);
    let mut roles = vec![station(10001, base.0, base.1)];
    roles.push(json!({
        "id": 10020, "pos": {"x": 12, "y": 24}, "roleType": "rocket",
        "health": 1000, "attackPower": 20, "attackRange": 10,
        "level": 1, "backPackCapability": 0, "backpack": []
    }));
    roles.push(worker(10002, 20, 15));
    let mut payload = world(base);
    payload["teamOur"]["roles"] = Value::Array(roles);
    let turn = turn_from(payload);

    let kinds: Vec<String> = tower_gaps(&turn, &BotState::default())
        .into_iter()
        .map(|(_, kind)| kind)
        .collect();
    // One tower stands, so the empty-slot count is CAP - 1 and slot 0 is the
    // head of the line again; the line simply runs out one entry earlier.
    let expected: Vec<String> = line
        .iter()
        .take(coregeek::config::TOWER_CAP - 1)
        .map(|kind| kind.to_string())
        .collect();
    assert_eq!(
        kinds, expected,
        "the free slot did not take the head of the line: {kinds:?}"
    );
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
