//! Combat model, cross-round state and validator tests built from synthetic
//! round payloads.

use std::collections::HashMap;

use serde_json::{json, Value};

use coregeek::brain::combat::{choose_attack, init_sim};
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request, RoleCommand};
use coregeek::state::BotState;
use coregeek::validate::sanitize;

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

fn robot(id: i64, x: i32, y: i32, hp: i64, team: &str) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "smallRobot",
        "health": hp, "abnormalState": "", "targetTeam": team
    })
}

fn world(our_roles: Vec<Value>, robots: Vec<Value>) -> Value {
    json!({
        "roundNo": 85, // night
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [], "roles": our_roles
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": robots},
    })
}

#[test]
fn gatling_prefers_robots_hunting_us() {
    let turn = turn_from(world(
        vec![gatling(10020, 10, 10, 1)],
        vec![
            robot(30001, 12, 10, 40, "challenger"), // hunts us → higher threat
            robot(30002, 8, 10, 40, "defender"),    // hunts the enemy team
        ],
    ));
    let tower = turn.role_by_id(10020).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(targets, vec![Pos { x: 12, y: 10 }]);
}

#[test]
fn towers_share_damage_simulation_no_overkill() {
    // R1 has 10 hp: one bullet kills it. Tower A must claim the kill and
    // tower B must move on to R2 instead of wasting bullets on the corpse.
    let turn = turn_from(world(
        vec![gatling(10020, 10, 10, 1), gatling(10021, 12, 10, 1)],
        vec![
            robot(30001, 11, 9, 10, "defender"),
            robot(30002, 13, 10, 40, "defender"),
        ],
    ));
    let mut sim = init_sim(&turn);
    let a = turn.role_by_id(10020).unwrap();
    let targets_a = choose_attack(&turn, a, &mut sim).unwrap();
    assert_eq!(targets_a, vec![Pos { x: 11, y: 9 }], "A takes the kill");
    assert_eq!(sim.get(&30001), Some(&0), "sim degraded");
    let b = turn.role_by_id(10021).unwrap();
    let targets_b = choose_attack(&turn, b, &mut sim).unwrap();
    assert_eq!(targets_b, vec![Pos { x: 13, y: 10 }], "B switches to R2");
}

#[test]
fn railgun_picks_the_piercing_line() {
    // Energy 30: shooting down the row hits r1 for 30 (60 value) which beats
    // the isolated 10hp kill (10*2 + 20 kill bonus = 40).
    let turn = turn_from(world(
        vec![json!({
            "id": 10030, "pos": {"x": 10, "y": 10}, "roleType": "railgun",
            "health": 1000, "attackPower": 30, "attackRange": 10,
            "level": 3, "backPackCapability": 0, "backpack": []
        })],
        vec![
            robot(30001, 12, 10, 40, "defender"),
            robot(30002, 14, 10, 40, "defender"),
            robot(30003, 10, 13, 10, "defender"),
        ],
    ));
    let tower = turn.role_by_id(10030).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(targets, vec![Pos { x: 12, y: 10 }]);
    assert_eq!(sim.get(&30001), Some(&10), "30 energy spent on r1");
}

#[test]
fn rocket_fires_level_missiles_at_cluster() {
    let turn = turn_from(world(
        vec![json!({
            "id": 10040, "pos": {"x": 0, "y": 0}, "roleType": "rocket",
            "health": 1000, "attackPower": 20, "attackRange": 15,
            "level": 2, "cooldown": 0, "backPackCapability": 0, "backpack": []
        })],
        vec![
            robot(30001, 5, 5, 40, "defender"),
            robot(30002, 6, 5, 40, "defender"),
            robot(30003, 30, 30, 40, "defender"), // out of range
        ],
    ));
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(targets.len(), 2, "level2 = two missiles");
    let cluster = [Pos { x: 5, y: 5 }, Pos { x: 6, y: 5 }];
    assert!(targets.iter().all(|t| cluster.contains(t)), "{targets:?}");
}

#[test]
fn rocket_on_cooldown_gets_no_targets_from_caller() {
    // choose_attack itself is cooldown-agnostic; night.rs gates it. Verify
    // the gate exists by checking a cooldown tower yields no attack command
    // through the full planner.
    let turn = turn_from(world(
        vec![
            json!({
                "id": 10040, "pos": {"x": 0, "y": 0}, "roleType": "rocket",
                "health": 1000, "attackPower": 20, "attackRange": 15,
                "level": 1, "cooldown": 2, "backPackCapability": 0, "backpack": []
            }),
            json!({
                "id": 10010, "pos": {"x": 1, "y": 0}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": []
            }),
        ],
        vec![robot(30001, 5, 5, 40, "challenger")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    assert!(!plan.commands.contains_key(&10040), "cooldown tower must not fire");
}

#[test]
fn state_resets_when_round_number_regresses() {
    // Second half of a match restarts roundNo: stale memory would poison
    // decisions (blacklisted cells from the other board etc).
    let mut state = BotState::default();
    let late = turn_from(world(vec![], vec![]));
    state.observe(&late); // roundNo 85
    state.llm_used_today = 2;
    state.seen_llm_resp = "stale".into();
    state.blacklisted_builds.insert((Pos { x: 1, y: 1 }, "wall".into()));

    let mut early_payload = world(vec![], vec![]);
    early_payload["roundNo"] = json!(5);
    let early = turn_from(early_payload);
    state.observe(&early);

    assert_eq!(state.last_round, 5);
    assert_eq!(state.llm_used_today, 0);
    assert!(state.seen_llm_resp.is_empty());
    assert!(state.blacklisted_builds.is_empty());
}

fn pioneer_with(items: Vec<&str>) -> Value {
    json!({
        "id": 10011, "pos": {"x": 10, "y": 10}, "roleType": "pioneer",
        "health": 200, "attackPower": 0, "attackRange": 0,
        "backPackCapability": 40, "backpack": items
    })
}

#[test]
fn summon_treasure_requires_full_item_multiplicity() {
    let base = |backpack: Vec<&str>| {
        turn_from(world(vec![pioneer_with(backpack)], vec![]))
    };

    // One StarSand in pack, two demanded → command must be dropped.
    let turn = base(vec!["StarSand"]);
    let mut commands = HashMap::new();
    commands.insert(
        10011,
        RoleCommand::summon_treasure(
            Pos { x: 11, y: 10 },
            vec!["StarSand".into(), "StarSand".into()],
        ),
    );
    assert!(sanitize(&turn, commands).is_empty());

    // Two in pack → kept.
    let turn = base(vec!["StarSand", "StarSand"]);
    let mut commands = HashMap::new();
    commands.insert(
        10011,
        RoleCommand::summon_treasure(
            Pos { x: 11, y: 10 },
            vec!["StarSand".into(), "StarSand".into()],
        ),
    );
    let out = sanitize(&turn, commands);
    assert_eq!(out.len(), 1);
}

#[test]
fn partial_requests_never_crash() {
    // Fields missing entirely must fall back to defaults and still yield a
    // well-formed response (5 strikes rule: malformed response = anomaly).
    for body in [
        &b"{\"roundNo\":7}"[..],
        &b"{}"[..],
        &b"{\"roundNo\":900,\"teamOur\":{\"roles\":[{\"id\":10010,\"roleType\":\"worker\",\"pos\":{\"x\":1,\"y\":1},\"health\":220}]}}"[..],
        &b"{\"roundNo\":-5}"[..],
    ] {
        let out = coregeek::brain::respond(body);
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid JSON response");
        assert!(value.get("roleCommandMap").is_some());
        assert_eq!(value.get("prompt").and_then(Value::as_str), Some(""));
        assert_eq!(value.get("executeCmd").and_then(Value::as_str), Some(""));
    }
}

// ---------------------------------------------------------------------------
// Problem 1: night weapons must be operated, controllers must walk to them
// ---------------------------------------------------------------------------

fn worker(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "backPackCapability": 100, "backpack": []
    })
}

fn zone(x: i32, y: i32, kind: &str) -> Value {
    json!({"pos": {"x": x, "y": y}, "neutralType": kind})
}

fn world_zones(our_roles: Vec<Value>, robots: Vec<Value>, zones: Vec<Value>) -> Value {
    json!({
        "roundNo": 85, // night
        "mapInfo": {"width": 41, "height": 32, "zones": zones},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [], "roles": our_roles
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": robots},
    })
}

#[test]
fn every_tower_gets_an_operator_at_night() {
    let turn = turn_from(world(
        vec![
            gatling(10020, 10, 10, 1),
            gatling(10021, 12, 10, 1),
            worker(10010, 11, 10), // adjacent to 10020
            worker(10011, 13, 10), // adjacent to 10021
            worker(10012, 10, 8),  // spare
        ],
        vec![robot(30001, 11, 9, 40, "challenger")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    for tower_id in [10020i64, 10021] {
        let cmd = plan.commands.get(&tower_id).expect("tower fires");
        assert_eq!(cmd.action, "attack", "tower {tower_id} is operated");
    }
}

#[test]
fn paired_controller_walks_to_weapon_not_economy() {
    // One tower, one far worker, with mines right next to the worker: the
    // worker must walk toward its tower, never collect the mine.
    let turn = turn_from(world_zones(
        vec![gatling(10020, 5, 5, 1), worker(10010, 20, 20)],
        vec![],
        vec![zone(20, 21, "stone"), zone(21, 20, "iron")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker acts");
    assert_eq!(cmd.action, "move", "paired controller walks to its tower, not the mine");
}

#[test]
fn stable_pairs_do_not_oscillate_between_rounds() {
    let turn = turn_from(world(
        vec![
            gatling(10020, 5, 5, 1),
            worker(10010, 4, 5),
            worker(10011, 10, 10),
        ],
        vec![],
    ));
    let mut state = BotState::default();
    let first = coregeek::brain::night::stable_pairs(&turn, &mut state);
    let second = coregeek::brain::night::stable_pairs(&turn, &mut state);
    assert_eq!(first, second, "pairings stay fixed within a night");
}

// ---------------------------------------------------------------------------
// Problem 2: weapon upgrades are bought and applied with priority
// ---------------------------------------------------------------------------

fn station(x: i32, y: i32, level: i64) -> Value {
    json!({
        "id": 10000, "pos": {"x": x, "y": y}, "roleType": "station",
        "health": 10000, "attackPower": 0, "attackRange": 0,
        "level": level, "backPackCapability": 0, "backpack": []
    })
}

fn day_world(our_roles: Vec<Value>, gold: i64, shop_items: Vec<Value>, zones: Vec<Value>) -> Value {
    json!({
        "roundNo": 5, // day
        "mapInfo": {"width": 41, "height": 32, "zones": zones},
        "teamOur": {
            "type": "challenger", "goldNum": gold, "totalScore": 0,
            "playerTasks": [], "roles": our_roles
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "vendorShopList": [],
        "weaponShopList": shop_items,
    })
}

#[test]
fn shopping_list_prioritizes_weapon_upgrade_voucher() {
    let turn = turn_from(day_world(
        vec![station(10, 20, 1), gatling(10020, 10, 10, 1), worker(10010, 5, 5)],
        75,
        vec![
            json!({"name": "WeaponUpgradeVoucher1", "price": 50}),
            json!({"name": "StationUpgradeVoucher1", "price": 50}),
        ],
        vec![zone(0, 0, "weaponShop")],
    ));
    let state = BotState::default();
    let gaps = coregeek::brain::day::tower_gaps(&turn, &state);
    let reserve = gaps.len() as i64 * coregeek::model::WEAPON_BUILD_COST;
    let list = coregeek::brain::economy::shopping_list(&turn, &state, reserve);
    assert_eq!(
        list.first().map(|need| need.name.as_str()),
        Some("WeaponUpgradeVoucher1"),
        "weapon upgrade outranks the station voucher and consumes the reserve"
    );
}

#[test]
fn buyer_worker_buys_voucher_at_shop() {
    let turn = turn_from(day_world(
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 1, 0), // adjacent to the shop at (0,0)
        ],
        75,
        vec![json!({"name": "WeaponUpgradeVoucher1", "price": 50})],
        vec![zone(0, 0, "weaponShop")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("dedicated buyer acts");
    assert_eq!(cmd.action, "buy");
    assert_eq!(cmd.name.as_deref(), Some("WeaponUpgradeVoucher1"));
}

#[test]
fn carried_voucher_upgrades_l1_tower() {
    let turn = turn_from(day_world(
        vec![
            gatling(10020, 10, 10, 1),
            json!({
                "id": 10010, "pos": {"x": 9, "y": 10}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": ["WeaponUpgradeVoucher1"]
            }),
        ],
        0,
        vec![],
        vec![],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker upgrades");
    assert_eq!(cmd.action, "use");
    assert_eq!(cmd.name.as_deref(), Some("WeaponUpgradeVoucher1"));
    assert_eq!(
        cmd.targetPos.as_ref().and_then(|list| list.first()).copied(),
        Some(Pos { x: 10, y: 10 }),
        "voucher applied to the L1 tower"
    );
}
