//! Rule 2 — our towers may shoot the opponent's WALLS — and the wall-break
//! tactic built on top of it.
//!
//! The owner's words: "可以操作我方火箭弹打对方围墙". Mechanically the guns have
//! always been able to (`turn.enemy` carries their walls and
//! `enemy_unit_value_with` prices `UnitKind::Wall`), but the ordering made it
//! practically unreachable — operator 600 > role 400 > tower 300 > station 200
//! > wall 60, with `station_focus` on by default. The first test pins the
//! reachability ("a wall CAN be the chosen target"), and the rest pin the
//! tactic that makes it happen on purpose.
//!
//! The tactic (P1): at night the robots attack BOTH bases, and in our own
//! matches their base fell to robots more often than to our guns. Their wall
//! line is the only thing that stops that wave (任务书 4.7.3, 2.2: a demolished
//! building leaves passable ground), so one hole in the ring turns the wall the
//! robots have been chewing on all night into a door — and 任务书 ch.7 decides
//! the half by which base falls first. That is worth far more than the same
//! volley chipping their station.
//!
//! Gate (all four, see `wall_breach_with`): firepower to spare, a robot
//! marching on THEM, their station damaged or ours not under the knife, and a
//! wall in reach that this volley actually covers. `station_killable` keeps its
//! absolute priority over all of it.

use serde_json::{json, Value};

use coregeek::brain::combat::{
    choose_attack, choose_attack_kind, enemy_station_damaged, enemy_unit_score_with, init_sim,
    spare_firepower, station_killable, station_safe, wall_breach, wall_breach_with, TargetKind,
    Weights,
};
use coregeek::model::{chebyshev, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn station(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "station",
        "health": hp, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn gatling(id: i64, x: i32, y: i32, level: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "gatling",
        "health": 1000, "attackPower": 10, "attackRange": 3,
        "level": level, "backPackCapability": 0, "backpack": []
    })
}

fn rocket(id: i64, x: i32, y: i32, level: i64, attack_range: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "rocket",
        "health": 1000, "attackPower": 20, "attackRange": attack_range,
        "level": level, "backPackCapability": 0, "backpack": []
    })
}

/// One of OUR walls — never a legal target, whatever the dial says.
fn our_wall(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "wall",
        "health": hp, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn enemy(id: i64, role: &str, x: i32, y: i32, hp: i64, level: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": role,
        "health": hp, "attackPower": 0, "attackRange": 0,
        "level": level, "backPackCapability": 0, "backpack": []
    })
}

fn robot(id: i64, x: i32, y: i32, hp: i64, team: &str) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "smallRobot",
        "health": hp, "abnormalState": "", "targetTeam": team
    })
}

fn world(our_roles: Vec<Value>, enemy_roles: Vec<Value>, robots: Vec<Value>) -> Turn {
    turn_from(json!({
        "roundNo": 85,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [], "roles": our_roles
        },
        "teamEnemy": {"roles": enemy_roles},
        "robot": {"roles": robots},
    }))
}

/// The board the tactic is written for. Our L2 rocket (two missiles, 40 damage
/// a volley) sits at (10,20) with the base behind it at (10,24); the opponent's
/// ring is one volley from opening at (14,20) and their station at (24,20) is
/// already hurt and inside our reach. `robots` and the two HP values are what
/// the tests vary — everything else is the same committed situation.
fn breach_board(robots: Vec<Value>, station_hp: i64, wall_hp: i64) -> Turn {
    world(
        vec![station(10001, 10, 24, 1500), rocket(10040, 10, 20, 2, 15)],
        vec![
            enemy(20001, "station", 24, 20, station_hp, 1),
            enemy(20030, "wall", 14, 20, wall_hp, 1),
        ],
        robots,
    )
}

/// A robot walking at THEIR base — the wave a breach would let in. Far enough
/// from our rocket (chebyshev 25 > range 15) that it is never shot at itself.
fn marching_on_them() -> Vec<Value> {
    vec![robot(30001, 35, 8, 40, "defender")]
}

// ---------------------------------------------------------------------------
// Rule 2: a wall is a legal, and sometimes the best, target
// ---------------------------------------------------------------------------

#[test]
fn a_tower_volley_can_pick_an_enemy_wall_when_it_is_the_best_target() {
    // No robots hunting us (the "有余力时" precondition), their station out of
    // every gun's reach: the wall is what is left, and it is worth a volley.
    let turn = world(
        vec![station(10001, 10, 24, 1500), gatling(10020, 10, 10, 2)],
        vec![
            enemy(20030, "wall", 12, 10, 200, 1),
            enemy(20001, "station", 38, 4, 1500, 1),
        ],
        vec![],
    );
    assert!(spare_firepower(&turn), "nothing hunting us is uncovered");
    let tower = turn.role_by_id(10020).unwrap();
    assert!(
        turn.enemy
            .iter()
            .find(|unit| unit.id == 20001)
            .unwrap()
            .footprint()
            .iter()
            .all(|cell| chebyshev(tower.pos, *cell) > tower.range_of_attack()),
        "the station must really be out of range for this test to mean anything"
    );

    let mut sim = init_sim(&turn);
    let (targets, kind) = choose_attack_kind(&turn, tower, &mut sim).expect("the wall is shootable");
    assert_eq!(kind, TargetKind::EnemyAssets);
    assert_eq!(
        targets,
        vec![Pos { x: 12, y: 10 }, Pos { x: 12, y: 10 }],
        "both bullets of the level-2 gatling go into the wall"
    );
}

// ---------------------------------------------------------------------------
// The breach gate
// ---------------------------------------------------------------------------

#[test]
fn the_breach_gate_reads_the_board() {
    let turn = breach_board(marching_on_them(), 900, 40);
    let tower = turn.role_by_id(10040).unwrap();
    let sim = init_sim(&turn);
    assert!(
        enemy_station_damaged(&turn, &sim),
        "900 of 1500 is a station already hurt"
    );
    assert!(station_safe(&turn), "nothing hunting us is in reach of the base");
    assert!(
        wall_breach(&turn, tower, &sim),
        "hurt station, safe base, a robot walking at them, wall one volley from opening"
    );

    // Same board, no robot marching on them: a hole nobody walks through is
    // just a wall waiting to be rebuilt.
    let quiet = breach_board(vec![], 900, 40);
    assert!(station_safe(&quiet));
    assert!(!wall_breach(
        &quiet,
        quiet.role_by_id(10040).unwrap(),
        &init_sim(&quiet)
    ));

    // Their station untouched AND a robot hunting us standing on the base: the
    // volley belongs at home.
    let pressed = breach_board(vec![robot(30001, 10, 26, 40, "challenger")], 1500, 40);
    let pressed_sim = init_sim(&pressed);
    assert!(!enemy_station_damaged(&pressed, &pressed_sim), "full HP station");
    assert!(
        !station_safe(&pressed),
        "a hunter three cells off the footprint can already be shooting the base"
    );
    assert!(!wall_breach(
        &pressed,
        pressed.role_by_id(10040).unwrap(),
        &pressed_sim
    ));

    // A wall the volley cannot open leaves the gate shut on its own: no wall
    // HP on this board is inside 40 damage.
    let solid = breach_board(marching_on_them(), 900, 900);
    assert!(!wall_breach(
        &solid,
        solid.role_by_id(10040).unwrap(),
        &init_sim(&solid)
    ));
}

#[test]
fn the_wall_breach_dial_turns_the_tactic_off_and_prices_the_wall() {
    let turn = breach_board(marching_on_them(), 900, 40);
    let tower = turn.role_by_id(10040).unwrap();
    let sim = init_sim(&turn);

    assert_eq!(
        Weights::default().wall_breach_value,
        320,
        "above tower_value (300) and station_value (200), below role_value (400)"
    );
    assert!(wall_breach_with(&Weights::default(), &turn, tower, &sim));
    assert!(
        !wall_breach_with(
            &Weights {
                wall_breach_value: 0,
                ..Weights::default()
            },
            &turn,
            tower,
            &sim
        ),
        "0 restores the committed ordering exactly"
    );

    let parsed = Weights::from_lookup(|name| match name {
        "WALL_BREACH" => Some("0".to_string()),
        _ => None,
    });
    assert_eq!(parsed.wall_breach_value, 0, "CG_TUNE_WALL_BREACH is the dial");

    // ...and it is a price in the ordinary ranking, not a flag: the same wall
    // outranks the station at the default and loses to it at zero.
    let wall = turn.enemy.iter().find(|unit| unit.id == 20030).unwrap();
    let station = turn.enemy.iter().find(|unit| unit.id == 20001).unwrap();
    let wall_cells = vec![wall.pos];
    let station_cells: Vec<Pos> = station
        .footprint()
        .into_iter()
        .filter(|cell| chebyshev(tower.pos, *cell) <= tower.range_of_attack())
        .collect();
    for (price, expect_wall_first) in [(320, true), (0, false)] {
        let dial = Weights {
            wall_value: price,
            ..Weights::default()
        };
        let wall_score = enemy_unit_score_with(&dial, &turn, tower, wall, &wall_cells, &sim);
        let station_score =
            enemy_unit_score_with(&dial, &turn, tower, station, &station_cells, &sim);
        assert_eq!(
            wall_score > station_score,
            expect_wall_first,
            "wall {wall_score} vs station {station_score} at wall_value {price}"
        );
    }
}

// ---------------------------------------------------------------------------
// The tactic firing
// ---------------------------------------------------------------------------

#[test]
fn a_spare_volley_opens_a_breach_instead_of_chipping_their_station() {
    // Without the tactic this exact board fires at the station: it is damaged
    // and in range, so `station_focus` claims the volley (that is the committed
    // behaviour, and `the_dial_off_...` below checks it is still reachable).
    // With it, the 40-damage volley lands whole on the 40-HP wall and their
    // ring is open.
    let turn = breach_board(marching_on_them(), 900, 40);
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    assert!(spare_firepower(&turn));
    assert!(!station_killable(&turn, &sim), "their station survives this round");

    let (targets, kind) = choose_attack_kind(&turn, tower, &mut sim).expect("a target exists");
    assert_eq!(kind, TargetKind::EnemyAssets);
    assert_eq!(
        targets,
        vec![Pos { x: 14, y: 20 }, Pos { x: 14, y: 20 }],
        "the whole volley goes into the breach cell, not one missile at it and one at the station"
    );
    let wall = turn.enemy.iter().find(|unit| unit.id == 20030).unwrap();
    assert_eq!(sim.building_hp(wall), 0, "the wall is down in the simulation");
}

#[test]
fn a_wall_this_volley_cannot_open_is_not_a_breach() {
    // 40 damage into a 900-HP wall is a scratch. The gate says so, so the
    // committed station pressure still owns the volley.
    let turn = breach_board(marching_on_them(), 900, 900);
    let tower = turn.role_by_id(10040).unwrap();
    assert!(!wall_breach(&turn, tower, &init_sim(&turn)));

    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).expect("the station is still a target");
    let footprint = station_footprint(Pos { x: 24, y: 20 });
    assert!(
        targets.iter().all(|cell| footprint.contains(cell)),
        "expected the station footprint {footprint:?}, got {targets:?}"
    );
}

#[test]
fn a_killable_station_still_outranks_the_breach() {
    // Two rockets, 40 damage each: 80 into their 40-HP station is lethal, so
    // the half is won this round. That is a rule of the game and keeps its
    // absolute priority — the breach waits.
    let turn = world(
        vec![
            station(10001, 10, 24, 1500),
            rocket(10040, 10, 19, 2, 15),
            rocket(10041, 10, 21, 2, 15),
        ],
        vec![
            enemy(20001, "station", 24, 20, 40, 1),
            enemy(20030, "wall", 14, 20, 40, 1),
        ],
        marching_on_them(),
    );
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    assert!(station_killable(&turn, &sim));
    assert!(
        wall_breach(&turn, tower, &sim),
        "the breach gate is open on this board — the station rule is what must win"
    );

    let targets = choose_attack(&turn, tower, &mut sim).expect("the station is the shot");
    let footprint = station_footprint(Pos { x: 24, y: 20 });
    assert!(
        targets.iter().all(|cell| footprint.contains(cell)),
        "expected the station footprint {footprint:?}, got {targets:?}"
    );
}

#[test]
fn the_breach_waits_but_the_station_does_not() {
    // The robot coming for us is 18 cells from the only gun, so there is no
    // firepower to spare — the breach gate still holds (issue #7's "有余力时"
    // applies to the breach tactic specifically). But offense-first
    // (issues #201-#205) means the rocket fires at their station instead of
    // sitting idle: the tower was going to do nothing anyway, and 20 damage
    // on the enemy station is permanent progress toward the win condition.
    let mut robots = marching_on_them();
    robots.push(robot(30002, 2, 2, 40, "challenger"));
    let turn = breach_board(robots, 900, 40);
    let tower = turn.role_by_id(10040).unwrap();
    assert!(!spare_firepower(&turn));
    assert!(!wall_breach(&turn, tower, &init_sim(&turn)));

    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).expect("the rocket fires at the enemy station");
    let footprint = station_footprint(Pos { x: 24, y: 20 });
    assert!(
        targets.iter().all(|cell| footprint.contains(cell)),
        "expected the enemy station footprint {footprint:?}, got {targets:?}"
    );
}

#[test]
fn our_own_walls_are_never_a_breach_target() {
    // A friendly wall is the closest thing in range and is still not a
    // candidate: the ranking walks `turn.enemy` only.
    let turn = world(
        vec![
            station(10001, 10, 24, 1500),
            rocket(10040, 10, 20, 2, 15),
            our_wall(10030, 11, 20, 300),
        ],
        vec![
            enemy(20001, "station", 24, 20, 900, 1),
            enemy(20030, "wall", 14, 20, 40, 1),
        ],
        marching_on_them(),
    );
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).expect("their wall is the shot");
    assert_eq!(targets, vec![Pos { x: 14, y: 20 }, Pos { x: 14, y: 20 }]);
    assert!(
        !targets.contains(&Pos { x: 11, y: 20 }),
        "our own wall at (11,20) must never be fired at"
    );
}
