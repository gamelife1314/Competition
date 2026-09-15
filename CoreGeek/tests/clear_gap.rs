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
//!
//! **What issues #176-#185 changed.** The STATION DEMOTION now needs an
//! ACTIONABLE gap — one that a single weapon level can actually close
//! (`combat::best_weapon_step_gain`), not merely a positive one. `firepower_gap`
//! is positive on every board this bot has ever fielded: the estimate starts at
//! 3150 and three level-1 towers put out 1600, so the demotion was permanently
//! on. The ten reports all log `coach_move {switch: gap_funding, to: on, why:
//! station_breached}` on the first breached night and `policy.gapFunding: true`
//! from then to the end of the match, and 表 2a over the ten holds one
//! `WeaponUpgradeVoucher1` and no `StationUpgradeVoucher1` at all — which is why
//! the base never left 1500 HP and fell in all ten. The harassment suppression
//! is unchanged and still keys on the plain `gap > 0`.

use serde_json::{json, Value};

use coregeek::brain::economy::{clear_gap_order_with, intent_list, shopping_list};
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// A rich day-2 board: three towers of the given level, station L1, 8 walls,
/// 600 gold — everything intent_list could possibly queue is queued.
fn rich_board_with(tower_level: i64, kinds: [&str; 3]) -> Value {
    let mut roles = vec![
        json!({"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
             "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
        json!({"id": 10002, "pos": {"x": 13, "y": 24}, "roleType": "worker",
             "health": 220, "attackPower": 0, "attackRange": 0,
             "backPackCapability": 100, "backpack": []}),
    ];
    let power = |kind: &str| match kind {
        "rocket" => 20,
        _ => 10,
    };
    for (index, kind) in kinds.iter().enumerate() {
        roles.push(json!({
            "id": 10020 + index as i64,
            "pos": {"x": 10 + index as i32, "y": 22},
            "roleType": kind,
            "health": 1000,
            "attackPower": power(kind) * tower_level,
            "attackRange": 3,
            "level": tower_level,
            "backPackCapability": 0, "backpack": []
        }));
    }
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

fn rich_board(tower_level: i64) -> Value {
    rich_board_with(tower_level, ["gatling", "railgun", "rocket"])
}

fn priority_of(needs: &[coregeek::brain::economy::Need], name: &str) -> Option<i32> {
    needs.iter().find(|need| need.name == name).map(|need| need.priority)
}

#[test]
fn a_gap_one_voucher_can_close_still_demotes_the_station() {
    // 3×L2 gatlings on day 2: capacity 3600 against the 4050 estimate — a gap of
    // 450, and one more weapon level is worth 600. A single voucher covers it,
    // so tonight IS a firepower problem and the guns keep the front of the queue.
    let turn = turn_from(rich_board_with(2, ["gatling", "gatling", "gatling"]));
    let state = BotState::default();
    let mut needs = intent_list(&turn, &state, 0);
    assert!(priority_of(&needs, "StationUpgradeVoucher1").is_some());
    assert!(priority_of(&needs, "BossRobotSummonOrder").is_some());

    clear_gap_order_with(true, &turn, &mut needs);
    assert_eq!(
        priority_of(&needs, "StationUpgradeVoucher1"),
        Some(5),
        "a bigger base behind guns one voucher from covering the wave waits"
    );
    assert!(
        priority_of(&needs, "BossRobotSummonOrder").is_none(),
        "harassment is silence until the wave is clearable"
    );
    assert_eq!(
        priority_of(&needs, "WeaponUpgradeVoucher2"),
        Some(0),
        "firepower keeps the front of the queue"
    );
}

#[test]
fn the_gap_no_voucher_can_close_leaves_the_station_at_the_front() {
    // THE TEN-MATCH BOARD (issues #176-#185), reproduced from the reports.
    //
    // 3×L1 towers on day 2: capacity 1560 against the 4050 estimate — a gap of
    // 2490 that one weapon level (worth 600) cannot close. All ten reports
    // reached this exact state and stayed in it: `coach_move {switch:
    // gap_funding, to: on, why: station_breached}` on the first night the
    // station took a hit, `policy.gapFunding` true from that round to the end,
    // `coach_night.gapPredicted` 1950 / 2250 / 4950 growing daily.
    //
    // Demoting the station on this board is what the ten reports measure: 表 2a
    // holds one `WeaponUpgradeVoucher1` (185, round 145, 121 gold) and not one
    // `StationUpgradeVoucher1`; 表 8's 我方基地等级 is 1 in all ten, so the base
    // never left 1500 HP; and the base falls in all ten matches against a
    // `score_3` worth 550 that we banked 10-30 of.
    let board = rich_board(1);
    let turn = turn_from(board.clone());
    let state = BotState::default();
    let mut needs = intent_list(&turn, &state, 0);
    assert!(priority_of(&needs, "StationUpgradeVoucher1").is_some());
    clear_gap_order_with(true, &turn, &mut needs);
    // Offense-first: station starts at priority 2 (behind weapon 0 and walls 1)
    // when weapons aren't maxed. clear_gap_order_with does NOT demote it further
    // because the gap is too large for one voucher to close.
    assert_eq!(
        priority_of(&needs, "StationUpgradeVoucher1"),
        Some(2),
        "station stays at its offense-first priority (2) when the gap is too large for one voucher"
    );
    assert_eq!(priority_of(&needs, "WeaponUpgradeVoucher1"), Some(0));
    // The harassment suppression is deliberately unchanged: it still keys on the
    // plain `gap > 0`, because "do not spend 200-500 gold on summons while the
    // wave out-HPs the guns" is a statement about the wave rather than about
    // what one voucher buys.
    assert!(priority_of(&needs, "BossRobotSummonOrder").is_none());

    // And the outcome: with exactly one voucher's worth of gold on this board,
    // offense-first (issues #201-#205) means the weapon voucher is bought —
    // firepower kills enemies for score, which is how the base survives.
    let mut poor = board;
    poor["teamOur"]["goldNum"] = json!(100);
    let turn = turn_from(poor);
    let bought: Vec<String> = shopping_list(&turn, &state, 0)
        .into_iter()
        .map(|need| need.name)
        .collect();
    assert_eq!(
        bought,
        vec!["WeaponUpgradeVoucher1".to_string()],
        "the only affordable voucher must be the weapon upgrade — offense-first"
    );
}

#[test]
fn the_committed_order_stands_when_the_dial_is_off_or_the_gap_is_closed() {
    // Offense-first (issues #201-#205): with 3×L1 towers, weapons are NOT maxed,
    // so the station voucher sits at priority 2 (behind weapon 0 and walls 1).
    // clear_gap_order_with(dial=false) does nothing, so it stays at 2.
    let turn = turn_from(rich_board(1));
    let state = BotState::default();
    let mut needs = intent_list(&turn, &state, 0);
    clear_gap_order_with(false, &turn, &mut needs);
    assert_eq!(priority_of(&needs, "StationUpgradeVoucher1"), Some(2));
    assert!(priority_of(&needs, "BossRobotSummonOrder").is_some());

    // 3×L3 towers: weapons are maxed, so station is at priority 0. No gap,
    // and even a switched-on dial changes nothing.
    let turn = turn_from(rich_board(3));
    let mut needs = intent_list(&turn, &state, 0);
    clear_gap_order_with(true, &turn, &mut needs);
    assert_eq!(priority_of(&needs, "StationUpgradeVoucher1"), Some(0));
    assert!(priority_of(&needs, "BossRobotSummonOrder").is_some());
}
