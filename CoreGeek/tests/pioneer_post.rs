//! The pioneer's post (issue #206 §7): 「最好把开拓者要操作的武器安排在最安全的
//! 位置，并且给它留个门，让他操作可远程攻击的导弹」.
//!
//! The greedy pass in `night::pairing` hands every gun the controller standing
//! nearest it, which is the right question for a worker and the wrong one for
//! the pioneer: it is the role whose day is spent outside the ring (task points,
//! the altar, the shop) and the one role that cannot demolish a wall, so where
//! it is posted for the night is a survivability decision rather than a
//! distance one. `night::pioneer_post` makes it, by swapping two controllers —
//! a permutation, so a gun can never come out of it unmanned.
//!
//! These tests assert the POST: which gun the pioneer ends up holding, by id,
//! on boards where the greedy pass demonstrably hands it the other one. The
//! door half of the same item lives in `brain::day::tests`, where the rescue
//! cut is made.
//!
//! Every board below puts the enemy station at (38,24), so exposure is one cell
//! per column of `x`: three columns is the margin at which two posts stop being
//! inside the same robots' reach (任务书 4.7.2 gives every robot class an attack
//! distance of 3), and two columns is a tie.

use serde_json::{json, Value};

use coregeek::brain::night;
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn unit(id: i64, kind: &str, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": kind,
        "health": 220, "attackPower": 10, "attackRange": 3,
        "level": 1, "backPackCapability": 100, "backpack": []
    })
}

fn world(ours: Vec<Value>) -> Value {
    json!({
        "roundNo": 85, // night of day 1: pairing runs, the wall crew does not
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [], "roles": ours
        },
        "teamEnemy": {"roles": [unit(20001, "station", 38, 24)]},
        "robot": {"roles": []},
    })
}

/// The gun `role` is holding tonight, as an id.
fn post_of(pairs: &[(i64, i64)], role: i64) -> Option<i64> {
    pairs
        .iter()
        .find(|(controller, _)| *controller == role)
        .map(|(_, tower)| *tower)
}

/// A gun at (9,22) and a gun at (12,25), one worker beside each and the pioneer
/// beside the exposed one. Every controller is one cell from its gun, so the
/// greedy pass is unambiguous: it hands each gun the role standing on it. The
/// two posts are three columns and three rows apart — 29 cells from the enemy
/// against 26 — so this is a safety decision and not a tie.
#[test]
fn the_pioneer_is_posted_on_the_gun_furthest_from_the_enemy() {
    let turn = turn_from(world(vec![
        unit(10001, "station", 10, 24),
        unit(10020, "gatling", 9, 22),  // 29 cells from the enemy
        unit(10030, "gatling", 12, 25), // 26: the exposed one
        unit(10002, "worker", 9, 23),
        unit(10004, "pioneer", 12, 26),
    ]));
    let state = BotState::default();
    let pairs = night::pairing(&turn, &state);

    assert_eq!(
        post_of(&pairs, 10004),
        Some(10020),
        "the pioneer was left on the gun nearest the enemy bearing: {pairs:?}"
    );
    // The swap is a permutation: the worker inherits the gun the pioneer left,
    // so both guns are manned and neither controller is dropped.
    assert_eq!(
        post_of(&pairs, 10002),
        Some(10030),
        "the displaced controller did not take the pioneer's old gun: {pairs:?}"
    );
    assert_eq!(pairs.len(), 2, "a gun came out of the swap unmanned");
    assert!(
        night::unpaired_towers(&turn, &state).is_empty(),
        "the swap left a tower without a controller"
    );
}

/// The long-range half of the same sentence: 「让他操作可远程攻击的导弹」. Both
/// guns here are in the same safety band — 26 cells from the enemy either way —
/// so the reach decides, and the rocket reaches 10 / 15 / 全图.
#[test]
fn the_pioneer_takes_the_rocket_when_one_is_in_the_same_safety_band() {
    let turn = turn_from(world(vec![
        unit(10001, "station", 10, 24),
        unit(10020, "rocket", 12, 22), // 26, and 全图 at level 3
        unit(10030, "gatling", 12, 25), // 26
        unit(10002, "worker", 12, 23),
        unit(10004, "pioneer", 12, 26),
    ]));
    let state = BotState::default();
    let pairs = night::pairing(&turn, &state);

    assert_eq!(
        post_of(&pairs, 10004),
        Some(10020),
        "the pioneer was posted on the short-range gun with a missile free: {pairs:?}"
    );
    assert_eq!(post_of(&pairs, 10002), Some(10030), "{pairs:?}");
}

/// …and the reach does not outrank the shelter. A rocket 15 cells from the
/// enemy is a rocket the pioneer may not live long enough to fire: it is moved
/// off it onto the short-range gun 13 columns further back, which is the
/// opposite of the test above and the reason the two questions are asked in
/// that order.
#[test]
fn a_rocket_inside_the_robots_reach_does_not_hold_the_pioneer() {
    let turn = turn_from(world(vec![
        unit(10001, "station", 10, 24),
        unit(10020, "rocket", 22, 24), // 15: in the robots' reach
        unit(10030, "gatling", 9, 24), // 28
        unit(10002, "worker", 9, 23),
        unit(10004, "pioneer", 22, 23),
    ]));
    let state = BotState::default();
    let pairs = night::pairing(&turn, &state);

    assert_eq!(
        post_of(&pairs, 10004),
        Some(10030),
        "the pioneer was left on a rocket standing in the robots' reach: {pairs:?}"
    );
    assert_eq!(post_of(&pairs, 10002), Some(10020), "{pairs:?}");
}

/// The same board always yields the same post. Two of the three guns here tie on
/// safety (29 cells from the enemy) and on reach, so the choice between them is
/// settled by `(x, y)` — the codebase's usual last word — and not by whichever
/// order the pairing happened to walk them in. Run eight times, because a
/// ranking that leaked a `HashMap` iteration order would pass once.
#[test]
fn the_post_is_the_same_on_every_run() {
    let turn = turn_from(world(vec![
        unit(10001, "station", 10, 24),
        unit(10020, "gatling", 9, 22),  // 29
        unit(10030, "gatling", 12, 25), // 26
        unit(10040, "gatling", 9, 25),  // 29, the tie
        unit(10002, "worker", 9, 23),
        unit(10006, "worker", 9, 26),
        unit(10004, "pioneer", 12, 26),
    ]));
    let state = BotState::default();
    let first = night::pairing(&turn, &state);
    for _ in 0..8 {
        assert_eq!(night::pairing(&turn, &state), first, "the pairing moved");
    }
    assert_eq!(
        post_of(&first, 10004),
        Some(10020),
        "of two equally safe posts the pioneer took the later one: {first:?}"
    );
}
