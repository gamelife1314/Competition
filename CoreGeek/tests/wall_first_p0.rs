//! P0-3/P0-4 (docs/FAILURE-ANALYSIS-2026-09-14.md §3.3, §3.4): the ring is the
//! day's first product, and nothing walks away from it while it is still open.
//!
//! * P0-4 — `worker_day`'s tower step fires the moment 25 gold is in hand, ring
//!   or no ring. With P0-2 removing the upgrade-voucher reserve that used to
//!   hold the third gun back, that put the rocket up at R3 — before a single
//!   wall — and the day's walking budget was spent shuttling between the tower
//!   pads and the wall line (`day1_sim`: 3 towers, 0 walls, every wall-first
//!   test red). Day 1's 2nd/3rd tower now waits for the ring; the first gun is
//!   exempt, because the ring needs stone and stone needs mining.
//! * P0-3 — a role with no gun to man has the INSIDE of the ring as its post,
//!   and the demolition escape hatch only fires for a wall whose removal
//!   actually reopens the way. A wall demolished for a route it does not open
//!   is a hole the night walks in through, bought for nothing.

use std::collections::HashSet;

use serde_json::{json, Value};

use coregeek::brain::day::{plan, tower_gaps};
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

fn tower(id: i64, kind: &str, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": kind,
        "health": 1000, "attackPower": 10, "attackRange": 3,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn worker(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 100, "backpack": []
    })
}

fn wall(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "wall",
        "health": 1000, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

/// Every on-board cell exactly two rings out from the station footprint — the
/// radius-2 shell `wall_gaps` wants filled on day 1.
fn ring_two(base: (i32, i32)) -> Vec<Pos> {
    let footprint = station_footprint(Pos {
        x: base.0,
        y: base.1,
    });
    let mut cells = Vec::new();
    for x in base.0 - 4..=base.0 + 5 {
        for y in base.1 - 4..=base.1 + 5 {
            let pos = Pos { x, y };
            if pos.x < 0 || pos.y < 0 || pos.x >= 41 || pos.y >= 32 {
                continue;
            }
            if footprint_distance(pos, &footprint) == 2 {
                cells.push(pos);
            }
        }
    }
    cells.sort_by_key(|pos| (pos.x, pos.y));
    cells
}

/// The complete day-1 ring, minus `holes`.
fn ring_walls(base: (i32, i32), holes: &[Pos]) -> Vec<Value> {
    ring_two(base)
        .into_iter()
        .filter(|pos| !holes.contains(pos))
        .enumerate()
        .map(|(index, pos)| wall(20000 + index as i64, pos.x, pos.y))
        .collect()
}

/// The station's gate cell: the one ring cell the wall crew leaves open until
/// the dusk seal (`wall_gate`). A shell with only this cell missing is a closed
/// ring as far as the day is concerned — `wall_gaps` filters it out — which is
/// exactly the "19/20 and the gate" state day 1 finishes on.
fn gate_cell(base: (i32, i32)) -> Pos {
    Pos {
        x: base.0 + 3,
        y: base.1 - 2,
    }
}

/// The complete day-1 shell but for its gate.
fn closed_ring(base: (i32, i32)) -> Vec<Value> {
    ring_walls(base, &[gate_cell(base)])
}

fn board(roles: Vec<Value>, gold: i64, round_no: i64) -> Value {
    json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": gold, "totalScore": 0,
            "playerTasks": [], "roles": roles
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    })
}

/// A free cell beside the first tower the plan wants to lay.
fn beside_first_gap(turn: &Turn) -> Pos {
    let state = BotState::default();
    let (site, _) = *tower_gaps(turn, &state)
        .first()
        .expect("the board still has a tower slot");
    let occupied: HashSet<Pos> = turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    coregeek::brain::stand_cells(&turn, site)
        .into_iter()
        .find(|pos| !occupied.contains(pos))
        .expect("a tower site always has a stand cell")
}

fn occupied_cells(turn: &Turn) -> HashSet<Pos> {
    turn.ours.iter().flat_map(|unit| unit.footprint()).collect()
}

/// Every tower the plan is trying to put up this round.
fn tower_builds(turn: &Turn, state: &mut BotState) -> Vec<String> {
    plan(turn, state)
        .commands
        .values()
        .filter(|cmd| cmd.action == "build")
        .filter_map(|cmd| cmd.name.clone())
        .filter(|name| matches!(name.as_str(), "gatling" | "railgun" | "rocket"))
        .collect()
}

/// What a weapon may do this round, with a builder standing beside the pad.
///
/// Finding that cell is an iteration, not a lookup. Which ring cell is accepted
/// as a build site depends on who is standing where — a body in the corridor
/// changes a site's operable-cell count — so placing the builder can move the
/// site out from under it, and a board with the shell complete has no free cell
/// a builder can occupy that is not itself part of the corridor. Standing
/// beside the site the plan just named settles in a round or two, and if it
/// does not, the test fails loudly rather than testing nothing.
fn build_with_builder_at(roles: Vec<Value>, gold: i64, round_no: i64) -> Vec<String> {
    let mut roles = roles;
    let mut pad = beside_first_gap(&turn_from(board(roles.clone(), gold, round_no)));
    for _ in 0..6 {
        let mut with = roles.clone();
        with.push(worker(10010, pad.x, pad.y));
        let turn = turn_from(board(with, gold, round_no));
        let Some(site) = tower_gaps(&turn, &BotState::default())
            .first()
            .map(|(site, _)| *site)
        else {
            break;
        };
        if coregeek::model::chebyshev(pad, site) == 1 {
            break;
        }
        let Some(next) = coregeek::brain::stand_cells(&turn, site)
            .into_iter()
            .find(|pos| !occupied_cells(&turn).contains(pos))
        else {
            break;
        };
        pad = next;
    }
    roles.push(worker(10010, pad.x, pad.y));
    let turn = turn_from(board(roles, gold, round_no));
    tower_builds(&turn, &mut BotState::default())
}

/// The same, on the day-1 opener.
fn builds_with_builder(roles: Vec<Value>, gold: i64) -> Vec<String> {
    build_with_builder_at(roles, gold, 5)
}

#[test]
fn the_first_weapon_is_exempt_while_the_ring_is_still_open() {
    // No gun at all, 25 gold, an untouched day-1 ring. The exemption is not a
    // courtesy: the ring is built from stone, the stone comes from a mine the
    // crew has to be able to hold, and a base with no gun is a base that loses
    // the first night outright.
    assert_eq!(
        builds_with_builder(vec![station(10, 20)], 25),
        vec!["gatling"],
        "the first weapon was held back for a ring that is not built yet"
    );
}

#[test]
fn the_second_weapon_goes_up_on_day_one_even_with_an_open_ring() {
    // Offense-first: Day 1 builds up to 2 towers immediately, regardless of
    // the wall ring. Two towers mean two workers have weapons at night — a
    // worker with no tower is dead weight during the assault. The third tower
    // still waits for the ring to be mostly up.
    assert_eq!(
        builds_with_builder(vec![station(10, 20), tower(10020, "gatling", 10, 18)], 25),
        vec!["railgun"],
        "the second weapon goes up on day 1 even with an open ring"
    );
}

#[test]
fn the_ring_being_closed_releases_the_second_weapon() {
    // The same board with the shell complete: nothing is holding the gun back
    // any more, and 25 gold in the purse is a gun's price.
    let mut roles = vec![station(10, 20), tower(10020, "gatling", 10, 18)];
    roles.extend(closed_ring((10, 20)));
    assert_eq!(
        builds_with_builder(roles, 25),
        vec!["railgun"],
        "a closed ring did not release the next weapon"
    );
}

#[test]
fn the_third_weapon_goes_up_on_day_one_once_the_ring_stands() {
    // P0-2 + P0-4 together, which is the pairing the failure analysis asks for:
    // the ring first, and then all three guns on the SAME day — 25 gold is the
    // third slot's price and no longer a reserve held for the 100-gold voucher.
    // The two existing guns sit on opposite sides of the station's corridor, so
    // the remaining pad is on the same arc as the builder — a pair of towers
    // either side of it would wall the pad off from the crew, which is a
    // question about `tower_gaps` and not about this rule.
    let mut roles = vec![
        station(10, 20),
        tower(10020, "gatling", 9, 18),
        tower(10030, "railgun", 12, 21),
    ];
    roles.extend(closed_ring((10, 20)));
    assert_eq!(
        builds_with_builder(roles, 25),
        vec!["rocket"],
        "the third weapon did not go up on day 1 with the ring closed"
    );
}

#[test]
fn the_second_weapon_does_not_wait_for_the_last_ring_cell() {
    // Offense-first: the second tower goes up on day 1 even with one open ring
    // cell. The old rule held the second gun back for a single cell — but that
    // meant 1 tower and 20 walls on day 1, and the worker with no tower was
    // dead weight during the night assault. The third tower still waits for
    // the ring to be mostly up.
    let base = (10, 20);
    let hole = ring_two(base)[0];
    let mut roles = vec![station(base.0, base.1), tower(10020, "gatling", 10, 18)];
    roles.extend(ring_walls(base, &[hole]));
    assert_eq!(
        builds_with_builder(roles, 25),
        vec!["railgun"],
        "the second weapon does not wait for the last ring cell"
    );
}

#[test]
fn a_later_day_never_waits_for_the_ring() {
    // P0-4 is a day-1 rule. From day 2 the shell is standing (or being
    // repaired), and a gun held back for a repair budget is a gun the coming
    // night does not have.
    let base = (10, 24);
    let hole = ring_two(base)[0];
    let mut roles = vec![station(base.0, base.1), tower(10020, "gatling", 10, 22)];
    roles.extend(ring_walls(base, &[hole]));
    // Round 135 is day 2, in-day round 4 — nowhere near the dusk lock-in, and
    // with a ring cell deliberately missing.
    assert_eq!(
        build_with_builder_at(roles, 25, 135),
        vec!["railgun"],
        "day 2 held a weapon back for a wall gap"
    );
}

/// The other half of P0-3: a role our own ring has sealed out may demolish one
/// of our walls to get home — but only a wall whose removal actually reopens
/// the way. Without that condition the day-2 simulation demolished two ring
/// cells, opened nothing (each led to a gun, its operator, or another teammate)
/// and ended the day three cells short with the gate still waiting on the role
/// outside.
#[test]
fn a_demolition_that_opens_nothing_is_not_worth_a_wall() {
    let base = (10, 24);
    let mut roles = vec![station(base.0, base.1), tower(10020, "gatling", 10, 22)];
    roles.extend(ring_walls(base, &[]));
    // The ring is complete, so the worker at (14,23) is outside a closed box —
    // and every cell its adjacent walls open on to is taken: the towers and the
    // teammates fill the whole x=12 column it would have to enter through.
    roles.push(worker(10010, 14, 23));
    for (index, y) in [22, 23, 24, 25].iter().enumerate() {
        roles.push(worker(10011 + index as i64, 12, *y));
    }
    let turn = turn_from(board(roles, 0, 65));

    let outer = turn.role_by_id(10010).expect("the sealed-out role exists");
    let home = coregeek::brain::interior_cells(&turn);
    assert!(!home.is_empty(), "test setup: the base has an inside");
    assert!(
        !coregeek::brain::can_reach_any(&turn, outer, &home),
        "test setup: the ring really has this role sealed out"
    );
    let mut claimed = HashSet::new();
    assert_eq!(
        coregeek::brain::walk_or_remove_wall(&turn, outer, &home, &mut claimed).map(|cmd| cmd.action),
        None,
        "a wall was demolished for a route it does not open"
    );
}

/// The other side of the same rule: when a demolition WOULD reopen the way, the
/// sealed-out role takes it. (This is the escape hatch the night recall has
/// always had, now shared by the daytime retreat.)
#[test]
fn a_sealed_out_role_demolishes_the_wall_that_lets_it_home() {
    let base = (10, 24);
    let mut roles = vec![station(base.0, base.1), tower(10020, "gatling", 10, 22)];
    roles.extend(ring_walls(base, &[]));
    // One teammate in the doorway cell, the rest of the inner column free: the
    // wall at (13,24) opens on to (12,25), so it is worth a ring cell.
    roles.push(worker(10010, 14, 23));
    roles.push(worker(10011, 12, 23));
    let turn = turn_from(board(roles, 0, 65));

    let outer = turn.role_by_id(10010).expect("the sealed-out role exists");
    let home = coregeek::brain::interior_cells(&turn);
    let mut claimed = HashSet::new();
    let cmd = coregeek::brain::walk_or_remove_wall(&turn, outer, &home, &mut claimed)
        .expect("a sealed-out role must be able to cut its way home");
    assert_eq!(cmd.action, "remove", "the escape hatch is a demolition");
    let target = cmd.targetPos.as_ref().and_then(|list| list.first()).copied();
    let target = target.expect("a demolition names its wall");
    assert_eq!(
        coregeek::model::chebyshev(outer.pos, target),
        1,
        "a wall can only be demolished from beside it"
    );
}

/// A sale may not strip the ring of the stone it still owes.
///
/// `sellable_ores` has always defined the sellable part of a pack as the
/// surplus over `stone_demand + STONE_BUFFER`, and `should_sell` decides
/// "there is something to sell" on that basis — but `sell_command` then handed
/// the vendor the WHOLE stack. On the day whose only stone demand is a gap
/// (day 2, one door) that is the whole wall budget: the crew mines stone all
/// day, the dusk cash-out sells it, and the door a role cut that morning is
/// still open at nightfall because no carrier has one left to close it with.
#[test]
fn a_sale_keeps_the_stone_the_ring_still_owes() {
    let base = (10, 24);
    let mut roles = vec![station(base.0, base.1)];
    roles.extend(closed_ring(base));
    roles.push(json!({
        "id": 10010, "pos": {"x": 20, "y": 20}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 100,
        "backpack": vec!["stone"; 11]
    }));
    let payload = {
        let mut board = board(roles, 0, 135);
        board["vendorShopList"] = json!([{"name": "stone", "price": 2}]);
        board["mapInfo"]["zones"] = json!([{"pos": {"x": 30, "y": 20}, "neutralType": "vendor"}]);
        board
    };
    let turn = turn_from(payload);
    let state = BotState::default();
    let role = turn.role_by_id(10010).expect("the carrier exists");

    // One cell of ring outstanding: keep it, plus the two-stone repair buffer.
    let sale = coregeek::brain::economy::sell_command(&turn, &state, role, 1)
        .expect("the surplus stone is sellable");
    assert_eq!(sale.name.as_deref(), Some("stone"));
    assert_eq!(
        sale.num,
        Some(8),
        "the sale took the stone the ring still owes (11 in pack, 1 owed, 2 buffer)"
    );

    // A ring that wants more stone than the team is carrying sells none of it.
    assert!(
        coregeek::brain::economy::sell_command(&turn, &state, role, 20).is_none(),
        "stone the wall line still needs was sold anyway"
    );

    // With nothing owed the buffer is still the floor: the two stones a repair
    // kit needs are not income.
    let free = coregeek::brain::economy::sell_command(&turn, &state, role, 0)
        .expect("surplus stone with no demand is sellable");
    assert_eq!(free.num, Some(9));
}
