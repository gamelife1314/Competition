//! Issue #16: the ring is rebuilt, but never repaired.
//!
//! The match was won 3:0 and still ended with the base at 35 HP and two roles
//! dead. The report names the debt exactly: "D1 夜间城墙池 16000→12860 …，城墙
//! 出现缺口。D2 白天虽补建 4 墙（数 16→19），但城墙 HP 未修复（wall_fixer 未使用
//! 或无此道具），缺口处的低 HP 城墙在 D2 夜间被怪…", and points at the cause:
//! "D2 白天补墙仅靠采石，无法买 WallFixer 修复城墙 HP（D2 夜间城墙缺口无法修复的
//! 直接原因）". A wall has 1000/1500/2000 HP; a `WallFixer` costs 10 gold and
//! restores its target to FULL — the cheapest HP in the game. We never used one:
//! the repair only fired for a role already standing beside a damaged wall once
//! every other errand was done, and the mining step returns long before that, so
//! a worker with stone to dig never walked to the wall it should be mending.
//!
//! The first two tests guard properties this issue did not change — the wall
//! line already blocks movement, and a sealed ring already gets a door cut. They
//! are here because the repair policy below is only worth anything while the
//! ring is watertight and the roles inside it can still get out.

use serde_json::{json, Value};

use coregeek::brain::day::{plan as day_plan, repair_target};
use coregeek::model::{chebyshev, footprint_distance, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

const STATION: (i32, i32) = (10, 20);
/// `roundNo` for in-day round `in_day` of day `day`.
fn round_of(day: i64, in_day: i64) -> i64 {
    (day - 1) * 130 + in_day + 1
}

fn station() -> Value {
    json!({
        "id": 10001, "pos": {"x": STATION.0, "y": STATION.1}, "roleType": "station",
        "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn worker(id: i64, x: i32, y: i32, items: Vec<&str>) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 100, "backpack": items
    })
}

fn wall(id: i64, pos: Pos, level: i64, health: i64) -> Value {
    json!({
        "id": id, "pos": {"x": pos.x, "y": pos.y}, "roleType": "wall",
        "health": health, "attackPower": 0, "attackRange": 0,
        "level": level, "backPackCapability": 0, "backpack": []
    })
}

/// Every cell exactly two rings out from the footprint: the wall line.
fn ring_cells() -> Vec<Pos> {
    let footprint = station_footprint(Pos {
        x: STATION.0,
        y: STATION.1,
    });
    let mut cells = Vec::new();
    for x in 0..41 {
        for y in 0..32 {
            let pos = Pos { x, y };
            if !footprint.contains(&pos) && footprint_distance(pos, &footprint) == 2 {
                cells.push(pos);
            }
        }
    }
    cells
}

/// A day board. The weapon shop sits far away so the buyer never sets off
/// during these tests and the repair step is what the plan reaches. The stone
/// vein sits far away in the opposite direction: mining is the errand that used
/// to swallow the repair, and a vein on the wall's side of the board would let
/// a worker mining it drift toward the wall and hide the difference.
fn world(round_no: i64, roles: Vec<Value>, gold: i64) -> Value {
    json!({
        "roundNo": round_no,
        "mapInfo": {
            "width": 41, "height": 32,
            "zones": [
                {"pos": {"x": 38, "y": 3}, "neutralType": "weaponShop"},
                {"pos": {"x": 2, "y": 3}, "neutralType": "stone"},
                {"pos": {"x": 5, "y": 28}, "neutralType": "vendor"},
            ],
        },
        "teamOur": {
            "type": "challenger", "goldNum": gold, "totalScore": 0,
            "playerTasks": [], "roles": roles,
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "weaponShopList": [{"name": "WallFixer", "price": 10}],
    })
}

#[test]
fn the_wall_line_blocks_movement() {
    // 任务书: "以下元素均会阻挡角色移动：己方/敌方建筑（基地、武器工事、围墙）…".
    // The planner gets this right today — a wall is an alive unit and every
    // alive unit's cell is in the blocked set (tests/day1_sim.rs models the
    // same rule). It is asserted here rather than assumed because the breach
    // policy below only means anything while the ring is watertight: if a wall
    // ever became walkable, the ring would be decoration.
    let cell = ring_cells()[0];
    let turn = turn_from(world(
        round_of(2, 5),
        vec![
            station(),
            wall(20001, cell, 1, 1000),
            worker(10010, 12, 21, vec![]),
        ],
        0,
    ));
    assert!(
        turn.blocked_for(10010).contains(&cell),
        "a wall is a building: the cell is not walkable"
    );
    assert!(
        turn.is_land(cell),
        "…but it is still land, so a wall may stand on it"
    );
}

#[test]
fn a_sealed_ring_makes_the_economy_cut_a_door() {
    // The whole point of the ring: with it closed, nothing inside can reach a
    // mine, a vendor or the front line. `open_door` is the answer — it cuts a
    // hole in the wall the round the ring leaves no other way out — and it is
    // quietly coupled to the wall line being solid, since "is there a route
    // out?" is answered by the same blocked set the movement rule uses. This
    // pins the coupling: cut a door, in an adjacent wall cell, instead of
    // standing still.
    let mut roles = vec![station(), worker(10010, 10, 21, vec![])];
    for (index, cell) in ring_cells().into_iter().enumerate() {
        roles.push(wall(20000 + index as i64, cell, 1, 1000));
    }
    let turn = turn_from(world(round_of(2, 5), roles, 0));
    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);

    let cmd = plan
        .commands
        .get(&10010)
        .expect("the worker does something");
    assert_eq!(
        cmd.action, "remove",
        "a role sealed inside the ring cuts a door; got {:?}",
        cmd.action
    );
    let target = cmd.targetPos.as_ref().expect("remove has a target")[0];
    assert_eq!(
        chebyshev(Pos { x: 10, y: 21 }, target),
        1,
        "the door is cut in an adjacent cell, got {target:?}"
    );
    assert!(
        ring_cells().contains(&target),
        "{target:?} is not on the wall line"
    );
}

#[test]
fn a_worker_beside_a_damaged_wall_mends_it() {
    let damaged = Pos { x: 12, y: 22 };
    let turn = turn_from(world(
        round_of(1, 5),
        vec![
            station(),
            wall(20001, damaged, 1, 300),
            worker(10010, 12, 21, vec!["WallFixer"]),
        ],
        0,
    ));
    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);

    let cmd = plan.commands.get(&10010).expect("the worker acts");
    assert_eq!(cmd.action, "use");
    assert_eq!(cmd.name.as_deref(), Some("WallFixer"));
    assert_eq!(
        cmd.targetPos.as_ref().expect("use has a target")[0],
        damaged,
        "the kit goes on the wall that lost its HP"
    );
}

#[test]
fn a_carried_wall_fixer_is_an_errand_not_a_coincidence() {
    // The heart of issue #16: the kit was in the pack and the wall was damaged,
    // but mining returns before the repair step, so a worker with stone to dig
    // never walked to the wall it should be mending. Repair has to be a
    // destination, not something that happens to a role standing in the right
    // place when the day runs out.
    let damaged = Pos { x: 12, y: 22 };
    let turn = turn_from(world(
        round_of(1, 5),
        vec![
            station(),
            wall(20001, damaged, 1, 300),
            worker(10010, 9, 21, vec!["WallFixer"]),
        ],
        0,
    ));
    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);

    let cmd = plan.commands.get(&10010).expect("the worker acts");
    assert_eq!(
        cmd.action, "move",
        "the worker walks to the wall instead of off to a mine"
    );
    let step = cmd.targetPos.as_ref().expect("move has a target")[0];
    let before = (9 - damaged.x).abs().max((21 - damaged.y).abs());
    let after = (step.x - damaged.x).abs().max((step.y - damaged.y).abs());
    assert!(
        after < before,
        "the step {step:?} does not close on the damaged wall ({before} → {after})"
    );
    // The stone vein is on the far side of the board: a worker that went
    // mining instead would be walking away from the wall, not toward it.
    let vein = Pos { x: 2, y: 3 };
    assert!(
        chebyshev(step, vein) >= chebyshev(Pos { x: 9, y: 21 }, vein),
        "the step {step:?} is a mining step, not a repair errand"
    );
}

#[test]
fn the_economy_worker_sells_before_it_mends() {
    // Two workers, and the higher id is the dedicated economy worker — the same
    // rule `plan` uses to pick the buyer. That role is the collect→sell→buy
    // loop, and it is also the role that walks to the shop and therefore ends up
    // holding the kits; the repair has to yield to the sale rather than replace
    // it, or the fix for issue #16 would spend the very loop that pays for it.
    // The other worker takes the errand this round.
    let damaged = Pos { x: 12, y: 22 };
    let vendor = Pos { x: 5, y: 28 };
    let turn = turn_from(world(
        round_of(2, 5),
        vec![
            station(),
            wall(20001, damaged, 1, 300),
            worker(10010, 9, 21, vec!["WallFixer"]),
            worker(
                10011,
                8,
                21,
                vec!["WallFixer", "iron", "iron", "iron", "iron"],
            ),
        ],
        0,
    ));
    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);

    let mend = plan.commands.get(&10010).expect("the free worker acts");
    assert_eq!(mend.action, "move");
    let step = mend.targetPos.as_ref().expect("move has a target")[0];
    assert!(
        chebyshev(step, damaged) < chebyshev(Pos { x: 9, y: 21 }, damaged),
        "the free worker walks to the damaged wall, got {step:?}"
    );

    let economy = plan.commands.get(&10011).expect("the economy worker acts");
    let step = economy
        .targetPos
        .as_ref()
        .expect("its command has a target")[0];
    assert!(
        chebyshev(step, damaged) > chebyshev(Pos { x: 8, y: 21 }, damaged),
        "the economy worker must not be pulled off its load toward the wall, \
         got {step:?}"
    );
    assert!(
        chebyshev(step, vendor) < chebyshev(Pos { x: 8, y: 21 }, vendor),
        "…it is walking its load to the vendor instead, got {step:?}"
    );
}

#[test]
fn the_wall_beside_an_opening_is_mended_first() {
    // "缺口处的低 HP 城墙在 D2 夜间被怪…": the wall holding the edge of a breach
    // is the one the next wave comes through. Here the nearest damaged wall is
    // NOT the one beside the opening, so a plain nearest-first rule mends the
    // wrong one.
    let opening = Pos { x: 12, y: 17 };
    let beside = Pos { x: 13, y: 17 }; // one cell from the opening
    let far = Pos { x: 8, y: 20 }; // four cells away from it
    let mut roles = vec![station(), worker(10010, 8, 24, vec!["WallFixer"])];
    for (index, cell) in ring_cells().into_iter().enumerate() {
        if cell == opening {
            continue; // the breach itself
        }
        let health = if cell == beside || cell == far {
            300
        } else {
            1000
        };
        roles.push(wall(20000 + index as i64, cell, 1, health));
    }
    let turn = turn_from(world(round_of(2, 5), roles, 0));
    let state = BotState::default();
    let role = turn.role_by_id(10010).unwrap();

    // Without an opening in the line the nearest damaged wall wins.
    assert_eq!(
        repair_target(&turn, &state, role, &[]),
        Some(far),
        "the nearest damaged wall is the far one, not the breach one"
    );
    // With the breach open, the wall holding its edge outranks it.
    assert_eq!(
        repair_target(&turn, &state, role, &[opening]),
        Some(beside),
        "the wall beside the breach is mended before a nearer one elsewhere"
    );
}

#[test]
fn a_half_destroyed_wall_is_funded_ahead_of_a_chipped_one() {
    // 10 gold buys a wall's full HP back. That is worth queueing ahead of the
    // luxuries when the wall is half gone, and not worth it for a scratch.
    let walls_at = |health: i64| {
        let mut roles = vec![station(), worker(10010, 12, 21, vec![])];
        for (index, cell) in ring_cells().into_iter().enumerate() {
            roles.push(wall(20000 + index as i64, cell, 1, health));
        }
        turn_from(world(round_of(2, 5), roles, 40))
    };
    let priority_of = |turn: &Turn| {
        let state = BotState::default();
        coregeek::brain::economy::budget(turn, &state, 0)
            .intent
            .iter()
            .find(|need| need.name == "WallFixer")
            .map(|need| need.priority)
    };

    assert_eq!(
        priority_of(&walls_at(400)),
        Some(2),
        "a wall at 40% is a breach waiting for the night"
    );
    assert_eq!(
        priority_of(&walls_at(950)),
        Some(3),
        "a chipped wall can wait its turn"
    );
}
