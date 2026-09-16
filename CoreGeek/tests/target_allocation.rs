//! The round's target allocation (issue #206 §3b): 「每个人攻击谁，能最快实现将
//! 敌人消灭，不要重叠攻击，提升攻击效率，都需要提前根据战场形式计算好」.
//!
//! The shared `Sim` already stopped a tower from shooting a corpse an earlier
//! tower made — but only a corpse. Each tower still answered "which robot?"
//! alone, against a simulation the tower before it had already changed, and the
//! answer it gave was locally best and globally wrong: with two 5 HP robots and
//! a 15 HP one, the first gun takes the robot the SECOND gun could also have
//! taken, and the robot only the first gun could reach survives the round
//! untouched. `combat::plan_round` asks the question once, for every tower that
//! will fire, and the round fires the answer.
//!
//! These tests assert the ASSIGNMENT the bot sends and what it does to the
//! board, never that a planner exists.
//!
//! The board below is the owner's own example: three towers of 10 damage
//! against a 15 HP robot and a 20 HP robot. Only one of them can die this
//! round, and the question is which, and with how much left over.

use serde_json::{json, Value};

use coregeek::brain::combat::{choose_attack_kind, init_sim, plan_round};
use coregeek::brain::coach::Policy;
use coregeek::brain::night;
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

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

fn worker(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "worker",
        "health": 220, "attackPower": 10, "attackRange": 3,
        "level": 1, "backPackCapability": 100, "backpack": []
    })
}

fn robot(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "smallRobot",
        "health": hp, "abnormalState": "", "targetTeam": "challenger"
    })
}

fn world(roles: Vec<Value>, robots: Vec<Value>) -> Value {
    json!({
        "roundNo": 85, // night of day 1: the towers are manned, the crew is home
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [], "roles": roles
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": robots},
    })
}

/// Three towers in a column, a worker beside each, and two robots in front of
/// them with a clear line from every tower to both (the only thing a bullet is
/// intercepted by is a robot — see `combat`'s module note — and no robot stands
/// on another's line here). So every assignment of towers to robots is
/// expressible, which is what makes the owner's example a fair question.
fn owners_board() -> Turn {
    turn_from(world(
        vec![
            gatling(10020, 10, 10),
            gatling(10021, 10, 12),
            gatling(10022, 10, 14),
            worker(10010, 10, 9),
            worker(10011, 10, 11),
            worker(10012, 10, 13),
        ],
        vec![
            robot(30001, 11, 11, 15), // A: 15 HP
            robot(30002, 13, 11, 20), // B: 20 HP
        ],
    ))
}

/// The cells every attack command in the round aimed at, in tower order.
fn shots(turn: &Turn) -> Vec<Pos> {
    let mut state = BotState::default();
    let plan = night::plan(turn, &mut state);
    let mut fired: Vec<(i64, Vec<Pos>)> = plan
        .commands
        .iter()
        .filter(|(_, cmd)| cmd.action == "attack")
        .map(|(tower, cmd)| (*tower, cmd.targetPos.clone().unwrap_or_default()))
        .collect();
    fired.sort_by_key(|(tower, _)| *tower);
    fired.into_iter().flat_map(|(_, targets)| targets).collect()
}

fn a() -> Pos {
    Pos { x: 11, y: 11 }
}
fn b() -> Pos {
    Pos { x: 13, y: 11 }
}

/// Ten damage a tower: the owner's example, exactly.
const SHOT: i64 = 10;

/// A robot's health after the round, from the assignment alone: every line on
/// this board is clear, so a shot aimed at a cell lands on the robot standing
/// there. This is the outcome the owner is asking about — who is dead, and who
/// is left standing and how badly hurt.
fn left_standing(hp: i64, cell: Pos, shots: &[Pos]) -> i64 {
    hp - shots.iter().filter(|shot| **shot == cell).count() as i64 * SHOT
}

#[test]
fn the_owner_example_resolves_to_the_no_waste_allocation() {
    let turn = owners_board();
    let shots = shots(&turn);
    assert_eq!(shots.len(), 3, "three towers, three volleys: {shots:?}");

    // The 20 HP robot is killed EXACTLY — two volleys, 20 damage, nothing
    // spilled past its health bar — and the 15 HP robot is left at 5, one
    // volley short rather than overkilled.
    assert_eq!(
        left_standing(20, b(), &shots),
        0,
        "expected the 20 HP robot to be killed with exactly 20 damage, got shots {shots:?}"
    );
    assert_eq!(
        left_standing(15, a(), &shots),
        5,
        "expected the 15 HP robot to be left standing at 5, got shots {shots:?}"
    );
    // …which is also the most any assignment can do here: 30 damage against 35
    // HP cannot kill both, and this is the only 1-kill allocation that spills
    // nothing. The naive per-tower pass finished the 15 HP robot with two
    // volleys (5 spilled) and left the 20 HP robot at 10.
    assert_eq!(
        shots.iter().filter(|shot| **shot == b()).count(),
        2,
        "the 20 HP robot takes two volleys: {shots:?}"
    );
    assert_eq!(
        shots.iter().filter(|shot| **shot == a()).count(),
        1,
        "the 15 HP robot takes one: {shots:?}"
    );
}

/// The round is planned, not guessed: the same board gives the same assignment
/// every time, down to the cell. Eight runs, because a ranking that leaked a
/// `HashMap` iteration order would pass once.
#[test]
fn the_assignment_is_the_same_on_every_run() {
    let turn = owners_board();
    let first = shots(&turn);
    assert_eq!(first.len(), 3);
    for _ in 0..8 {
        assert_eq!(shots(&turn), first, "the round's assignment moved");
    }
}

/// A round that already spills nothing is not churned. Two towers and one
/// 20 HP robot: the pair kills it exactly, so there is no better assignment to
/// find and the plan must leave the round alone — the free pass is scored
/// first and only a strict improvement displaces it.
#[test]
fn a_round_that_already_wastes_nothing_is_left_alone() {
    let turn = turn_from(world(
        vec![
            gatling(10020, 10, 10),
            gatling(10021, 10, 12),
            worker(10010, 10, 9),
            worker(10011, 10, 11),
        ],
        vec![robot(30002, 13, 11, 20)],
    ));
    let towers: Vec<(i64, i64)> = turn.towers().iter().map(|t| (t.id, t.id)).collect();
    assert_eq!(towers.len(), 2);
    assert!(
        plan_round(&Policy::committed(), &turn, &towers).is_empty(),
        "an already-optimal round was re-planned"
    );

    // …and the round itself still lands both volleys on the robot.
    let shots = shots(&turn);
    assert_eq!(shots, vec![b(), b()], "both towers fire at the robot");
    assert_eq!(left_standing(20, b(), &shots), 0);
}

/// One tower cannot overlap with anybody, so there is no assignment to make and
/// the plan must not spend a round's worth of simulations finding that out.
#[test]
fn a_single_tower_round_is_never_re_planned() {
    let turn = turn_from(world(
        vec![gatling(10020, 10, 10), worker(10010, 10, 9)],
        vec![robot(30001, 11, 11, 15), robot(30002, 13, 11, 20)],
    ));
    let towers: Vec<(i64, i64)> = turn.towers().iter().map(|t| (t.id, t.id)).collect();
    assert_eq!(towers.len(), 1);
    assert!(plan_round(&Policy::committed(), &turn, &towers).is_empty());

    // And the lone tower still fires the way it always did.
    let tower = turn.role_by_id(10020).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack_kind(&turn, tower, &mut sim).expect("the volley exists");
    assert_eq!(targets.0.len(), 1, "{targets:?}");
    assert!(
        targets.0[0] == a() || targets.0[0] == b(),
        "a lone tower shoots one of the two robots: {targets:?}"
    );
}

/// The absolute priority is untouched by the plan: when the enemy station dies
/// this round, every tower spends its volley on the station footprint and the
/// assignment has nothing to say — `simulate_round` faces the same choice the
/// round does, so the two agree and the round is not re-planned.
#[test]
fn a_killable_station_outranks_the_assignment() {
    let turn = turn_from(json!({
        "roundNo": 85,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0, "playerTasks": [],
            "roles": [
                {"id": 10040, "pos": {"x": 20, "y": 20}, "roleType": "rocket",
                 "health": 1000, "attackPower": 20, "attackRange": 15, "level": 1,
                 "backPackCapability": 0, "backpack": []},
                {"id": 10041, "pos": {"x": 20, "y": 22}, "roleType": "rocket",
                 "health": 1000, "attackPower": 20, "attackRange": 15, "level": 1,
                 "backPackCapability": 0, "backpack": []},
                {"id": 10010, "pos": {"x": 20, "y": 19}, "roleType": "worker",
                 "health": 220, "attackPower": 10, "attackRange": 3, "level": 1,
                 "backPackCapability": 100, "backpack": []},
                {"id": 10011, "pos": {"x": 20, "y": 21}, "roleType": "worker",
                 "health": 220, "attackPower": 10, "attackRange": 3, "level": 1,
                 "backPackCapability": 100, "backpack": []}
            ]
        },
        "teamEnemy": {"roles": [
            {"id": 20001, "pos": {"x": 30, "y": 24}, "roleType": "station",
             "health": 40, "attackPower": 0, "attackRange": 0, "level": 1,
             "backPackCapability": 0, "backpack": []}
        ]},
        "robot": {"roles": [
            {"id": 30001, "pos": {"x": 25, "y": 22}, "roleType": "smallRobot",
             "health": 20, "abnormalState": "", "targetTeam": "challenger"}
        ]},
    }));
    let footprint = coregeek::model::station_footprint(Pos { x: 30, y: 24 });
    let shots = shots(&turn);
    assert_eq!(shots.len(), 2, "both rockets fire: {shots:?}");
    assert!(
        shots.iter().all(|shot| footprint.contains(shot)),
        "the station that dies this round is the shot, not the robot: {shots:?}"
    );
}
