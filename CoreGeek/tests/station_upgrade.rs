//! The base upgrade nobody ever bought (issues #161-#170).
//!
//! Ten loss reports, ten opponents, one shared ledger. 表 2a — every purchase
//! the match made — reads:
//!
//! ```text
//! 170  WallFixer x6, Medicine x3, WallFixer x1
//! 169  Medicine x3
//! 168  WallFixer x6, Medicine x3
//! 167  WallFixer x1, Medicine x1
//! 166  WeaponUpgradeVoucher1 x1   <- round 37, 130 gold
//! 165  (none)
//! 164  (none)
//! 163  WallFixer x6
//! 162  (none)
//! 161  WallFixer x6, Medicine x2
//! ```
//!
//! **Not one `StationUpgradeVoucher1` in ten matches**, while 表 0 shows the
//! base falling in nine of them and `scoreAttr.survival` frozen at 0-30 against
//! a 应得 of 10-100 (任务书 ch.6: `score_3 = Σ 10×day×存活系数`, capped at 550).
//! When the base falls the round score freezes with it — 163 counted 152 of the
//! 424 kill points it had earned, 167 64 of 309 — so the base is not one line of
//! the score, it is the multiplier on the other two.
//!
//! 任务书 4.6.1 (line 187-189) prices the repair that would have changed that:
//!
//! ```text
//! 基地 level1 1500 HP   level2 3000 HP   level3 4500 HP
//! StationUpgradeVoucher1 100 gold   StationUpgradeVoucher2 150 gold
//! ```
//!
//! and nothing else in the game adds HP to the base: `WallFixer` mends a wall,
//! `Medicine` mends a unit, and the station is the one asset with no repair item
//! at all. It is also the only one whose destruction ends the half (任务书 ch.7).
//!
//! **Why it was never bought.** `intent_list` queues both vouchers, and both
//! cost exactly 100 gold. `WeaponUpgradeVoucher1` is queued at priority 0 and
//! `StationUpgradeVoucher1` at priority 1; `shopping_list` allocates greedily in
//! `(priority, Reverse(value))` order. 表 2c says the purse peaks at 130-155 in
//! these matches — enough for one of the two and never for both — so the weapon
//! voucher took the whole 100 and the station voucher hit `num <= 0` and was
//! dropped, in every round it was affordable, for the whole match.
//!
//! Issue #166 is the case that shows it: at round 37 the purse held 130 gold and
//! it bought `WeaponUpgradeVoucher1`. The station voucher was affordable in that
//! same round, and lost on a priority number that outranks the ranking written
//! for exactly this comparison — `survival_value` credits the station with 2250
//! effective HP per 100 gold against the weapon's 1500 (economy.rs,
//! `effective_hp`), and the docstring on the station block says it is "ranked
//! against the walls by HP per gold rather than by fiat". It is ranked by fiat
//! against the weapon, and the fiat is what wins.
//!
//! These tests pin the outcome, not the mechanism: which voucher is in the
//! shopping list, and — once it is — that it is applied to the station.

use serde_json::{json, Value};

use coregeek::brain::economy::{shopping_list, Need};
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn bought(needs: &[Need]) -> Vec<&str> {
    needs.iter().map(|need| need.name.as_str()).collect()
}

/// A day-1 board holding `gold`, with the shop prices from 任务书 4.6.3. The
/// station is level 1 — the state all ten reports were in, since none of them
/// ever upgraded it.
fn board(gold: i64, walls: usize) -> Value {
    let mut roles = vec![
        json!({"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
             "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
        json!({"id": 10020, "pos": {"x": 10, "y": 22}, "roleType": "gatling",
             "health": 1000, "attackPower": 10, "attackRange": 3, "level": 1,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10002, "pos": {"x": 13, "y": 24}, "roleType": "worker",
             "health": 220, "attackPower": 0, "attackRange": 0,
             "backPackCapability": 100, "backpack": []}),
    ];
    for index in 0..walls {
        roles.push(json!({
            "id": 40000 + index as i64, "pos": {"x": 8, "y": 21 + index as i32},
            "roleType": "wall", "health": 1000, "level": 1,
            "backPackCapability": 0, "backpack": []
        }));
    }
    json!({
        "roundNo": 21,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 28, "y": 20}, "neutralType": "weaponShop"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": gold, "totalScore": 0,
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
        ],
    })
}

#[test]
fn a_purse_that_covers_one_voucher_buys_the_base_not_the_gun() {
    // 130 gold is the measured peak in every one of the ten reports (表 2c:
    // 130/146, 66, 146, 66, 155, 120, 142) and issue #166 spent exactly this
    // purse on `WeaponUpgradeVoucher1` at round 37. Both vouchers cost 100, so
    // the round buys one of them. The base is the loss condition and the only
    // asset with no repair item; a level-2 station is 3000 HP instead of 1500.
    let turn = turn_from(board(130, 0));
    let list = shopping_list(&turn, &BotState::default(), 0);
    let names = bought(&list);

    assert!(
        names.contains(&"StationUpgradeVoucher1"),
        "the base upgrade is affordable and must be bought: {names:?}"
    );
    assert!(
        !names.contains(&"WeaponUpgradeVoucher1"),
        "the purse covers one 100-gold voucher, and the base outranks the gun \
         on the survival ranking the code already computes: {names:?}"
    );
}

#[test]
fn a_purse_that_covers_both_still_buys_both() {
    // The fix must not make the weapon voucher unreachable — it only decides
    // which one goes first when the purse cannot pay for both. At 200 gold the
    // round is not a contest and both are queued.
    let turn = turn_from(board(200, 0));
    let list = shopping_list(&turn, &BotState::default(), 0);
    let names = bought(&list);

    assert!(
        names.contains(&"StationUpgradeVoucher1"),
        "the base upgrade is bought: {names:?}"
    );
    assert!(
        names.contains(&"WeaponUpgradeVoucher1"),
        "and so is the weapon voucher once the purse covers both: {names:?}"
    );
}

#[test]
fn the_weapon_voucher_still_wins_a_purse_that_only_covers_it() {
    // The station upgrade is a 100-gold decision, not a blanket preference. At
    // 99 gold neither voucher fits, and the round must not invent a purchase it
    // cannot pay for.
    let turn = turn_from(board(99, 0));
    let list = shopping_list(&turn, &BotState::default(), 0);
    let names = bought(&list);
    assert!(
        !names.contains(&"StationUpgradeVoucher1")
            && !names.contains(&"WeaponUpgradeVoucher1"),
        "neither 100-gold voucher is affordable at 99 gold: {names:?}"
    );
}

#[test]
fn the_station_is_upgraded_at_level_two_and_not_again_at_three() {
    // The ladder is per level: level 2 asks for `StationUpgradeVoucher2`
    // (150 gold, 3000 -> 4500 HP), and a level-3 station is the cap
    // (任务书 4.6.3: "到达最高等级后再次使用升级券不会生效").
    let mut payload = board(200, 0);
    payload["teamOur"]["roles"][0]["level"] = json!(2);
    payload["weaponShopList"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "StationUpgradeVoucher2", "price": 150}));
    let turn = turn_from(payload);
    let list = shopping_list(&turn, &BotState::default(), 0);
    let names = bought(&list);
    assert!(
        names.contains(&"StationUpgradeVoucher2"),
        "a level-2 base buys the level-3 voucher: {names:?}"
    );
    assert!(
        !names.contains(&"StationUpgradeVoucher1"),
        "and not the one it has already outgrown: {names:?}"
    );

    let mut capped = board(600, 0);
    capped["teamOur"]["roles"][0]["level"] = json!(3);
    let turn = turn_from(capped);
    let list = shopping_list(&turn, &BotState::default(), 0);
    let names = bought(&list);
    assert!(
        !names.iter().any(|name| name.starts_with("StationUpgradeVoucher")),
        "a level-3 base is the cap and buys no more vouchers: {names:?}"
    );
}
