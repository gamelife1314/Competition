//! P2-3: summon-order rhythm — a boss wave is a half-ender against a
//! wall-less enemy base. The rich gate (500 gold + three towers) is
//! untouched; the finisher trigger arms only while the enemy ring is thin
//! (≤3 walls, globally visible) and keeps two orders in stock so the
//! pressure runs across nights.

use serde_json::{json, Value};

use coregeek::brain::economy::intent_list;
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// Day-2 board: three towers, `gold`, a worker carrying `orders` boss
/// orders, and an enemy station ringed by `enemy_walls` walls.
fn board(gold: i64, orders: usize, enemy_walls: usize) -> Value {
    let mut enemy = vec![json!({
        "id": 20013, "pos": {"x": 30, "y": 6}, "roleType": "station",
        "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []
    })];
    for index in 0..enemy_walls {
        enemy.push(json!({
            "id": 41000 + index as i64, "pos": {"x": 28, "y": 4 + index as i32},
            "roleType": "wall", "health": 1000, "level": 1,
            "backPackCapability": 0, "backpack": []
        }));
    }
    json!({
        "roundNo": 140,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 28, "y": 20}, "neutralType": "weaponShop"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": gold, "totalScore": 0,
            "playerTasks": [],
            "roles": [
                {"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                 "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []},
                {"id": 10020, "pos": {"x": 10, "y": 22}, "roleType": "gatling",
                 "health": 1000, "attackPower": 10, "attackRange": 3, "level": 1,
                 "backPackCapability": 0, "backpack": []},
                {"id": 10030, "pos": {"x": 11, "y": 22}, "roleType": "railgun",
                 "health": 1000, "attackPower": 10, "attackRange": 6, "level": 1,
                 "backPackCapability": 0, "backpack": []},
                {"id": 10040, "pos": {"x": 12, "y": 22}, "roleType": "rocket",
                 "health": 1000, "attackPower": 20, "attackRange": 10, "level": 1,
                 "backPackCapability": 0, "backpack": []},
                {"id": 10002, "pos": {"x": 13, "y": 24}, "roleType": "worker",
                 "health": 220, "attackPower": 0, "attackRange": 0,
                 "backPackCapability": 100,
                 "backpack": vec!["BossRobotSummonOrder"; orders]},
            ],
        },
        "teamEnemy": {"roles": enemy},
        "robot": {"roles": []},
        "weaponShopList": [{"name": "BossRobotSummonOrder", "price": 200}],
    })
}

fn has_boss_order(needs: &[coregeek::brain::economy::Need]) -> bool {
    needs.iter().any(|need| need.name == "BossRobotSummonOrder")
}

#[test]
fn a_wall_less_enemy_arms_the_finisher_from_300_gold() {
    let turn = turn_from(board(300, 0, 2));
    let needs = intent_list(&turn, &BotState::default(), 0);
    assert!(
        has_boss_order(&needs),
        "two enemy walls: the boss wave can end the half"
    );
}

#[test]
fn a_walled_enemy_keeps_the_old_500_gold_gate() {
    // Ten walls: the finisher stays off, and 300 gold never bought harassment.
    let turn = turn_from(board(300, 0, 10));
    assert!(!has_boss_order(&intent_list(&turn, &BotState::default(), 0)));
    // The rich gate is untouched: 500 gold and three towers still buy it.
    let turn = turn_from(board(500, 0, 10));
    assert!(has_boss_order(&intent_list(&turn, &BotState::default(), 0)));
}

#[test]
fn the_stock_caps_at_two_so_the_pressure_is_a_rhythm() {
    let turn = turn_from(board(300, 2, 2));
    assert!(
        !has_boss_order(&intent_list(&turn, &BotState::default(), 0)),
        "two already in stock: tonight's pressure is paid for"
    );
    let turn = turn_from(board(300, 1, 2));
    assert!(
        has_boss_order(&intent_list(&turn, &BotState::default(), 0)),
        "one in stock: top up for tomorrow night"
    );
}
