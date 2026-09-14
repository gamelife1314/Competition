//! Combat model, cross-round state and validator tests built from synthetic
//! round payloads.

use std::collections::HashMap;

use serde_json::{json, Value};

use coregeek::brain::combat::{bomb_impact, choose_attack, dizzy_impact, init_sim};
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

// ---------------------------------------------------------------------------
// Issue #18/#19: night survivability — a focused controller dies inside the
// round it is shot in, and the gun it manned goes silent with it.
// ---------------------------------------------------------------------------

fn worker_hp(id: i64, x: i32, y: i32, hp: i64) -> Value {
    worker_hp_items(id, x, y, hp, vec!["Medicine"])
}

fn worker_hp_items(id: i64, x: i32, y: i32, hp: i64, items: Vec<&str>) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "worker",
        "health": hp, "attackPower": 0, "attackRange": 0,
        "backPackCapability": 100, "backpack": items
    })
}

#[test]
fn night_controller_heals_at_the_night_threshold_not_the_day_one() {
    // 121/220 HP = 55%. The day rule (`day::use_medicine`, 30%) would let this
    // role keep working; at night a robot two cells away is one volley from
    // deleting it, and the tower it operates dies with it. Issue #18 lost
    // 20011 over six rounds and 20010 in two; issue #19 lost all three
    // operators on D2 night and the base fell to 105 HP behind them.
    let turn = turn_from(world(
        vec![gatling(10020, 10, 10, 1), worker_hp(10010, 11, 10, 121)],
        vec![robot(30001, 13, 10, 40, "challenger")], // chebyshev 2 from the operator
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("operator acts");
    assert_eq!(cmd.action, "use");
    assert_eq!(
        cmd.name.as_deref(),
        Some("Medicine"),
        "an operator under fire heals at 55%, not at 30%"
    );
    assert!(
        !plan.commands.contains_key(&10020),
        "the gun stays silent for the one round the operator spends healing"
    );
}

#[test]
fn night_threshold_does_not_fire_without_a_robot_in_reach() {
    // Same 55%, nobody within the 3-cell threat radius: a scratch at 3 a.m. can
    // wait, and the 10 gold potion is not spent for nothing.
    let turn = turn_from(world(
        vec![gatling(10020, 10, 10, 1), worker_hp(10010, 11, 10, 121)],
        vec![robot(30001, 20, 20, 40, "challenger")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let healed = plan
        .commands
        .get(&10010)
        .map(|cmd| cmd.name.as_deref() == Some("Medicine"))
        .unwrap_or(false);
    assert!(
        !healed,
        "no threat in reach ⇒ the day threshold still applies"
    );
}

#[test]
fn a_healthy_operator_keeps_the_gun_firing() {
    // 170/220 = 77% is above both thresholds: the potion is not burned and the
    // tower shoots, so the heal never costs firepower it did not have to.
    let turn = turn_from(world(
        vec![gatling(10020, 10, 10, 1), worker_hp(10010, 11, 10, 170)],
        vec![robot(30001, 13, 10, 40, "challenger")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    assert_eq!(
        plan.commands.get(&10010).map(|cmd| cmd.action.as_str()),
        None,
        "a healthy operator holds position"
    );
    let fired = plan.commands.get(&10020).expect("tower fires");
    assert_eq!(fired.action, "attack");
}

#[test]
fn a_critically_wounded_operator_breaks_contact_instead_of_dying_at_its_post() {
    // Issue #20: 20010 manned tower 20020 from HP 220 down to 30 across D1 night
    // (R105-R115) with robots already inside the ring, and never once had a move
    // command — it was still firing on the round it died, and the gun went
    // silent anyway. 30/220 = 13% with no Medicine in the backpack and a robot
    // two cells away: the post is a grave, the ring is a plan.
    let turn = turn_from(world_zones(
        vec![
            station(20, 20, 1),
            gatling(10020, 10, 10, 1),
            worker_hp_items(10010, 11, 10, 30, vec![]),
        ],
        vec![robot(30001, 13, 10, 40, "challenger")],
        vec![],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("the operator acts");
    assert_eq!(cmd.action, "move", "a dying operator breaks contact");
    let destination = cmd
        .targetPos
        .as_ref()
        .and_then(|targets| targets.first())
        .copied()
        .expect("a move carries a destination");
    assert!(
        coregeek::model::chebyshev(destination, Pos { x: 20, y: 20 })
            < coregeek::model::chebyshev(Pos { x: 11, y: 10 }, Pos { x: 20, y: 20 }),
        "and walks toward the ring, not deeper into the fight"
    );
    assert!(
        !plan.commands.contains_key(&10020),
        "the gun it cannot survive manning stays silent this round"
    );
}

#[test]
fn a_wounded_operator_with_a_potion_fights_on() {
    // The same 30 HP, but with a Medicine in hand: the potion restores FULL
    // health and puts the gun back in action, which beats losing the post for
    // the rest of the night. Retreat is the fallback, never the first answer.
    let turn = turn_from(world_zones(
        vec![
            station(20, 20, 1),
            gatling(10020, 10, 10, 1),
            worker_hp(10010, 11, 10, 30),
        ],
        vec![robot(30001, 13, 10, 40, "challenger")],
        vec![],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("the operator acts");
    assert_eq!(cmd.action, "use");
    assert_eq!(
        cmd.name.as_deref(),
        Some("Medicine"),
        "a potion outranks the retreat it would make unnecessary"
    );
}

#[test]
fn a_wounded_controller_out_of_position_never_walks_back_into_the_robots() {
    // Out of position AND bleeding out with nothing to heal with: the recall
    // must not drag it back across the map through the robots that are shooting
    // it. Same predicate as the adjacent case, so the two never alternate.
    let turn = turn_from(world_zones(
        vec![
            station(20, 20, 1),
            gatling(10020, 10, 10, 1),
            worker_hp_items(10010, 18, 20, 30, vec![]),
        ],
        vec![robot(30001, 20, 21, 40, "challenger")],
        vec![],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("the operator acts");
    assert_eq!(cmd.action, "move");
    let destination = cmd
        .targetPos
        .as_ref()
        .and_then(|targets| targets.first())
        .copied()
        .expect("a move carries a destination");
    assert!(
        coregeek::model::chebyshev(destination, Pos { x: 20, y: 20 })
            < coregeek::model::chebyshev(Pos { x: 18, y: 20 }, Pos { x: 20, y: 20 }),
        "it steps toward the ring, not back toward its tower"
    );
}

#[test]
fn a_wounded_controller_with_nobody_nearby_is_still_recalled() {
    // The recall stays unconditional for every controller that is not in
    // immediate danger: 30 HP with the nearest robot ten cells away is a wound
    // the post can wait out, and an unmanned tower is the defect the recall
    // exists to prevent.
    let turn = turn_from(world_zones(
        vec![
            station(20, 20, 1),
            gatling(10020, 10, 10, 1),
            worker_hp_items(10010, 14, 10, 30, vec![]),
        ],
        vec![robot(30001, 14, 20, 40, "challenger")],
        vec![],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("the operator acts");
    assert_eq!(cmd.action, "move");
    let destination = cmd
        .targetPos
        .as_ref()
        .and_then(|targets| targets.first())
        .copied()
        .expect("a move carries a destination");
    assert!(
        coregeek::model::chebyshev(destination, Pos { x: 10, y: 10 })
            < coregeek::model::chebyshev(Pos { x: 14, y: 10 }, Pos { x: 10, y: 10 }),
        "an unthreatened controller walks to its tower as usual"
    );
}

#[test]
fn a_gun_goes_to_a_controller_that_can_hold_it() {
    // Issue #22's P1: 20012 came out of D1 night at 20 HP and never healed —
    // nothing in the game restores health except a Medicine — so every night it
    // was threatened `night_withdraw` pulled it off tower 20020, the gun stayed
    // silent behind it, and the pairing held for 235 rounds while
    // `controller_withdrawn` fired 35 times. A fit controller standing one cell
    // away is the whole fix: pair the gun with the controller that can man it.
    let turn = turn_from(world_zones(
        vec![
            station(20, 20, 1),
            gatling(10020, 10, 10, 1),
            worker_hp_items(10010, 11, 10, 20, vec![]), // 9%, nothing to heal with
            worker(10011, 9, 10),                       // fit, same distance
        ],
        vec![robot(30001, 11, 9, 40, "challenger")],
        vec![],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let fired = plan.commands.get(&10020).expect("the gun still fires");
    assert_eq!(fired.action, "attack");
    assert_eq!(
        fired.controllerId.as_deref(),
        Some("10011"),
        "the fit controller mans the gun the wounded one cannot"
    );
    let wounded = plan.commands.get(&10010).expect("the wounded role acts");
    assert!(
        !(wounded.action == "attack"),
        "the wounded controller is a spare this round, not a gunner"
    );
}

#[test]
fn a_wounded_controller_is_still_paired_when_nobody_else_can_reach_the_gun() {
    // The preference is a preference, not a veto. One gun, one controller, and
    // that controller is bleeding: an unmanned tower is still the worse defect,
    // so the wounded one keeps the pairing and the ordinary withdrawal rule
    // decides what it does with it.
    let turn = turn_from(world_zones(
        vec![
            station(20, 20, 1),
            gatling(10020, 10, 10, 1),
            worker_hp_items(10010, 11, 10, 20, vec![]),
        ],
        vec![],
        vec![],
    ));
    let mut state = BotState::default();
    let pairs = coregeek::brain::night::stable_pairs(&turn, &mut state);
    assert_eq!(
        pairs,
        vec![(10010, 10020)],
        "with no one else to take the gun, the wounded controller keeps it"
    );
}

#[test]
fn a_wounded_controller_is_handed_back_its_gun_once_it_is_fit_again() {
    // The pairing cache keys on who is too wounded to hold a gun, so the swap
    // is not a one-way door: when the potion lands and the wound clears, the
    // nearest fit controller is the wounded one again, and it gets its post
    // back in the next round rather than at the next day rollover.
    let armed = |hp: i64| {
        turn_from(world_zones(
            vec![
                station(20, 20, 1),
                gatling(10020, 10, 10, 1),
                worker_hp_items(10010, 11, 10, hp, vec![]),
                worker(10011, 9, 10),
            ],
            vec![robot(30001, 11, 9, 40, "challenger")],
            vec![],
        ))
    };
    let mut state = BotState::default();
    let bleeding = armed(20);
    assert!(
        coregeek::brain::night::stable_pairs(&bleeding, &mut state).contains(&(10011, 10020)),
        "the fit controller takes the gun while the wound is untreated"
    );
    let healed = armed(220);
    assert!(
        coregeek::brain::night::stable_pairs(&healed, &mut state).contains(&(10010, 10020)),
        "and the nearer controller takes it back once it is fit"
    );
}

#[test]
fn a_critically_wounded_worker_walks_to_the_shop_for_the_only_cure() {
    // Issue #22's "撤退后无恢复路径": 20012 survived D1 night at 20 HP and stayed
    // there for 235 rounds. Nothing but a Medicine restores health, and the old
    // errand only fired for a role that already happened to be standing at the
    // shop — so a wounded controller could never leave the state that kept it
    // off its gun. Below the night withdrawal threshold the walk is the cure.
    let wounded = |hp: i64| {
        turn_from(day_world_at(
            day_round(5),
            vec![
                station(10, 20, 1),
                // All three weapon slots are filled, so no gold is competing
                // for a build site: the only question left is the errand.
                gatling(10020, 10, 10, 1),
                railgun(10030, 12, 10, 1),
                rocket(10040, 10, 12, 1),
                worker_hp_items(10010, 30, 30, hp, vec![]),
                worker(10011, 28, 30), // the last worker is the buyer
            ],
            100,
            vec![
                voucher("Medicine", 20),
                voucher("WeaponUpgradeVoucher1", 100),
            ],
            vec![zone(0, 0, "weaponShop"), zone(30, 31, "copper")],
        ))
    };
    let state = || BotState::default();
    let hurt = wounded(20);
    let cmd = {
        let mut state = state();
        let plan = coregeek::brain::day::plan(&hurt, &mut state);
        plan.commands.get(&10010).cloned()
    }
    .expect("the wounded worker acts");
    assert_eq!(cmd.action, "move", "it sets off for the shop");
    let destination = cmd
        .targetPos
        .as_ref()
        .and_then(|targets| targets.first())
        .copied()
        .expect("a move carries a destination");
    assert!(
        coregeek::model::chebyshev(destination, Pos { x: 0, y: 0 })
            < coregeek::model::chebyshev(Pos { x: 30, y: 30 }, Pos { x: 0, y: 0 }),
        "and the step is toward the shop, not the mine under its feet"
    );

    // Above the threshold the same hurt worker stays on the day's errand: the
    // shop trip is only worth abandoning everything for when the role has
    // already lost its gun.
    let scratched = wounded(150);
    let cmd = {
        let mut state = state();
        let plan = coregeek::brain::day::plan(&scratched, &mut state);
        plan.commands.get(&10010).cloned()
    }
    .expect("the hurt worker acts");
    assert_ne!(
        cmd.action, "move",
        "a scratch is not worth a cross-map walk when the mine is right there"
    );
}

#[test]
fn the_buyer_sells_its_pack_before_it_walks_to_the_shop() {
    // The buyer's shop trip outranks the sale, so a buyer that set off with an
    // unsold pack never got to sell it: the shop is paid in gold, the purchase
    // was refused, and the pack rode back to the mine. Issue #22's wallet froze
    // that way. The sale comes first; the shop trip happens next round, with
    // the gold in hand.
    let turn = turn_from(with_vendor_prices(
        day_world_at(
            // Late enough that the pre-night Medicine need is on the list, so
            // the worker is the nominated buyer and the shop trip is live.
            day_round(45),
            vec![
                station(10, 20, 1),
                json!({
                    "id": 10010, "pos": {"x": 2, "y": 2}, "roleType": "worker",
                    "health": 220, "attackPower": 0, "attackRange": 0,
                    "backPackCapability": 100,
                    "backpack": ["copper", "copper", "copper", "copper", "copper"]
                }),
            ],
            0,
            vec![
                voucher("Medicine", 20),
                voucher("WeaponUpgradeVoucher1", 100),
            ],
            vec![zone(0, 0, "weaponShop"), zone(9, 5, "vendor")],
        ),
        vec![voucher("copper", 20), voucher("stone", 5)],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10010).expect("the buyer acts");
    assert_eq!(cmd.action, "move");
    let destination = cmd
        .targetPos
        .as_ref()
        .and_then(|targets| targets.first())
        .copied()
        .expect("a move carries a destination");
    assert!(
        coregeek::model::chebyshev(destination, Pos { x: 9, y: 5 })
            < coregeek::model::chebyshev(Pos { x: 2, y: 2 }, Pos { x: 9, y: 5 }),
        "the step closes on the vendor that pays, not on the shop it cannot pay"
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
fn the_third_weapon_is_gated_by_the_purse_and_the_three_tower_cap() {
    // P0-2 (docs/FAILURE-ANALYSIS-2026-09-14.md §2.2): the two-tower rule that
    // held the whole purse for WeaponUpgradeVoucher1 made the third gun wait on
    // a 100-gold purchase that a frozen income never delivered — five matches
    // fought with two guns, and a first night that was always out-gunned. The
    // gun is now gated by the purse and by the three-tower cap, and nothing
    // else; the upgrade keeps its own priority through `intent_list`.
    let state = BotState::default();
    let towers = |gold: i64, count: usize, level: i64| {
        let mut roles = vec![station(10, 20, 1)];
        if count >= 1 {
            roles.push(gatling(10020, 10, 10, level));
        }
        if count >= 2 {
            roles.push(railgun(10030, 12, 10, level));
        }
        if count >= 3 {
            roles.push(rocket(10040, 10, 12, level));
        }
        turn_from(day_world_at(5, roles, gold, vec![], vec![]))
    };

    assert!(
        coregeek::brain::economy::may_build_weapon(&towers(25, 0, 1), &state),
        "first weapon builds with 25g"
    );
    assert!(
        coregeek::brain::economy::may_build_weapon(&towers(25, 1, 1), &state),
        "second weapon builds with 25g"
    );
    // The third slot is no longer hostage to the level-2 voucher: 25 gold in
    // hand and no level-2 gun is exactly the board the analysis was about.
    assert!(
        coregeek::brain::economy::may_build_weapon(&towers(25, 2, 1), &state),
        "the third weapon is bought with 25g, not with 125"
    );
    assert!(
        coregeek::brain::economy::may_build_weapon(&towers(25, 2, 2), &state),
        "…and an already-upgraded main gun does not change that"
    );
    // The two doors that remain. One gold short is one gold short — the gate is
    // the price, and a level-1 main gun does not lower it.
    assert!(
        !coregeek::brain::economy::may_build_weapon(&towers(24, 2, 1), &state),
        "24 gold does not buy a 25-gold weapon"
    );
    // Three towers is the cap, whatever the purse says (build order and the
    // no-overwrite rule are `tower_gaps`' and `validate`'s, unchanged).
    assert!(
        !coregeek::brain::economy::may_build_weapon(&towers(500, 3, 1), &state),
        "the three-tower cap holds with any amount of gold"
    );
}

#[test]
fn the_worker_lays_the_third_weapon_the_moment_the_gold_is_there() {
    // Two level-1 weapons + 25g, with a worker standing right next to the open
    // rocket slot. P0-2: that 25 gold is the third gun's, not a reserve held
    // for the 100-gold voucher — the analysis' five two-gun matches are exactly
    // this board. Day 2 on purpose: while day 1's ring is still being built the
    // 2nd/3rd tower waits for the ring (see `wall_first_p0.rs`:
    // `no_second_weapon_while_the_day_one_ring_is_still_open`).
    let turn = turn_from(day_world_at(
        day_round(5 + coregeek::model::ROUNDS_PER_DAY),
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
    let built: Vec<&str> = plan
        .commands
        .values()
        .filter(|cmd| cmd.action == "build")
        .filter_map(|cmd| cmd.name.as_deref())
        .collect();
    assert_eq!(
        built,
        vec!["rocket"],
        "25 gold in hand and the third weapon is not being laid: {:?}",
        plan.commands
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
fn a_task_that_timed_out_without_running_anything_retires_its_point() {
    // Issue #21: four of the eight sessions ended in `timeout` with cmdRounds=0
    // — accept, wait, expire. Nothing about the judger's sandbox changes round
    // to round, so re-accepting the same point just buys the next timeout: the
    // point is retired for the rest of the day. This does NOT re-open the
    // `cmdRounds=0` premature exit (which stays fixed): the judger ended this
    // session first, and the pioneer is free to take a LIVE point instead.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 3;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();
    state.task.accepted_round = 10;
    state.task.timeout_round = 40;
    state.task.point = Some(Pos { x: 10, y: 10 });
    state.task.stage = coregeek::state::TaskStage::WaitingDescription;

    let turn = turn_from(world_full(
        40,
        vec![pioneer_with(vec![])],
        vec![],
        vec![task_point(10, 10, true, 0)],
        vec![],
    ));
    state.observe(&turn);
    assert!(!state.task.active, "the timeout ends the session");
    assert_eq!(
        state.task_refusals.get(&Pos { x: 10, y: 10 }).copied(),
        Some(130),
        "the point that never opened a window is refused through end of day 1"
    );
}

#[test]
fn a_task_that_ran_commands_is_not_retired_by_a_timeout() {
    // The other half: a session that got commands executed hit a WORKING
    // window, so the point stays available — a timeout there is this script's
    // failure, not the point's, and the next session may well solve it.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 4;
    state.task.description = "count files".into();
    state.task.accepted_round = 10;
    state.task.timeout_round = 40;
    state.task.point = Some(Pos { x: 10, y: 10 });
    state.task.cmd_history = vec!["ls /tmp/selfEvolutionTask".into()];
    state.task.stage = coregeek::state::TaskStage::Planning;

    let turn = turn_from(world_full(
        40,
        vec![pioneer_with(vec![])],
        vec![],
        vec![task_point(10, 10, true, 0)],
        vec![],
    ));
    state.observe(&turn);
    assert!(!state.task.active);
    assert!(
        state.task_refusals.is_empty(),
        "a point whose window worked is still worth re-accepting"
    );
}

// ---------------------------------------------------------------------------
// Issue #21: the ring was breached on D2 night (walls 17 → 7) and the station
// bled from 1500 to 20 with it (-30 residual). Rebuilding it is not "upkeep".
// ---------------------------------------------------------------------------

/// Every radius-2 ring cell around the 2x2 station at `(sx, sy)`, as walls,
/// minus the cells in `missing`.
fn ring_world(round_no: i64, sx: i32, sy: i32, missing: &[(i32, i32)], roles: Vec<Value>) -> Value {
    // Footprint: x in sx..=sx+1, y in sy-1..=sy (see `station_footprint`), so
    // the radius-2 shell is the perimeter of x in sx-2..=sx+3, y in sy-3..=sy+2.
    let (x0, x1, y0, y1) = (sx - 2, sx + 3, sy - 3, sy + 2);
    let mut walls = Vec::new();
    let mut id = 20000;
    for x in x0..=x1 {
        for y in y0..=y1 {
            if x != x0 && x != x1 && y != y0 && y != y1 {
                continue; // interior, not the shell
            }
            if missing.contains(&(x, y)) {
                continue;
            }
            id += 1;
            walls.push(wall(id, x, y, 1, 5000));
        }
    }
    let mut all = vec![station(sx, sy, 1)];
    all.extend(walls);
    all.extend(roles);
    day_world_at(round_no, all, 0, vec![], vec![])
}

/// A day-2 stone carrier standing one step from the gap at (13, 19).
fn breach_roles() -> Vec<Value> {
    vec![json!({
        "id": 10010, "pos": {"x": 12, "y": 19}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "backPackCapability": 100, "backpack": ["stone"]
    })]
}

/// Ring cells left open in the breach fixtures: the one the carrier is next to
/// (13,19) plus seven others. The gate (13,18) is left standing so the fixture
/// is about the breach and nothing else.
fn breach_holes() -> Vec<(i32, i32)> {
    vec![
        (13, 19),
        (8, 17),
        (9, 17),
        (10, 17),
        (11, 17),
        (12, 17),
        (8, 22),
        (9, 22),
    ]
}

#[test]
fn a_breached_ring_is_repaired_past_the_maintenance_budget() {
    // Day 2, six ring cells already fortified today: the later-day budget is
    // spent. But the ring WAS closed (day 1 sealed it) and these eight cells
    // are holes the night punched in it — the crew must keep closing them. The
    // old 6-cell budget left the ring open with the stations's HP paying for it.
    let day_two = 135; // day 2, in_day_round 5
    let complete = turn_from(ring_world(day_two, 10, 20, &[], vec![]));
    let mut state = BotState::default();
    coregeek::brain::day::plan(&complete, &mut state);
    assert!(
        state.ring_ever_complete,
        "a ring with no holes is remembered as closed"
    );

    for cell in 0..6 {
        state.walled_cells_today.insert(Pos { x: cell, y: 0 });
    }
    let breached = turn_from(ring_world(day_two, 10, 20, &breach_holes(), breach_roles()));
    let plan = coregeek::brain::day::plan(&breached, &mut state);
    let cmd = plan
        .commands
        .get(&10010)
        .expect("the stone carrier is still on the wall line");
    assert_eq!(cmd.action, "build", "a breached ring outranks the budget");
    assert_eq!(
        cmd.targetPos.as_ref().and_then(|t| t.first()).copied(),
        Some(Pos { x: 13, y: 19 }),
        "and it closes the hole it is standing next to"
    );
}

#[test]
fn a_ring_that_was_never_closed_keeps_the_maintenance_budget() {
    // Same day, same holes, same spent budget — but this ring has never been
    // closed, so this is build-out and the 6-cell budget still binds. Otherwise
    // the cap the economy depends on would be gone on every later day.
    let mut state = BotState::default();
    for cell in 0..6 {
        state.walled_cells_today.insert(Pos { x: cell, y: 0 });
    }
    let breached = turn_from(ring_world(135, 10, 20, &breach_holes(), breach_roles()));
    let plan = coregeek::brain::day::plan(&breached, &mut state);
    assert!(
        plan.commands.get(&10010).is_none(),
        "the maintenance budget still stops build-out for the day"
    );
    assert!(
        !state.ring_ever_complete,
        "a holed ring is not a closed one"
    );
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

#[test]
fn build_prompt_budgets_the_sandbox_clock() {
    // 接口文档 §executeCmd: "执行时长不得超过15秒，否则视为执行指令超时" — a
    // timed-out command comes back `[TIMEOUT]` with no output, so the round is
    // spent twice. The prompt asked for a lot and never mentioned the ceiling;
    // a script with a `sleep`, a retry loop or an unqualified `find /` is a
    // guaranteed way to hit it.
    let mut state = BotState::default();
    state.task.description = "请统计 /tmp 下的文件数量".into();
    let turn = turn_from(day_world_at(5, vec![station(10, 20, 1)], 0, vec![], vec![]));
    let prompt = coregeek::brain::task::build_prompt(&state, &turn);
    assert!(
        prompt.contains("15 秒"),
        "prompt states the judger's hard timeout"
    );
    for banned in ["sleep", "重试"] {
        assert!(
            prompt.contains(banned),
            "prompt warns against `{banned}` in the script"
        );
    }
    assert!(
        prompt.contains("find / -maxdepth 4"),
        "an absent task directory has a documented fallback search"
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
fn a_rejection_keeps_the_cached_sop_on_the_first_strike() {
    // P1-3: a wrong answer no longer wipes the cache. The session did not run
    // the cached template (no SOP fast path), so nothing is even charged; the
    // task stays active for a retry with the cache intact.
    let mut state = BotState::default();
    state.sop_cache.push(coregeek::state::SopEntry {
        task_type: "自进化类1".into(),
        keywords: vec!["count".into(), "files".into()],
        template: "ls | wc -l".into(),
        ..Default::default()
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
    assert!(
        !state.sop_cache.is_empty(),
        "a template that did not run is never charged, let alone dropped"
    );
    assert_eq!(state.sop_cache[0].rejections, 0);
}

#[test]
fn the_used_template_is_evicted_only_on_the_second_consecutive_strike() {
    // P1-3 eviction rule: the template the session actually ran gets one
    // strike — reuse with feedback; the second consecutive rejection evicts.
    let run_session = |state: &mut BotState, round_no: i64| {
        state.task.active = true;
        state.task.accepted_round = round_no - 1;
        state.task.timeout_round = round_no + 300;
        state.task.task_type = "自进化类1".into();
        state.task.description = "count files".into();
        state.task.cmd_history = vec!["ls | wc -l".into()];
        state.task.sop_used_template = Some("ls | wc -l".into());
        state.task.stage = coregeek::state::TaskStage::WaitingSubmit { attempts: 0 };
        let mut payload = day_world_at(round_no, vec![pioneer_with(vec![])], 0, vec![], vec![]);
        payload["errors"] = json!([{"errorCode": 2}]);
        let turn = turn_from(payload);
        state.observe(&turn);
        state.task = Default::default();
    };

    let mut state = BotState::default();
    state.sop_cache.push(coregeek::state::SopEntry {
        task_type: "自进化类1".into(),
        keywords: vec!["count".into(), "files".into()],
        template: "ls | wc -l".into(),
        ..Default::default()
    });

    run_session(&mut state, 6);
    assert_eq!(
        state.sop_cache.len(),
        1,
        "first strike: the template survives for reuse with feedback"
    );
    assert_eq!(state.sop_cache[0].rejections, 1);
    assert_eq!(
        state.sop_cache[0].last_rejected.as_deref(),
        Some("ls | wc -l"),
        "the rejected bytes are remembered so they are never replayed"
    );

    run_session(&mut state, 40);
    assert!(
        state.sop_cache.is_empty(),
        "second consecutive strike: the template is evicted"
    );
}

#[test]
fn a_template_carrying_a_strike_is_reused_only_with_new_bytes() {
    // The rejected script must never be replayed verbatim: with a strike on
    // record and a binding that produces the SAME bytes, find_sop passes the
    // task to the LLM; a binding that produces new bytes (new parameters) is
    // the feedback-driven retry and is allowed.
    let mut entry = sop(
        "自进化类1",
        "统计城市名：北京 的人口",
        "python3 report.py --city {{城市名}}",
    );
    entry.rejections = 1;
    entry.last_rejected = Some("python3 report.py --city 北京".into());
    let mut state = BotState::default();
    state.sop_cache.push(entry);

    assert!(
        state
            .find_sop("自进化类1", "任务：统计城市名：北京 的人口")
            .is_none(),
        "identical bytes are not replayed"
    );
    let rebound = state.find_sop("自进化类1", "任务：统计城市名：上海 的人口");
    assert_eq!(
        rebound.map(|pair| pair.answer).as_deref(),
        Some("python3 report.py --city 上海"),
        "new parameters are a new attempt"
    );
}

#[test]
fn a_confirmed_success_clears_the_templates_strikes() {
    // Reuse with feedback converging: the rejected template is run again with
    // new bytes and the multi-signal probe confirms the success — the strikes
    // reset, because the script demonstrably works when its inputs are right.
    let mut state = BotState::default();
    state.sop_cache.push(coregeek::state::SopEntry {
        task_type: "自进化类1".into(),
        keywords: vec!["count".into(), "files".into()],
        template: "ls | wc -l".into(),
        explore: None,
        rejections: 1,
        last_rejected: Some("ls | wc -l".into()),
    });
    state.task.active = true;
    state.task.accepted_round = 5;
    state.task.timeout_round = 300;
    state.task.task_type = "自进化类1".into();
    state.task.description = "count files".into();
    state.task.point = Some(Pos { x: 3, y: 3 });
    state.task.sop_used_template = Some("ls | wc -l".into());
    state.task.submitted_round = Some(6);
    state.task.phase_missing_rounds = 2;
    state.task.point_closed_round = Some(7);

    let mut payload = day_world_at(8, vec![pioneer_with(vec![])], 0, vec![], vec![]);
    payload["teamOur"]["playerTasks"] = json!([{
        "taskType": "自进化类1",
        "taskPosition": {"x": 3, "y": 3},
        "coldDownRounds": 30,
        "scoreReward": 40, "goldReward": 40,
        "isValid": false, "timeoutRounds": 100
    }]);
    let turn = turn_from(payload);
    state.observe(&turn);

    assert!(!state.task.active, "the confirmed success closes the session");
    assert_eq!(state.sop_cache.len(), 1, "the working template stays cached");
    assert_eq!(state.sop_cache[0].rejections, 0, "strikes cleared");
    assert!(state.sop_cache[0].last_rejected.is_none());
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
                "id": 10002, "pos": {"x": 5, "y": 5}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": []
            }),
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
    // P1-4 续航包: one bottle per controller — two controllers, both bottles
    // in one command.
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

// ---------------------------------------------------------------------------
// P1: deadline budgeter, travel-cost selling, wall vouchers, third tower
// ---------------------------------------------------------------------------

fn wall(id: i64, x: i32, y: i32, level: i64, health: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "wall",
        "health": health, "attackPower": 0, "attackRange": 0,
        "level": level, "backPackCapability": 0, "backpack": []
    })
}

fn voucher(name: &str, price: i64) -> Value {
    json!({"name": name, "price": price})
}

/// `day_world_at` builds its payload with an empty `vendorShopList`, so ore
/// prices default to 1. The economy's "can we pay for this" questions are
/// priced, so the tests that ask them have to publish what the vendor pays.
fn with_vendor_prices(mut world: Value, prices: Vec<Value>) -> Value {
    world["vendorShopList"] = json!(prices);
    world
}

/// Day round (1-based `roundNo`) at which `in_day_round` is reached on day 1.
fn day_round(in_day_round: i64) -> i64 {
    in_day_round + 1
}

#[test]
fn budget_keeps_an_unaffordable_goal_as_purchase_intent() {
    // 25 gold cannot buy the 100-gold weapon upgrade, but the intent must
    // survive with a deadline so the buyer can start moving and the economy
    // worker knows what the gold is FOR. An empty list was the old freeze.
    let turn = turn_from(day_world_at(
        day_round(5),
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            railgun(10030, 12, 10, 1),
            worker(10010, 5, 5),
        ],
        25,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "weaponShop")],
    ));
    let state = BotState::default();
    let budget = coregeek::brain::economy::budget(&turn, &state, 0);
    let upgrade = budget
        .intent
        .iter()
        .find(|need| need.name == "WeaponUpgradeVoucher1")
        .expect("the upgrade is still intended");
    assert_eq!(upgrade.num, 1);
    assert!(
        upgrade.latest_round > 0 && upgrade.latest_round < coregeek::brain::economy::DUSK_ROUND,
        "the goal carries a buy-by round before dusk (got {})",
        upgrade.latest_round
    );
    assert!(
        budget.shopping.is_empty(),
        "nothing is affordable yet, so nothing is acted on"
    );
    assert_eq!(
        budget.head().map(|need| need.name.as_str()),
        Some("WeaponUpgradeVoucher1"),
        "the head of the budget is the goal we are saving for"
    );
}

#[test]
fn purchase_deadline_shrinks_with_the_walk_to_the_shop() {
    let near = turn_from(day_world_at(
        day_round(5),
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 1, 1),
        ],
        25,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "weaponShop")],
    ));
    let far = turn_from(day_world_at(
        day_round(5),
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 38, 30),
        ],
        25,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "weaponShop")],
    ));
    let state = BotState::default();
    let latest = |turn: &Turn| {
        coregeek::brain::economy::intent_list(turn, &state, 0)
            .into_iter()
            .find(|need| need.name == "WeaponUpgradeVoucher1")
            .expect("goal present")
            .latest_round
    };
    let near_round = latest(&near);
    let far_round = latest(&far);
    assert!(
        far_round < near_round,
        "a buyer 38 cells from the shop must start earlier ({far_round}) than one next to it ({near_round})"
    );
    assert!(
        coregeek::brain::economy::shop_travel(&far, Pos { x: 38, y: 30 })
            > coregeek::brain::economy::shop_travel(&near, Pos { x: 1, y: 1 }),
        "the trip cost itself is distance-based"
    );
}

#[test]
fn buyer_sets_off_before_the_gold_arrives_but_only_for_a_goal_its_pack_can_pay_for() {
    // Deadline two rounds away, buyer one trip away, and 100 gold's worth of
    // copper in its pack: the intent must push it toward the shop even though
    // the purse holds 10, so the purchase lands the round the sale does.
    let carrying: Vec<Value> = vec![
        station(10, 20, 1),
        gatling(10020, 10, 10, 1),
        json!({
            "id": 10010, "pos": {"x": 1, "y": 1}, "roleType": "worker",
            "health": 220, "attackPower": 0, "attackRange": 0,
            "backPackCapability": 100,
            "backpack": ["copper", "copper", "copper", "copper", "copper",
                         "copper", "copper", "copper", "copper", "copper",
                         "copper", "copper", "copper", "copper", "copper",
                         "copper", "copper", "copper", "copper", "copper"]
        }),
    ];
    let turn = turn_from(with_vendor_prices(
        day_world_at(
            day_round(45),
            carrying,
            10,
            vec![voucher("WeaponUpgradeVoucher1", 100)],
            vec![zone(0, 0, "weaponShop")],
        ),
        vec![voucher("copper", 5)],
    ));
    let state = BotState::default();
    let buyer = turn.role_by_id(10010).unwrap();
    let budget = coregeek::brain::economy::budget(&turn, &state, 0);
    assert!(budget.shopping.is_empty(), "10 gold buys nothing yet");
    assert!(
        coregeek::brain::economy::buyer_must_preposition(&turn, buyer, &budget.intent),
        "the ore in the pack covers the price: be there the round the sale lands"
    );

    // The same intent with an EMPTY pack must not drag the buyer off the
    // economy. Issue #22: a shop 20 rounds away and 5 gold in the purse fired
    // this on day-round 11 of every day, and the one role that could have
    // earned the 100 gold stood at the counter for the remaining 44 rounds.
    let empty = turn_from(day_world_at(
        day_round(45),
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 1, 1),
        ],
        10,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "weaponShop")],
    ));
    let empty_buyer = empty.role_by_id(10010).unwrap();
    let empty_budget = coregeek::brain::economy::budget(&empty, &state, 0);
    assert!(
        !coregeek::brain::economy::buyer_must_preposition(
            &empty,
            empty_buyer,
            &empty_budget.intent
        ),
        "a goal nothing can pay for is not a reason to stop earning"
    );

    // Early in the day the same intent must not drag the buyer off the economy
    // either — there is still time for the gold to arrive the honest way.
    let early = turn_from(day_world_at(
        day_round(5),
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 1, 1),
        ],
        10,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "weaponShop")],
    ));
    let early_buyer = early.role_by_id(10010).unwrap();
    let early_budget = coregeek::brain::economy::budget(&early, &state, 0);
    assert!(
        !coregeek::brain::economy::buyer_must_preposition(
            &early,
            early_buyer,
            &early_budget.intent
        ),
        "no pointless shop camping at round 5"
    );
}

#[test]
fn held_back_wall_stone_is_not_spending_power() {
    // Issue #22's frozen economy, in one assertion. The ring still wants 10
    // walls, the worker carries 20 stone, and the shop sells the upgrade for
    // 100: pricing that stone as cash made the team look funded, so the only
    // economic worker walked to the shop to buy a voucher it could not pay for
    // — and `sell_command` refuses to sell the ring's stone anyway.
    let stone_pack = |stone: usize, gold: i64| {
        let pack: Vec<&str> = std::iter::repeat("stone").take(stone).collect();
        turn_from(with_vendor_prices(
            day_world_at(
                day_round(45),
                vec![
                    station(10, 20, 1),
                    gatling(10020, 10, 10, 1),
                    json!({
                        "id": 10010, "pos": {"x": 1, "y": 1}, "roleType": "worker",
                        "health": 220, "attackPower": 0, "attackRange": 0,
                        "backPackCapability": 100, "backpack": pack
                    }),
                    // The wall line is ten cells short, so none of that stone is
                    // surplus: the vendor would turn all of it away.
                    worker(10011, 30, 30),
                ],
                gold,
                vec![voucher("WeaponUpgradeVoucher1", 100)],
                vec![zone(0, 0, "weaponShop")],
            ),
            vec![voucher("stone", 5)],
        ))
    };
    let state = BotState::default();
    let rich_in_stone = stone_pack(20, 5);
    assert_eq!(
        coregeek::brain::economy::liquid_gold(&rich_in_stone),
        5,
        "20 stone the ring still needs is not 100 gold"
    );
    assert!(
        !coregeek::brain::economy::upgrade_reachable(&rich_in_stone, &state),
        "a pack of wall stone must not keep the 100-gold upgrade 'reachable'"
    );

    // And the third tower it was gatekeeping: past the fallback round with 25
    // gold the rocket must be built, stone in the packs or not.
    let late = turn_from(day_world_at(
        day_round(coregeek::brain::economy::DUSK_ROUND - 15),
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            railgun(10030, 12, 10, 1),
            json!({
                "id": 10010, "pos": {"x": 3, "y": 3}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100,
                "backpack": ["stone", "stone", "stone", "stone", "stone",
                             "stone", "stone", "stone", "stone", "stone"]
            }),
        ],
        25,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "weaponShop")],
    ));
    assert!(
        coregeek::brain::economy::may_build_weapon(&late, &state),
        "the third tower must not be postponed by stone that cannot buy it"
    );
}

#[test]
fn sell_batch_grows_when_the_vendor_is_far() {
    // 11 rounds of walking to the vendor: worth a bigger load, and early enough
    // that the trip is not yet urgent.
    let far = turn_from(day_world_at(
        day_round(5),
        vec![station(10, 20, 1), worker(10010, 12, 12)],
        0,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "vendor")],
    ));
    let near = turn_from(day_world_at(
        day_round(5),
        vec![station(10, 20, 1), worker(10010, 2, 2)],
        0,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "vendor")],
    ));
    let state = BotState::default();
    let far_batch =
        coregeek::brain::economy::sell_batch(&far, &state, far.role_by_id(10010).unwrap());
    let near_batch =
        coregeek::brain::economy::sell_batch(&near, &state, near.role_by_id(10010).unwrap());
    assert_eq!(
        near_batch,
        coregeek::brain::economy::SELL_BATCH,
        "a vendor next door is worth cashing in at the base batch"
    );
    assert!(
        far_batch > near_batch,
        "a distant vendor is worth one bigger load ({far_batch} vs {near_batch})"
    );
    // Constraint kept: 4+ ore may always be sold — near the vendor that IS the
    // batch, and the dusk cash-out overrides the batch wherever the worker is.
    assert!(near_batch <= 4);

    // Once the purchase deadline is within one trip the batch collapses back to
    // the base size: waiting for a cheaper load would miss the purchase.
    let urgent = turn_from(day_world_at(
        day_round(coregeek::brain::economy::DUSK_ROUND - 2),
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 12, 12),
        ],
        0,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "vendor")],
    ));
    let urgent_role = urgent.role_by_id(10010).unwrap();
    assert_eq!(
        coregeek::brain::economy::sell_batch(&urgent, &state, urgent_role),
        coregeek::brain::economy::SELL_BATCH,
        "an imminent purchase deadline cashes out immediately"
    );
}

#[test]
fn distant_miner_cashes_out_before_dusk() {
    // 30 cells from the vendor: the walk home starts ~29 rounds before dusk,
    // not at dusk (the old flat constant left the ore unsold at nightfall).
    let turn = turn_from(day_world_at(
        day_round(5),
        vec![
            station(10, 20, 1),
            json!({
                "id": 10010, "pos": {"x": 30, "y": 30}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": ["iron"]
            }),
        ],
        0,
        vec![],
        vec![zone(0, 0, "vendor")],
    ));
    let state = BotState::default();
    let role = turn.role_by_id(10010).unwrap();
    let deadline = coregeek::brain::economy::sell_deadline(&turn, role);
    assert!(
        deadline < coregeek::brain::economy::DUSK_ROUND,
        "the sell deadline is pulled earlier by the trip ({deadline})"
    );
    assert!(
        !coregeek::brain::economy::should_sell(&turn, &state, role, 0),
        "one ore at round 5 is not worth the walk"
    );

    let mut payload = day_world_at(
        day_round(deadline),
        vec![
            station(10, 20, 1),
            json!({
                "id": 10010, "pos": {"x": 30, "y": 30}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": ["iron"]
            }),
        ],
        0,
        vec![],
        vec![zone(0, 0, "vendor")],
    );
    payload["roundNo"] = json!(day_round(deadline));
    let late = turn_from(payload);
    let late_role = late.role_by_id(10010).unwrap();
    assert!(
        coregeek::brain::economy::should_sell(&late, &state, late_role, 0),
        "the walk to the vendor must start on the deadline round"
    );
}

#[test]
fn wall_upgrade_vouchers_join_the_shopping_candidates() {
    // A standing ring plus a level-2 weapon path: 500 HP for 20 gold is now a
    // purchase candidate instead of an unused item in the shop.
    let ring: Vec<Value> = (0..8)
        .map(|index| wall(20000 + index, 6 + index as i32, 18, 1, 1000))
        .collect();
    let mut roles = vec![
        station(10, 20, 1),
        gatling(10020, 10, 10, 2),
        railgun(10030, 12, 10, 1),
        worker(10010, 3, 3),
    ];
    roles.extend(ring);
    let turn = turn_from(day_world_at(
        day_round(30),
        roles,
        120,
        vec![
            voucher("WallUpgradeVoucher1", 20),
            voucher("WallUpgradeVoucher2", 30),
        ],
        vec![zone(0, 0, "weaponShop")],
    ));
    let state = BotState::default();
    let list = coregeek::brain::economy::shopping_list(&turn, &state, 0);
    assert!(
        list.iter().any(|need| need.name == "WallUpgradeVoucher1"),
        "the wall upgrade is a candidate: {:?}",
        list.iter()
            .map(|need| need.name.as_str())
            .collect::<Vec<_>>()
    );
    let wall_need = list
        .iter()
        .find(|need| need.name == "WallUpgradeVoucher1")
        .unwrap();
    assert!(wall_need.num >= 1);
    assert!(
        wall_need.value > 0,
        "wall HP per gold is ranked, not fiat-ordered"
    );
}

#[test]
fn wall_vouchers_wait_for_the_ring_and_the_weapon_path() {
    // No ring yet → no wall voucher: the 20 gold belongs to the first towers.
    let turn = turn_from(day_world_at(
        day_round(30),
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            worker(10010, 3, 3),
        ],
        120,
        vec![voucher("WallUpgradeVoucher1", 20)],
        vec![zone(0, 0, "weaponShop")],
    ));
    let state = BotState::default();
    let list = coregeek::brain::economy::shopping_list(&turn, &state, 0);
    assert!(
        !list.iter().any(|need| need.name == "WallUpgradeVoucher1"),
        "no wall to upgrade: the voucher stays in the shop"
    );
}

#[test]
fn the_third_weapon_no_longer_waits_for_the_upgrade_voucher() {
    let two_l1 = |round_no: i64, gold: i64| {
        turn_from(day_world_at(
            round_no,
            vec![
                station(10, 20, 1),
                gatling(10020, 10, 10, 1),
                railgun(10030, 12, 10, 1),
                worker(10010, 3, 3),
            ],
            gold,
            vec![voucher("WeaponUpgradeVoucher1", 100)],
            vec![zone(0, 0, "weaponShop")],
        ))
    };
    let state = BotState::default();

    // P0-2 (docs/FAILURE-ANALYSIS-2026-09-14.md §2.2). The old rule held the
    // purse for WeaponUpgradeVoucher1 until the upgrade was *provably* out of
    // reach, and "out of reach" was measured in `liquid_gold` — cash plus every
    // sellable ore in a backpack. A worker that picks up iron on the way to the
    // stone vein therefore carried ≥ 100 of it all day, so the fallback never
    // fired and the third gun never came. Both halves of that door are gone:
    // the gun is bought whenever the purse can pay for it, at any hour.
    let early = two_l1(day_round(5), 25);
    assert!(
        coregeek::brain::economy::may_build_weapon(&early, &state),
        "25 gold at round 5 is a third gun, not a reserve for a 100-gold voucher"
    );
    let late = two_l1(day_round(coregeek::brain::economy::DUSK_ROUND - 15), 25);
    assert!(
        coregeek::brain::economy::may_build_weapon(&late, &state),
        "the rocket must not be postponed indefinitely (issue #9)"
    );

    // The upgrade keeps its OWN priority: it is the head of the shopping list
    // and is funded before the larger purchases (`intent_list`, priority 0).
    // What it lost is the veto over the gun.
    let funded = two_l1(day_round(coregeek::brain::economy::DUSK_ROUND - 15), 100);
    assert!(
        coregeek::brain::economy::upgrade_reachable(&funded, &state),
        "100 gold in hand keeps the upgrade path alive"
    );
    let intent = coregeek::brain::economy::intent_list(&funded, &state, 0);
    assert_eq!(
        intent.first().map(|need| need.name.as_str()),
        Some("WeaponUpgradeVoucher1"),
        "the 100-gold upgrade is still the first thing the economy funds"
    );
    assert!(
        coregeek::brain::economy::may_build_weapon(&funded, &state),
        "…and it no longer blocks the third slot while it is being saved for"
    );

    // A voucher already in a backpack is the same story: the upgrade is on its
    // way, and the gun is built alongside it rather than after it.
    let mut carried: Vec<Value> = vec![
        station(10, 20, 1),
        gatling(10020, 10, 10, 1),
        railgun(10030, 12, 10, 1),
    ];
    carried.push(json!({
        "id": 10010, "pos": {"x": 3, "y": 3}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "backPackCapability": 100, "backpack": ["WeaponUpgradeVoucher1"]
    }));
    let held = turn_from(day_world_at(
        day_round(coregeek::brain::economy::DUSK_ROUND - 15),
        carried,
        25,
        vec![voucher("WeaponUpgradeVoucher1", 100)],
        vec![zone(0, 0, "weaponShop")],
    ));
    assert!(
        coregeek::brain::economy::upgrade_reachable(&held, &state),
        "a carried voucher makes the upgrade reachable without any gold"
    );
    assert!(
        coregeek::brain::economy::may_build_weapon(&held, &state),
        "the banked upgrade no longer costs the third slot"
    );
}

// ---------------------------------------------------------------------------
// P1: parameterised SOPs and schema-checked answers
// ---------------------------------------------------------------------------

fn sop(task_type: &str, description: &str, template: &str) -> coregeek::state::SopEntry {
    coregeek::state::SopEntry {
        task_type: task_type.into(),
        keywords: coregeek::state::keywords_of(description),
        template: template.into(),
        ..Default::default()
    }
}

#[test]
fn sop_template_binds_the_new_task_parameters() {
    // A cached script is a TEMPLATE: the values that differ between two tasks
    // of the same kind are placeholders, so replaying it on a new task cannot
    // silently answer with the previous task's inputs.
    let entry = sop(
        "自进化类1",
        "统计 /tmp/a 的文件数量",
        "python3 count.py --dir {{目录}} --city {{城市}}",
    );
    let bound = entry
        .bind("统计 /tmp/b 的文件数量。目录：/tmp/b 城市：上海")
        .expect("both parameters bind from the new description");
    assert!(bound.contains("--dir /tmp/b"), "got {bound}");
    assert!(bound.contains("--city 上海"), "got {bound}");
    assert!(!bound.contains("{{"), "no placeholder survives: {bound}");

    // A placeholder the new description does not name makes the template
    // unusable — running it with the old value would answer a different task.
    assert!(
        entry.bind("统计 /tmp/b 的文件数量").is_none(),
        "an unbindable parameter must not fall back to the previous value"
    );
}

#[test]
fn sop_reuse_requires_a_fingerprint_match_not_just_a_task_type() {
    let mut state = BotState::default();
    state.sop_cache.push(sop(
        "自进化类1",
        "统计 /tmp/data 目录下的文件数量",
        "ls /tmp/data | wc -l",
    ));

    // Same task type AND matching keywords: the cached script is reused.
    assert!(
        state
            .find_sop("自进化类1", "请统计 /tmp/data 目录下的文件数量")
            .is_some(),
        "a fingerprint match reuses the SOP"
    );
    // Same task type, different task: NOT reused (this was the issue #9 bug —
    // the type alone was treated as proof the script still applied).
    assert!(
        state
            .find_sop("自进化类1", "统计 /tmp/data 的字节数")
            .is_none(),
        "the same type is not enough on its own"
    );
    // Same task, different type: also not reused.
    assert!(
        state
            .find_sop("自进化类2", "请统计 /tmp/data 目录下的文件数量")
            .is_none(),
        "type must match too"
    );
}

#[test]
fn incomplete_answer_is_banked_with_feedback_for_the_replan() {
    // Submit-as-accumulating (P0-1): an answer missing required fields is
    // banked the round it exists — the judger keeps the highest pass rate ever
    // submitted, so holding it back can only lower the floor. The missing
    // fields are recorded so the replan that follows the rejection overwrites
    // the banked attempt with a better one.
    let turn = turn_from(day_world_at(
        day_round(30),
        vec![pioneer_with(vec![])],
        0,
        vec![],
        vec![],
    ));
    let pioneer = turn.role_by_id(10011).unwrap();
    let mut state = BotState::default();
    state.task.active = true;
    state.task.timeout_round = turn.round_no + 50;
    state.task.description = "输出：城市、人口、面积".into();
    state.task.stage = coregeek::state::TaskStage::HaveAnswer {
        answer: "{\"城市\":\"上海\"}".into(),
    };

    let mut plan = coregeek::brain::Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan)
        .expect("the partial answer is banked, not held back");
    assert_eq!(cmd.action, "submitAnswer");
    assert!(
        matches!(state.task.stage, coregeek::state::TaskStage::WaitingSubmit { .. }),
        "then the session waits for the verdict, got {:?}",
        state.task.stage
    );
    assert_eq!(
        state.task.schema_gaps,
        vec!["人口".to_string(), "面积".to_string()],
        "the missing fields are named for the next prompt"
    );
    assert!(
        coregeek::brain::task::build_prompt(&state, &turn).contains("人口"),
        "the retry prompt asks for the missing fields"
    );

    // The deadline changes nothing: the banked partial is never traded for a
    // gamble on a fresh plan, near the timeout or far from it.
    state.task.timeout_round = turn.round_no + 2;
    state.task.stage = coregeek::state::TaskStage::HaveAnswer {
        answer: "{\"城市\":\"上海\"}".into(),
    };
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);
    assert_eq!(
        cmd.map(|command| command.action),
        Some("submitAnswer".to_string()),
        "the partial answer is submitted at the deadline too"
    );
}

#[test]
fn schema_gate_passes_a_complete_answer_and_an_unknown_schema() {
    let description = "输出：城市、人口、面积";
    assert_eq!(
        coregeek::brain::task::expected_fields(description),
        vec!["城市".to_string(), "人口".to_string(), "面积".to_string()]
    );
    assert!(
        coregeek::brain::task::answer_schema_gaps(
            description,
            "{\"城市\":\"上海\",\"人口\":1,\"面积\":2}"
        )
        .is_empty(),
        "a complete answer passes"
    );
    assert_eq!(
        coregeek::brain::task::answer_schema_gaps(description, "{\"城市\":\"上海\"}"),
        vec!["人口".to_string(), "面积".to_string()]
    );
    // No schema could be derived: the answer is accepted as-is, because a
    // false positive here would reject a correct answer.
    assert!(
        coregeek::brain::task::answer_schema_gaps("随便写点什么", "whatever").is_empty(),
        "an unknown schema never blocks a submission"
    );
}

// ---------------------------------------------------------------------------
// Issue #15: the third weapon vs. the upgrade reserve, and a cash-starved
// economy that would not sell what it was carrying.
// ---------------------------------------------------------------------------

/// `n` ores in a worker's pack at `(x, y)`.
fn laden_worker(id: i64, x: i32, y: i32, n: usize) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "backPackCapability": 100,
        "backpack": vec!["iron"; n]
    })
}

fn two_l1_with_gold(gold: i64, in_day: i64) -> Turn {
    turn_from(day_world_at(
        day_round(in_day),
        vec![
            station(10, 20, 1),
            gatling(10020, 10, 10, 1),
            railgun(10030, 12, 10, 1),
        ],
        gold,
        vec![],
        vec![],
    ))
}

#[test]
fn a_rich_purse_buys_the_third_weapon_and_the_voucher_together() {
    // Issue #15: "may_build_weapon 判定逻辑未在金币充裕时触发第3座建造" — the
    // opponent's three guns out-shot our two for the whole first night. Under
    // P0-2 this is the easy case rather than the only one: the gun is gated by
    // its own price, so a purse that covers the voucher too spends both.
    let state = BotState::default();
    let both = coregeek::model::WEAPON_BUILD_COST + coregeek::brain::economy::WEAPON_VOUCHER1_PRICE;
    assert!(
        coregeek::brain::economy::may_build_weapon(&two_l1_with_gold(both, 5), &state),
        "{both} gold covers the third weapon and the upgrade voucher together"
    );
    // The upgrade's own funding is untouched by the build: 125 - 25 = 100 is
    // exactly the voucher's price, and the voucher is still head of the list.
    assert_eq!(
        both - coregeek::model::WEAPON_BUILD_COST,
        coregeek::brain::economy::WEAPON_VOUCHER1_PRICE,
        "the build leaves the voucher's price in the purse"
    );
    assert_eq!(
        coregeek::brain::economy::intent_list(
            &two_l1_with_gold(both, 5),
            &state,
            coregeek::model::WEAPON_BUILD_COST
        )
        .first()
        .map(|need| need.name.as_str()),
        Some("WeaponUpgradeVoucher1"),
        "the upgrade is funded before anything else, gun or no gun"
    );
    // Issue #15's other half, unchanged: one gold short is one gold short.
    assert!(
        !coregeek::brain::economy::may_build_weapon(
            &two_l1_with_gold(coregeek::model::WEAPON_BUILD_COST - 1, 5),
            &state
        ),
        "the gun still costs its own 25 gold"
    );
}

#[test]
fn a_cash_starved_team_sells_its_pack_instead_of_waiting_for_a_load() {
    // Issue #15: the economy produced its first coin at r=27 — by which time
    // the window for the second and third weapon had closed — because a worker
    // stood on three ore waiting for the amortized batch an eleven-round walk
    // to the vendor is worth. The batch is an optimization; the next gun is
    // not.
    let state = BotState::default();
    let pack = |n: usize, gold: i64| {
        turn_from(day_world_at(
            day_round(5),
            vec![
                station(10, 20, 1),
                gatling(10020, 10, 10, 1),
                laden_worker(10010, 12, 12, n),
            ],
            gold,
            vec![voucher("WeaponUpgradeVoucher1", 100)],
            vec![zone(0, 0, "vendor")],
        ))
    };

    let broke = pack(3, 0);
    assert_eq!(
        coregeek::brain::economy::sell_batch(&broke, &state, broke.role_by_id(10010).unwrap()),
        coregeek::brain::economy::SELL_BATCH,
        "three ore in hand beat six ore promised by a walk we cannot afford"
    );

    // Once the team can pay for the next gun the walk is a cost again, and the
    // bigger load is worth carrying — the old amortization, unchanged.
    let funded = pack(3, coregeek::model::WEAPON_BUILD_COST);
    let funded_batch =
        coregeek::brain::economy::sell_batch(&funded, &state, funded.role_by_id(10010).unwrap());
    assert!(
        funded_batch > coregeek::brain::economy::SELL_BATCH,
        "with the next gun already paid for the far vendor is worth a load ({funded_batch})"
    );

    // Nothing to sell means nothing to decide: an empty pack leaves the batch
    // alone (this is the constraint that keeps the far-vendor test honest).
    let empty = pack(0, 0);
    assert!(
        coregeek::brain::economy::sell_batch(&empty, &state, empty.role_by_id(10010).unwrap())
            > coregeek::brain::economy::SELL_BATCH,
        "an empty pack is not a reason to shorten the batch"
    );
}

// ---------------------------------------------------------------------------
// Issue #18: the purse froze at 0 from R60 to the end of the match — base
// level 1, no voucher ever bought — because every worker was on the ring's
// stone errand and stone is the one ore the vendor is refused.
// ---------------------------------------------------------------------------

/// Two workers and a ring full of gaps, with a stone vein and an iron vein
/// both touching the crew. `round_no` picks the day; the ring has no walls in
/// either case, so `stone_demand` is at its largest.
fn two_vein_world(round_no: i64, extra: Vec<Value>) -> Value {
    let mut zones = vec![
        zone(5, 6, "stone"),
        zone(6, 5, "iron"),
        zone(0, 0, "vendor"),
    ];
    zones.extend(extra);
    day_world_at(
        round_no,
        vec![
            station(10, 20, 1),
            worker(10010, 30, 30), // the wall crew
            worker(10011, 5, 5),   // the dedicated economy worker (highest id)
        ],
        0,
        vec![],
        zones,
    )
}

fn first_target(cmd: &RoleCommand) -> Option<Pos> {
    cmd.targetPos
        .as_ref()
        .and_then(|list| list.first())
        .copied()
}

#[test]
fn economy_worker_digs_ore_the_vendor_buys_not_the_ring_stone() {
    // Day 2, ring wide open, stone short: the wall crew's errand is stone, and
    // it never ends — `build` spends the pack, so `team_ores(STONE) <
    // stone_demand` is restored by the act of closing a gap. A worker kept on
    // it carries nothing sellable, and `sellable_ores` refuses to sell the
    // stone itself, so the team earns nothing, buys nothing and repairs
    // nothing: issue #18's purse at exactly 0 from R60 to R361, base level 1.
    // One worker has to be digging iron. The second stone vein is what keeps
    // this an honest test: without it the wall crew reserves the only stone on
    // the map, the economy worker falls through `choose_mine`'s "no stone
    // reachable" path to the iron by accident, and the freeze this test exists
    // to prevent is masked by a claim collision.
    let turn = turn_from(two_vein_world(135, vec![zone(20, 20, "stone")]));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);

    let economy = plan.commands.get(&10011).expect("economy worker acts");
    assert_eq!(economy.action, "collect");
    assert_eq!(
        first_target(economy),
        Some(Pos { x: 6, y: 5 }),
        "the economy worker digs the iron, not the stone beside it"
    );

    // The ring still gets its digger: the same round, the other worker walks to
    // the stone vein. Only ONE role leaves the wall line, never both.
    let crew = plan.commands.get(&10010).expect("wall crew acts");
    assert_ne!(
        (crew.action.as_str(), first_target(crew)),
        ("collect", Some(Pos { x: 6, y: 5 })),
        "the wall crew must not follow the economy worker onto the iron"
    );
}

#[test]
fn the_gold_loop_actually_sells_at_the_vendor() {
    // Issue #20: 180 shopping events, 0 sells, gold frozen at 1 from R162 on —
    // the crew mined, built and bought nothing, because the worker that was
    // supposed to keep the money moving followed the wall crew onto stone
    // instead. The stash is only worth what the vendor pays for it, so the
    // designated economy worker walks its load to the vendor and SELLS, on the
    // map where the ring is still asking for stone. This is the invariant the
    // keep-list protects; everything else about the day may bend around it.
    let turn = turn_from(day_world_at(
        135,
        vec![
            station(10, 20, 1),
            json!({
                "id": 10010, "pos": {"x": 30, "y": 30}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": []
            }),
            json!({
                "id": 10011, "pos": {"x": 1, "y": 0}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": ["iron", "iron", "iron", "iron"]
            }),
        ],
        0,
        vec![],
        vec![zone(20, 20, "stone"), zone(0, 0, "vendor")],
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let cmd = plan.commands.get(&10011).expect("economy worker acts");
    assert_eq!(cmd.action, "sell", "the loop must actually sell");
    assert_eq!(
        cmd.name.as_deref(),
        Some("iron"),
        "and sell the load it is carrying"
    );
    assert!(
        cmd.num.unwrap_or(0) > 0,
        "a sell of nothing is still a frozen economy"
    );
}

#[test]
fn day_one_keeps_the_whole_crew_on_the_ring() {
    // The exemption is for the ring's build-out, not for the ring's upkeep: on
    // day 1 both workers fetch stone, because the ring is what makes every
    // later day affordable (wall-first build order). This is the behaviour the
    // economy worker's gold loop must not preempt.
    let turn = turn_from(two_vein_world(
        5,
        vec![zone(20, 20, "stone")], // nearer to 10010, so 10011 keeps its own vein
    ));
    let mut state = BotState::default();
    let plan = coregeek::brain::day::plan(&turn, &mut state);
    let economy = plan.commands.get(&10011).expect("economy worker acts");
    assert_eq!(economy.action, "collect");
    assert_eq!(
        first_target(economy),
        Some(Pos { x: 5, y: 6 }),
        "on day 1 the economy worker is a wall builder like everyone else"
    );
}

#[test]
fn a_sellable_vein_beats_the_nearer_stone() {
    // The low-level rule the day planner leans on: the sellable selector skips
    // stone even when the stone is closer, and falls back to it only when the
    // map has nothing else to dig.
    let turn = turn_from(day_world_at(
        135,
        vec![station(10, 20, 1), worker(10011, 5, 5)],
        0,
        vec![],
        vec![zone(5, 6, "stone"), zone(9, 9, "copper")],
    ));
    let state = BotState::default();
    let role = turn.role_by_id(10011).unwrap();
    let pick = coregeek::brain::economy::choose_sellable_mine(
        &turn,
        &state,
        role.pos,
        &std::collections::HashSet::new(),
    );
    assert_eq!(
        pick,
        Some((Pos { x: 9, y: 9 }, "copper".to_string())),
        "the nearer stone is not income"
    );

    // Nothing but stone on the map: income is impossible anyway, and idling is
    // worse than a load the ring can hold back.
    let stone_only = turn_from(day_world_at(
        135,
        vec![station(10, 20, 1), worker(10011, 5, 5)],
        0,
        vec![],
        vec![zone(5, 6, "stone")],
    ));
    let pick = coregeek::brain::economy::choose_sellable_mine(
        &stone_only,
        &state,
        stone_only.role_by_id(10011).unwrap().pos,
        &std::collections::HashSet::new(),
    );
    assert_eq!(pick, Some((Pos { x: 5, y: 6 }, "stone".to_string())));
}

// ---------------------------------------------------------------------------
// 任务书 4.5: the Bomb and the DizzyWeapon act on ROBOTS of both teams and on
// nothing else — "眩晕法宝、范围炸弹仅对双方机器人有效，对敌方建筑、角色无效".
// ---------------------------------------------------------------------------

/// One of the opponent's roles. They arrive in `teamEnemy.roles` — a different
/// list from `robot.roles` — which is the whole reason the two items below
/// cannot touch them.
fn enemy_role(id: i64, role: &str, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": role,
        "health": if role == "worker" { 220 } else { 200 },
        "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

/// `world` with the opponent's roster filled in.
fn world_with_enemy(our_roles: Vec<Value>, enemy_roles: Vec<Value>, robots: Vec<Value>) -> Value {
    let mut payload = world(our_roles, robots);
    payload["teamEnemy"]["roles"] = json!(enemy_roles);
    payload
}

#[test]
fn an_enemy_only_cluster_is_never_a_bomb_or_dizzy_target() {
    // A 2x2 block of their workers and pioneers is the juiciest-looking 3x3 on
    // the board, and with no robot anywhere it attracts nothing: `turn.enemy`
    // is not a victim list.
    let turn = turn_from(world_with_enemy(
        vec![station(10, 24, 1)],
        vec![
            enemy_role(20010, "worker", 20, 20),
            enemy_role(20011, "worker", 21, 20),
            enemy_role(20012, "pioneer", 20, 21),
            enemy_role(20013, "pioneer", 21, 21),
        ],
        vec![],
    ));
    assert_eq!(turn.enemy.len(), 4, "their roles really are on the board");
    assert_eq!(bomb_impact(&turn), None);
    assert_eq!(dizzy_impact(&turn), None);
}

#[test]
fn a_bomb_or_dizzy_impact_is_decided_by_the_robots_alone() {
    // Two robots in one 3x3, close enough to the base that both items clear
    // their thresholds. Their roles then stand right beside that cluster — on
    // one side in `west`, on the other in `east`. Moving them, or deleting them
    // entirely, must not move the chosen impact by a single cell.
    let robots = vec![
        robot(30001, 14, 22, 40, "challenger"),
        robot(30002, 15, 22, 40, "challenger"),
    ];
    let ours = || vec![station(10, 24, 1)];
    let west = vec![
        enemy_role(20010, "worker", 13, 22),
        enemy_role(20011, "pioneer", 13, 21),
        enemy_role(20012, "worker", 12, 22),
    ];
    let east = vec![
        enemy_role(20010, "worker", 16, 22),
        enemy_role(20011, "pioneer", 17, 22),
        enemy_role(20012, "worker", 17, 21),
    ];

    let bare = turn_from(world_with_enemy(ours(), vec![], robots.clone()));
    let with_west = turn_from(world_with_enemy(ours(), west, robots.clone()));
    let with_east = turn_from(world_with_enemy(ours(), east, robots));
    assert_eq!(with_west.enemy.len(), 3, "their roles really are on the board");

    assert_eq!(bomb_impact(&with_west), bomb_impact(&bare));
    assert_eq!(bomb_impact(&with_east), bomb_impact(&bare));
    assert_eq!(dizzy_impact(&with_west), dizzy_impact(&bare));
    assert_eq!(dizzy_impact(&with_east), dizzy_impact(&bare));

    // Both items do land on the robot cluster, and never on one of their roles.
    let bomb = bomb_impact(&with_west).expect("the robots are a real cluster");
    let dizzy = dizzy_impact(&with_west).expect("the cluster clears the dizzy threshold");
    assert_eq!(bomb, Pos { x: 14, y: 22 });
    assert_eq!(dizzy, Pos { x: 14, y: 22 });
    assert!(
        with_west.enemy.iter().all(|unit| unit.pos != bomb && unit.pos != dizzy),
        "an enemy role cell must never be picked as an impact"
    );
}
