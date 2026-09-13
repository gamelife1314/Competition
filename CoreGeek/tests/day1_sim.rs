//! Day-1 simulation harness.
//!
//! Issues 12/13/14 are all multi-round failures, not single-round ones:
//!
//! * #14 — "420 log lines and *zero* wall builds"; 75 gold went into two towers
//!   and the wall ring never appeared at all.
//! * #12 — "60 day rounds, 0 used for defense"; the same 2-towers-0-walls board.
//! * #13 — "20011 stood idle all night"; the third role has no tower to man.
//!
//! A per-round unit test cannot see any of that, so this drives the real
//! planner (`brain::respond`) round by round against a minimal judger model
//! and asserts the invariants the issues were filed about. The model is
//! deliberately small — movement, collect, build, sell, buy, use — because the
//! day phase has no combat in it.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use coregeek::model::{chebyshev, footprint_distance};
use coregeek::protocol::Pos;

const WIDTH: i32 = 41;
const HEIGHT: i32 = 32;
/// Round 1..=70 are day 1's daytime; 71..=130 are its night.
const DAY_END: i64 = 70;

/// Shop prices from 任务书 4.6.3 (consumables + upgrade vouchers).
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

/// Vendor ore prices. Stone is cheap on purpose: the wall line must be fed by
/// mining, not by buying stone with gold.
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
    /// Every wall build the planner issued, in round order.
    wall_builds: Vec<(i64, Pos)>,
    /// Walls the planner demolished to open a door in its own ring.
    removed_walls: Vec<(i64, Pos)>,
    tower_builds: Vec<(i64, Pos, String)>,
    /// Rounds on which each role produced no command at all.
    idle_rounds: HashMap<i64, Vec<i64>>,
    /// Per-role command count per round, for the "never stands idle" invariant.
    commands: Vec<(i64, i64, String)>,
    /// Ore sold per round, so a frozen economy is visible.
    gold_track: Vec<(i64, i64)>,
    /// Every vein the planner dug this match, with the round it was dug from.
    collects: Vec<(i64, Pos)>,
    /// Role id -> (round, position, backpack size), for diagnosing stalls.
    positions: HashMap<i64, Vec<(i64, Pos, i64)>>,
    /// This match's own cross-round memory. Private on purpose: the planner's
    /// decisions on round N depend on rounds 1..N-1, so a test that shares it
    /// with another test is not testing a day, it is testing an interleaving.
    state: coregeek::state::BotState,
}

impl World {
    fn new() -> Self {
        // Stone close to the base (the wall line's food), iron/copper further
        // out so the economy has to choose, vendor and shop on the far side.
        Self::with_stone(&[(16, 24), (16, 27), (18, 21), (20, 28)])
    }

    /// The same opening with the stone vein somewhere else. Issue #17 ran a
    /// board where the mine was far enough that the day ended before the wall
    /// crew ever reached it (14 collects, 0 walls, gold frozen at the 75 it
    /// started with), so where the stone is cannot be a detail of the harness.
    fn with_stone(stone: &[(i32, i32)]) -> Self {
        let mut zones: HashMap<Pos, String> = HashMap::new();
        for (x, y) in stone {
            zones.insert(Pos { x: *x, y: *y }, "stone".to_string());
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
            wall_builds: vec![],
            removed_walls: vec![],
            tower_builds: vec![],
            idle_rounds: HashMap::new(),
            commands: vec![],
            gold_track: vec![],
            collects: vec![],
            positions: HashMap::new(),
            state: coregeek::state::BotState::default(),
        }
    }

    /// Cells a unit may not enter: neutral zones and every other unit.
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

    /// Ring cells that still have no wall in them.
    fn open_cells(&self) -> Vec<Pos> {
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

    fn open_ring(&self) -> usize {
        self.open_cells().len()
    }

    /// Round the last ring cell was walled on. `None` when the ring never got
    /// a single wall.
    fn ring_closed_round(&self) -> Option<i64> {
        let ring = self.ring();
        self.wall_builds
            .iter()
            .filter(|(_, pos)| ring.contains(pos))
            .map(|(round, _)| *round)
            .max()
    }

    /// Apply one round's command map the way the judger would, then advance.
    fn step(&mut self) {
        let payload = self.payload();
        let body = serde_json::to_vec(&payload).unwrap();
        // Every simulation owns its cross-round memory: `respond` uses the
        // process-wide singleton, and two simulations sharing it read each
        // other's round numbers as a half change and wipe each other's state.
        let raw = match coregeek::brain::decide_with(&mut self.state, &body) {
            Ok(raw) => raw,
            Err(err) => panic!("decision failed: {err}"),
        };
        let response: Value = serde_json::from_str(&raw).expect("response parses");
        let map = response["roleCommandMap"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        for unit in &self.units {
            if unit.kind_is_role() {
                self.positions.entry(unit.id).or_default().push((
                    self.round,
                    unit.pos,
                    unit.backpack.len() as i64,
                ));
            }
        }

        // Roles that produced no command this round. A paired, adjacent
        // controller deliberately holds position, so this is only meaningful
        // together with the position check in the test body.
        for unit in &self.units {
            if !unit.kind_is_role() {
                continue;
            }
            if !map.contains_key(&unit.id.to_string()) {
                self.idle_rounds
                    .entry(unit.id)
                    .or_default()
                    .push(self.round);
            }
        }

        let ids: Vec<i64> = map.keys().filter_map(|key| key.parse().ok()).collect();
        for id in ids {
            let cmd = map[&id.to_string()].clone();
            let action = cmd["action"].as_str().unwrap_or("").to_string();
            self.commands.push((self.round, id, action.clone()));
            self.apply(id, &action, &cmd);
        }
        self.gold_track.push((self.round, self.gold));
        self.round += 1;
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
                self.collects.push((self.round, target));
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
                        id: 20000 + self.wall_builds.len() as i64,
                        kind: "wall".into(),
                        pos: target,
                        level: 1,
                        health: 1000,
                        capacity: 0,
                        backpack: vec![],
                    });
                    self.wall_builds.push((self.round, target));
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
                // Demolishing our own wall — the door the economy cuts when the
                // ring has closed around it. The judger only allows it from an
                // adjacent cell, so the sim does too.
                let Some(target) = target else { return };
                let Some(role) = self.units.iter().find(|unit| unit.id == id) else {
                    return;
                };
                if chebyshev(role.pos, target) != 1 {
                    return;
                }
                let before = self.units.len();
                self.units
                    .retain(|unit| !(unit.kind == "wall" && unit.pos == target));
                if self.units.len() != before {
                    self.removed_walls.push((self.round, target));
                }
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
                    _ => matches!(
                        self.units[index].kind.as_str(),
                        "gatling" | "railgun" | "rocket"
                    ),
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

    /// Day 1's ring: the radius-2 shell around the station footprint.
    fn ring(&self) -> Vec<Pos> {
        let station = self
            .units
            .iter()
            .find(|unit| unit.kind == "station")
            .unwrap();
        let footprint = station.footprint();
        let xs: Vec<i32> = footprint.iter().map(|cell| cell.x).collect();
        let ys: Vec<i32> = footprint.iter().map(|cell| cell.y).collect();
        let (xmin, xmax) = (*xs.iter().min().unwrap(), *xs.iter().max().unwrap());
        let (ymin, ymax) = (*ys.iter().min().unwrap(), *ys.iter().max().unwrap());
        let mut cells = Vec::new();
        for x in xmin - 2..=xmax + 2 {
            for y in ymin - 2..=ymax + 2 {
                let pos = Pos { x, y };
                if x < 0 || y < 0 || x >= WIDTH || y >= HEIGHT {
                    continue;
                }
                if footprint.contains(&pos) || self.zones.contains_key(&pos) {
                    continue;
                }
                let dist = footprint
                    .iter()
                    .map(|cell| (pos.x - cell.x).abs().max((pos.y - cell.y).abs()))
                    .min()
                    .unwrap();
                if dist == 2 {
                    cells.push(pos);
                }
            }
        }
        cells
    }

    fn wall_count(&self) -> usize {
        self.units.iter().filter(|unit| unit.kind == "wall").count()
    }

    fn towers(&self) -> Vec<&SimUnit> {
        self.units
            .iter()
            .filter(|unit| matches!(unit.kind.as_str(), "gatling" | "railgun" | "rocket"))
            .collect()
    }

    /// Where a role stood on `round`, if the sim recorded it.
    fn pos_at(&self, id: i64, round: i64) -> Option<Pos> {
        self.positions
            .get(&id)?
            .iter()
            .find(|(seen, _, _)| *seen == round)
            .map(|(_, pos, _)| *pos)
    }

    /// Was this role somewhere it was asked to be? Two answers count: adjacent
    /// to one of our guns (the post it must man at dusk) or inside the wall ring
    /// (the shelter the night recall sends it to). A role that is neither is
    /// standing in the open with nothing to do — the "站桩闲置" of issue #13.
    fn holds_a_post(&self, id: i64, round: i64) -> bool {
        let Some(pos) = self.pos_at(id, round) else {
            return true; // no record: the role had not spawned yet
        };
        if self
            .towers()
            .iter()
            .any(|tower| chebyshev(pos, tower.pos) <= 1)
        {
            return true;
        }
        let Some(station) = self.units.iter().find(|unit| unit.kind == "station") else {
            return true;
        };
        footprint_distance(pos, &station.footprint()) <= 1
    }
}

impl SimUnit {
    fn kind_is_role(&self) -> bool {
        self.kind == "worker" || self.kind == "pioneer"
    }
}

/// Run day 1's daytime and hand back the world. Kept as one helper so every
/// test below shares the exact same opening.
fn run_day_one() -> World {
    let mut world = World::new();
    while world.round <= DAY_END {
        world.step();
    }
    world
}

/// Play `day`'s daylight rounds on an existing match. `roundNo` is the only
/// thing the planner reads for the calendar (day = (roundNo-1)/130 + 1,
/// in-day round = (roundNo-1) % 130), so a later day is just a different
/// starting round. Combat is out of scope for this harness, so the night
/// rounds in between are skipped — the night planner is exercised by its own
/// unit tests.
fn run_day(world: &mut World, day: i64) {
    world.round = (day - 1) * 130 + 1;
    let last = world.round + DAY_END - 1;
    while world.round <= last {
        world.step();
    }
}

/// One-line summary of a match, printed by the tests that measure a whole day.
/// Deliberately not a `#[test]` — it is instrumentation for the assertions
/// below, and a test whose body is a `println!` can never fail.
fn report(label: &str, world: &World) {
    println!(
        "{label}: walls={}/{} open={:?} towers={:?} gold={} ringClosed={:?}",
        world.wall_count(),
        world.ring().len(),
        world.open_cells().len(),
        world
            .tower_builds
            .iter()
            .map(|(round, pos, kind)| format!("R{round} {kind} ({},{})", pos.x, pos.y))
            .collect::<Vec<_>>(),
        world.gold,
        world.ring_closed_round(),
    );
}

#[test]
fn day_one_closes_the_wall_ring_before_nightfall() {
    // P0 acceptance: "两边出生点均在 R71 前至少形成可承伤闭环". A ring with a
    // robot-sized hole in it is not closed, so this asserts the real thing —
    // every ring cell but the deliberate gate is walled BEFORE dusk, and the
    // gate itself is sealed by the dusk checkpoint.
    let world = run_day_one();
    report("day 1", &world);
    let ring = world.ring().len();
    let built = world.wall_count();
    let open = world.open_ring();
    assert!(
        built >= ring - 1,
        "issue #12/#13/#14: the day-1 ring never got built ({built} walls, ring is {ring})"
    );
    // At most the single cell a teammate was standing on when the sweep went
    // past may still be open; the deliberate gate has to be walled by then.
    assert!(
        open <= 1,
        "{open} ring cells were still open at nightfall: {:?}",
        world.open_cells()
    );
    // And it happened before the night: `SEAL_GRACE` (8) rounds after dusk is
    // the last moment a carrier may still walk the final stone out. A ring that
    // only closes at R70 is a ring that spent the night open.
    let closed = world
        .ring_closed_round()
        .expect("the ring never received a single wall");
    assert!(
        closed <= coregeek::brain::economy::DUSK_ROUND + 8,
        "the last ring wall went up at R{closed}, after the night had started"
    );
}

#[test]
fn the_wall_ring_is_up_before_the_third_tower() {
    // "Day 1 墙优先于武器" — and AUDIT_PLAN defines wall-first as forming the
    // minimal closed load-bearing layer fast, not as never building a gun. The
    // opener is deliberately one or two towers (the ring needs stone, and stone
    // needs mining); what must not happen is spending the third 25 gold on a
    // gun while the base is still wide open, which is exactly the 3-tower,
    // zero-wall day that issues #12/#13/#14 all describe.
    let world = run_day_one();
    let third_tower = world.tower_builds.get(2).map(|(round, _, _)| *round);
    let Some(third_tower) = third_tower else {
        return; // fewer than three guns: nothing to trade off
    };
    let walls_then = world
        .wall_builds
        .iter()
        .filter(|(round, _)| *round < third_tower)
        .count();
    let ring = world.ring().len();
    assert!(
        walls_then * 2 >= ring,
        "the third gun went up at R{third_tower} with only {walls_then}/{ring} walls"
    );
}

#[test]
fn the_economy_earns_once_the_ring_is_up() {
    // Issue #14: gold climbed to 105 at R18 and then sat untouched for 216
    // rounds. Day 1 is allowed to spend itself on the ring — that trade is the
    // whole point of the wall-first order — but the freeze must not survive it:
    // with the ring closed, day 2 has to mine, sell and buy again.
    let mut world = run_day_one();
    let day_one_gold = world.gold;
    let ring = world.ring().len();
    assert!(
        world.wall_count() >= ring - 1,
        "day 1 did not finish the ring, so the freeze below is not a fair test"
    );
    world.commands.clear();
    world.gold_track.clear();
    run_day(&mut world, 2);

    let actions: HashSet<&str> = world.commands.iter().map(|(_, _, a)| a.as_str()).collect();
    assert!(
        actions.contains("sell"),
        "day 2 never sold anything (issue #14: 216 frozen rounds); saw {actions:?}"
    );
    assert!(
        actions.contains("collect"),
        "day 2 never mined anything; saw {actions:?}"
    );
    assert!(
        world.gold > day_one_gold,
        "gold went {day_one_gold} -> {} across a full day of mining and selling",
        world.gold
    );
}

#[test]
fn both_workers_work_every_round_of_the_day() {
    // Issue #12: "R43-R57: 15 rounds of purposeless movement after the task
    // ended, no defense built". A worker with no command is a wasted round.
    let world = run_day_one();
    for id in [10002, 10003] {
        let idle = world.idle_rounds.get(&id).cloned().unwrap_or_default();
        // The last rounds are the dusk lock-in: an adjacent controller holding
        // its tower position issues nothing on purpose.
        let idle_before_dusk: Vec<i64> = idle.iter().copied().filter(|round| *round < 50).collect();
        assert!(
            idle_before_dusk.len() <= 3,
            "worker {id} had no command on {idle_before_dusk:?}"
        );
    }
}

#[test]
fn no_controller_is_ever_marooned_from_its_gun() {
    // Issue #13: "0 角色站桩闲置" — three roles standing still with nothing to
    // do. A controller whose tower it cannot reach spends the whole day
    // walking into a wall (the planner returns no command once every route is
    // gone), and it also blocks the ring: `wall_would_trap` refuses to seal
    // while a role would still be cut off. Pairing must therefore only hand a
    // controller a gun it can actually get to.
    //
    // Holding a post is not idling. From `preposition_round` the planner
    // deliberately stops issuing commands to a role that is already adjacent to
    // its gun, so that a late errand cannot drag it away before dusk — that is
    // the night recall the issues ask for, and counting it as a dead spell
    // would make the fix look like the bug. What must never happen is a role
    // that is *nowhere useful* and silent: not at a gun, not inside the ring.
    let world = run_day_one();
    let marooned: Vec<(i64, usize)> = world
        .idle_rounds
        .iter()
        .map(|(id, rounds)| {
            let stranded = rounds
                .iter()
                .filter(|round| !world.holds_a_post(*id, **round))
                .count();
            (*id, stranded)
        })
        .filter(|(_, stranded)| *stranded > 12)
        .collect();
    assert!(
        marooned.is_empty(),
        "roles {marooned:?} stood away from every tower and every shelter for \
         more than 12 rounds with no command; where={:?}",
        marooned
            .iter()
            .map(|(id, _)| (
                *id,
                world
                    .positions
                    .get(id)
                    .map(|track| track
                        .iter()
                        .filter(|(round, _, _)| world
                            .idle_rounds
                            .get(id)
                            .map(|idle| idle.contains(round))
                            .unwrap_or(false))
                        .map(|(round, pos, pack)| format!("R{round}({},{})p{pack}", pos.x, pos.y))
                        .collect::<Vec<_>>())
                    .unwrap_or_default()
            ))
            .collect::<Vec<_>>()
    );
}

/// A board whose stone is eight cells out instead of three: far enough that the
/// wall crew has to commit to a trip, close enough that the trip can still pay
/// for itself before dusk.
fn stone_eight_cells_out() -> World {
    World::with_stone(&[(21, 24), (21, 28), (19, 29), (23, 30)])
}

/// A board whose stone is on the far side of the map — twenty-two cells from
/// the spawn, with the retreat deadline sixteen rounds closer than that. Issue
/// #17 played on a board like this one: 0 walls, 14 collects, and a purse that
/// never saw a coin.
fn stone_out_of_reach() -> World {
    World::with_stone(&[(36, 28), (37, 20), (38, 25), (35, 30)])
}

#[test]
fn a_distant_stone_vein_still_puts_the_ring_up_before_nightfall() {
    // Issue #17: "城墙体系彻底缺失（0 墙 vs 对手 101 次建墙）". The day-1 wall rule
    // is a batch rule — `load_complete` wants RING_BATCH (20) stone in hand, or
    // the team's stone to cover every gap, before a single wall goes down — and
    // the batch is what makes a distant vein expensive: the whole trip (walk
    // out, dig, walk back, place) has to finish before the crew must be at its
    // guns, or the stone arrives after the night has already started. Here it
    // does: the same opening with the vein eight cells further still walls the
    // ring before dusk.
    let mut world = stone_eight_cells_out();
    while world.round <= DAY_END {
        world.step();
    }
    report("distant stone", &world);
    let ring = world.ring().len();
    assert!(
        world.wall_count() * 2 >= ring,
        "the stone in the packs never became wall: {} collects, {} walls (ring {ring})",
        world.collects.len(),
        world.wall_count()
    );
    let closed = world
        .ring_closed_round()
        .expect("not one ring cell was ever walled");
    assert!(
        closed <= coregeek::brain::economy::DUSK_ROUND + 8,
        "the last ring wall went up at R{closed}, after the night had started"
    );
}

#[test]
fn stone_out_of_reach_becomes_a_day_of_ore_that_sells() {
    // The other half of issue #17: "我方仅 14 次 collect，金币峰值仅 75（初始值），
    // 从未通过采矿获得增量收入". When the ring's stone cannot be brought home in
    // time, the day is not a wall day — and the one thing it must not be is a
    // march to the far side of the map. `stone_trip_fits` measures the trip
    // against this role's own gun deadline: walk out, dig a load, walk back,
    // and still be at the gun. A vein that fails that test is not a wall this
    // day, so the crew mines ore that sells instead of stone that cannot.
    let mut world = stone_out_of_reach();
    let start = world.gold;
    while world.round <= DAY_END {
        world.step();
    }
    report("stone out of reach", &world);
    let peak = world
        .gold_track
        .iter()
        .map(|(_, gold)| *gold)
        .max()
        .unwrap_or(start);
    assert!(
        peak > start,
        "day 1 never earned a coin: gold peaked at {peak}, the opening purse was {start}"
    );
    assert!(
        world.commands.iter().any(|(_, _, a)| a == "sell"),
        "the collect→sell→buy loop never ran; saw {:?}",
        world.commands.iter().map(|(_, _, a)| a).collect::<Vec<_>>()
    );
    // No carrier walks to a vein it cannot come back from. The far veins sit 22
    // to 25 cells from the spawn; the iron and copper it is worth digging sit
    // inside 16.
    let far: Vec<(i64, Pos)> = world
        .collects
        .iter()
        .copied()
        .filter(|(_, pos)| chebyshev(*pos, Pos { x: 10, y: 24 }) > 18)
        .collect();
    assert!(
        far.is_empty(),
        "the crew marched out to veins it could not carry home from: {far:?}"
    );
}

#[test]
fn a_door_cut_in_the_morning_is_resealed_before_night() {
    // P0-3: day 2's economy lives outside the ring, so `open_door` cuts a cell
    // through it in the morning. That cell used to stay open all night whenever
    // nobody happened to carry a stone at dusk: the seal step waits for the
    // `wall_gate_sealed` flag (stragglers keep it down) and the daily wall
    // budget may already be spent — so the door was a robot-sized hole until
    // the next morning (v1 §5.5). With the door now counted in the day's stone
    // demand, a carrier keeps a stone back and walls the cell from dusk.
    let mut world = run_day_one();
    let ring = world.ring().len();
    assert!(
        world.wall_count() >= ring - 1,
        "day 1 did not finish the ring, so the door question is not being tested"
    );
    run_day(&mut world, 2);
    let open = world.open_cells();
    assert!(
        open.len() <= 1,
        "the ring — door included — must be closed again at nightfall: {open:?}"
    );
    // Direct evidence for the mechanism: every cell the economy cut open has
    // been walled again by the end of the day it was cut on.
    for (cut_round, door) in world.removed_walls.clone() {
        let sealed = world
            .wall_builds
            .iter()
            .any(|(round, pos)| *round > cut_round && *pos == door);
        assert!(
            sealed,
            "the door at {door:?} cut on R{cut_round} was never re-walled; \
             wall builds: {:?}",
            world.wall_builds
        );
    }
}

#[test]
fn the_workers_build_the_ring_from_their_own_stone() {
    // Hard constraint: at least one worker keeps the loop running, and the ring
    // is built from stone a worker carries itself — a builder can only spend
    // its own pack, so stone "held back" in a role that is exempt from wall
    // duty is a hole in the ring, not a reserve. Day 1 finished 19/20 with two
    // stones stuck in a backpack exactly that way.
    let world = run_day_one();
    let actions: HashSet<&str> = world.commands.iter().map(|(_, _, a)| a.as_str()).collect();
    for needed in ["collect", "build"] {
        assert!(
            actions.contains(needed),
            "day 1 never issued `{needed}`; saw {actions:?}"
        );
    }
    assert!(
        !world.tower_builds.is_empty(),
        "no tower was ever built from a 75-gold opening"
    );
    let builders: HashSet<i64> = world
        .wall_builds
        .iter()
        .filter_map(|(round, _)| {
            world
                .commands
                .iter()
                .find(|(r, _, action)| r == round && action == "build")
                .map(|(_, id, _)| *id)
        })
        .collect();
    assert!(
        builders.len() >= 2,
        "only {builders:?} built walls; the second worker's stone never became ring"
    );
}
