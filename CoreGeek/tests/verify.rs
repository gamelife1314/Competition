//! Attack-result verification: joining the judger's verdict on last night's
//! volleys with the robot HP that round left behind.

use std::collections::HashMap;

use serde_json::{json, Value};

use coregeek::brain::verify::{review_volley, VolleyResult};
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request};
use coregeek::state::IssuedCmd;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn gatling(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "gatling",
        "health": 1000, "attackPower": 10, "attackRange": 3,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn station(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "station",
        "health": hp, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn robot(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "smallRobot",
        "health": hp, "abnormalState": "", "targetTeam": "challenger"
    })
}

/// A night round: what we sent last round (`results`), what the board looks
/// like now (`robots`), and the opponent's units. Our own station is always on
/// the board — a real match never lacks one, and the review reports its HP.
fn world(results: Value, robots: Vec<Value>, enemy: Vec<Value>) -> Value {
    json!({
        "roundNo": 85,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [],
            "roles": [station(10001, 5, 5, 1500), gatling(10020, 10, 10)]
        },
        "teamEnemy": {"roles": enemy},
        "robot": {"roles": robots},
        "lastRoundRoleActionResults": results,
    })
}

fn attack_order(tower: i64, x: i32, y: i32) -> (i64, IssuedCmd) {
    (
        tower,
        IssuedCmd {
            action: "attack".into(),
            target: Some(Pos { x, y }),
            name: None,
        },
    )
}

#[test]
fn accepted_volley_is_reported_as_executed() {
    let turn = turn_from(world(
        json!({"10020": true}),
        vec![robot(30001, 12, 10, 25)],
        vec![],
    ));
    let issued: HashMap<i64, IssuedCmd> = [attack_order(10020, 12, 10)].into_iter().collect();
    let prev: HashMap<i64, i64> = [(30001, 40)].into_iter().collect();

    let review = review_volley(&turn, &issued, &prev);
    assert_eq!(review.volleys.len(), 1);
    assert_eq!(review.volleys[0].tower, 10020);
    assert_eq!(review.volleys[0].result, VolleyResult::Executed);
    assert_eq!(review.volleys[0].target, Some(Pos { x: 12, y: 10 }));
    assert!(review.rejected_towers().is_empty());
    assert_eq!(review.robot_damage(), 15);
    assert_eq!(review.robots_damaged, vec![(30001, 15)]);
    assert!(!review.no_robot_damage_round());
}

#[test]
fn rejected_volley_is_flagged_with_its_tower() {
    let turn = turn_from(world(
        json!({"10020": false}),
        vec![robot(30001, 12, 10, 40)],
        vec![],
    ));
    let issued: HashMap<i64, IssuedCmd> = [attack_order(10020, 12, 10)].into_iter().collect();
    let prev: HashMap<i64, i64> = [(30001, 40)].into_iter().collect();

    let review = review_volley(&turn, &issued, &prev);
    assert_eq!(review.rejected_towers(), vec![10020]);
    assert_eq!(review.executed(), 0);
    // A rejection explains the missing damage: this is not a dry volley, it is
    // an illegal one, and the two failures need different fixes.
    assert!(!review.no_robot_damage_round());
}

#[test]
fn dry_round_is_flagged_when_an_accepted_volley_takes_no_hp() {
    let turn = turn_from(world(
        json!({"10020": true}),
        vec![robot(30001, 12, 10, 40)],
        vec![],
    ));
    let issued: HashMap<i64, IssuedCmd> = [attack_order(10020, 12, 10)].into_iter().collect();
    let prev: HashMap<i64, i64> = [(30001, 40)].into_iter().collect();

    let review = review_volley(&turn, &issued, &prev);
    assert!(review.no_robot_damage_round());
    assert_eq!(review.robot_damage(), 0);
}

#[test]
fn missing_report_is_unreported_and_never_claims_a_dry_round() {
    // No `lastRoundRoleActionResults` at all: the judger is silent, so nothing
    // about this round can be concluded.
    let turn = turn_from(world(json!({}), vec![robot(30001, 12, 10, 40)], vec![]));
    let issued: HashMap<i64, IssuedCmd> = [attack_order(10020, 12, 10)].into_iter().collect();
    let prev: HashMap<i64, i64> = [(30001, 40)].into_iter().collect();

    let review = review_volley(&turn, &issued, &prev);
    assert_eq!(review.volleys[0].result, VolleyResult::Unreported);
    assert_eq!(review.unreported(), 1);
    assert!(review.rejected_towers().is_empty());
    assert!(!review.no_robot_damage_round());
}

#[test]
fn a_kill_counts_even_when_the_robot_is_gone_from_the_board() {
    let turn = turn_from(world(
        json!({"10020": true}),
        vec![robot(30002, 12, 11, 40)],
        vec![],
    ));
    let issued: HashMap<i64, IssuedCmd> = [attack_order(10020, 12, 10)].into_iter().collect();
    let prev: HashMap<i64, i64> = [(30001, 40), (30002, 40)].into_iter().collect();

    let review = review_volley(&turn, &issued, &prev);
    assert_eq!(review.kills, vec![30001]);
    assert!(review.robots_damaged.is_empty());
    assert!(!review.no_robot_damage_round());
}

#[test]
fn damage_is_totalled_across_robots_and_ignores_healed_ones() {
    let turn = turn_from(world(
        json!({"10020": true}),
        vec![robot(30001, 12, 10, 10), robot(30002, 12, 11, 60)],
        vec![],
    ));
    let issued: HashMap<i64, IssuedCmd> = [attack_order(10020, 12, 10)].into_iter().collect();
    let prev: HashMap<i64, i64> = [(30001, 40), (30002, 40)].into_iter().collect();

    let review = review_volley(&turn, &issued, &prev);
    // 30001 lost 30; 30002 gained HP (a heal or a fresh spawn with the same id)
    // and is not damage we dealt.
    assert_eq!(review.robots_damaged, vec![(30001, 30)]);
    assert_eq!(review.robot_damage(), 30);
}

#[test]
fn enemy_station_hp_is_exposed_for_the_win_condition() {
    let turn = turn_from(world(
        json!({"10020": true}),
        vec![robot(30001, 12, 10, 25)],
        vec![station(20001, 30, 20, 900)],
    ));
    let issued: HashMap<i64, IssuedCmd> = [attack_order(10020, 12, 10)].into_iter().collect();
    let prev: HashMap<i64, i64> = [(30001, 40)].into_iter().collect();

    let review = review_volley(&turn, &issued, &prev);
    assert_eq!(review.enemy_station_hp, Some(900));
    assert_eq!(review.station_hp, Some(1500));
    assert_eq!(review.summary()["enemyStationHp"], json!(900));
}

#[test]
fn non_attack_commands_are_not_volleys() {
    let turn = turn_from(world(
        json!({"10020": true}),
        vec![robot(30001, 12, 10, 25)],
        vec![],
    ));
    let issued: HashMap<i64, IssuedCmd> = [(
        10020,
        IssuedCmd {
            action: "move".into(),
            target: Some(Pos { x: 12, y: 10 }),
            name: None,
        },
    )]
    .into_iter()
    .collect();
    let prev: HashMap<i64, i64> = [(30001, 40)].into_iter().collect();

    let review = review_volley(&turn, &issued, &prev);
    assert!(review.volleys.is_empty());
    assert!(!review.no_robot_damage_round());
}

#[test]
fn volleys_are_reported_in_tower_order() {
    let turn = turn_from(world(
        json!({"10020": true, "10021": true}),
        vec![robot(30001, 12, 10, 25)],
        vec![],
    ));
    let mut issued: HashMap<i64, IssuedCmd> = HashMap::new();
    let (id, cmd) = attack_order(10021, 12, 10);
    issued.insert(id, cmd);
    let (id, cmd) = attack_order(10020, 12, 10);
    issued.insert(id, cmd);
    let prev: HashMap<i64, i64> = [(30001, 40)].into_iter().collect();

    let review = review_volley(&turn, &issued, &prev);
    assert_eq!(
        review
            .volleys
            .iter()
            .map(|volley| volley.tower)
            .collect::<Vec<_>>(),
        vec![10020, 10021]
    );
    assert_eq!(review.executed(), 2);
}
