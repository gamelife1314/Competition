//! P1-1: the clear-gap funding reorder.
//!
//! The fixed funding order (weapon vouchers > wall/station vouchers >
//! consumables > harassment) ignores the night's arithmetic: when the
//! estimated wave out-HPs the guns, every gold that does not add firepower is
//! spent on a bigger base that gets demolished anyway (issues #8/#18/#19/#20:
//! D2–D3 night deaths behind 2×L1 towers). `clear_gap_order_with` demotes the
//! station upgrade below the sustain pack and suppresses harassment while the
//! gap is positive. It is dial-gated (`CG_TUNE_CLEAR_GAP`, default OFF), so
//! the committed order owns until the A/B report proves the reorder.

use serde_json::{json, Value};

use coregeek::brain::economy::{clear_gap_order_with, intent_list};
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// A rich day-2 board: towers of the given level, station L1, 8 walls, 600
/// gold — everything intent_list could possibly queue is queued.
fn rich_board(tower_level: i64) -> Value {
    let mut roles = vec![
        json!({"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
             "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
        json!({"id": 10020, "pos": {"x": 10, "y": 22}, "roleType": "gatling",
             "health": 1000, "attackPower": 10, "attackRange": 3, "level": tower_level,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10030, "pos": {"x": 11, "y": 22}, "roleType": "railgun",
             "health": 1000, "attackPower": 10 * tower_level, "attackRange": 6, "level": tower_level,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10040, "pos": {"x": 12, "y": 22}, "roleType": "rocket",
             "health": 1000, "attackPower": 20, "attackRange": 10, "level": tower_level,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10002, "pos": {"x": 13, "y": 24}, "roleType": "worker",
             "health": 220, "attackPower": 0, "attackRange": 0,
             "backPackCapability": 100, "backpack": []}),
    ];
    for index in 0..8 {
        roles.push(json!({
            "id": 40000 + index as i64, "pos": {"x": 8, "y": 21 + index as i32},
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
            "type": "challenger", "goldNum": 600, "totalScore": 0,
            "playerTasks": [], "roles": roles,
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "weaponShopList": [
            {"name": "Medicine", "price": 10},
            {"name": "WallFixer", "price": 10},
            {"name": "WallUpgradeVoucher1", "price": 20},
            {"name": "WeaponUpgradeVoucher1", "price": 100},
            {"name": "StationUpgradeVoucher1", "price": 100},
            {"name": "BossRobotSummonOrder", "price": 200},
        ],
    })
}

fn priority_of(needs: &[coregeek::brain::economy::Need], name: &str) -> Option<i32> {
    needs.iter().find(|need| need.name == name).map(|need| need.priority)
}

#[test]
fn a_positive_gap_demotes_the_station_and_suppresses_harassment() {
    // 3×L1 towers on day 2: capacity (10+10+6)×60 = 1560 against an estimated
    // 4050 HP wave — a clear gap.
    let turn = turn_from(rich_board(1));
    let state = BotState::default();
    let mut needs = intent_list(&turn, &state, 0);
    assert!(priority_of(&needs, "StationUpgradeVoucher1").is_some());
    assert!(priority_of(&needs, "BossRobotSummonOrder").is_some());

    clear_gap_order_with(true, &turn, &mut needs);
    assert_eq!(
        priority_of(&needs, "StationUpgradeVoucher1"),
        Some(5),
        "a bigger base behind too few guns waits for the guns"
    );
    assert!(
        priority_of(&needs, "BossRobotSummonOrder").is_none(),
        "harassment is silence until the wave is clearable"
    );
    assert_eq!(
        priority_of(&needs, "WeaponUpgradeVoucher1"),
        Some(0),
        "firepower keeps the front of the queue"
    );
}

#[test]
fn the_committed_order_stands_when_the_dial_is_off_or_the_gap_is_closed() {
    // The dial-off order IS the committed order, and it now puts the station
    // voucher in the same tier as the weapon voucher (priority 0, decided by
    // `survival_value`). This assertion used to read `Some(1)`: issues
    // #161-#170 overturn that, and the evidence is in `tests/station_upgrade.rs`
    // and in the block comment in `economy::intent_list` — ten matches, ten
    // opponents, not one `StationUpgradeVoucher1` bought (表 2a), nine bases
    // lost, `scoreAttr.survival` 0-30 against a 应得 of 10-100. The tier was set
    // when the guns were killing nothing (the batches behind `clear_gap_order_with`
    // read kill=0); this batch reads kill 70-424, so firepower is no longer the
    // binding constraint and the base is.
    let turn = turn_from(rich_board(1));
    let state = BotState::default();
    let mut needs = intent_list(&turn, &state, 0);
    clear_gap_order_with(false, &turn, &mut needs);
    assert_eq!(priority_of(&needs, "StationUpgradeVoucher1"), Some(0));
    assert!(priority_of(&needs, "BossRobotSummonOrder").is_some());

    // 3×L3 towers: capacity 80×60 = 4800 ≥ the 4050 estimate — no gap, and
    // even a switched-on dial changes nothing.
    let turn = turn_from(rich_board(3));
    let mut needs = intent_list(&turn, &state, 0);
    clear_gap_order_with(true, &turn, &mut needs);
    assert_eq!(priority_of(&needs, "StationUpgradeVoucher1"), Some(0));
    assert!(priority_of(&needs, "BossRobotSummonOrder").is_some());
}
