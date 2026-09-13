//! P1-4: the controller sustain pack — one Medicine per controller by dusk,
//! and 2–4 WallFixer a day depending on whether the ring has ever closed
//! (a ring that was breached before is the ring the night tears open again).

use serde_json::{json, Value};

use coregeek::brain::economy::intent_list;
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// Day-2 dusk-readiness board (in-day round 30): three healthy controllers,
/// two towers, eight undamaged walls, nothing in any backpack.
fn readiness_board() -> Value {
    let mut roles = vec![
        json!({"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
             "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
        json!({"id": 10020, "pos": {"x": 10, "y": 22}, "roleType": "gatling",
             "health": 1000, "attackPower": 10, "attackRange": 3, "level": 1,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10030, "pos": {"x": 11, "y": 22}, "roleType": "railgun",
             "health": 1000, "attackPower": 10, "attackRange": 6, "level": 1,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10002, "pos": {"x": 13, "y": 24}, "roleType": "worker",
             "health": 220, "attackPower": 0, "attackRange": 0,
             "backPackCapability": 100, "backpack": []}),
        json!({"id": 10003, "pos": {"x": 14, "y": 25}, "roleType": "worker",
             "health": 220, "attackPower": 0, "attackRange": 0,
             "backPackCapability": 100, "backpack": []}),
        json!({"id": 10004, "pos": {"x": 13, "y": 26}, "roleType": "pioneer",
             "health": 200, "attackPower": 0, "attackRange": 0,
             "backPackCapability": 40, "backpack": []}),
    ];
    for index in 0..8 {
        roles.push(json!({
            "id": 40000 + index as i64, "pos": {"x": 8, "y": 21 + index as i32},
            "roleType": "wall", "health": 1000, "level": 1,
            "backPackCapability": 0, "backpack": []
        }));
    }
    json!({
        "roundNo": 161,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 28, "y": 20}, "neutralType": "weaponShop"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [], "roles": roles,
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "weaponShopList": [
            {"name": "Medicine", "price": 10},
            {"name": "WallFixer", "price": 10},
        ],
    })
}

fn num_of(needs: &[coregeek::brain::economy::Need], name: &str) -> i64 {
    needs
        .iter()
        .find(|need| need.name == name)
        .map(|need| need.num)
        .unwrap_or(0)
}

#[test]
fn every_controller_gets_a_bottle_by_dusk() {
    let turn = turn_from(readiness_board());
    let state = BotState::default();
    let needs = intent_list(&turn, &state, 0);
    assert_eq!(
        num_of(&needs, "Medicine"),
        3,
        "one potion per controller — the withdrawal rule almost never fires \
         when the gun crew can heal itself (issues #22/#23)"
    );
}

#[test]
fn a_previously_breached_ring_stocks_four_kits_a_day() {
    let turn = turn_from(readiness_board());
    let mut state = BotState::default();
    let needs = intent_list(&turn, &state, 0);
    assert_eq!(
        num_of(&needs, "WallFixer"),
        2,
        "an unbreached ring keeps the two-kit readiness stock"
    );

    state.ring_ever_complete = true;
    let needs = intent_list(&turn, &state, 0);
    assert_eq!(
        num_of(&needs, "WallFixer"),
        4,
        "a ring the night has torn open before gets the four-kit stock (issue #21)"
    );
}
