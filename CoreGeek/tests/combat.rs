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

fn railgun(id: i64, x: i32, y: i32, level: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "railgun",
        "health": 1000, "attackPower": 10, "attackRange": 7,
        "level": level, "backPackCapability": 0, "backpack": []
    })
}

fn rocket(id: i64, x: i32, y: i32, level: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "rocket",
        "health": 1000, "attackPower": 20, "attackRange": 15,
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
    night_world(85, our_roles, robots)
}

fn night_world(round_no: i64, our_roles: Vec<Value>, robots: Vec<Value>) -> Value {
    json!({
        "roundNo": round_no,
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
fn gatling_front_robot_blocks_the_rear_robot() {
    let turn = turn_from(world(
        vec![gatling(10020, 10, 10, 1)],
        vec![
            robot(30001, 11, 10, 40, "defender"),
            robot(30002, 12, 10, 40, "challenger"),
        ],
    ));
    let tower = turn.role_by_id(10020).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(sim.get(&30001), Some(&30), "front robot absorbs the bullet");
    assert_eq!(sim.get(&30002), Some(&40), "rear robot is blocked");
}

#[test]
fn railgun_energy_pierces_front_robot_into_rear_robot() {
    let turn = turn_from(world(
        vec![json!({
            "id": 10030, "pos": {"x": 10, "y": 10}, "roleType": "railgun",
            "health": 1000, "attackPower": 30, "attackRange": 10,
            "level": 3, "backPackCapability": 0, "backpack": []
        })],
        vec![
            robot(30001, 12, 10, 10, "challenger"),
            robot(30002, 14, 10, 40, "challenger"),
        ],
    ));
    let tower = turn.role_by_id(10030).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(targets, vec![Pos { x: 14, y: 10 }]);
    assert_eq!(
        sim.get(&30001),
        Some(&0),
        "front robot takes the first 10 energy"
    );
    assert_eq!(
        sim.get(&30002),
        Some(&20),
        "remaining 20 energy reaches the rear robot"
    );
}

#[test]
fn idle_weapon_targets_enemy_weapon_before_station() {
    let mut payload = world(vec![rocket(10040, 10, 10, 1)], vec![]);
    payload["teamEnemy"]["roles"] = json!([
        {
            "id": 20013, "pos": {"x": 14, "y": 10}, "roleType": "station",
            "health": 1500, "attackPower": 0, "attackRange": 0,
            "level": 1, "backPackCapability": 0, "backpack": []
        },
        {
            "id": 20020, "pos": {"x": 13, "y": 10}, "roleType": "gatling",
            "health": 1000, "attackPower": 10, "attackRange": 3,
            "level": 1, "backPackCapability": 0, "backpack": []
        }
    ]);
    let turn = turn_from(payload);
    let tower = turn.role_by_id(10040).unwrap();
    let mut sim = init_sim(&turn);
    let targets = choose_attack(&turn, tower, &mut sim).unwrap();
    assert_eq!(targets, vec![Pos { x: 13, y: 10 }]);
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
    assert!(
        !plan.commands.contains_key(&10040),
        "cooldown tower must not fire"
    );
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
    state
        .blacklisted_builds
        .insert((Pos { x: 1, y: 1 }, "wall".into()));

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
    let base = |backpack: Vec<&str>| turn_from(world(vec![pioneer_with(backpack)], vec![]));

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
    assert_eq!(
        cmd.action, "move",
        "paired controller walks to its tower, not the mine"
    );
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
    day_world_at(5, our_roles, gold, shop_items, zones)
}

fn day_world_at(
    round_no: i64,
    our_roles: Vec<Value>,
    gold: i64,
    shop_items: Vec<Value>,
    zones: Vec<Value>,
) -> Value {
    json!({
        "roundNo": round_no, // day
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
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 5, 5),
        ],
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
    // All three tower slots are filled (no build gaps left), so the buyer's
    // only remaining job is shopping — building weapons now outranks it.
    let turn = turn_from(day_world(
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            railgun(10030, 12, 10, 2),
            rocket(10040, 14, 10, 2),
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
        cmd.targetPos
            .as_ref()
            .and_then(|list| list.first())
            .copied(),
        Some(Pos { x: 10, y: 10 }),
        "voucher applied to the L1 tower"
    );
}

// ---------------------------------------------------------------------------
// Problem 1b: dusk converts ore to gold; Problem 2b: night-1 operation
// ---------------------------------------------------------------------------

#[test]
fn dusk_worker_sells_ore_instead_of_mining() {
    // Round 65 (in_day 64) is inside the final 15 day rounds: the worker must
    // stop mining and head to the vendor to convert ore to gold.
    let turn = turn_from(day_world_at(
        65,
        vec![
            station(10, 20, 1),
            json!({
                "id": 10010, "pos": {"x": 5, "y": 5}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": ["iron"]
            }),
        ],
        0,
        vec![],
        vec![zone(5, 6, "iron"), zone(0, 0, "vendor")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker acts");
    assert_ne!(cmd.action, "collect", "dusk worker must not mine");
    assert_eq!(
        cmd.action, "move",
        "dusk worker walks to the vendor to sell"
    );
}

#[test]
fn dusk_worker_sells_at_vendor() {
    // Round 65, worker already adjacent to the vendor: the dusk window must
    // turn ore into gold, not dig for more.
    let turn = turn_from(day_world_at(
        65,
        vec![
            station(10, 20, 1),
            json!({
                "id": 10010, "pos": {"x": 1, "y": 0}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": ["iron", "iron", "copper"]
            }),
        ],
        0,
        vec![],
        vec![zone(0, 0, "vendor")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker acts");
    assert_eq!(
        cmd.action, "sell",
        "dusk worker sells its ore at the vendor"
    );
}

#[test]
fn worker_prepositions_to_tower_before_nightfall() {
    // Round 66 (in_day 65): a worker far from its tower must drop everything
    // and walk there so the first night round is spent firing, not walking.
    let turn = turn_from(day_world_at(
        66,
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 25, 25),
        ],
        0,
        vec![],
        vec![zone(25, 24, "iron")], // adjacent, tempting economy
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker acts");
    assert_eq!(cmd.action, "move", "worker prepositions toward its tower");
    assert_ne!(
        cmd.action, "collect",
        "far worker ignores the adjacent mine"
    );
}

#[test]
fn day_two_night_all_weapons_operated() {
    // Day 2's night starts at round 201 (roundNo is 1-based: day=(roundNo-1)/130+1,
    // night begins at in_day_round 70 → 130 + 70 + 1). Every tower must fire.
    let turn = turn_from(night_world(
        201,
        vec![
            gatling(10020, 10, 10, 1),
            gatling(10021, 14, 10, 1),
            gatling(10022, 10, 14, 1),
            worker(10010, 11, 10), // adjacent to 10020
            worker(10011, 13, 10), // adjacent to 10021
            worker(10012, 11, 14), // adjacent to 10022
        ],
        vec![
            robot(30001, 11, 9, 40, "challenger"),
            robot(30002, 13, 9, 40, "challenger"),
            robot(30003, 9, 13, 40, "challenger"),
        ],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    for tower_id in [10020i64, 10021, 10022] {
        let cmd = plan.commands.get(&tower_id).expect("tower fires");
        assert_eq!(
            cmd.action, "attack",
            "tower {tower_id} is operated on day-2 night"
        );
    }
}

#[test]
fn night_round_one_response_contains_attack_commands() {
    // Round 71 = day-1 night (in_day_round 70). Three towers, three adjacent
    // controllers, robots in range. Exercise the FULL path — decide →
    // sanitize → serialize — because that is what the judger receives. A
    // planner-level check would not catch an attack command silently dropped
    // by validation or mis-serialized away.
    let payload = night_world(
        71,
        vec![
            gatling(10020, 10, 10, 1),
            gatling(10021, 14, 10, 1),
            gatling(10022, 10, 14, 1),
            worker(10010, 11, 10), // adjacent to 10020
            worker(10011, 13, 10), // adjacent to 10021
            worker(10012, 11, 14), // adjacent to 10022
        ],
        vec![
            robot(30001, 11, 9, 40, "challenger"),
            robot(30002, 13, 9, 40, "challenger"),
            robot(30003, 9, 13, 40, "challenger"),
        ],
    );
    let out = coregeek::brain::respond(payload.to_string().as_bytes());
    let value: Value = serde_json::from_str(&out).expect("valid JSON response");
    let map = value.get("roleCommandMap").expect("has roleCommandMap");
    for tower_id in ["10020", "10021", "10022"] {
        let cmd = map
            .get(tower_id)
            .unwrap_or_else(|| panic!("tower {tower_id} has a command in the response"));
        assert_eq!(
            cmd.get("action").and_then(Value::as_str),
            Some("attack"),
            "tower {tower_id} must fire through the full response path"
        );
    }
}

// ---------------------------------------------------------------------------
// Problem 3: day economy + fortification survival strategy
// ---------------------------------------------------------------------------

#[test]
fn worker_builds_wall_before_weapon() {
    // Compute a wall gap first, then place a stone-carrying worker next to it.
    // Even with enough gold for a tower, the stone wall must win — the ring
    // protects the base and roles before the weapons do.
    let probe = turn_from(day_world_at(5, vec![station(10, 20, 1)], 0, vec![], vec![]));
    let state = BotState::default();
    let gaps = coregeek::brain::day::wall_gaps(&probe, &state);
    let first = *gaps.first().expect("wall gaps exist");
    let turn = turn_from(day_world_at(
        5,
        vec![
            station(10, 20, 1),
            json!({
                "id": 10010, "pos": {"x": first.x + 1, "y": first.y},
                "roleType": "worker", "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100,
                "backpack": ["stone", "stone", "stone", "stone", "stone", "stone"]
            }),
        ],
        100,
        vec![],
        vec![zone(0, 0, "weaponShop")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker acts");
    assert_eq!(cmd.action, "build");
    assert_eq!(
        cmd.name.as_deref(),
        Some("wall"),
        "wall before weapon despite 100 gold"
    );
}

#[test]
fn choose_mine_prefers_nearest_not_most_valuable() {
    // Near iron vs far copper: distance beats value — a short walk keeps the
    // build loop moving faster than a high-value ore on the far side.
    let turn = turn_from(day_world_at(
        5,
        vec![worker(10010, 5, 5)],
        0,
        vec![],
        vec![zone(5, 6, "iron"), zone(30, 30, "copper")],
    ));
    let state = BotState::default();
    let mine = coregeek::brain::economy::choose_mine(
        &turn,
        &state,
        Pos { x: 5, y: 5 },
        0,
        &std::collections::HashSet::new(),
    );
    assert_eq!(
        mine.map(|(pos, _)| pos),
        Some(Pos { x: 5, y: 6 }),
        "nearest mine wins even though copper is more valuable"
    );
}

#[test]
fn choose_mine_prefers_stone_when_walls_needed() {
    // Walls first: with stone demand unmet, a stone mine beats a nearer iron.
    let turn = turn_from(day_world_at(
        5,
        vec![worker(10010, 10, 10)],
        0,
        vec![],
        vec![
            zone(9, 10, "iron"),
            zone(10, 11, "stone"),
            zone(20, 20, "stone"),
        ],
    ));
    let state = BotState::default();
    let mine = coregeek::brain::economy::choose_mine(
        &turn,
        &state,
        Pos { x: 10, y: 10 },
        5,
        &std::collections::HashSet::new(),
    );
    assert_eq!(
        mine.map(|(pos, _)| pos),
        Some(Pos { x: 10, y: 11 }),
        "stone for the wall line wins"
    );
}

#[test]
fn night_spare_worker_shelters_not_mine() {
    // A spare worker far from base with a mine right next to it must walk
    // toward the base — night is for manning weapons and sheltering, never
    // for mining outside the walls.
    let turn = turn_from(world_zones(
        vec![
            station(10, 20, 1),
            gatling(10020, 9, 20, 1),
            worker(10011, 11, 20), // paired: nearest to the tower
            worker(10010, 30, 30), // spare, far away
        ],
        vec![],
        vec![zone(30, 31, "iron")], // adjacent to the spare
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("spare worker acts");
    assert_eq!(
        cmd.action, "move",
        "spare worker shelters instead of mining"
    );
}

#[test]
fn dying_worker_heals_first() {
    // 40/220 HP with a Medicine in the backpack: healing outranks every other
    // daytime duty — a dead worker builds nothing.
    let turn = turn_from(day_world_at(
        5,
        vec![
            station(10, 20, 1),
            json!({
                "id": 10010, "pos": {"x": 5, "y": 5}, "roleType": "worker",
                "health": 40, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": ["Medicine"]
            }),
        ],
        0,
        vec![],
        vec![zone(5, 6, "iron")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker acts");
    assert_eq!(cmd.action, "use");
    assert_eq!(
        cmd.name.as_deref(),
        Some("Medicine"),
        "dying worker heals first"
    );
}

// ---------------------------------------------------------------------------
// Problem 4 (Issue #3): workers trapped outside the wall ring + economy stall
// ---------------------------------------------------------------------------

#[test]
fn wall_gaps_builds_inner_box_first_and_leaves_gate_open() {
    // A tight box (the distance-2 inner ring) must close around the base before
    // the outer ring scatters far away — battle pk575557 left walls scattered
    // with no enclosure. The entrance corridor stays open either way.
    let probe = turn_from(day_world_at(5, vec![station(10, 20, 1)], 0, vec![], vec![]));
    let state = BotState::default();
    let gaps = coregeek::brain::day::wall_gaps(&probe, &state);
    assert!(!gaps.is_empty(), "wall gaps exist");
    let gate = [Pos { x: 13, y: 18 }, Pos { x: 14, y: 18 }];
    assert!(
        gate.iter().all(|cell| !gaps.contains(cell)),
        "gate cells stay open"
    );
    let footprint = coregeek::model::station_footprint(Pos { x: 10, y: 20 });
    assert_eq!(
        coregeek::model::footprint_distance(gaps[0], &footprint),
        2,
        "inner box (distance 2) builds before the outer ring"
    );
}

#[test]
fn trapped_worker_removes_wall_to_escape() {
    // A worker sealed into a one-cell pocket by its own wall cannot pathfind
    // out — walk_or_remove_wall must fall back to demolishing the wall.
    let wall_unit = json!({
        "id": 10050, "pos": {"x": 3, "y": 2}, "roleType": "wall",
        "health": 1000, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    });
    let mut zones = Vec::new();
    for (zx, zy) in [(1, 1), (2, 1), (3, 1), (1, 2), (1, 3), (2, 3), (3, 3)] {
        zones.push(zone(zx, zy, "iron"));
    }
    let turn = turn_from(world_zones(
        vec![worker(10010, 2, 2), wall_unit],
        vec![],
        zones,
    ));
    let role = turn.role_by_id(10010).unwrap();
    let stands = vec![Pos { x: 4, y: 2 }];
    let mut claimed = std::collections::HashSet::new();
    let cmd = coregeek::brain::walk_or_remove_wall(&turn, role, &stands, &mut claimed)
        .expect("worker demolishes the wall");
    assert_eq!(cmd.action, "remove");
    assert_eq!(
        cmd.targetPos
            .as_ref()
            .and_then(|list| list.first())
            .copied(),
        Some(Pos { x: 3, y: 2 }),
        "the blocking wall is the demolition target"
    );
}

#[test]
fn task_description_persists_when_phase_task_clears() {
    // The judger sometimes stops echoing phaseTask while the task is still
    // live. A cleared field must NOT end the task or erase the description —
    // that was the 97 acceptTask / 0 submitAnswer spin.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.description = "请实现一个排序函数".into();
    state.task.description_round = 5;
    state.task.accepted_round = 5;
    state.task.timeout_round = 300;
    state.task.stage = coregeek::state::TaskStage::Planning;

    let turn = turn_from(day_world_at(
        20,
        vec![station(10, 20, 1)],
        0,
        vec![],
        vec![],
    ));
    state.observe(&turn);

    assert!(state.task.active, "cleared phaseTask must not end the task");
    assert_eq!(
        state.task.description, "请实现一个排序函数",
        "description must persist"
    );
}

#[test]
fn economy_holds_gold_reserve_for_main_weapon_upgrade() {
    let no_tower = turn_from(day_world_at(
        5,
        vec![station(10, 20, 1)],
        25,
        vec![],
        vec![],
    ));
    assert!(
        coregeek::brain::economy::may_build_weapon(&no_tower),
        "first weapon builds with 25g"
    );

    let one_tower = turn_from(day_world_at(
        5,
        vec![station(10, 20, 1), gatling(10020, 10, 10, 1)],
        25,
        vec![],
        vec![],
    ));
    assert!(
        coregeek::brain::economy::may_build_weapon(&one_tower),
        "second weapon builds with 25g"
    );

    // Two level-1 weapons: the gold must be reserved for the main weapon's
    // WeaponUpgradeVoucher1 instead of a third level-1 weapon.
    let two_l1 = turn_from(day_world_at(
        5,
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            railgun(10030, 12, 10, 1),
        ],
        25,
        vec![],
        vec![],
    ));
    assert!(
        !coregeek::brain::economy::may_build_weapon(&two_l1),
        "reserve held for upgrade voucher"
    );

    // Once the main weapon is level 2, more weapons may be built while gold
    // still leaves the 25g reserve untouched.
    let two_with_l2 = turn_from(day_world_at(
        5,
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 2),
            railgun(10030, 12, 10, 1),
        ],
        50,
        vec![],
        vec![],
    ));
    assert!(
        coregeek::brain::economy::may_build_weapon(&two_with_l2),
        "level-2 main weapon unlocks more builds"
    );
}

#[test]
fn worker_keeps_gold_reserve_for_weapon_upgrade() {
    // Two level-1 weapons + 25g, with a worker standing right next to the open
    // rocket slot: the gold must be held for the main weapon's upgrade voucher,
    // so the worker must NOT build a third level-1 weapon.
    let turn = turn_from(day_world_at(
        5,
        vec![
            station(10, 20, 1),
            gatling(10020, 5, 5, 1),
            railgun(10030, 7, 7, 1),
            worker(10010, 12, 19), // adjacent to the rocket gap at (12,18)
        ],
        25,
        vec![],
        vec![],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    assert!(
        plan.commands.values().all(|cmd| cmd.action != "build"),
        "gold reserved for the upgrade: no third level-1 weapon"
    );
}

// ---------------------------------------------------------------------------
// Issue #8: task empty-loop, economy stall, night survivability, wall cap
// ---------------------------------------------------------------------------

#[test]
fn should_sell_relaxes_batch_threshold() {
    // 5 ores used to be below SELL_BATCH=8; now it sells, so a miner carrying
    // a small stack never sits idle with gold trapped in its backpack.
    let turn = turn_from(day_world_at(
        5,
        vec![
            station(10, 20, 1),
            json!({
                "id": 10010, "pos": {"x": 5, "y": 5}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100,
                "backpack": ["iron", "iron", "iron", "iron", "iron"]
            }),
        ],
        0,
        vec![],
        vec![zone(0, 0, "vendor")],
    ));
    let state = BotState::default();
    let role = turn.role_by_id(10010).unwrap();
    assert!(
        coregeek::brain::economy::should_sell(&turn, &state, role, 6),
        "5 ores must trigger a sell (SELL_BATCH relaxed from 8 to 5)"
    );

    // A half-full backpack (>= capacity/2) also sells, even below the batch.
    let half: Vec<Value> = (0..50).map(|_| json!("iron")).collect();
    let turn2 = turn_from(day_world_at(
        5,
        vec![
            station(10, 20, 1),
            json!({
                "id": 10010, "pos": {"x": 5, "y": 5}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100,
                "backpack": half
            }),
        ],
        0,
        vec![],
        vec![zone(0, 0, "vendor")],
    ));
    let role2 = turn2.role_by_id(10010).unwrap();
    assert!(
        coregeek::brain::economy::should_sell(&turn2, &state, role2, 6),
        "half-full backpack triggers a sell"
    );
}

#[test]
fn task_empty_loop_does_not_end_prematurely() {
    // WaitingDescription with no description yet and no command run: the task
    // must NOT be abandoned just because a few rounds passed — that was the
    // acceptTask → end (cmdRounds=0) spin. It only ends on timeout.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.accepted_round = 10;
    state.task.timeout_round = 300;
    state.task.stage = coregeek::state::TaskStage::WaitingDescription;

    let turn = turn_from(day_world_at(
        16,
        vec![pioneer_with(vec![])],
        0,
        vec![],
        vec![],
    ));
    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = coregeek::brain::Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);

    assert!(state.task.active, "an empty task must not be abandoned");
    assert!(cmd.is_none(), "no command until the description arrives");
}

#[test]
fn build_prompt_includes_task_environment_path() {
    // The empty-loop came from the LLM guessing `cat task_X.md` in the root
    // directory. The prompt must point it at /tmp/selfEvolutionTask/ and tell
    // it to list files first.
    let mut state = BotState::default();
    state.task.description = "请统计 /tmp 下的文件数量".into();
    let turn = turn_from(day_world_at(5, vec![station(10, 20, 1)], 0, vec![], vec![]));
    let prompt = coregeek::brain::task::build_prompt(&state, &turn);
    assert!(
        prompt.contains("/tmp/selfEvolutionTask/"),
        "prompt names the task directory"
    );
    assert!(
        prompt.contains("find /tmp/selfEvolutionTask/"),
        "prompt suggests listing files first"
    );
    assert!(
        prompt.contains("maxdepth 4"),
        "prompt searches nested subdirectories"
    );
    assert!(
        prompt.contains(&state.task.description),
        "prompt still carries the task description"
    );
}

// ---------------------------------------------------------------------------
// Issue #9: pre-positioning before dusk, rocket second, strict answer parsing
// ---------------------------------------------------------------------------

#[test]
fn worker_prepositions_to_tower_before_dusk() {
    // Round 50 (in_day 49): a worker 15 cells from its tower must drop the
    // adjacent mine and walk, so it is adjacent BEFORE dusk (round 55). The old
    // deadline (66 - dist) would have left it mining here and walking back all
    // night — the cause of the 17/20 fired:false rounds in battle pk575060.
    let turn = turn_from(day_world_at(
        50,
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 25, 25),
        ],
        0,
        vec![],
        vec![zone(25, 24, "iron")], // adjacent, tempting economy
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker acts");
    assert_eq!(
        cmd.action, "move",
        "worker prepositions toward its tower before dusk"
    );
    assert_ne!(
        cmd.action, "collect",
        "far worker ignores the adjacent mine"
    );
}

#[test]
fn gatling_and_railgun_are_built_before_rocket() {
    // Gatling controls the nearest lane and railgun pierces lined-up waves.
    // Rocket remains the third early slot, deferred until those two defenses
    // exist and the upgrade reserve allows another build.
    let turn = turn_from(day_world_at(5, vec![station(10, 20, 1)], 0, vec![], vec![]));
    let state = BotState::default();
    let gaps = coregeek::brain::day::tower_gaps(&turn, &state);
    let kinds: Vec<&str> = gaps.iter().map(|(_, kind)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["gatling", "railgun", "rocket"],
        "tower build order is gatling → railgun → rocket"
    );
}

#[test]
fn exploratory_cmd_output_is_not_an_answer() {
    // `find`/`ls` stdout is exploration, not a solution. A cmd result that is a
    // plain file listing must NOT be submitted as an answer — the task goes
    // back to Planning for another script instead of scoring a wrong answer.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.stage = coregeek::state::TaskStage::WaitingCmdResult { attempts: 0 };
    coregeek::brain::task::on_cmd_result(
        &mut state,
        "[exitCode:0]\ntask_1.md\ninput.txt\n/tmp/selfEvolutionTask/data.csv",
    );
    assert!(
        matches!(state.task.stage, coregeek::state::TaskStage::Planning),
        "find/ls output must not be treated as an answer"
    );
    assert!(state.task.best_answer.is_empty(), "no answer was extracted");
}

// ---------------------------------------------------------------------------
// Issues #10/#11: unconditional night recall, meta-answer filtering, task
// retry/reset, and economy reserve/wall-box fixes
// ---------------------------------------------------------------------------

#[test]
fn night_recall_outranks_heal() {
    // A badly hurt operator with a Medicine in hand must still walk to its
    // tower instead of healing — the recall outranks every other night duty.
    // Battle pk575098 / pk575557 left towers idle all night because healing
    // (and other duties) swallowed the recall round.
    let hurt_worker = json!({
        "id": 10010, "pos": {"x": 20, "y": 20}, "roleType": "worker",
        "health": 50, "attackPower": 0, "attackRange": 0,
        "backPackCapability": 100, "backpack": ["Medicine"]
    });
    let turn = turn_from(world(vec![gatling(10020, 5, 5, 1), hurt_worker], vec![]));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("operator acts");
    assert_eq!(cmd.action, "move", "recall outranks healing");
}

#[test]
fn night_recall_moves_every_round_until_adjacent() {
    // A controller 2 cells away gets a move; once adjacent it stops being
    // issued moves — the recall runs every round until it lands.
    let turn_far = turn_from(world(
        vec![gatling(10020, 5, 5, 1), worker(10010, 7, 7)],
        vec![],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn_far, &mut state);
    let cmd = plan.commands.get(&10010).expect("far operator acts");
    assert_eq!(
        cmd.action, "move",
        "controller 2+ cells away walks toward its tower"
    );

    let turn_near = turn_from(world(
        vec![gatling(10020, 5, 5, 1), worker(10010, 6, 5)],
        vec![],
    ));
    let mut state2 = BotState::default();
    let plan2 = coregeek::brain::night::plan(&turn_near, &mut state2);
    assert!(
        !matches!(
            plan2.commands.get(&10010).map(|c| c.action.as_str()),
            Some("move")
        ),
        "adjacent controller is not issued a move"
    );
}

#[test]
fn is_meta_answer_detects_parsing_descriptions() {
    assert!(coregeek::brain::task::is_meta_answer(
        "{\"status\":\"parsed\",\"content_length\":534}"
    ));
    assert!(coregeek::brain::task::is_meta_answer("contentLength 123"));
    assert!(!coregeek::brain::task::is_meta_answer(
        "{\"city\":\"Beijing\",\"count\":3}"
    ));
}

#[test]
fn meta_answer_is_not_submitted() {
    // The LLM echoes a parsing step ({"status":"parsed","content_length":...})
    // instead of the answer. It must NOT be submitted: the task re-plans.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.stage = coregeek::state::TaskStage::WaitingCmdResult { attempts: 0 };
    coregeek::brain::task::on_cmd_result(
        &mut state,
        "[exitCode:0]\nANSWER: {\"status\":\"parsed\",\"content_length\":534}",
    );
    assert!(
        matches!(state.task.stage, coregeek::state::TaskStage::Planning),
        "meta answer is filtered, task re-plans"
    );
    assert!(state.task.best_answer.is_empty(), "no answer is kept");
}

#[test]
fn finish_task_does_not_cache_rejected_answer() {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.task_type = "自进化类1".into();
    state.task.description = "count files in directory".into();
    state.task.best_answer = "42".into();
    state.task.cmd_history = vec!["ls | wc -l".into()];
    state.task.wrong_answers = 1;
    state.finish_task(false, "rejected");
    assert!(!state.task.active, "task is reset");
    assert!(
        state.sop_cache.is_empty(),
        "rejected answer is never cached"
    );
}

#[test]
fn finish_task_caches_unrejected_answer() {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.task_type = "自进化类1".into();
    state.task.description = "count files in directory".into();
    state.task.best_answer = "42".into();
    state.task.cmd_history = vec!["ls | wc -l".into()];
    // Only a confirmed success may cache an SOP: the absence of an error is
    // not evidence that the script worked.
    state.finish_task(true, "confirmed_success");
    assert!(!state.task.active, "task is reset");
    assert!(
        !state.sop_cache.is_empty(),
        "working answer is cached for reuse"
    );
}

#[test]
fn rejected_answer_drops_cached_sop_and_keeps_task_active() {
    // A cached SOP for a task type whose answer was just rejected must be
    // dropped, and the task must stay active for a retry (the "instant
    // re-accept same type with stale state" loop).
    let mut state = BotState::default();
    state.sop_cache.push(coregeek::state::SopEntry {
        task_type: "自进化类1".into(),
        keywords: vec!["count".into(), "files".into()],
        script: "ls | wc -l".into(),
    });
    state.task.active = true;
    state.task.accepted_round = 5;
    state.task.timeout_round = 300;
    state.task.task_type = "自进化类1".into();
    state.task.description = "count files".into();
    state.task.stage = coregeek::state::TaskStage::WaitingSubmit { attempts: 0 };

    let mut payload = day_world_at(6, vec![pioneer_with(vec![])], 0, vec![], vec![]);
    payload["errors"] = json!([{"errorCode": 2}]);
    let turn = turn_from(payload);
    state.observe(&turn);

    assert!(state.task.active, "rejected task stays active for retry");
    assert_eq!(
        state.task.wrong_answers, 1,
        "wrong-answer counter increments"
    );
    assert!(
        matches!(state.task.stage, coregeek::state::TaskStage::Planning),
        "re-plan after rejection"
    );
    assert!(state.sop_cache.is_empty(), "rejected type's SOP is dropped");
}

#[test]
fn tower_build_reserve_covers_two_towers_not_three() {
    // Battle pk575557 all-in'd 75g on three towers and starved every consumable.
    // The reserve must cover only the 1-2 towers we actually build.
    assert_eq!(coregeek::brain::day::tower_build_reserve(0, 3), 50);
    assert_eq!(coregeek::brain::day::tower_build_reserve(1, 2), 25);
    assert_eq!(
        coregeek::brain::day::tower_build_reserve(2, 1),
        0,
        "no third tower until an upgrade"
    );
}

#[test]
fn four_ores_trigger_one_batch_sale_before_more_mining() {
    let turn = turn_from(day_world_at(
        5,
        vec![
            station(10, 20, 3),
            gatling(10020, 9, 20, 2),
            railgun(10030, 10, 21, 2),
            rocket(10040, 9, 21, 2),
            json!({
                "id": 10010, "pos": {"x": 1, "y": 0}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100,
                "backpack": ["iron", "iron", "iron", "iron"]
            }),
        ],
        0,
        vec![],
        vec![zone(0, 0, "vendor"), zone(1, 1, "iron")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker sells");
    assert_eq!(cmd.action, "sell");
    assert_eq!(cmd.name.as_deref(), Some("iron"));
    assert_eq!(
        cmd.num,
        Some(4),
        "the complete ore stack is sold in one command"
    );
}

#[test]
fn planner_buys_consumables_in_a_batch() {
    let turn = turn_from(day_world_at(
        5,
        vec![
            station(10, 20, 3),
            gatling(10020, 9, 20, 2),
            railgun(10030, 10, 21, 2),
            rocket(10040, 9, 21, 2),
            json!({
                "id": 10010, "pos": {"x": 1, "y": 0}, "roleType": "worker",
                "health": 100, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": []
            }),
        ],
        20,
        vec![json!({"name": "Medicine", "price": 10})],
        vec![zone(0, 0, "weaponShop")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("worker buys");
    assert_eq!(cmd.action, "buy");
    assert_eq!(cmd.name.as_deref(), Some("Medicine"));
    assert_eq!(cmd.num, Some(2), "both medicines are bought in one command");
}

#[test]
fn sanitizer_enforces_shared_gold_and_never_overwrites_towers() {
    let turn = turn_from(day_world_at(
        5,
        vec![
            gatling(10020, 5, 5, 1),
            worker(10010, 4, 4),
            worker(10012, 8, 8),
        ],
        25,
        vec![],
        vec![],
    ));
    let mut commands = HashMap::new();
    commands.insert(10010, RoleCommand::build(Pos { x: 5, y: 5 }, "rocket"));
    commands.insert(10012, RoleCommand::build(Pos { x: 9, y: 9 }, "railgun"));
    let out = sanitize(&turn, commands);
    assert_eq!(
        out.len(),
        1,
        "only one 25-gold build fits the shared budget"
    );
    assert!(
        out.get("10010").is_none(),
        "the existing gatling is never overwritten"
    );
}

#[test]
fn batch_buy_must_fit_remaining_backpack_capacity() {
    let mut pack = vec!["stone"; 99];
    pack.push("iron");
    let turn = turn_from(day_world_at(
        5,
        vec![json!({
            "id": 10010, "pos": {"x": 1, "y": 0}, "roleType": "worker",
            "health": 220, "attackPower": 0, "attackRange": 0,
            "backPackCapability": 100, "backpack": pack
        })],
        20,
        vec![json!({"name": "Medicine", "price": 10})],
        vec![zone(0, 0, "weaponShop")],
    ));
    let mut commands = HashMap::new();
    commands.insert(10010, RoleCommand::buy("Medicine", 2));
    assert!(
        sanitize(&turn, commands).is_empty(),
        "batch cannot overflow the backpack"
    );
}

// ---------------------------------------------------------------------------
// P0: dynamic pairing, wall sealing, task closure, defense arbitration
// ---------------------------------------------------------------------------

/// Payload builder that also sets `playerTasks` and this round's errors.
fn world_full(
    round_no: i64,
    our_roles: Vec<Value>,
    robots: Vec<Value>,
    player_tasks: Vec<Value>,
    errors: Vec<Value>,
) -> Value {
    json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": player_tasks, "roles": our_roles
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": robots},
        "errors": errors,
    })
}

fn task_point(x: i32, y: i32, valid: bool, cooldown: i64) -> Value {
    json!({
        "taskType": "自进化类1",
        "taskPosition": {"x": x, "y": y},
        "coldDownRounds": cooldown,
        "scoreReward": 50,
        "goldReward": 30,
        "isValid": valid,
        "timeoutRounds": 100,
    })
}

#[test]
fn pair_recomputes_the_round_a_controller_dies() {
    // Battle pk575098: controllers died at R83/R92/R102 while the surviving
    // towers kept their stale assignment, so guns stood idle all night.
    let mut state = BotState::default();
    let full = turn_from(world(
        vec![
            gatling(10020, 5, 5, 1),
            gatling(10021, 30, 30, 1),
            worker(10010, 4, 5),
            worker(10011, 29, 30),
            worker(10012, 10, 10),
        ],
        vec![],
    ));
    let before = coregeek::brain::night::stable_pairs(&full, &mut state);
    assert_eq!(before.len(), 2, "two towers, two operators");

    // The controller manning tower 10021 dies. That tower must be handed to a
    // living controller in the SAME round, not left idle all night.
    let thinned = turn_from(world(
        vec![
            gatling(10020, 5, 5, 1),
            gatling(10021, 30, 30, 1),
            worker(10010, 4, 5),
            worker(10012, 10, 10),
        ],
        vec![],
    ));
    let pairs = coregeek::brain::night::stable_pairs(&thinned, &mut state);
    let controllers: Vec<i64> = pairs.iter().map(|(controller, _)| *controller).collect();
    assert!(!controllers.contains(&10011), "the dead controller is gone");
    let mut towers: Vec<i64> = pairs.iter().map(|(_, tower)| *tower).collect();
    towers.sort_unstable();
    assert_eq!(
        towers,
        vec![10020, 10021],
        "the orphaned tower is re-manned"
    );
}

#[test]
fn pair_recomputes_when_the_pioneer_is_taken_by_a_task() {
    let mut state = BotState::default();
    let turn = turn_from(world(
        vec![
            gatling(10020, 5, 5, 1),
            gatling(10021, 20, 20, 1),
            worker(10010, 4, 5),
            pioneer_with(vec![]),
        ],
        vec![],
    ));
    let pairs = coregeek::brain::night::stable_pairs(&turn, &mut state);
    assert_eq!(pairs.len(), 2);

    // The pioneer accepts a task: it leaves the pairing immediately.
    state.task.active = true;
    let pairs = coregeek::brain::night::stable_pairs(&turn, &mut state);
    let controllers: Vec<i64> = pairs.iter().map(|(controller, _)| *controller).collect();
    assert!(
        !controllers.contains(&10011),
        "task-bound pioneer is excluded"
    );
    assert_eq!(pairs.len(), 1, "only the free worker mans a tower");
}

#[test]
fn pairing_covers_every_tower_when_controllers_are_available() {
    let turn = turn_from(world(
        vec![
            gatling(10020, 5, 5, 1),
            gatling(10021, 20, 20, 1),
            worker(10010, 4, 5),
            worker(10011, 19, 20),
        ],
        vec![],
    ));
    let mut state = BotState::default();
    let pairs = coregeek::brain::night::stable_pairs(&turn, &mut state);
    let mut towers: Vec<i64> = pairs.iter().map(|(_, tower)| *tower).collect();
    towers.sort_unstable();
    assert_eq!(towers, vec![10020, 10021], "no tower is left unmanned");
}

#[test]
fn wall_gate_seals_after_the_dusk_retreat() {
    // The gate stays open only while somebody is still outside; once every role
    // has retreated, the last ring cell is admitted and the ring closes.
    let payload = day_world_at(
        66, // in_day 65 >= DUSK_ROUND
        vec![
            station(10, 20, 1),
            worker(10010, 11, 21),
            worker(10012, 10, 21),
        ],
        0,
        vec![],
        vec![],
    );
    let turn = turn_from(payload);
    let mut state = BotState::default();
    // Station station(10,20) has footprint x in 10..=11, y in 19..=20, so the
    // gate cell is (xmax + 2, ymin - 1) = (13, 18).
    let gate = Pos { x: 13, y: 18 };
    assert!(
        !coregeek::brain::day::wall_gaps(&turn, &state).contains(&gate),
        "the gate stays open while roles may still be outside"
    );

    coregeek::brain::day::plan(&turn, &mut state);
    assert!(
        state.wall_gate_sealed,
        "everyone retreated, so the gate seals"
    );
    assert!(
        coregeek::brain::day::wall_gaps(&turn, &state).contains(&gate),
        "the final seal cell is planned once sealed"
    );
}

#[test]
fn task_closes_only_after_all_success_signals() {
    // Success requires the task point to close, the description to stay gone
    // and no error after submission. A single empty phaseTask never ends a task
    // (the issue #3 regression).
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 7;
    state.task.task_type = "自进化类1".into();
    state.task.description = "count files".into();
    state.task.best_answer = "42".into();
    state.task.cmd_history = vec!["ls | wc -l".into()];
    state.task.point = Some(Pos { x: 10, y: 10 });
    state.task.submitted_round = Some(10);
    state.task.timeout_round = 500;
    state.task.stage = coregeek::state::TaskStage::WaitingSubmit { attempts: 1 };

    // Round 11: phaseTask gone but the point is still valid → not confirmed.
    let turn = turn_from(world_full(
        11,
        vec![pioneer_with(vec![])],
        vec![],
        vec![task_point(10, 10, true, 0)],
        vec![],
    ));
    state.observe(&turn);
    assert!(
        state.task.active,
        "an empty phaseTask alone never ends a task"
    );

    // Round 12: point invalid + description still gone + still clean → success.
    let turn = turn_from(world_full(
        12,
        vec![pioneer_with(vec![])],
        vec![],
        vec![task_point(10, 10, false, 30)],
        vec![],
    ));
    state.observe(&turn);
    assert!(!state.task.active, "confirmed success releases the pioneer");
    assert!(
        !state.sop_cache.is_empty(),
        "a confirmed success caches the SOP"
    );
}

#[test]
fn task_dedupe_is_scoped_to_session_and_request_round() {
    // Identical `lastCmdResult` text in a NEW session must still be consumed.
    // The old global string dedupe swallowed exactly this case (issue #11).
    let mut state = BotState::default();
    state.seen_cmd_result = "[exitCode:0]\nANSWER: 42".into();
    state.task.active = true;
    state.task.session_id = 9;
    state.task.stage = coregeek::state::TaskStage::WaitingCmdResult { attempts: 0 };
    state.task.timeout_round = 500;
    state.task.cmd_request_round = Some(50);

    let mut payload = world_full(51, vec![pioneer_with(vec![])], vec![], vec![], vec![]);
    payload["lastCmdResult"] = json!("[exitCode:0]\nANSWER: 42");
    let turn = turn_from(payload);
    state.observe(&turn);
    assert_eq!(
        state.task.best_answer, "42",
        "same text in a new session is not swallowed"
    );
}

#[test]
fn night_aborts_a_task_when_a_tower_would_be_unmanned() {
    // Three towers, two workers and a tasking pioneer: the base cannot afford to
    // lose a gun, so the task yields to the defense.
    let turn = turn_from(world(
        vec![
            station(10, 20, 1),
            gatling(10020, 9, 24, 1),
            gatling(10021, 9, 25, 1),
            gatling(10022, 10, 25, 1),
            worker(10010, 9, 23),
            worker(10012, 10, 23),
            pioneer_with(vec![]),
        ],
        vec![robot(30001, 12, 24, 40, "challenger")],
    ));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 3;
    state.task.description = "count files".into();
    assert!(
        coregeek::brain::night::defense_needs_pioneer(&turn),
        "an unmanned tower under threat needs its operator back"
    );
    coregeek::brain::night::plan(&turn, &mut state);
    assert!(
        !state.task.active,
        "task yields when a tower lacks an operator"
    );
}

#[test]
fn timeout_partial_answer_never_uses_the_task_description() {
    // The old fallback submitted the first 100 characters of the task prose,
    // which scored zero every time (issues #9 / #11).
    let mut state = BotState::default();
    state.task.active = true;
    state.task.description = "请统计 /tmp/selfEvolutionTask/ 下所有文件的行数".into();
    assert!(
        coregeek::brain::task::partial_answer(&state).is_none(),
        "task prose is never an answer"
    );
    state.task.result_history = vec!["[exitCode:0]\nANSWER: {\"lines\": 42}".into()];
    assert_eq!(
        coregeek::brain::task::partial_answer(&state).as_deref(),
        Some("{\"lines\": 42}"),
        "a previously observed real answer is the fallback"
    );
}
