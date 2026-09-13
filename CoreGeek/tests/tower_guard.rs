//! P0-4: the third tower's 25 gold is guarded while the fallback window nears.
//!
//! Issues #9/#15/#20/#22/#23: five-plus matches with no third tower, because
//! small purchases (Medicine, WallFixer, wall vouchers) spent the purse below
//! 25 in exactly the rounds the fallback window was about to open —
//! `tower_plan mayBuild=false` all day, and the rocket pad was never laid.
//! `third_tower_guard` detects the window, the build reserve holds the fund,
//! the shopping list respects it (wall vouchers included), and the fallback
//! then spends precisely that 25 gold.

use serde_json::{json, Value};

use coregeek::brain::economy::{
    may_build_weapon, shopping_list, third_tower_guard, DUSK_ROUND, FALLBACK_LEAD, GUARD_LEAD,
};
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// In-day round from which the guard is armed: the fallback window minus the
/// guard lead.
const GUARD_OPENS: i64 = DUSK_ROUND - FALLBACK_LEAD - GUARD_LEAD;

/// Day-1 board at in-day round `in_day` with two level-1 towers, `gold`, a
/// worker carrying `iron` pieces of iron, and `walls` standing walls.
fn two_tower_board(gold: i64, in_day: i64, iron: usize, walls: usize) -> Value {
    let mut roles = vec![
        json!({"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
             "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
        json!({"id": 10020, "pos": {"x": 10, "y": 21}, "roleType": "gatling",
             "health": 1000, "attackPower": 10, "attackRange": 3, "level": 1,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10030, "pos": {"x": 12, "y": 21}, "roleType": "railgun",
             "health": 1000, "attackPower": 10, "attackRange": 6, "level": 1,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10002, "pos": {"x": 13, "y": 24}, "roleType": "worker",
             "health": 220, "attackPower": 0, "attackRange": 0,
             "backPackCapability": 100, "backpack": vec!["iron"; iron]}),
    ];
    for index in 0..walls {
        roles.push(json!({
            "id": 40000 + index as i64, "pos": {"x": 8, "y": 19 - index as i32},
            "roleType": "wall", "health": 1000, "level": 1,
            "backPackCapability": 0, "backpack": []
        }));
    }
    json!({
        "roundNo": in_day + 1,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 30, "y": 20}, "neutralType": "weaponShop"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": gold, "totalScore": 0,
            "playerTasks": [],
            "roles": roles,
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "vendorShopList": [{"name": "iron", "price": 8}],
        "weaponShopList": [
            {"name": "Medicine", "price": 10},
            {"name": "WallFixer", "price": 10},
            {"name": "WallUpgradeVoucher1", "price": 20},
            {"name": "WeaponUpgradeVoucher1", "price": 100},
        ],
    })
}

#[test]
fn the_guard_holds_the_rocket_fund_while_the_upgrade_is_out_of_reach() {
    // 30 gold, guard window just opened, nothing sellable in the pack: the
    // level-2 upgrade is out of reach, so the 25 must be guarded.
    let turn = turn_from(two_tower_board(30, GUARD_OPENS, 0, 0));
    let state = BotState::default();
    assert!(
        third_tower_guard(&turn, &state),
        "window near + upgrade unreachable = the rocket fund is guarded"
    );
    // …and the shopping list may not spend it: every candidate purchase
    // (10-gold Medicine, 20-gold wall voucher) would drop the purse below the
    // rocket's price.
    let shopping = shopping_list(&turn, &state, 25);
    assert!(
        shopping.is_empty(),
        "with 30 gold and a guarded 25 nothing may be bought: {shopping:?}"
    );
}

#[test]
fn the_guard_stays_off_until_the_window_nears() {
    let turn = turn_from(two_tower_board(30, GUARD_OPENS - 1, 0, 0));
    assert!(
        !third_tower_guard(&turn, &BotState::default()),
        "one round before the window there is nothing to guard yet"
    );
}

#[test]
fn the_guard_stays_off_while_the_upgrade_is_reachable() {
    // 13 iron × 8 = 104 liquid gold: the level-2 voucher is within reach, so
    // the upgrade keeps its priority and no fund is guarded (the issue #13
    // "两塔先升级" lesson is untouched).
    let turn = turn_from(two_tower_board(30, GUARD_OPENS, 13, 0));
    assert!(
        !third_tower_guard(&turn, &BotState::default()),
        "the upgrade being reachable keeps the upgrade priority"
    );
}

#[test]
fn the_guard_stays_off_with_a_third_tower_or_an_upgraded_gun() {
    // Three towers: nothing left to fund.
    let mut board = two_tower_board(30, GUARD_OPENS, 0, 0);
    board["teamOur"]["roles"].as_array_mut().unwrap().push(json!({
        "id": 10040, "pos": {"x": 11, "y": 21}, "roleType": "rocket",
        "health": 1000, "attackPower": 20, "attackRange": 10, "level": 1,
        "backPackCapability": 0, "backpack": []
    }));
    let turn = turn_from(board);
    assert!(!third_tower_guard(&turn, &BotState::default()));

    // A level-2 gun: the upgrade the guard protects is already applied.
    let mut board = two_tower_board(30, GUARD_OPENS, 0, 0);
    board["teamOur"]["roles"].as_array_mut().unwrap()[1]["level"] = json!(2);
    let turn = turn_from(board);
    assert!(!third_tower_guard(&turn, &BotState::default()));
}

#[test]
fn a_wall_voucher_may_not_eat_the_guarded_fund() {
    // The exact leak from issues #22/#23: with the ring up (6+ walls), a
    // 20-gold wall voucher queues every afternoon — and used to be allowed to
    // spend the purse to 10 in the very rounds the fallback needed 25.
    // Round 40: late enough that wall vouchers queue (the weapon-upgrade
    // deadline has passed), still inside the guard window.
    let turn = turn_from(two_tower_board(30, 40, 0, 6));
    let state = BotState::default();
    assert!(third_tower_guard(&turn, &state), "test setup: guard is on");

    let guarded = shopping_list(&turn, &state, 25);
    assert!(
        guarded
            .iter()
            .all(|need| !need.name.starts_with("WallUpgradeVoucher")),
        "a wall voucher may not spend the guarded 25: {guarded:?}"
    );
    // Sanity: the same list with NO reserve does offer the voucher — the floor
    // is the only thing that changed.
    let unguarded = shopping_list(&turn, &state, 0);
    assert!(
        unguarded
            .iter()
            .any(|need| need.name.starts_with("WallUpgradeVoucher")),
        "without the guard the voucher is buyable: {unguarded:?}"
    );
}

#[test]
fn the_guarded_fund_is_what_the_fallback_spends() {
    // The whole point: once the window opens, the guarded 25 is still in the
    // purse and `may_build_weapon` lets the rocket pad through.
    let turn = turn_from(two_tower_board(25, DUSK_ROUND - FALLBACK_LEAD, 0, 0));
    let state = BotState::default();
    assert!(
        third_tower_guard(&turn, &state),
        "window open, upgrade unreachable"
    );
    assert!(
        may_build_weapon(&turn, &state),
        "the guarded 25 gold builds the third tower"
    );
}
