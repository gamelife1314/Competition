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
    day_world_at(5, our_roles, gold, shop_items, zones)
}

fn day_world_at(round_no: i64, our_roles: Vec<Value>, gold: i64, shop_items: Vec<Value>, zones: Vec<Value>) -> Value {
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
        cmd.targetPos.as_ref().and_then(|list| list.first()).copied(),
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
    assert_eq!(cmd.action, "move", "dusk worker walks to the vendor to sell");
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
    assert_eq!(cmd.action, "sell", "dusk worker sells its ore at the vendor");
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
    assert_ne!(cmd.action, "collect", "far worker ignores the adjacent mine");
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
        assert_eq!(cmd.action, "attack", "tower {tower_id} is operated on day-2 night");
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
    assert_eq!(cmd.name.as_deref(), Some("wall"), "wall before weapon despite 100 gold");
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
        vec![zone(9, 10, "iron"), zone(10, 11, "stone"), zone(20, 20, "stone")],
    ));
    let state = BotState::default();
    let mine = coregeek::brain::economy::choose_mine(
        &turn,
        &state,
        Pos { x: 10, y: 10 },
        5,
        &std::collections::HashSet::new(),
    );
    assert_eq!(mine.map(|(pos, _)| pos), Some(Pos { x: 10, y: 11 }), "stone for the wall line wins");
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
    assert_eq!(cmd.action, "move", "spare worker shelters instead of mining");
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
    assert_eq!(cmd.name.as_deref(), Some("Medicine"), "dying worker heals first");
}

// ---------------------------------------------------------------------------
// Problem 4 (Issue #3): workers trapped outside the wall ring + economy stall
// ---------------------------------------------------------------------------

#[test]
fn wall_gaps_builds_outer_ring_first_and_leaves_gate_open() {
    // Two-cell-thick ring: the entrance corridor (one cell per ring) is never
    // built, and the FURTHEST cells (outer ring) come first so the gate stays
    // open until every role has retreated inside.
    let probe = turn_from(day_world_at(5, vec![station(10, 20, 1)], 0, vec![], vec![]));
    let state = BotState::default();
    let gaps = coregeek::brain::day::wall_gaps(&probe, &state);
    assert!(!gaps.is_empty(), "wall gaps exist");
    let gate = [Pos { x: 13, y: 18 }, Pos { x: 14, y: 18 }];
    assert!(gate.iter().all(|cell| !gaps.contains(cell)), "gate cells stay open");
    let footprint = coregeek::model::station_footprint(Pos { x: 10, y: 20 });
    assert_eq!(
        coregeek::model::footprint_distance(gaps[0], &footprint),
        3,
        "furthest (outer) ring builds before the inner ring"
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
    let turn = turn_from(world_zones(vec![worker(10010, 2, 2), wall_unit], vec![], zones));
    let role = turn.role_by_id(10010).unwrap();
    let stands = vec![Pos { x: 4, y: 2 }];
    let mut claimed = std::collections::HashSet::new();
    let cmd = coregeek::brain::walk_or_remove_wall(&turn, role, &stands, &mut claimed)
        .expect("worker demolishes the wall");
    assert_eq!(cmd.action, "remove");
    assert_eq!(
        cmd.targetPos.as_ref().and_then(|list| list.first()).copied(),
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

    let turn = turn_from(day_world_at(20, vec![station(10, 20, 1)], 0, vec![], vec![]));
    state.observe(&turn);

    assert!(state.task.active, "cleared phaseTask must not end the task");
    assert_eq!(state.task.description, "请实现一个排序函数", "description must persist");
}

#[test]
fn economy_holds_gold_reserve_for_main_weapon_upgrade() {
    let no_tower = turn_from(day_world_at(5, vec![station(10, 20, 1)], 25, vec![], vec![]));
    assert!(coregeek::brain::economy::may_build_weapon(&no_tower), "first weapon builds with 25g");

    let one_tower = turn_from(day_world_at(
        5,
        vec![station(10, 20, 1), gatling(10020, 10, 10, 1)],
        25,
        vec![],
        vec![],
    ));
    assert!(coregeek::brain::economy::may_build_weapon(&one_tower), "second weapon builds with 25g");

    // Two level-1 weapons: the gold must be reserved for the main weapon's
    // WeaponUpgradeVoucher1 instead of a third level-1 weapon.
    let two_l1 = turn_from(day_world_at(
        5,
        vec![station(10, 20, 1), gatling(10020, 10, 10, 1), railgun(10030, 12, 10, 1)],
        25,
        vec![],
        vec![],
    ));
    assert!(!coregeek::brain::economy::may_build_weapon(&two_l1), "reserve held for upgrade voucher");

    // Once the main weapon is level 2, more weapons may be built while gold
    // still leaves the 25g reserve untouched.
    let two_with_l2 = turn_from(day_world_at(
        5,
        vec![station(10, 20, 1), gatling(10020, 10, 10, 2), railgun(10030, 12, 10, 1)],
        50,
        vec![],
        vec![],
    ));
    assert!(coregeek::brain::economy::may_build_weapon(&two_with_l2), "level-2 main weapon unlocks more builds");
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
