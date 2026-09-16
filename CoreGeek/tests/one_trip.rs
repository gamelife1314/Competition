//! One trip out, everything bought and collected (issue #206 §2, 「一趟买齐」).
//!
//! The owner's words: 「每个工人有200格，开拓者有40格，可以一次性外出采集很多
//! 东西再回来，我们可以一次性出去把该买的券，该采集的石头都买好，等开拓者工人
//! 回来之后一次性建墙，升级武器，然后再等待机器人来临。」
//!
//! The pack size is the board's, not the owner's recollection: it is read off the
//! role's `backPackCapability` (100 for a worker, 40 for the pioneer — 任务书
//! 3.2) and never guessed here. What did not exist was the TRIP: `intent_list`
//! knew what to buy, `sell_batch` knew when to cash in, `choose_mine` knew which
//! vein was worth working — each answered alone, and no one of them ever asked
//! whether the errands could share one walk and still be home before the dusk
//! recall. `economy::outing` answers that: one departure, its stops in the order
//! they are walked, and the round cost of the whole thing against the daylight
//! left.
//!
//! These tests assert the OUTING — which stops, in what order, at what cost —
//! and the command the day sends because of it. Never that a helper exists.

use serde_json::{json, Value};

use coregeek::brain::day::plan as day_plan;
use coregeek::brain::economy::{self, Stop};
use coregeek::brain::route::trip_rounds;
use coregeek::model::{chebyshev, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// The radius-2 ring around the (10,24) station, every cell walled: the wall
/// line is paid for, so the day names a dedicated buyer and the outing is the
/// thing under test rather than the wall work.
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

/// The board: a complete ring, a level-1 gun (so the weapon voucher is a real
/// need), an iron vein, a vendor and a weapon shop, and two workers. 10002 is
/// the crew's first worker — the dedicated buyer once the ring stands — and its
/// pack is the parameter under test.
fn board(round_no: i64, buyer_load: &[&str]) -> Value {
    let mut roles = vec![
        json!({"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
             "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
        json!({"id": 10020, "pos": {"x": 11, "y": 22}, "roleType": "gatling",
             "health": 1000, "attackPower": 10, "attackRange": 3, "level": 1,
             "backPackCapability": 0, "backpack": []}),
        json!({"id": 10002, "pos": {"x": 15, "y": 24}, "roleType": "worker",
             "health": 220, "attackPower": 10, "attackRange": 3, "level": 1,
             "backPackCapability": 100, "backpack": buyer_load}),
        json!({"id": 10003, "pos": {"x": 16, "y": 25}, "roleType": "worker",
             "health": 220, "attackPower": 10, "attackRange": 3, "level": 1,
             "backPackCapability": 100, "backpack": []}),
    ];
    roles.extend(ring_roles());
    json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 4, "y": 24}, "neutralType": "weaponShop"},
            {"pos": {"x": 20, "y": 24}, "neutralType": "vendor"},
            {"pos": {"x": 24, "y": 19}, "neutralType": "iron"}
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

const IRON_VEIN: Pos = Pos { x: 24, y: 19 };
const VENDOR: Pos = Pos { x: 20, y: 24 };
/// West of the ring, opposite the vendor: a step toward one errand is then
/// measurably a step away from the other, which is what lets a single round's
/// command say WHICH errand the day took first.
const SHOP: Pos = Pos { x: 4, y: 24 };

/// The real shopping list the day would hand the trip, so the voucher on the
/// route is the one the economy actually wants rather than a fixture.
fn shopping(turn: &Turn) -> Vec<economy::Need> {
    let list = economy::shopping_list(turn, &BotState::default(), 0);
    assert!(
        list.iter().any(|need| need.name == "WeaponUpgradeVoucher1"),
        "premise: the crew wants the weapon voucher: {list:?}"
    );
    list
}

/// The cell the trip walks to at a zone (a stand beside it) and the walk cost —
/// the test's own arithmetic, so the outing's numbers are checked against the
/// day's walk model rather than against itself.
fn stand_walk(turn: &Turn, from: Pos, zone: Pos) -> (Pos, i64) {
    coregeek::brain::stand_cells(turn, zone)
        .into_iter()
        .map(|stand| (stand, trip_rounds(turn, from, stand)))
        .min_by_key(|(stand, walk)| (*walk, stand.x, stand.y))
        .expect("a zone has a stand")
}

/// 「一趟买齐」: the vouchers and the ore in ONE departure. The buyer's pack is
/// empty, so the trip is dig-then-shop, and both stops are on it.
#[test]
fn a_trip_buys_and_collects_in_one_departure() {
    let turn = turn_from(board(140, &[]));
    let buyer = turn.role_by_id(10002).expect("the buyer");
    let outing = economy::outing(&turn, &BotState::default(), buyer, &shopping(&turn), 0)
        .expect("an empty pack with gold in the purse is a trip");
    let Some(Stop::Collect { pos, ore, loads }) = outing.stops.first().cloned() else {
        panic!("the trip starts at the vein: {:?}", outing.stops);
    };
    assert_eq!(pos, IRON_VEIN, "the nearest sellable vein: {:?}", outing.stops);
    assert_eq!(ore, "iron");
    assert_eq!(loads, 4, "the load is the batch the sale is worth");
    assert!(
        matches!(outing.stops.get(1), Some(Stop::Buy { items })
            if items.iter().any(|(name, num)| name == "WeaponUpgradeVoucher1" && *num == 1)),
        "and the shopping rides on the same departure: {:?}",
        outing.stops
    );
    assert_eq!(outing.stops.len(), 2, "{:?}", outing.stops);

    // The walk home happens once, not twice: the whole trip costs less than
    // going out to the vein and back and then out to the shop and back.
    let (vein_stand, to_vein) = stand_walk(&turn, buyer.pos, IRON_VEIN);
    let (_, home_from_vein) = stand_walk(&turn, vein_stand, Pos { x: 10, y: 24 });
    let (shop_stand, to_shop) = stand_walk(&turn, buyer.pos, SHOP);
    let (_, home_from_shop) = stand_walk(&turn, shop_stand, Pos { x: 10, y: 24 });
    let separate = to_vein + loads + home_from_vein + to_shop + 1 + home_from_shop;
    assert!(
        outing.rounds < separate,
        "one departure ({} rounds) must beat two ({} rounds)",
        outing.rounds,
        separate
    );
    assert!(
        outing.rounds <= outing.daylight,
        "the trip fits the day it was planned for: {} of {}",
        outing.rounds,
        outing.daylight
    );

    // And the day acts on it: the buyer leaves the ring for the errand instead
    // of idling at home.
    let plan = day_plan(&turn, &mut BotState::default());
    let cmd = plan.commands.get(&10002).expect("the buyer has an errand");
    let step = cmd
        .targetPos
        .as_ref()
        .and_then(|targets| targets.first().copied())
        .expect("a movement target");
    assert!(
        chebyshev(step, buyer.pos) == 1,
        "one step out of the ring, not a jump: {step:?} from {:?}",
        buyer.pos
    );
    assert!(
        chebyshev(step, SHOP) < chebyshev(buyer.pos, SHOP)
            || chebyshev(step, IRON_VEIN) < chebyshev(buyer.pos, IRON_VEIN),
        "the step serves the trip: {step:?} (action {:?})",
        cmd.action
    );
}

/// A merge that does not fit the daylight is trimmed, and the trim takes the
/// TAIL. Late afternoon with the same pack and the same shopping list: the
/// walk to the counter and home still fits, the shop round trip does not, so
/// the trip keeps the sale — the errand that pays — and gives up the voucher.
#[test]
fn a_trip_that_cannot_fit_the_daylight_gives_up_the_shopping() {
    let turn = turn_from(board(40, &["iron"; 20])); // day 1, 16 rounds of daylight left
    let buyer = turn.role_by_id(10002).expect("the buyer");
    let state = BotState::default();
    let outing = economy::outing(&turn, &state, buyer, &shopping(&turn), 0)
        .expect("there is still time to cash the pack in");
    assert_eq!(outing.daylight, 16, "premise: a short afternoon");
    assert_eq!(
        outing.stops,
        vec![Stop::Sell],
        "the sale survives the trim and the shopping is what goes"
    );
    assert!(
        outing.rounds <= outing.daylight,
        "and what is left still gets home before dusk: {} of {}",
        outing.rounds,
        outing.daylight
    );
    assert_eq!(economy::vendor_travel(&turn, buyer.pos), 4, "premise: a near vendor");

    // The untrimmed trip is the same board with the whole afternoon ahead of
    // it — the trim is about the daylight, not about the trip.
    let early = turn_from(board(140, &["iron"; 20]));
    let full = economy::outing(
        &early,
        &state,
        early.role_by_id(10002).unwrap(),
        &shopping(&early),
        0,
    )
    .expect("a trip");
    assert!(
        matches!(full.stops.as_slice(), [Stop::Sell, Stop::Buy { .. }]),
        "with the day ahead of it the same trip shops too: {:?}",
        full.stops
    );
    assert!(full.rounds > outing.rounds, "the trimmed trip is the shorter one");
}

/// The merged trip never parks the crew's earner at a counter. The pack holds
/// ore the vendor will take and the purse holds the voucher's price, so the day
/// has both errands available and the temptation is to spend the trip shopping
/// — walking out with the pack still full and coming home with a voucher and no
/// gold. The trip cashes in first, and the round's command is the walk to the
/// vendor.
#[test]
fn a_merged_trip_cashes_the_pack_in_before_it_spends() {
    let turn = turn_from(board(140, &["iron"; 20]));
    let buyer = turn.role_by_id(10002).expect("the buyer");
    let state = BotState::default();
    let outing = economy::outing(&turn, &state, buyer, &shopping(&turn), 0).expect("a trip");
    assert_eq!(
        outing.stops,
        vec![
            Stop::Sell,
            Stop::Buy {
                items: vec![("WeaponUpgradeVoucher1".to_string(), 1)]
            }
        ],
        "the counter comes before the shop"
    );

    let plan = day_plan(&turn, &mut BotState::default());
    let cmd = plan.commands.get(&10002).expect("the buyer has an errand");
    assert_ne!(cmd.action, "buy", "the gold is not spent with the ore unsold");
    let step = cmd
        .targetPos
        .as_ref()
        .and_then(|targets| targets.first().copied())
        .expect("a movement target");
    let before = chebyshev(buyer.pos, VENDOR);
    let after = chebyshev(step, VENDOR);
    assert!(
        after < before,
        "the buyer walks to the vendor first ({before} → {after} via {step:?})"
    );
    assert!(
        chebyshev(step, SHOP) > chebyshev(buyer.pos, SHOP),
        "and not to the shop: the step is a counter step, not a shop step \
         ({step:?}, action {:?})",
        cmd.action
    );
}
