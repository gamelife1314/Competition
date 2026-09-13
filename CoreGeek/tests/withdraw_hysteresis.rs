//! P1-4: withdrawal hysteresis.
//!
//! Issues #22/#23: a critically wounded controller was pulled off its gun by
//! `night_withdraw` — correctly — but re-entered the pairing the moment the
//! nearest robot stepped one cell outside the threat radius. It walked back
//! toward the gun, the robot re-entered the radius, and it was withdrawn
//! again: 30+ `controller_withdrawn` events of pacing per match, with the
//! tower silent in between and no heal ever landing.
//!
//! The holdout memory (`BotState::withdraw_holdout`, fed by
//! `update_withdraw_holdout`) keeps a withdrawn controller OUT of the pairing
//! until it has actually recovered (Medicine back above the withdraw line —
//! the spare-duty heal is what delivers it) or the threat has been gone for
//! `WITHDRAW_HYSTERESIS_ROUNDS` straight rounds.

use serde_json::{json, Value};

use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn worker(id: i64, x: i32, y: i32, hp: i64, pack: Vec<&str>) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "worker",
        "health": hp, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 100, "backpack": pack
    })
}

/// A night board (round 71+) with one gatling at (11,22), whose inner
/// operating cells are (10,22) and (12,22), and robots as given.
fn night_world(round: i64, roles: Vec<Value>, robots: Vec<Value>) -> Value {
    json!({
        "roundNo": round,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [],
            "roles": roles,
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": robots},
    })
}

fn station() -> Value {
    json!({
        "id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
        "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn gatling() -> Value {
    json!({
        "id": 30000, "pos": {"x": 11, "y": 22}, "roleType": "gatling",
        "health": 1000, "attackPower": 10, "attackRange": 3, "level": 1,
        "backPackCapability": 0, "backpack": []
    })
}

fn robot(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "smallRobot",
        "health": 40, "abnormalState": "", "targetTeam": "challenger"
    })
}

/// Wounded A beside the tower at (10,22), fit B beside it at (12,22).
fn crew(a_hp: i64, a_pack: Vec<&str>) -> Vec<Value> {
    vec![
        station(),
        gatling(),
        worker(10002, 10, 22, a_hp, a_pack),
        worker(10003, 12, 22, 220, vec![]),
    ]
}

#[test]
fn a_withdrawn_controller_stays_out_of_the_pairing_until_the_calm_holds() {
    let mut state = BotState::default();

    // R71: A is at 20 HP (< 30% of 220), no Medicine, robot two cells away —
    // the withdrawal rule owns it. B is fit and takes the gun.
    let turn = turn_from(night_world(71, crew(20, vec![]), vec![robot(30001, 10, 20)]));
    coregeek::brain::night::plan(&turn, &mut state);
    assert!(
        state.withdraw_holdout.contains(&10002),
        "the withdrawal rule marks the controller"
    );
    assert_eq!(
        state.night_pairs,
        vec![(10003, 30000)],
        "the wounded controller does not take a gun it would only be pulled from"
    );

    // The robot vanishes. Without hysteresis A would re-enter the pairing the
    // very next round and start pacing; with it, A stays out for four more
    // rounds and comes back only once the calm has held for five.
    for round in 72..=75 {
        let turn = turn_from(night_world(round, crew(20, vec![]), vec![]));
        coregeek::brain::night::plan(&turn, &mut state);
        assert_eq!(
            state.night_pairs,
            vec![(10003, 30000)],
            "round {round}: the calm has not held long enough yet"
        );
    }
    let turn = turn_from(night_world(76, crew(20, vec![]), vec![]));
    coregeek::brain::night::plan(&turn, &mut state);
    assert!(
        !state.withdraw_holdout.contains(&10002),
        "five calm rounds clear the holdout"
    );
    assert_eq!(
        state.night_pairs,
        vec![(10002, 30000)],
        "and the nearer controller mans the gun again"
    );
}

#[test]
fn a_potion_drunk_in_the_spare_duty_clears_the_holdout_faster() {
    let mut state = BotState::default();
    let turn = turn_from(night_world(71, crew(20, vec![]), vec![robot(30001, 10, 20)]));
    coregeek::brain::night::plan(&turn, &mut state);
    assert!(state.withdraw_holdout.contains(&10002));

    // R72: no robot around, but A is still wounded — holdout keeps it off the
    // gun, and the spare duty spends the round drinking the Medicine it now
    // carries instead of pacing back and forth.
    let turn = turn_from(night_world(72, crew(20, vec!["Medicine"]), vec![]));
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan
        .commands
        .get(&10002)
        .expect("the spare controller heals rather than idles");
    assert_eq!(cmd.action, "use");
    assert_eq!(cmd.name.as_deref(), Some("Medicine"));

    // R73: healed to full, A is out of the holdout and back on the gun the
    // same round — that is the whole point of the hysteresis: recover, then
    // man, with zero pacing rounds in between.
    let turn = turn_from(night_world(73, crew(220, vec![]), vec![]));
    coregeek::brain::night::plan(&turn, &mut state);
    assert!(
        !state.withdraw_holdout.contains(&10002),
        "recovered HP clears the holdout immediately"
    );
    assert_eq!(state.night_pairs, vec![(10002, 30000)]);
}
