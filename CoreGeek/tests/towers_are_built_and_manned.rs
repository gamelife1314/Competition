//! P0-1 (issue #206 §3/§5): the configured tower line has to actually reach the
//! board, and every tower on it has to have a controller at nightfall.
//!
//! Measured on matches #203/#205, one tower was standing at the end of the
//! match, `tower_plan.gaps` sat at 2-3 for the whole afternoon, and
//! `tower_unpaired` fired seven times with the reasons `pairing_invariant` and
//! `no_live_controller`. The plan's diagnosis is that the bottleneck was never
//! "the sites are poorly chosen" — it was that the towers were not raised, and
//! that a raised tower was not always manned.
//!
//! Both halves are multi-round facts, so neither a single-round unit test nor a
//! look at one event can see them. This drives the real planner round by round
//! against a minimal judger model (the same shape as `tests/day1_sim.rs`) and
//! then asks the NIGHT planner the question the log event answers: is any tower
//! left without a controller?

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use coregeek::model::{chebyshev, footprint_distance};
use coregeek::protocol::Pos;

const WIDTH: i32 = 41;
const HEIGHT: i32 = 32;
/// Round 1..=70 are day 1's daytime; 71..=130 are its night.
const DAY_END: i64 = 70;
const NIGHT_START: i64 = 71;

const SHOP: &[(&str, i64)] = &[
    ("Medicine", 10),
    ("WallFixer", 10),
    ("DizzyWeapon", 100),
    ("Bomb", 100),
    ("WeaponUpgradeVoucher1", 100),
    ("WallUpgradeVoucher1", 20),
    ("StationUpgradeVoucher1", 100),
    ("WeaponUpgradeVoucher2", 150),
    ("WallUpgradeVoucher2", 30),
    ("StationUpgradeVoucher2", 150),
    ("SmallRobotSummonOrder", 20),
];

const VENDOR: &[(&str, i64)] = &[("stone", 2), ("iron", 8), ("copper", 12)];

#[derive(Debug, Clone)]
struct SimUnit {
    id: i64,
    kind: String,
    pos: Pos,
    level: i32,
    health: i64,
    capacity: i64,
    backpack: Vec<String>,
}

impl SimUnit {
    fn footprint(&self) -> Vec<Pos> {
        if self.kind == "station" {
            vec![
                self.pos,
                Pos {
                    x: self.pos.x + 1,
                    y: self.pos.y,
                },
                Pos {
                    x: self.pos.x,
                    y: self.pos.y - 1,
                },
                Pos {
                    x: self.pos.x + 1,
                    y: self.pos.y - 1,
                },
            ]
        } else {
            vec![self.pos]
        }
    }
    fn kind_is_role(&self) -> bool {
        self.kind == "worker" || self.kind == "pioneer"
    }
    fn is_tower(&self) -> bool {
        matches!(self.kind.as_str(), "gatling" | "railgun" | "rocket")
    }
    fn max_hp(&self) -> i64 {
        match self.kind.as_str() {
            "station" => match self.level {
                1 => 1500,
                2 => 3000,
                _ => 4500,
            },
            "gatling" | "railgun" | "rocket" => match self.level {
                1 => 1000,
                2 => 1500,
                _ => 2000,
            },
            "wall" => match self.level {
                1 => 1000,
                2 => 1500,
                _ => 2000,
            },
            "worker" => 220,
            _ => 200,
        }
    }
}

struct World {
    round: i64,
    gold: i64,
    units: Vec<SimUnit>,
    zones: HashMap<Pos, String>,
    tower_builds: Vec<(i64, Pos, String)>,
    state: coregeek::state::BotState,
}

impl World {
    /// The same opening board `tests/day1_sim.rs` uses: stone close to the base,
    /// iron and copper further out, vendor and shop on the far side.
    fn new() -> Self {
        let mut zones: HashMap<Pos, String> = HashMap::new();
        for (x, y) in [(16, 24), (16, 27), (18, 21), (20, 28)] {
            zones.insert(Pos { x, y }, "stone".to_string());
        }
        for (x, y, kind) in [
            (22, 19, "iron"),
            (24, 30, "iron"),
            (26, 17, "copper"),
            (30, 26, "vendor"),
            (28, 20, "weaponShop"),
        ] {
            zones.insert(Pos { x, y }, kind.to_string());
        }
        Self {
            round: 1,
            gold: 75,
            units: vec![
                SimUnit {
                    id: 10001,
                    kind: "station".into(),
                    pos: Pos { x: 10, y: 24 },
                    level: 1,
                    health: 1500,
                    capacity: 0,
                    backpack: vec![],
                },
                SimUnit {
                    id: 10002,
                    kind: "worker".into(),
                    pos: Pos { x: 13, y: 24 },
                    level: 1,
                    health: 220,
                    capacity: 100,
                    backpack: vec![],
                },
                SimUnit {
                    id: 10003,
                    kind: "worker".into(),
                    pos: Pos { x: 14, y: 25 },
                    level: 1,
                    health: 220,
                    capacity: 100,
                    backpack: vec![],
                },
                SimUnit {
                    id: 10004,
                    kind: "pioneer".into(),
                    pos: Pos { x: 13, y: 26 },
                    level: 1,
                    health: 200,
                    capacity: 40,
                    backpack: vec![],
                },
            ],
            zones,
            tower_builds: vec![],
            state: coregeek::state::BotState::default(),
        }
    }

    fn blocked(&self, mover: i64) -> HashSet<Pos> {
        let mut cells: HashSet<Pos> = self.zones.keys().copied().collect();
        for unit in &self.units {
            if unit.id == mover {
                continue;
            }
            cells.extend(unit.footprint());
        }
        cells
    }

    /// One robot standing outside the ring on the enemy side of the base: the
    /// night board needs a threat, or `withdrawing`/`defense_needs_pioneer`
    /// never fire and the pairing question is asked of a board that cannot
    /// answer it. It never moves and never shoots — combat is out of scope
    /// here; what is under test is who is holding which gun.
    fn night_payload(&self) -> Value {
        self.night_payload_at(NIGHT_START)
    }

    /// The same night board as of an arbitrary night round. Day 2 needs the
    /// calendar to actually move: `stable_pairs` keys its cache on the day, so a
    /// night pinned at round 71 would ask day 1's question forever.
    fn night_payload_at(&self, round: i64) -> Value {
        let mut payload = self.payload();
        payload["roundNo"] = json!(round);
        payload["robot"] = json!({"roles": [
            {"id": 90001, "pos": {"x": 30, "y": 24}, "robotType": "normal",
             "health": 100, "attackPower": 10, "attackRange": 1, "targetTeam": "challenger"}
        ]});
        payload
    }

    /// Tonight's board as the planner sees it — the same payload `step_night`
    /// hands it, so a test can ask the pairing about the round that just ran.
    fn night_turn(&self) -> coregeek::model::Turn {
        self.night_turn_at(NIGHT_START)
    }

    fn night_turn_at(&self, round: i64) -> coregeek::model::Turn {
        coregeek::model::Turn::from_request(
            serde_json::from_value(self.night_payload_at(round))
                .expect("the night payload parses"),
        )
    }

    fn payload(&self) -> Value {
        let roles: Vec<Value> = self
            .units
            .iter()
            .map(|unit| {
                json!({
                    "id": unit.id,
                    "pos": {"x": unit.pos.x, "y": unit.pos.y},
                    "roleType": unit.kind,
                    "health": unit.health,
                    "attackPower": if unit.kind == "gatling" { 10 } else { 0 },
                    "attackRange": match unit.kind.as_str() {
                        "gatling" => 3,
                        "railgun" => 6,
                        "rocket" => 10,
                        _ => 0,
                    },
                    "level": unit.level,
                    "cooldown": 0,
                    "backPackCapability": unit.capacity,
                    "backpack": unit.backpack,
                })
            })
            .collect();
        let zones: Vec<Value> = self
            .zones
            .iter()
            .map(|(pos, kind)| json!({"pos": {"x": pos.x, "y": pos.y}, "neutralType": kind}))
            .collect();
        json!({
            "roundNo": self.round,
            "mapInfo": {"width": WIDTH, "height": HEIGHT, "zones": zones},
            "teamOur": {
                "type": "challenger",
                "goldNum": self.gold,
                "totalScore": 0,
                "playerTasks": [],
                "roles": roles,
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
            "vendorShopList": VENDOR.iter()
                .map(|(name, price)| json!({"name": name, "price": price}))
                .collect::<Vec<_>>(),
            "weaponShopList": SHOP.iter()
                .map(|(name, price)| json!({"name": name, "price": price}))
                .collect::<Vec<_>>(),
        })
    }

    fn price(&self, name: &str) -> Option<i64> {
        SHOP.iter()
            .find(|(item, _)| *item == name)
            .map(|(_, price)| *price)
    }

    fn towers(&self) -> Vec<&SimUnit> {
        self.units.iter().filter(|unit| unit.is_tower()).collect()
    }

    fn step(&mut self) {
        let payload = self.payload();
        let body = serde_json::to_vec(&payload).unwrap();
        let raw = match coregeek::brain::decide_with(&mut self.state, &body) {
            Ok(raw) => raw,
            Err(err) => panic!("decision failed: {err}"),
        };
        let response: Value = serde_json::from_str(&raw).expect("response parses");
        let map = response["roleCommandMap"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let ids: Vec<i64> = map.keys().filter_map(|key| key.parse().ok()).collect();
        for id in ids {
            let cmd = map[&id.to_string()].clone();
            let action = cmd["action"].as_str().unwrap_or("").to_string();
            self.apply(id, &action, &cmd);
        }
        self.round += 1;
    }

    fn step_night(&mut self, round: i64) {
        let payload = self.night_payload_at(round);
        let body = serde_json::to_vec(&payload).unwrap();
        let raw = match coregeek::brain::decide_with(&mut self.state, &body) {
            Ok(raw) => raw,
            Err(err) => panic!("night decision failed: {err}"),
        };
        let response: Value = serde_json::from_str(&raw).expect("response parses");
        let map = response["roleCommandMap"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let ids: Vec<i64> = map.keys().filter_map(|key| key.parse().ok()).collect();
        for id in ids {
            let cmd = map[&id.to_string()].clone();
            let action = cmd["action"].as_str().unwrap_or("").to_string();
            self.apply(id, &action, &cmd);
        }
        self.round = round + 1;
    }

    fn apply(&mut self, id: i64, action: &str, cmd: &Value) {
        let target = cmd["targetPos"]
            .as_array()
            .and_then(|list| list.first())
            .map(|pos| Pos {
                x: pos["x"].as_i64().unwrap_or(0) as i32,
                y: pos["y"].as_i64().unwrap_or(0) as i32,
            });
        match action {
            "move" => {
                let Some(target) = target else { return };
                if self.blocked(id).contains(&target) {
                    return; // collision: the role stays where it is
                }
                if let Some(unit) = self.units.iter_mut().find(|unit| unit.id == id) {
                    unit.pos = target;
                }
            }
            "collect" => {
                let Some(target) = target else { return };
                let Some(ore) = self.zones.get(&target).cloned() else {
                    return;
                };
                if let Some(unit) = self.units.iter_mut().find(|unit| unit.id == id) {
                    if unit.backpack.len() as i64 >= unit.capacity {
                        return;
                    }
                    unit.backpack.push(ore);
                }
            }
            "build" => {
                let Some(target) = target else { return };
                let name = cmd["name"].as_str().unwrap_or("").to_string();
                if name == "wall" {
                    let Some(unit) = self.units.iter_mut().find(|unit| unit.id == id) else {
                        return;
                    };
                    let Some(index) = unit.backpack.iter().position(|item| item == "stone") else {
                        return;
                    };
                    unit.backpack.remove(index);
                    self.units.push(SimUnit {
                        id: 20000 + self.tower_builds.len() as i64,
                        kind: "wall".into(),
                        pos: target,
                        level: 1,
                        health: 1000,
                        capacity: 0,
                        backpack: vec![],
                    });
                } else {
                    if self.gold < 25 {
                        return;
                    }
                    self.gold -= 25;
                    self.units.push(SimUnit {
                        id: 30000 + self.tower_builds.len() as i64,
                        kind: name.clone(),
                        pos: target,
                        level: 1,
                        health: 1000,
                        capacity: 0,
                        backpack: vec![],
                    });
                    self.tower_builds.push((self.round, target, name));
                }
            }
            "remove" => {
                let Some(target) = target else { return };
                let Some(role) = self.units.iter().find(|unit| unit.id == id) else {
                    return;
                };
                if chebyshev(role.pos, target) != 1 {
                    return;
                }
                self.units
                    .retain(|unit| !(unit.kind == "wall" && unit.pos == target));
            }
            "sell" => {
                let name = cmd["name"].as_str().unwrap_or("").to_string();
                let num = cmd["num"].as_i64().unwrap_or(1);
                let price = VENDOR
                    .iter()
                    .find(|(item, _)| *item == name)
                    .map(|(_, price)| *price)
                    .unwrap_or(0);
                let Some(unit) = self.units.iter_mut().find(|unit| unit.id == id) else {
                    return;
                };
                let mut removed = 0;
                unit.backpack.retain(|item| {
                    if *item == name && removed < num {
                        removed += 1;
                        false
                    } else {
                        true
                    }
                });
                self.gold += price * removed;
            }
            "buy" => {
                let name = cmd["name"].as_str().unwrap_or("").to_string();
                let num = cmd["num"].as_i64().unwrap_or(1);
                let Some(price) = self.price(&name) else {
                    return;
                };
                let Some(unit) = self.units.iter_mut().find(|unit| unit.id == id) else {
                    return;
                };
                let free = unit.capacity - unit.backpack.len() as i64;
                let affordable = self.gold / price.max(1);
                let count = num.min(free).min(affordable);
                if count <= 0 {
                    return;
                }
                self.gold -= price * count;
                for _ in 0..count {
                    unit.backpack.push(name.clone());
                }
            }
            "drop" => {
                let name = cmd["name"].as_str().unwrap_or("").to_string();
                if let Some(unit) = self.units.iter_mut().find(|unit| unit.id == id) {
                    if let Some(index) = unit.backpack.iter().position(|item| *item == name) {
                        unit.backpack.remove(index);
                    }
                }
            }
            "use" => {
                let name = cmd["name"].as_str().unwrap_or("").to_string();
                let Some(target) = target else { return };
                let upgrade = match name.as_str() {
                    "WallUpgradeVoucher1" | "WallUpgradeVoucher2" => "wall",
                    "WeaponUpgradeVoucher1" | "WeaponUpgradeVoucher2" => "tower",
                    "StationUpgradeVoucher1" | "StationUpgradeVoucher2" => "station",
                    _ => return,
                };
                let Some(index) = self
                    .units
                    .iter()
                    .position(|unit| unit.footprint().contains(&target))
                else {
                    return;
                };
                let kind_ok = match upgrade {
                    "wall" => self.units[index].kind == "wall",
                    "station" => self.units[index].kind == "station",
                    _ => self.units[index].is_tower(),
                };
                if !kind_ok {
                    return;
                }
                self.units[index].level += 1;
                let full = self.units[index].max_hp();
                self.units[index].health = full;
                if let Some(unit) = self.units.iter_mut().find(|unit| unit.id == id) {
                    if let Some(index) = unit.backpack.iter().position(|item| *item == name) {
                        unit.backpack.remove(index);
                    }
                }
            }
            _ => {}
        }
    }

    /// The ring radius the day-1 shell is built on, for the "ring closed"
    /// precondition the tests below assert before they judge the towers.
    fn ring(&self) -> Vec<Pos> {
        let station = self
            .units
            .iter()
            .find(|unit| unit.kind == "station")
            .expect("the base is on the board");
        let footprint = station.footprint();
        let mut cells = Vec::new();
        for x in 0..WIDTH {
            for y in 0..HEIGHT {
                let pos = Pos { x, y };
                if footprint.contains(&pos) || self.zones.contains_key(&pos) {
                    continue;
                }
                if footprint_distance(pos, &footprint) == 2 {
                    cells.push(pos);
                }
            }
        }
        cells
    }

    fn open_ring(&self) -> Vec<Pos> {
        let walls: HashSet<Pos> = self
            .units
            .iter()
            .filter(|unit| unit.kind == "wall")
            .map(|unit| unit.pos)
            .collect();
        self.ring()
            .into_iter()
            .filter(|cell| !walls.contains(cell))
            .collect()
    }

    fn wall_count(&self) -> usize {
        self.units.iter().filter(|unit| unit.kind == "wall").count()
    }
}

impl World {
    /// The same opening board with `kinds` already standing on the ring.
    fn with_standing_towers(kinds: &[&str]) -> Self {
        let mut world = Self::new();
        for (index, kind) in kinds.iter().enumerate() {
            world.units.push(SimUnit {
                id: 30001 + index as i64,
                kind: (*kind).to_string(),
                pos: Pos {
                    x: 12,
                    y: 22 + 2 * index as i32,
                },
                level: 1,
                health: 1000,
                capacity: 0,
                backpack: vec![],
            });
        }
        world
    }
}

/// Drive day 1's daylight through the real planner.
fn run_day_one() -> World {
    let mut world = World::new();
    while world.round <= DAY_END {
        world.step();
    }
    world
}

/// The kinds of tower the day actually raised, in build order.
fn built_kinds(world: &World) -> Vec<String> {
    world
        .tower_builds
        .iter()
        .map(|(_, _, kind)| kind.clone())
        .collect()
}

#[test]
fn day_one_raises_a_tower_for_every_slot_of_the_configured_line() {
    // The P0-1 acceptance, first half. `config::TOWER_BUILD_ORDER` is the
    // owner's line; a slot that produced no tower is a gun the night does not
    // have, and on #203/#205 that is exactly what the match ended with (one
    // tower standing, `tower_plan.gaps` at 2-3 all afternoon).
    let world = run_day_one();
    let built = built_kinds(&world);
    let line: Vec<String> = coregeek::config::TOWER_BUILD_ORDER
        .iter()
        .map(|kind| kind.to_string())
        .collect();
    assert!(
        world.wall_count() > 0,
        "test setup: the day built no wall at all, so nothing about the \
         tower/wall ordering is under test"
    );
    assert_eq!(
        built, line,
        "day 1 raised {built:?} but the configured line is {line:?} \
         (towers standing: {:?})",
        world
            .towers()
            .iter()
            .map(|tower| (tower.kind.clone(), tower.pos))
            .collect::<Vec<_>>()
    );
}

#[test]
fn every_tower_has_a_controller_at_nightfall() {
    // The P0-1 acceptance, second half — the `tower_unpaired` event of
    // #203/#205, which fired seven times. `night::unpaired_towers` is the same
    // predicate the log event is emitted from, so an empty list here and an
    // empty `tower_unpaired` count in a captured match are the same statement.
    let mut world = run_day_one();
    let towers = world.towers().len();
    assert!(
        towers > 0,
        "test setup: no tower stood at nightfall, so nothing is under test"
    );

    // Walk the night the way the match does. The pairing is recomputed when its
    // membership key moves, and the key moves when a controller is wounded, so
    // one round is not enough to catch a pairing that only breaks later.
    let mut unpaired_rounds: Vec<(i64, Vec<(i64, &'static str)>)> = Vec::new();
    while world.round <= NIGHT_START + 20 {
        world.step_night(world.round);
        let turn = world.night_turn_at(world.round);
        let unpaired = coregeek::brain::night::unpaired_towers(&turn, &world.state);
        if !unpaired.is_empty() {
            unpaired_rounds.push((world.round - 1, unpaired));
        }
    }
    assert!(
        unpaired_rounds.is_empty(),
        "{towers} tower(s) stood at nightfall but towers went unpaired on \
         {unpaired_rounds:?} (round, count)"
    );
}

#[test]
fn no_night_plans_more_towers_than_it_has_controllers() {
    // The invariant `pairing_invariant` is named after, and the reason the
    // three-tower plan and the three-role crew have to be counted the same way:
    // if the number of towers the day plans can exceed the number of roles that
    // can hold one, a tower is guaranteed to stand dark. Guards the tower line
    // against an owner edit that adds slots the crew cannot man.
    let mut world = run_day_one();
    let crew = world.units.iter().filter(|unit| unit.kind_is_role()).count();
    let towers = world.towers().len();
    assert!(
        towers <= crew,
        "{towers} towers for {crew} controllers: at least one gun can never be \
         manned (issue #206 §3)"
    );

    world.step_night(NIGHT_START);
    let turn = world.night_turn();
    let crew = turn.controllable().len();
    assert!(
        turn.towers().len() <= crew,
        "{} towers for {crew} live controllers",
        turn.towers().len()
    );
}

#[test]
fn a_held_out_controller_is_not_counted_as_available() {
    // The `pairing_invariant` reason is an assertion, not a guess: it may only be
    // reported when every live controller really was available to pair and a
    // tower still went dark. `withdraw_holdout` is the one exclusion the pairing
    // applies that the raw crew count does not, so a board with three towers,
    // three roles and one role held out is a three-versus-two board and has to
    // say `no_live_controller`.
    //
    // The old tally counted `turn.controllable()` minus a task-busy pioneer and
    // ignored `withdraw_holdout` entirely, so it summed to 3, matched the tower
    // count, and reported `pairing_invariant` on this board — blaming a pairing
    // bug for what was simply a controller who had stepped away. That is the
    // defect this asserts against; put the old tally back and this goes red.
    let world = World::with_standing_towers(&["rocket", "railgun", "rocket"]);
    let mut state = coregeek::state::BotState::default();
    state.withdraw_holdout.insert(10002);

    let turn = world.night_turn();
    assert_eq!(
        turn.towers().len(),
        3,
        "test setup: expected three standing towers"
    );
    assert!(
        coregeek::brain::night::pairing_controllers(&turn, &state).len() == 2,
        "test setup: exactly one controller is held out, so two remain"
    );

    let unpaired = coregeek::brain::night::unpaired_towers(&turn, &state);
    assert_eq!(
        unpaired.len(),
        1,
        "one controller short of three guns leaves exactly one gun dark: {unpaired:?}"
    );
    for (tower, reason) in &unpaired {
        assert_eq!(
            *reason, "no_live_controller",
            "tower {tower} went dark with three roles alive and only two \
             available — that is a shortage of controllers, not a pairing bug, \
             and saying `pairing_invariant` hides the real cause"
        );
    }
}

#[test]
fn no_reason_is_an_invariant_violation_on_a_board_the_crew_can_man() {
    // The converse, so the fix cannot be "always say no_live_controller": when
    // the crew outnumbers the guns and nobody is held out, every tower pairs and
    // the list is empty. Together with the test above this pins the reason to the
    // availability tally rather than to whichever branch is checked first.
    let world = World::with_standing_towers(&["rocket", "railgun"]);
    let state = coregeek::state::BotState::default();
    let turn = world.night_turn();
    let crew = coregeek::brain::night::pairing_controllers(&turn, &state).len();
    assert!(
        crew >= turn.towers().len(),
        "test setup: {crew} controllers for {} towers",
        turn.towers().len()
    );
    assert!(
        coregeek::brain::night::unpaired_towers(&turn, &state).is_empty(),
        "a gun went dark while spare controllers stood idle"
    );
}

/// Drive day 1, its night, and day 2 through the real planner.
fn run_two_days() -> World {
    let mut world = run_day_one();
    while world.round <= DAY_END + 60 {
        world.step_night(world.round);
    }
    while world.round <= 2 * (DAY_END + 60) {
        world.step();
    }
    world
}

#[test]
fn the_second_day_keeps_every_gun_manned_and_never_passes_the_cap() {
    // P0-1 across the day boundary, which is where the tower line actually has
    // room to grow: with positional slots the empty-slot count on day 2 is
    // `TOWER_CAP - standing`, so a crew that lost a role overnight, a purse that
    // did not fill, or a site that the closed ring vetoed all land HERE and not
    // on day 1. The two things that must hold whatever the line says: the game's
    // three-tower cap is never exceeded, and no gun stands dark while the crew
    // watches. `every_tower_has_a_controller_at_nightfall` covers the board; this
    // covers the calendar.
    let mut world = run_two_days();
    let towers = world.towers().len();
    let crew = world.units.iter().filter(|unit| unit.kind_is_role()).count();
    assert!(
        towers > 0,
        "test setup: two days ended with no tower standing at all"
    );
    assert!(
        towers <= coregeek::config::TOWER_CAP,
        "{towers} towers standing after two days, cap is {}",
        coregeek::config::TOWER_CAP
    );
    assert!(
        towers <= crew,
        "{towers} towers for a crew of {crew} after two days: at least one gun \
         can never be manned (issue #206 §3)"
    );

    // The same question at every nightfall of the second night, so a pairing
    // that only breaks once a controller is wounded is caught too.
    let mut unpaired_rounds: Vec<(i64, Vec<(i64, &'static str)>)> = Vec::new();
    while world.round <= 2 * (DAY_END + 60) + 20 {
        world.step_night(world.round);
        let turn = world.night_turn_at(world.round);
        let unpaired = coregeek::brain::night::unpaired_towers(&turn, &world.state);
        if !unpaired.is_empty() {
            unpaired_rounds.push((world.round - 1, unpaired));
        }
    }
    assert!(
        unpaired_rounds.is_empty(),
        "day 2 ended with {towers} tower(s) and {crew} role(s) but towers went \
         unpaired on {unpaired_rounds:?} (round, tower/reason)"
    );
}
