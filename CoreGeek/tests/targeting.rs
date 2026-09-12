//! P2 item 4: the unified win-value objective for robots and enemy assets.
//!
//! These exercise `combat` directly: what a robot kill is worth (`score2`
//! plus the `score3` damage it would have done), how enemy assets are ranked
//! when no robot is in reach, the absolute priority of a station that can be
//! destroyed this round, the full-map reach of a level-3 rocket, and the
//! cross-tower damage reservation that stops two towers re-killing one target.

use serde_json::{json, Value};

use coregeek::brain::combat::{
    asset_damage_value, choose_attack, enemy_unit_score_with, init_sim, kill_score_value,
    rounds_to_contact, spare_firepower, station_killable, urgency_value, weights, win_value,
    win_value_with, Weights,
};
use coregeek::model::{station_footprint, Turn, UnitKind};
use coregeek::protocol::{Pos, Request};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
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

fn wall(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "wall",
        "health": hp, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

/// `role` is the `roleType` string used by the protocol (`station`, `gatling`,
/// `wall`, `worker`, `pioneer`, …).
fn enemy(id: i64, role: &str, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": role,
        "health": hp, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn small_robot(id: i64, x: i32, y: i32, hp: i64, team: &str) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "smallRobot",
        "health": hp, "abnormalState": "", "targetTeam": team
    })
}

fn big_robot(id: i64, x: i32, y: i32, hp: i64, team: &str) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "largeRobot",
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

// ---------------------------------------------------------------------------
// The objective itself
// ---------------------------------------------------------------------------

#[test]
fn a_robot_hunting_us_is_worth_more_than_one_hunting_the_other_team() {
    // Same class, same position — only the target team differs. The one coming
    // for us is worth its kill score plus the damage it would do to our line;
    // the one chasing the opponent is worth its kill score alone.
    let turn = world(
        vec![gatling(10020, 10, 10, 1)],
        vec![],
        vec![
            small_robot(30001, 12, 10, 40, "challenger"),
            small_robot(30002, 12, 10, 40, "defender"),
        ],
    );
    let hunter = &turn.robots[0];
    let other = &turn.robots[1];
    assert!(
        win_value(&turn, hunter) > win_value(&turn, other),
        "hunter {} vs other {}",
        win_value(&turn, hunter),
        win_value(&turn, other)
    );
    assert_eq!(
        asset_damage_value(&turn, other),
        0,
        "a robot hunting the other team does not chew on our base"
    );
    assert_eq!(urgency_value(&turn, other), 0);
}

#[test]
fn a_bigger_robot_class_is_worth_more_than_a_smaller_one() {
    let turn = world(
        vec![gatling(10020, 10, 10, 1)],
        vec![],
        vec![
            small_robot(30001, 12, 10, 40, "challenger"),
            big_robot(30002, 12, 10, 40, "challenger"),
        ],
    );
    assert_eq!(kill_score_value(&turn.robots[0]), 20, "small = 1 x 20");
    assert_eq!(kill_score_value(&turn.robots[1]), 80, "large = 4 x 20");
    assert!(win_value(&turn, &turn.robots[1]) > win_value(&turn, &turn.robots[0]));
}

#[test]
fn a_robot_touching_our_line_is_charged_urgency_and_asset_damage() {
    // R1 is meleeing our wall; R2 is the same class five cells out in the open.
    let turn = world(
        vec![gatling(10020, 10, 10, 1), wall(10030, 13, 10, 1000)],
        vec![],
        vec![
            small_robot(30001, 12, 10, 40, "challenger"),
            small_robot(30002, 8, 10, 40, "challenger"),
        ],
    );
    let melee = &turn.robots[0];
    let far = &turn.robots[1];
    assert_eq!(rounds_to_contact(&turn, melee), 0, "already on the wall");
    assert!(rounds_to_contact(&turn, far) > 0);
    assert!(urgency_value(&turn, melee) > 0, "damage lands this round");
    assert_eq!(urgency_value(&turn, far), 0);
    assert!(
        asset_damage_value(&turn, melee) > asset_damage_value(&turn, far),
        "the closer robot has more rounds left to hit us"
    );
}

#[test]
fn the_objective_prefers_the_robot_chewing_on_our_wall() {
    // One bullet, two one-shot targets: the kill score is identical, so the
    // asset-damage and urgency terms decide — and they point at the wall.
    let turn = world(
        vec![gatling(10020, 10, 10, 1), wall(10030, 13, 10, 1000)],
        vec![],
        vec![
            small_robot(30001, 8, 10, 10, "challenger"),
            small_robot(30002, 12, 10, 10, "challenger"),
        ],
    );
    let tower = turn.role_by_id(10020).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(
        targets,
        vec![Pos { x: 12, y: 10 }],
        "shoot the robot that is already eating the wall"
    );
}

// ---------------------------------------------------------------------------
// Enemy asset scoring
// ---------------------------------------------------------------------------

#[test]
fn operator_beside_a_tower_outranks_the_tower_itself() {
    // A visible enemy worker next to their gun is what makes that gun fire, so
    // it is worth more than the gun: silencing it disables the weapon.
    let turn = world(
        vec![rocket(10040, 10, 10, 1, 15)],
        vec![
            enemy(20020, "gatling", 12, 10, 1000),
            enemy(20010, "worker", 12, 11, 200),
        ],
        vec![],
    );
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(targets, vec![Pos { x: 12, y: 11 }], "the operator first");
}

#[test]
fn a_target_at_the_edge_of_our_reach_is_more_urgent_than_one_deep_inside_it() {
    // Two identical enemy walls. The far one is one step from leaving our
    // envelope; the near one will still be there next round. Kill probability
    // is the same for both, so the visibility window decides.
    let turn = world(
        vec![rocket(10040, 10, 10, 1, 15)],
        vec![
            enemy(20030, "wall", 12, 10, 200),
            enemy(20031, "wall", 24, 10, 200),
        ],
        vec![],
    );
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(
        targets,
        vec![Pos { x: 24, y: 10 }],
        "the wall about to step out of range is the one to hit now"
    );
}

#[test]
fn a_wall_is_not_worth_a_volley_that_barely_scratches_it() {
    // 10 damage into 1000 HP is a 1% kill probability on the least valuable
    // unit class: below the point where spending the volley is justified.
    let turn = world(
        vec![gatling(10020, 10, 10, 1)],
        vec![enemy(20030, "wall", 12, 10, 1000)],
        vec![],
    );
    let tower = turn.role_by_id(10020).unwrap();
    let mut sim = init_sim(&turn);
    assert_eq!(choose_attack(&turn, tower, &mut sim), None);
}

// ---------------------------------------------------------------------------
// Station priority
// ---------------------------------------------------------------------------

#[test]
fn the_enemy_station_is_shot_first_when_it_can_be_destroyed_this_round() {
    // Two rockets, 20 damage each: 40 into a 40 HP station is lethal, so the
    // half is won this round and the robot in front of them waits.
    let turn = world(
        vec![rocket(10040, 20, 20, 1, 15), rocket(10041, 20, 22, 1, 15)],
        vec![enemy(20001, "station", 30, 24, 40)],
        vec![small_robot(30001, 25, 22, 20, "challenger")],
    );
    assert!(station_killable(&turn, &init_sim(&turn)));
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    let footprint = station_footprint(Pos { x: 30, y: 24 });
    assert!(
        targets.iter().all(|target| footprint.contains(target)),
        "expected the station footprint {footprint:?}, got {targets:?}"
    );
}

#[test]
fn a_station_that_survives_the_round_is_not_a_priority() {
    // Same board, 1500 HP station: the volley cannot finish it, so the robot
    // in front of the towers is the right target.
    let turn = world(
        vec![rocket(10040, 20, 20, 1, 15), rocket(10041, 20, 22, 1, 15)],
        vec![enemy(20001, "station", 30, 24, 1500)],
        vec![small_robot(30001, 25, 22, 20, "challenger")],
    );
    assert!(!station_killable(&turn, &init_sim(&turn)));
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(targets, vec![Pos { x: 25, y: 22 }]);
}

#[test]
fn a_station_out_of_every_towers_reach_is_never_a_priority() {
    let turn = world(
        vec![gatling(10020, 10, 10, 1)],
        vec![enemy(20001, "station", 39, 30, 10)],
        vec![],
    );
    assert!(
        !station_killable(&turn, &init_sim(&turn)),
        "10 HP is irrelevant if no weapon can reach it"
    );
}

// ---------------------------------------------------------------------------
// Reach and reservation
// ---------------------------------------------------------------------------

#[test]
fn a_level_three_rocket_reaches_the_far_corner_of_the_map() {
    // The request reports the level-3 rocket's range as 2147483647; nothing on
    // the board may be filtered out as "too far".
    let turn = world(
        vec![rocket(10040, 0, 0, 3, 2147483647)],
        vec![],
        vec![small_robot(30001, 40, 31, 40, "defender")],
    );
    let tower = turn.role_by_id(10040).unwrap();
    assert_eq!(tower.range_of_attack(), coregeek::model::FULL_MAP_RANGE);
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(targets.len(), 3, "level 3 = three missiles");
    assert!(
        targets.iter().all(|target| *target == Pos { x: 40, y: 31 }),
        "{targets:?}"
    );
}

#[test]
fn a_level_three_rocket_reaches_it_through_the_range_table_too() {
    // Same weapon with no explicit attackRange: the level table must also give
    // full-map reach rather than a stale local radius.
    let turn = world(
        vec![rocket(10040, 0, 0, 3, 0)],
        vec![],
        vec![small_robot(30001, 40, 31, 40, "defender")],
    );
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    assert!(choose_attack(&turn, tower, &mut sim).is_some());
}

#[test]
fn an_enemy_building_destroyed_by_one_tower_is_not_shot_by_the_next() {
    // Tower A finishes the enemy gun; tower B must move on to the wall behind
    // it instead of spending its volley on a corpse.
    let turn = world(
        vec![gatling(10020, 10, 10, 1), gatling(10021, 10, 12, 1)],
        vec![
            enemy(20020, "gatling", 12, 10, 10),
            enemy(20030, "wall", 12, 12, 200),
        ],
        vec![],
    );
    let a = turn.role_by_id(10020).unwrap();
    let mut sim = init_sim(&turn);
    let targets_a = choose_attack(&turn, a, &mut sim).unwrap();
    assert_eq!(targets_a, vec![Pos { x: 12, y: 10 }], "A kills the gun");
    let gun = turn.enemy.iter().find(|unit| unit.id == 20020).unwrap();
    assert_eq!(sim.building_hp(gun), 0);

    let b = turn.role_by_id(10021).unwrap();
    let targets_b = choose_attack(&turn, b, &mut sim).unwrap();
    assert_eq!(targets_b, vec![Pos { x: 12, y: 12 }], "B takes the wall");
}

// ---------------------------------------------------------------------------
// "有余力时" — opportunistic fire only with firepower to spare
// ---------------------------------------------------------------------------

#[test]
fn an_idle_weapon_holds_fire_while_a_hunter_is_out_of_every_towers_reach() {
    // The only gun is a level-1 gatling (range 3); the robot coming for us is
    // ten cells away and nothing can touch it. Plinking the enemy now would be
    // spending the night's attention elsewhere, so the gun holds.
    let turn = world(
        vec![gatling(10020, 10, 10, 1)],
        vec![enemy(20020, "gatling", 12, 10, 1000)],
        vec![small_robot(30001, 20, 20, 40, "challenger")],
    );
    assert!(!spare_firepower(&turn));
    let tower = turn.role_by_id(10020).unwrap();
    let mut sim = init_sim(&turn);
    assert_eq!(choose_attack(&turn, tower, &mut sim), None);
}

#[test]
fn with_the_board_clear_an_idle_weapon_does_harass_the_opponent() {
    // Same board without the unengaged hunter: now there is firepower to
    // spare, and the enemy gun pays for it.
    let turn = world(
        vec![gatling(10020, 10, 10, 1)],
        vec![enemy(20020, "gatling", 12, 10, 1000)],
        vec![],
    );
    assert!(spare_firepower(&turn));
    let tower = turn.role_by_id(10020).unwrap();
    let mut sim = init_sim(&turn);
    assert_eq!(
        choose_attack(&turn, tower, &mut sim).unwrap(),
        vec![Pos { x: 12, y: 10 }]
    );
}

#[test]
fn a_hunter_inside_a_ready_towers_reach_does_not_suppress_opportunistic_fire() {
    // The hunter is covered by the gatling, so the tower has spare capacity
    // even though it is not this tower's chosen target this round.
    let turn = world(
        vec![gatling(10020, 10, 10, 1)],
        vec![enemy(20020, "gatling", 12, 10, 1000)],
        vec![small_robot(30001, 12, 11, 400, "challenger")],
    );
    assert!(spare_firepower(&turn));
}

// ---------------------------------------------------------------------------
// The tuning dial
// ---------------------------------------------------------------------------

#[test]
fn the_default_dial_matches_the_committed_constants() {
    let default = Weights::default();
    assert_eq!(default.damage_weight, 2);
    assert_eq!(default.kill_bonus_per_score, 20);
    assert_eq!(default.asset_damage_divisor, 4);
    assert_eq!(default.fire_horizon, 10);
    assert_eq!(default.urgency_rounds, 3);
    assert_eq!(default.urgency_lead, 8);
    assert_eq!(default.station_value, 200);
    assert_eq!(default.operator_value, 600);
    assert_eq!(default.role_value, 400);
    assert_eq!(default.tower_value, 300);
    assert_eq!(default.wall_value, 60);
    // The live weights are the defaults unless someone turned the dial.
    let live = weights();
    assert_eq!(live.damage_weight, default.damage_weight);
    assert_eq!(live.kill_bonus_per_score, default.kill_bonus_per_score);
}

#[test]
fn the_dial_reads_overrides_and_ignores_junk() {
    let tuned = Weights::from_lookup(|name| match name {
        "KILL_BONUS" => Some(" 35 ".to_string()),
        "STATION_VALUE" => Some("900".to_string()),
        "WALL_VALUE" => Some("not a number".to_string()),
        _ => None,
    });
    assert_eq!(tuned.kill_bonus_per_score, 35, "whitespace is trimmed");
    assert_eq!(tuned.station_value, 900);
    assert_eq!(
        tuned.wall_value,
        Weights::default().wall_value,
        "an unparseable override leaves the default alone"
    );
    assert_eq!(
        tuned.tower_value,
        Weights::default().tower_value,
        "an absent override leaves the default alone"
    );
}

#[test]
fn the_dial_can_invert_the_robot_ranking_which_is_what_ab_needs() {
    // A dial that cannot change an outcome is not a dial. With the kill bonus
    // driven negative, the class order inverts: the small robot becomes the
    // better target because the large one's kill score is now a liability.
    let turn = world(
        vec![gatling(10020, 10, 10, 1)],
        vec![],
        vec![
            small_robot(30001, 12, 10, 40, "challenger"),
            big_robot(30002, 12, 10, 40, "challenger"),
        ],
    );
    let base = *weights();
    let tuned = Weights {
        kill_bonus_per_score: -100,
        ..base
    };
    let small = win_value_with(&tuned, &turn, &turn.robots[0]);
    let large = win_value_with(&tuned, &turn, &turn.robots[1]);
    assert!(
        small > large,
        "the dial moved the ranking: small {small} vs large {large}"
    );
    assert!(
        win_value(&turn, &turn.robots[1]) > win_value(&turn, &turn.robots[0]),
        "while the default still ranks the large robot first"
    );
}

#[test]
fn the_dial_can_make_a_survivable_station_outrank_their_gun() {
    // The realistic A/B knob: how much a station that survives the round is
    // worth relative to silencing a weapon. Default leans weapon; raising
    // CG_TUNE_STATION_VALUE past the tower value flips it.
    let turn = world(
        vec![rocket(10040, 10, 10, 1, 15)],
        vec![
            enemy(20020, "gatling", 12, 10, 1000),
            enemy(20001, "station", 12, 14, 1500),
        ],
        vec![],
    );
    let tower = turn.role_by_id(10040).unwrap();
    let gun = turn
        .enemy
        .iter()
        .find(|unit| unit.kind == UnitKind::Gatling)
        .unwrap();
    let station = turn
        .enemy
        .iter()
        .find(|unit| unit.kind == UnitKind::Station)
        .unwrap();
    let sim = init_sim(&turn);
    let gun_cells = vec![gun.pos];
    let station_cells = vec![station.pos];

    let base = *weights();
    let default_gun = enemy_unit_score_with(&base, &turn, tower, gun, &gun_cells, &sim);
    let default_station = enemy_unit_score_with(&base, &turn, tower, station, &station_cells, &sim);
    assert!(
        default_gun > default_station,
        "default: disable the gun ({default_gun}) before chipping the station ({default_station})"
    );

    let station_first = Weights {
        station_value: 4000,
        ..base
    };
    assert!(
        enemy_unit_score_with(&station_first, &turn, tower, station, &station_cells, &sim)
            > enemy_unit_score_with(&station_first, &turn, tower, gun, &gun_cells, &sim),
        "the dial makes the station the target"
    );
}
