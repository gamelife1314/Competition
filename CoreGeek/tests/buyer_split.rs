//! P1-2: buyer/economy-worker split.
//!
//! One role used to run the whole mine→vendor→shop triangle: every purchase
//! cost 30–40 rounds of walking, one or two buys a day, and the ore sat in
//! the backpack while the buyer idled at the counter (issues #20/#22, frozen
//! at 5–25 gold). Once the wall work is done, the FIRST worker takes the shop
//! trips and the dedicated economy worker stays on the collect→sell loop.

use serde_json::{json, Value};

use coregeek::brain::day::plan as day_plan;
use coregeek::model::{chebyshev, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// The radius-2 ring around the (10,24) station, every cell walled — the
/// precondition for the split (no wall work left today).
fn ring_roles() -> Vec<Value> {
    let mut roles = Vec::new();
    for x in 8..=13 {
        for y in 21..=26 {
            let pos = Pos { x, y };
            let footprint = [
                Pos { x: 10, y: 24 },
                Pos { x: 11, y: 24 },
                Pos { x: 10, y: 23 },
                Pos { x: 11, y: 23 },
            ];
            if footprint.contains(&pos) {
                continue;
            }
            let dist = footprint
                .iter()
                .map(|cell| chebyshev(pos, *cell))
                .min()
                .unwrap();
            if dist == 2 {
                roles.push(json!({
                    "id": 40000 + (x * 100 + y) as i64, "pos": {"x": x, "y": y},
                    "roleType": "wall", "health": 1000, "level": 1,
                    "backPackCapability": 0, "backpack": []
                }));
            }
        }
    }
    roles
}

fn split_board() -> Value {
    let mut roles = vec![
        json!({"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
             "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
        json!({"id": 10020, "pos": {"x": 11, "y": 22}, "roleType": "gatling",
             "health": 1000, "attackPower": 10, "attackRange": 3, "level": 1,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10030, "pos": {"x": 12, "y": 24}, "roleType": "railgun",
             "health": 1000, "attackPower": 10, "attackRange": 6, "level": 1,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10002, "pos": {"x": 15, "y": 24}, "roleType": "worker",
             "health": 220, "attackPower": 0, "attackRange": 0,
             "backPackCapability": 100, "backpack": []}),
        json!({"id": 10003, "pos": {"x": 16, "y": 25}, "roleType": "worker",
             "health": 220, "attackPower": 0, "attackRange": 0,
             "backPackCapability": 100, "backpack": []}),
    ];
    roles.extend(ring_roles());
    json!({
        "roundNo": 140,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 28, "y": 20}, "neutralType": "weaponShop"},
            {"pos": {"x": 22, "y": 19}, "neutralType": "iron"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": 100, "totalScore": 0,
            "playerTasks": [], "roles": roles,
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "vendorShopList": [{"name": "iron", "price": 8}],
        "weaponShopList": [{"name": "WeaponUpgradeVoucher1", "price": 100}],
    })
}

fn nearest_stand_dist(from: Pos, target: Pos) -> i32 {
    let mut best = i32::MAX;
    for dx in -1..=1 {
        for dy in -1..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            best = best.min(chebyshev(
                from,
                Pos {
                    x: target.x + dx,
                    y: target.y + dy,
                },
            ));
        }
    }
    best
}

#[test]
fn once_the_ring_stands_the_first_worker_shops_and_the_second_mines() {
    let turn = turn_from(split_board());
    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);

    let shop = Pos { x: 28, y: 20 };
    let mine = Pos { x: 22, y: 19 };
    let buyer_cmd = plan
        .commands
        .get(&10002)
        .expect("the first worker has an errand");
    let buyer_step = buyer_cmd
        .targetPos
        .as_ref()
        .map(|targets| targets[0])
        .expect("a movement or action target");
    let before = nearest_stand_dist(Pos { x: 15, y: 24 }, shop);
    let after = nearest_stand_dist(buyer_step, shop);
    assert!(
        after < before,
        "the first worker heads for the shop ({before} → {after} via {buyer_step:?}, action {:?})",
        buyer_cmd.action
    );

    let economy_cmd = plan
        .commands
        .get(&10003)
        .expect("the economy worker keeps the loop running");
    assert_ne!(
        economy_cmd.action, "buy",
        "the economy worker never spends — that is the buyer's job now"
    );
    // Shop and mine are collinear from here, so shop-distance alone cannot
    // tell the errands apart; the mine leg is the one that must shrink.
    let economy_step = economy_cmd
        .targetPos
        .as_ref()
        .map(|targets| targets[0]);
    if let Some(step) = economy_step {
        assert!(
            nearest_stand_dist(step, mine) < nearest_stand_dist(Pos { x: 16, y: 25 }, mine),
            "and it heads for the mine instead (step {step:?}, action {:?})",
            economy_cmd.action
        );
    }
}
