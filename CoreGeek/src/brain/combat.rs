//! Weapon ballistics models and target selection.
//!
//! Gatling: `level` bullets, every target pair must sit inside one 90° cone;
//! each bullet damages the first robot along its trajectory (10 dmg).
//! Railgun: single target, energy pierces along the line, each robot takes
//! min(remaining energy, current hp).
//! Rocket: `level` missiles, 20 center + 10 splash (8 neighbours = half of
//! center), impacts stack, 3-round cooldown (checked by the caller).
//!
//! All three consult a shared per-round HP simulation (`Sim`) so towers
//! processed later do not overkill robots — or enemy buildings — an earlier
//! tower already claimed.
//!
//! Targeting runs on one objective, `win_value`: the `score2` points a kill is
//! worth, plus the `score3` damage that robot would have done to our assets,
//! plus how soon that damage lands. The same shape ranks enemy assets when no
//! robot is in reach (`enemy_unit_score`): kill probability × disable value ×
//! visibility window, with a station that can be destroyed this round
//! outranking everything.
//!
//! ASSUMPTION (see 需求分析 Q5): trajectories are intercepted by ROBOTS only;
//! walls and buildings do not block bullet/rail paths.
//!
//! RULE (任务书 4.5): the Bomb and the DizzyWeapon act on ROBOTS of both teams
//! and on nothing else — "仅对双方机器人有效，对敌方建筑、角色无效". Both impact
//! pickers read `turn.robots` alone; the opponent's roles live in `turn.enemy`
//! and are not candidates, not victims and not splash.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::model::{
    chebyshev, footprint_distance, station_footprint, Robot, RobotKind, Turn, Unit, UnitKind,
    FULL_MAP_RANGE,
};
use crate::protocol::Pos;

// ---------------------------------------------------------------------------
// Tuning dial
//
// Every weight the win-value objective uses lives here, so one parameter set
// can be compared against another across opponents without a code change:
// each field is read from `CG_TUNE_<NAME>` on first use and falls back to the
// default below. An unset environment reproduces the committed behaviour
// exactly — the dial only ever moves when someone turns it.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Weights {
    /// Score points per point of damage dealt (progress toward a kill).
    pub damage_weight: i64,
    /// Score points per unit of `RobotKind::score()` when a robot dies.
    pub kill_bonus_per_score: i64,
    /// Divisor applied to the expected-damage term (score points are the real
    /// objective; asset damage is a proxy for the score it would cost us).
    pub asset_damage_divisor: i64,
    /// How many rounds ahead a robot's damage to our assets is counted over.
    pub fire_horizon: i64,
    /// Rounds of damage charged as terminal risk when the robot is already in
    /// contact with something of ours.
    pub urgency_rounds: i64,
    /// Urgency lead for enemy assets: a target about to leave our reach scores
    /// `base * urgency_lead / window` extra.
    pub urgency_lead: i64,
    /// Disable value of each enemy unit class.
    pub station_value: i64,
    pub operator_value: i64,
    pub role_value: i64,
    pub tower_value: i64,
    pub wall_value: i64,
    /// Sustained enemy-station pressure switch (P2-2): non-zero = a full-map
    /// rocket, or any tower facing an already-damaged station, aims at the
    /// station before ranked enemy assets. Default on: pk577297 won the half
    /// by battering the station for 43 rounds, and a destroyed station ends
    /// the half outright (任务书 ch.7).
    pub station_focus: i64,
    /// Kill-gap-driven funding switch (P1-1): non-zero = `intent_list`
    /// reorders tonight's funding by [`firepower_gap`]; zero keeps the
    /// committed fixed priorities. Default OFF so the dial proves itself in
    /// A/B before it owns behaviour (Improve.kimi.md §7, P1-1 row).
    pub clear_gap_drive: i64,
    /// Wall-breach tactic (P1): what an enemy WALL is worth once
    /// [`wall_breach_with`] has opened the gate. `0` switches the tactic off
    /// and restores the committed ordering exactly. The default sits above
    /// `tower_value` — a hole in the opponent's line is worth more than
    /// silencing one of their guns — and below `role_value` / `operator_value`,
    /// because the roles are what makes those guns fire.
    ///
    /// It is a price in the same ranking every other building is priced in, not
    /// a flag (the gate above is what decides whether a breach is on the table
    /// at all): an A/B run can move it, and a wall only outranks their station
    /// while this stays above `station_value`.
    pub wall_breach_value: i64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            damage_weight: 2,
            kill_bonus_per_score: 20,
            asset_damage_divisor: 4,
            fire_horizon: 10,
            urgency_rounds: 3,
            urgency_lead: 8,
            station_value: 200,
            operator_value: 600,
            role_value: 400,
            tower_value: 300,
            wall_value: 60,
            station_focus: 1,
            clear_gap_drive: 0,
            wall_breach_value: 320,
        }
    }
}

impl Weights {
    /// Read the dial from `CG_TUNE_*` in the process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(format!("CG_TUNE_{name}")).ok())
    }

    /// Parse the dial from an arbitrary name→value source (the environment in
    /// production, a closure in tests). A name that is absent or unparseable
    /// leaves its default untouched.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut weights = Self::default();
        let read = |name: &str, slot: &mut i64| {
            if let Some(raw) = lookup(name) {
                if let Ok(value) = raw.trim().parse::<i64>() {
                    *slot = value;
                }
            }
        };
        read("DAMAGE_WEIGHT", &mut weights.damage_weight);
        read("KILL_BONUS", &mut weights.kill_bonus_per_score);
        read("ASSET_DIVISOR", &mut weights.asset_damage_divisor);
        read("FIRE_HORIZON", &mut weights.fire_horizon);
        read("URGENCY_ROUNDS", &mut weights.urgency_rounds);
        read("URGENCY_LEAD", &mut weights.urgency_lead);
        read("STATION_VALUE", &mut weights.station_value);
        read("OPERATOR_VALUE", &mut weights.operator_value);
        read("ROLE_VALUE", &mut weights.role_value);
        read("TOWER_VALUE", &mut weights.tower_value);
        read("WALL_VALUE", &mut weights.wall_value);
        read("STATION_FOCUS", &mut weights.station_focus);
        read("CLEAR_GAP", &mut weights.clear_gap_drive);
        read("WALL_BREACH", &mut weights.wall_breach_value);
        weights
    }
}

/// The active weights (parsed from the environment once).
pub fn weights() -> &'static Weights {
    static WEIGHTS: OnceLock<Weights> = OnceLock::new();
    WEIGHTS.get_or_init(Weights::from_env)
}

/// Robot id → HP remaining in this round's firing simulation.
pub type SimHp = HashMap<i64, i64>;
/// Enemy building id → HP remaining in this round's firing simulation.
pub type SimBuildings = HashMap<i64, i64>;

/// Per-round damage simulation shared by every tower, so a target an earlier
/// tower already destroyed is never shot at again — robots and enemy
/// buildings alike. This is the cross-tower damage reservation: it is what
/// keeps two railguns from spending a full volley on the same corpse.
#[derive(Debug, Clone, Default)]
pub struct Sim {
    pub robots: SimHp,
    pub buildings: SimBuildings,
    /// Towers that already have a firing command this round.
    pub fired: HashSet<i64>,
}

impl Sim {
    /// Simulated HP a robot has left (mirrors the old bare-`HashMap` API).
    pub fn get(&self, id: &i64) -> Option<&i64> {
        self.robots.get(id)
    }
    /// Simulated HP an enemy building has left.
    pub fn building_hp(&self, unit: &Unit) -> i64 {
        self.buildings.get(&unit.id).copied().unwrap_or(unit.health)
    }
    fn damage_robot(&mut self, id: i64, dmg: i64) {
        let hp = self.robots.get(&id).copied().unwrap_or(0);
        self.robots.insert(id, (hp - dmg).max(0));
    }
    fn damage_building(&mut self, id: i64, dmg: i64) {
        let hp = self.buildings.get(&id).copied().unwrap_or(0);
        self.buildings.insert(id, (hp - dmg).max(0));
    }
}

pub fn init_sim(turn: &Turn) -> Sim {
    Sim {
        robots: turn
            .robots
            .iter()
            .filter(|robot| robot.health > 0)
            .map(|robot| (robot.id, robot.health))
            .collect(),
        buildings: turn
            .enemy
            .iter()
            .filter(|unit| unit.alive())
            .map(|unit| (unit.id, unit.health))
            .collect(),
        fired: HashSet::new(),
    }
}

/// Threat weight: robots hunting OUR side count more, scaled by how close
/// they are to our base and whether they are meleeing our walls.
pub fn threat(turn: &Turn, robot: &Robot) -> i64 {
    if robot.target_team != turn.team_type {
        return 0;
    }
    let mut value = 10i64;
    if let Some(station) = turn.station() {
        let footprint = station_footprint(station.pos);
        let dist = footprint_distance(robot.pos, &footprint);
        value += ((14 - dist).max(0) * 2) as i64;
    }
    let meleeing_wall = turn
        .walls()
        .iter()
        .any(|wall| chebyshev(wall.pos, robot.pos) <= 1);
    if meleeing_wall {
        value += 15;
    }
    value
}

/// Damage a robot class deals to our assets per round. The task book weights
/// the classes 1/2/4/10 for a kill, and their bite scales the same way, so the
/// kill score doubles as the per-round pressure proxy.
pub fn robot_pressure(kind: RobotKind) -> i64 {
    kind.score().max(1) * 10
}

/// Rounds until `robot` can start hitting one of our assets (station, towers,
/// walls), ignoring our fire. 0 means it is already in contact. Capped at the
/// fire horizon, past which it is not a threat this night.
pub fn rounds_to_contact(turn: &Turn, robot: &Robot) -> i64 {
    let mut best = i64::MAX;
    let mut consider = |footprint: Vec<Pos>| {
        let dist = footprint_distance(robot.pos, &footprint) as i64 - 1;
        best = best.min(dist.max(0));
    };
    if let Some(station) = turn.station() {
        consider(station_footprint(station.pos));
    }
    for tower in turn.towers() {
        consider(vec![tower.pos]);
    }
    for wall in turn.walls() {
        consider(vec![wall.pos]);
    }
    if best == i64::MAX {
        return weights().fire_horizon;
    }
    best.min(weights().fire_horizon)
}

/// Score points of removing this robot (`score2`, exact).
pub fn kill_score_value(robot: &Robot) -> i64 {
    kill_score_value_with(weights(), robot)
}

/// As `kill_score_value`, against an explicit dial (used by the A/B report).
pub fn kill_score_value_with(weights: &Weights, robot: &Robot) -> i64 {
    robot.kind.score() * weights.kill_bonus_per_score
}

/// Expected damage this robot still does to our assets before our towers kill
/// it: its per-round pressure over the rounds left before it reaches us, over
/// the horizon. Robots that hunt the other team score nothing here — they are
/// not going to chew on our station.
pub fn asset_damage_value(turn: &Turn, robot: &Robot) -> i64 {
    asset_damage_value_with(weights(), turn, robot)
}

fn asset_damage_value_with(weights: &Weights, turn: &Turn, robot: &Robot) -> i64 {
    if robot.target_team != turn.team_type || robot.health <= 0 {
        return 0;
    }
    let contact = rounds_to_contact(turn, robot);
    let rounds = (weights.fire_horizon - contact).max(0);
    robot_pressure(robot.kind) * rounds / weights.asset_damage_divisor.max(1)
}

/// Terminal risk: damage that lands NOW, on top of the expected total above —
/// a robot already touching our line is taking `score3` apart this round.
pub fn urgency_value(turn: &Turn, robot: &Robot) -> i64 {
    urgency_value_with(weights(), turn, robot)
}

fn urgency_value_with(weights: &Weights, turn: &Turn, robot: &Robot) -> i64 {
    if robot.target_team != turn.team_type || robot.health <= 0 {
        return 0;
    }
    if rounds_to_contact(turn, robot) > 0 {
        return 0;
    }
    robot_pressure(robot.kind) * weights.urgency_rounds / weights.asset_damage_divisor.max(1)
}

/// The single objective both robot targeting and night prioritisation use:
/// what killing this robot is worth to the scoreboard (`score2`) plus what it
/// saves us (`score3`), plus how soon that saving happens.
pub fn win_value(turn: &Turn, robot: &Robot) -> i64 {
    win_value_with(weights(), turn, robot)
}

/// As `win_value`, against an explicit dial.
pub fn win_value_with(weights: &Weights, turn: &Turn, robot: &Robot) -> i64 {
    kill_score_value_with(weights, robot)
        + asset_damage_value_with(weights, turn, robot)
        + urgency_value_with(weights, turn, robot)
        + threat(turn, robot)
}

/// Expected value of `dmg` damage on `robot`: progress toward removing it,
/// paid in full when the damage is lethal (the whole win value lands) and at a
/// quarter rate while it is only being softened.
fn hit_value(turn: &Turn, robot: &Robot, hp_before: i64, dmg: i64) -> i64 {
    let weights = weights();
    let dealt = dmg.min(hp_before).max(0);
    let mut value = dealt * weights.damage_weight;
    let win = win_value(turn, robot);
    if dealt >= hp_before {
        value += win;
    } else if hp_before > 0 {
        value += win * dealt / hp_before / 4;
    }
    value
}

/// Pairwise angle check: all targets inside one 90° cone ⇔ every pair of
/// direction vectors has a non-negative dot product.
pub fn within_cone(tower: Pos, targets: &[Pos]) -> bool {
    for i in 0..targets.len() {
        for j in (i + 1)..targets.len() {
            let vx = (targets[i].x - tower.x) as i64;
            let vy = (targets[i].y - tower.y) as i64;
            let wx = (targets[j].x - tower.x) as i64;
            let wy = (targets[j].y - tower.y) as i64;
            if vx * wx + vy * wy < 0 {
                return false;
            }
        }
    }
    true
}

/// Cells along the segment a→b (inclusive), grid-DDA sampled.
pub fn line_cells(a: Pos, b: Pos) -> Vec<Pos> {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let steps = dx.abs().max(dy.abs());
    if steps == 0 {
        return vec![a];
    }
    let mut cells = Vec::with_capacity(steps as usize + 1);
    for t in 0..=steps {
        let x = a.x + (dx as f64 * t as f64 / steps as f64).round() as i32;
        let y = a.y + (dy as f64 * t as f64 / steps as f64).round() as i32;
        let pos = Pos { x, y };
        if cells.last() != Some(&pos) {
            cells.push(pos);
        }
    }
    cells
}

fn in_range(tower: &Unit, pos: Pos) -> bool {
    let range = tower.range_of_attack();
    range >= FULL_MAP_RANGE || chebyshev(tower.pos, pos) <= range
}

/// What a tower decided to spend its volley on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Robots,
    EnemyAssets,
}

/// Choose attack target positions for a ready tower, degrading `sim` with the
/// damage this tower is expected to deal. Returns None when nothing to shoot.
pub fn choose_attack(turn: &Turn, tower: &Unit, sim: &mut Sim) -> Option<Vec<Pos>> {
    choose_attack_kind(turn, tower, sim).map(|(targets, _)| targets)
}

/// As `choose_attack`, but also reports whether the volley went at robots or
/// at the opponent's assets (night.rs logs the difference).
pub fn choose_attack_kind(
    turn: &Turn,
    tower: &Unit,
    sim: &mut Sim,
) -> Option<(Vec<Pos>, TargetKind)> {
    choose_attack_kind_with(&crate::brain::coach::Policy::committed(), turn, tower, sim)
}

/// As `choose_attack_kind`, against the adaptive policy the coach currently
/// holds (`BotState::coach`). The committed policy reproduces the shipped
/// behaviour bit for bit, so the plain entry point above is unchanged for every
/// caller that has no coach to consult.
///
/// The two dials keep their meaning: `CG_TUNE_STATION_FOCUS=0` still pins the
/// opportunistic station fire off (it is ANDed below), and
/// `CG_TUNE_CLEAR_GAP` is untouched here (it lives in `economy::intent_list`).
pub fn choose_attack_kind_with(
    policy: &crate::brain::coach::Policy,
    turn: &Turn,
    tower: &Unit,
    sim: &mut Sim,
) -> Option<(Vec<Pos>, TargetKind)> {
    let projectiles = crate::model::tower_projectiles(tower.kind, tower.level.max(1)) as usize;
    if projectiles == 0 {
        return None;
    }
    // Absolute priority: a station that dies this round ends the half. Nothing
    // else on the board — not even the wave in front of us — is worth more, so
    // this is the one case that outranks robot targeting.
    if station_killable(turn, sim) {
        if let Some(targets) = station_targets(turn, tower, projectiles, sim) {
            sim.fired.insert(tower.id);
            return Some((targets, TargetKind::EnemyAssets));
        }
    }
    let robots: Vec<&Robot> = turn
        .robots
        .iter()
        .filter(|robot| sim.get(&robot.id).copied().unwrap_or(0) > 0)
        .collect();
    let targets = match tower.kind {
        UnitKind::Gatling => choose_gatling(turn, tower, &robots, projectiles, sim),
        UnitKind::Railgun => choose_railgun(turn, tower, &robots, sim),
        UnitKind::Rocket => choose_rocket(turn, tower, &robots, projectiles, sim),
        _ => None,
    };
    if let Some(targets) = targets {
        if !targets.is_empty() {
            sim.fired.insert(tower.id);
            return Some((targets, TargetKind::Robots));
        }
    }
    // Offense-first: when no NPC robot is threatening us, check for enemy
    // summoned robots. After our robots are cleared, the opponent's summoned
    // units are still marching — rockets with splash damage can eliminate them
    // for kill points and reduce enemy pressure.
    if tower.kind == UnitKind::Rocket {
        let enemy_robots: Vec<&Robot> = turn
            .robots
            .iter()
            .filter(|robot| {
                robot.target_team != turn.team_type
                    && sim.get(&robot.id).copied().unwrap_or(0) > 0
                    && in_range(tower, robot.pos)
            })
            .collect();
        if !enemy_robots.is_empty() {
            if let Some(targets) = choose_rocket(turn, tower, &enemy_robots, projectiles, sim) {
                if !targets.is_empty() {
                    sim.fired.insert(tower.id);
                    return Some((targets, TargetKind::Robots));
                }
            }
        }
    }
    // Offense-first: fire at the opponent's assets when nothing else is in
    // range. This tower was going to sit idle anyway — every idle volley into
    // the enemy is permanent progress toward the win condition (issues #201-#205).
    let targets = choose_enemy_targets_with(policy, turn, tower, projectiles, sim);
    targets.map(|targets| {
        sim.fired.insert(tower.id);
        (targets, TargetKind::EnemyAssets)
    })
}

/// True when every robot marching on us is already covered by a ready tower —
/// i.e. we have firepower to spare for the opponent. A robot hunting us that
/// no ready tower can reach is the one case where plinking the enemy would
/// split our attention, so it is the one case that suppresses it.
pub fn spare_firepower(turn: &Turn) -> bool {
    let ready: Vec<&Unit> = turn
        .towers()
        .into_iter()
        .filter(|tower| tower.cooldown == 0)
        .collect();
    !turn.robots.iter().any(|robot| {
        robot.health > 0
            && threat(turn, robot) > 0
            && !ready.iter().any(|tower| in_range(tower, robot.pos))
    })
}

fn robot_at<'a>(robots: &[&'a Robot], pos: Pos) -> Option<&'a Robot> {
    robots.iter().copied().find(|robot| robot.pos == pos)
}

/// First robot (by simulated HP > 0) along the trajectory tower→target.
fn first_on_line<'r>(robots: &[&'r Robot], sim: &Sim, from: Pos, to: Pos) -> Option<&'r Robot> {
    for cell in line_cells(from, to) {
        if cell == from {
            continue;
        }
        if let Some(robot) = robot_at(robots, cell) {
            if sim.get(&robot.id).copied().unwrap_or(0) > 0 {
                return Some(robot);
            }
        }
    }
    None
}

fn choose_gatling(
    turn: &Turn,
    tower: &Unit,
    robots: &[&Robot],
    bullets: usize,
    sim: &mut Sim,
) -> Option<Vec<Pos>> {
    let candidates: Vec<&&Robot> = robots
        .iter()
        .filter(|robot| in_range(tower, robot.pos))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let mut chosen: Vec<Pos> = Vec::new();
    while chosen.len() < bullets {
        let mut best: Option<(i64, Pos)> = None;
        for robot in &candidates {
            let mut trial = chosen.clone();
            trial.push(robot.pos);
            if !within_cone(tower.pos, &trial) {
                continue;
            }
            // Bullet hits the first robot along the trajectory.
            let Some(victim) = first_on_line(robots, sim, tower.pos, robot.pos) else {
                continue;
            };
            let hp = sim.get(&victim.id).copied().unwrap_or(0);
            let value = hit_value(turn, victim, hp, 10);
            if best.map(|(v, _)| value > v).unwrap_or(true) {
                best = Some((value, robot.pos));
            }
        }
        match best {
            Some((_, pos)) => {
                // Degrade the simulation with this bullet.
                if let Some(victim) = first_on_line(robots, sim, tower.pos, pos) {
                    sim.damage_robot(victim.id, 10);
                }
                chosen.push(pos);
            }
            None => break,
        }
    }
    if chosen.is_empty() {
        return None;
    }
    while chosen.len() < bullets {
        let last = *chosen.last().unwrap();
        chosen.push(last);
    }
    Some(chosen)
}

fn choose_railgun(turn: &Turn, tower: &Unit, robots: &[&Robot], sim: &mut Sim) -> Option<Vec<Pos>> {
    let energy0 = if tower.attack_power > 0 {
        tower.attack_power
    } else {
        match tower.level.max(1) {
            1 => 10,
            2 => 20,
            _ => 30,
        }
    };
    let mut best: Option<(i64, i32, Pos, Vec<(i64, i64)>)> = None; // (value, dist, target, (id,dmg) list)
    for robot in robots {
        if !in_range(tower, robot.pos) {
            continue;
        }
        let mut energy = energy0;
        let mut value = 0i64;
        let mut hits: Vec<(i64, i64)> = Vec::new();
        for cell in line_cells(tower.pos, robot.pos) {
            if cell == tower.pos || energy <= 0 {
                continue;
            }
            let Some(victim) = robot_at(robots, cell) else {
                continue;
            };
            let hp = sim.get(&victim.id).copied().unwrap_or(0);
            if hp <= 0 {
                continue;
            }
            let dmg = energy.min(hp);
            energy -= dmg;
            value += hit_value(turn, victim, hp, dmg);
            hits.push((victim.id, dmg));
        }
        if value <= 0 {
            continue;
        }
        let dist = chebyshev(tower.pos, robot.pos);
        let better = match &best {
            Some((bv, bd, _, _)) => value > *bv || (value == *bv && dist < *bd),
            None => true,
        };
        if better {
            best = Some((value, dist, robot.pos, hits));
        }
    }
    let (_, _, target, hits) = best?;
    for (id, dmg) in hits {
        sim.damage_robot(id, dmg);
    }
    Some(vec![target])
}

fn choose_rocket(
    turn: &Turn,
    tower: &Unit,
    robots: &[&Robot],
    missiles: usize,
    sim: &mut Sim,
) -> Option<Vec<Pos>> {
    let candidates: Vec<Pos> = robots
        .iter()
        .filter(|robot| in_range(tower, robot.pos))
        .map(|robot| robot.pos)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let mut impacts: Vec<Pos> = Vec::new();
    for _ in 0..missiles {
        let mut best: Option<(i64, Pos)> = None;
        for impact in &candidates {
            let value = splash_value(turn, *impact, robots, sim);
            if best.map(|(v, _)| value > v).unwrap_or(true) {
                best = Some((value, *impact));
            }
        }
        match best {
            Some((value, pos)) => {
                if value <= 0 && !impacts.is_empty() {
                    break;
                }
                apply_splash(pos, robots, sim);
                impacts.push(pos);
            }
            None => break,
        }
    }
    if impacts.is_empty() {
        return None;
    }
    while impacts.len() < missiles {
        let last = *impacts.last().unwrap();
        impacts.push(last);
    }
    Some(impacts)
}

fn splash_value(turn: &Turn, impact: Pos, robots: &[&Robot], sim: &Sim) -> i64 {
    let mut value = 0i64;
    for robot in robots {
        let d = chebyshev(impact, robot.pos);
        if d > 1 {
            continue;
        }
        let hp = sim.get(&robot.id).copied().unwrap_or(0);
        if hp <= 0 {
            continue;
        }
        // Center 20, eight neighbours half of center (10).
        let raw = if d == 0 { 20 } else { 10 };
        value += hit_value(turn, robot, hp, raw);
    }
    value
}

fn apply_splash(impact: Pos, robots: &[&Robot], sim: &mut Sim) {
    for robot in robots {
        let d = chebyshev(impact, robot.pos);
        if d > 1 {
            continue;
        }
        let raw = if d == 0 { 20 } else { 10 };
        sim.damage_robot(robot.id, raw);
    }
}

/// Damage a single projectile of `tower` deals to one enemy building.
fn tower_shot_damage(tower: &Unit) -> i64 {
    match tower.kind {
        UnitKind::Gatling => 10,
        UnitKind::Railgun => {
            if tower.attack_power > 0 {
                tower.attack_power
            } else {
                match tower.level.max(1) {
                    1 => 10,
                    2 => 20,
                    _ => 30,
                }
            }
        }
        UnitKind::Rocket => 20,
        _ => 0,
    }
}

/// Damage `tower` puts on one building if it aims its whole volley at it.
fn tower_volley_damage(tower: &Unit) -> i64 {
    let shots = crate::model::tower_projectiles(tower.kind, tower.level.max(1)).max(1) as i64;
    tower_shot_damage(tower) * shots
}

/// Rounds the target is expected to stay inside this tower's reach. A unit
/// sitting on the range boundary can step out next round (window 1, act now);
/// one deep inside the envelope will still be there tomorrow (window large,
/// no rush). This is the "visible window" of the enemy scoring.
fn visibility_window(tower: &Unit, cell: Pos) -> i64 {
    const WINDOW_MAX: i64 = 8;
    let range = tower.range_of_attack();
    if range >= FULL_MAP_RANGE {
        return WINDOW_MAX;
    }
    let margin = (range - chebyshev(tower.pos, cell)).max(0) as i64;
    (margin + 1).min(WINDOW_MAX)
}

/// What disabling one enemy unit is worth. Operators come first: a visible
/// human next to one of their towers is the thing that makes that tower fire,
/// so removing it silences a weapon outright. The station is worth less than a
/// tower *unless* it can be levelled this round (`station_killable`), because
/// only a destroyed station ends the half.
fn enemy_unit_value_with(weights: &Weights, turn: &Turn, unit: &Unit) -> i64 {
    match unit.kind {
        UnitKind::Station => weights.station_value,
        UnitKind::Pioneer | UnitKind::Worker => {
            let manning = turn.enemy.iter().any(|other| {
                other.alive()
                    && matches!(
                        other.kind,
                        UnitKind::Gatling | UnitKind::Railgun | UnitKind::Rocket
                    )
                    && chebyshev(other.pos, unit.pos) <= 1
            });
            if manning {
                weights.operator_value
            } else {
                weights.role_value
            }
        }
        UnitKind::Gatling | UnitKind::Railgun | UnitKind::Rocket => weights.tower_value,
        UnitKind::Wall => weights.wall_value,
        UnitKind::Unknown => 0,
    }
}

/// Value of aiming this tower's whole volley at `unit`: kill probability ×
/// disable value × visibility window. `sim` carries the damage earlier towers
/// in this same round already committed, so the probability is computed
/// against what is actually left of the target.
///
/// The dial is explicit rather than read from the environment because the
/// caller sometimes hands in a DERIVED one: while a breach is on the table
/// (`wall_breach_with`) walls are scored at `wall_breach_value` through this
/// same ranking. The base dial is still the knob an A/B run turns to trade
/// "disable their guns" against "kill their station".
pub fn enemy_unit_score_with(
    weights: &Weights,
    turn: &Turn,
    tower: &Unit,
    unit: &Unit,
    cells: &[Pos],
    sim: &Sim,
) -> i64 {
    let hp = sim.building_hp(unit);
    if hp <= 0 || cells.is_empty() {
        return 0;
    }
    let damage = tower_volley_damage(tower);
    let kill_prob = (damage * 100 / hp.max(1)).clamp(0, 100);
    let base = kill_prob * enemy_unit_value_with(weights, turn, unit) / 100;
    // The most urgent reachable cell sets the window: a unit half inside our
    // envelope can be gone next round.
    let window = cells
        .iter()
        .map(|cell| visibility_window(tower, *cell))
        .min()
        .unwrap_or(1);
    base + base * weights.urgency_lead / window
}

/// Can the towers still to fire this round level the enemy station? Destroying
/// it decides the half, so when this holds the station outranks everything —
/// including the robots, which is the one exception to "clear the wave first".
pub fn station_killable(turn: &Turn, sim: &Sim) -> bool {
    let Some(station) = turn
        .enemy
        .iter()
        .find(|unit| unit.kind == UnitKind::Station && unit.alive())
    else {
        return false;
    };
    let hp = sim.building_hp(station);
    if hp <= 0 {
        return false;
    }
    if station.footprint().iter().all(|cell| {
        !turn
            .towers()
            .iter()
            .any(|tower| tower.cooldown == 0 && in_range(tower, *cell))
    }) {
        return false;
    }
    let total: i64 = turn
        .towers()
        .iter()
        .filter(|tower| tower.cooldown == 0 && !sim.fired.contains(&tower.id))
        .filter(|tower| {
            station
                .footprint()
                .iter()
                .any(|cell| in_range(tower, *cell))
        })
        .map(|tower| tower_volley_damage(tower))
        .sum();
    total >= hp
}

/// Fill `targets` up to `projectiles` repeats and enforce gatling cone
/// legality (a cone violation invalidates the whole volley, so it degrades to
/// a single target rather than being rejected by the judger).
fn pad_targets(tower: &Unit, targets: &mut Vec<Pos>, projectiles: usize) {
    if targets.is_empty() {
        return;
    }
    if tower.kind == UnitKind::Gatling && !within_cone(tower.pos, targets) {
        targets.truncate(1);
    }
    // The judger wants EXACTLY `projectiles` targets — validate.rs drops the
    // whole volley on a length mismatch, and a station footprint offers four
    // cells, more than a level-1/2 volley may carry. Trimming here keeps the
    // station volleys of `station_targets` legal instead of silently dropped.
    targets.truncate(projectiles);
    while targets.len() < projectiles {
        let last = *targets.last().unwrap();
        targets.push(last);
    }
}

/// Full HP of a station at the given level (任务书 4.5.1). The protocol only
/// carries CURRENT health, so "already damaged" must be derived from the
/// level table — mirrors `wall_max_hp`.
pub fn station_max_hp(level: i32) -> i64 {
    match level.max(1).min(3) {
        1 => 1500,
        2 => 3000,
        _ => 4500,
    }
}

/// Sustained enemy-station pressure (P2-2). Two cases:
///
/// * a level-3 rocket, whose range is the whole map, can batter the station
///   every cooldown cycle — pk577297 (issue #21) won the half exactly that
///   way: 1490 damage over 43 rounds while the opponent's three guns cleared
///   robots in the wrong half of the map;
/// * any tower facing an already-damaged station finishes it, because a
///   destroyed station ends the half outright (任务书 ch.7: 先被摧毁的一方判负),
///   and station damage is permanent while operators revive in a day.
///
/// Ranked asset scoring (`enemy_unit_score`) still owns every other case:
/// silencing a manned enemy tower is worth more than scratching a full-HP
/// station nobody else is working on.
pub fn station_focus_with(weights: &Weights, turn: &Turn, tower: &Unit, sim: &Sim) -> bool {
    if weights.station_focus == 0 {
        return false;
    }
    let Some(station) = turn
        .enemy
        .iter()
        .find(|unit| unit.kind == UnitKind::Station && unit.alive())
    else {
        return false;
    };
    let hp = sim.building_hp(station);
    if hp <= 0 {
        return false;
    }
    if !station.footprint().iter().any(|cell| in_range(tower, *cell)) {
        return false;
    }
    tower.range_of_attack() >= FULL_MAP_RANGE || hp < station_max_hp(station.level)
}

/// As [`station_focus_with`], against the active dial.
pub fn station_focus(turn: &Turn, tower: &Unit, sim: &Sim) -> bool {
    station_focus_with(weights(), turn, tower, sim)
}

/// Robot attack range (任务书 4.7.2: small/middle/large/BOSS all shoot 3 cells).
const ROBOT_ATTACK_RANGE: i32 = 3;

/// Our station is not being hit this round: no robot hunting us stands inside
/// its own attack range of the station footprint, so nothing on the board can
/// be chewing on the base right now. A robot nine cells out is a different
/// problem — and `spare_firepower` already covers it — this is only the
/// question the breach gate asks: is the base the thing under the knife?
pub fn station_safe(turn: &Turn) -> bool {
    let Some(station) = turn.station() else {
        return false;
    };
    let footprint = station_footprint(station.pos);
    !turn.robots.iter().any(|robot| {
        robot.health > 0
            && robot.target_team == turn.team_type
            && footprint_distance(robot.pos, &footprint) <= ROBOT_ATTACK_RANGE
    })
}

/// Their station is already hurt. The protocol carries CURRENT health only, so
/// "damaged" comes off the level table (`station_max_hp`) — the same trick
/// [`station_focus_with`] uses, and `sim` is consulted so damage an earlier
/// tower committed this same round counts.
pub fn enemy_station_damaged(turn: &Turn, sim: &Sim) -> bool {
    match turn.enemy_station().filter(|station| station.alive()) {
        Some(station) => sim.building_hp(station) < station_max_hp(station.level),
        None => false,
    }
}

/// Wall-breach tactic (P1): should this tower spend a spare volley opening a
/// hole in the opponent's wall line?
///
/// At night the robots attack BOTH bases, and in our own matches their base
/// fell to robots more often than to our guns. A wall is the only thing that
/// stops that wave — 任务书 4.7.3 "机器人会攻击阻挡其移动的单位", and 2.2 "建筑被
/// 拆除后，对应区域变为可穿越空地" — so one demolished wall cell turns the ring
/// the robots have been chewing on all night into a door. 任务书 ch.7 decides
/// the half by which base falls first, and that is worth far more than the
/// same volley chipping their station or one of their guns.
///
/// Gated on evidence rather than appetite:
/// * `spare_firepower` — issue #7's "有余力时" still owns the ordering, and a
///   robot hunting us that no ready tower reaches is exactly the case this
///   must never fire in (the caller checks it too; repeated here so the gate
///   cannot be lost when this is reached from somewhere else);
/// * a robot is marching on THEM — a hole nobody walks through is just a
///   repaired wall waiting to happen;
/// * their station already damaged, or ours is not being hit this round — the
///   one night the volley belongs at home is the night our base is the one
///   coming apart;
/// * an enemy wall inside this tower's reach whose remaining HP this volley
///   covers. The dial raises what a wall is WORTH; it does not turn a scratch
///   into a breach, so a full-HP ring leaves the committed order untouched.
///
/// `station_killable` is checked ahead of this in `choose_enemy_targets_with`
/// and is deliberately NOT part of the gate: a station that dies this round
/// ends the half, which stays the highest rule on the board. The coach's 收火
/// (`Policy::station_pressure`) is not part of it either — 收火 pulls the
/// campaign off their STATION while still firing at their other buildings, and
/// the `station_safe` evidence above is the stronger version of the same worry.
pub fn wall_breach_with(weights: &Weights, turn: &Turn, tower: &Unit, sim: &Sim) -> bool {
    if weights.wall_breach_value <= 0 {
        return false;
    }
    if !spare_firepower(turn) {
        return false;
    }
    if !turn
        .robots
        .iter()
        .any(|robot| robot.health > 0 && robot.target_team != turn.team_type)
    {
        return false;
    }
    if !enemy_station_damaged(turn, sim) && !station_safe(turn) {
        return false;
    }
    let damage = tower_volley_damage(tower);
    turn.enemy_walls().iter().any(|wall| {
        let hp = sim.building_hp(wall);
        hp > 0 && hp <= damage && wall.footprint().iter().any(|cell| in_range(tower, *cell))
    })
}

/// As [`wall_breach_with`], against the active dial.
pub fn wall_breach(turn: &Turn, tower: &Unit, sim: &Sim) -> bool {
    wall_breach_with(weights(), turn, tower, sim)
}

/// Estimated total HP of tonight's robot wave (P1-1). Measured anchors from
/// the issues: D1 = 70 smalls (issue #14), D2 = 90+ mixed (#8). Linear count
/// model `50 + 20 × day` at a 45 HP blended average (smalls dominate; the
/// middles and larges mixed in from D2 raise the mean above 40).
pub fn estimated_wave_hp(day: i64) -> i64 {
    (50 + 20 * day.max(1)) * 45
}

/// Damage our towers can theoretically put out across one 60-round night,
/// assuming every tower is manned every round. The rocket's 3-round cooldown
/// is amortised to a third of its volley; gatling bullets and railgun energy
/// are per-round. This is the ceiling the wave estimate is compared against —
/// the arithmetic of Improve.kimi.md §3.1: 2×L1 = 1200 < the D1 wave's 3150.
pub fn night_fire_capacity(turn: &Turn) -> i64 {
    night_fire_capacity_with(turn, -1, 0)
}

/// [`night_fire_capacity`] with one tower's level shifted, so the funding order
/// can ask what a voucher would actually buy. `bump_id < 0` shifts nothing.
fn night_fire_capacity_with(turn: &Turn, bump_id: i64, bump: i32) -> i64 {
    turn.towers()
        .iter()
        .map(|tower| {
            let raw = if tower.id == bump_id {
                tower.level + bump
            } else {
                tower.level
            };
            let level = raw.max(1) as i64;
            let per_round = match tower.kind {
                UnitKind::Gatling => 10 * level,
                UnitKind::Railgun => {
                    if tower.attack_power > 0 {
                        tower.attack_power
                    } else {
                        10 * level
                    }
                }
                UnitKind::Rocket => 20 * level / 3,
                _ => 0,
            };
            per_round * 60
        })
        .sum()
}

/// Tonight's clear gap: positive means the wave out-HPs our guns and the
/// difference lands on the wall ring and the base. The P1-1 funding order
/// (`economy::intent_list` under `CG_TUNE_CLEAR_GAP`) keys on this: a gap
/// forces firepower funding (third tower, weapon vouchers) before any
/// station upgrade or harassment spend.
pub fn firepower_gap(turn: &Turn) -> i64 {
    estimated_wave_hp(turn.day) - night_fire_capacity(turn)
}

/// The most `night_fire_capacity` one more weapon level can still add: the best
/// single step available among the towers that are not level 3 yet.
///
/// This is the yardstick that tells an ACTIONABLE gap from a background one, and
/// [`firepower_gap`] alone cannot (issues #176-#185). `estimated_wave_hp` starts
/// at 3150 on day 1 and grows 900 a day; three level-1 towers — the board this
/// bot actually fields, and the one `night_fire_capacity`'s own docstring names
/// ("2×L1 = 1200 < the D1 wave's 3150") — put out 1600. So the gap is positive
/// from round 1 of day 1 and grows; closing it needs 4800, i.e. three level-3
/// towers. A test that is true on every board this bot has ever fielded is not
/// detecting a firepower emergency, it is describing the game.
///
/// The distinction the funding order needs is whether ONE voucher closes the
/// gap. If it does, funding firepower wins tonight and the order is right. If it
/// does not, the voucher does not win tonight either — the wave still out-HPs
/// the guns — and the gold is better spent on the asset whose destruction ends
/// the half outright (任务书 ch.7) and whose survival is `score_3`
/// (`Σ 10×day`, 550 over ten).
pub fn best_weapon_step_gain(turn: &Turn) -> i64 {
    let base = night_fire_capacity(turn);
    turn.towers()
        .iter()
        .filter(|tower| tower.level < 3)
        .map(|tower| night_fire_capacity_with(turn, tower.id, 1) - base)
        .max()
        .unwrap_or(0)
}

/// Cells of the enemy station, in range, padded out to `projectiles`, with the
/// damage reserved in `sim`.
fn station_targets(
    turn: &Turn,
    tower: &Unit,
    projectiles: usize,
    sim: &mut Sim,
) -> Option<Vec<Pos>> {
    let station = turn
        .enemy
        .iter()
        .find(|unit| unit.kind == UnitKind::Station && unit.alive())?;
    if sim.building_hp(station) <= 0 {
        return None;
    }
    let mut targets: Vec<Pos> = station
        .footprint()
        .into_iter()
        .filter(|cell| in_range(tower, *cell))
        .collect();
    if targets.is_empty() {
        return None;
    }
    pad_targets(tower, &mut targets, projectiles);
    sim.damage_building(station.id, tower_volley_damage(tower));
    Some(targets)
}

/// Best enemy building to shoot when no robot is in reach: units are ranked by
/// kill probability × disable value × visibility window, the volley goes to
/// the best one, and any projectiles still unspent move down the ranking.
/// Damage is reserved in `sim` as it is committed, so a later tower never
/// re-kills a building this one already finished.
///
/// The sustained-pressure branch (P2-2) is
/// gated by the coach's `station_pressure` switch: when our own station is the
/// one taking hits, the guns come home instead of plinking their base.
/// `station_killable` is NOT gated — a station that dies this round ends the
/// half, and that is a rule, not a preference.
///
/// The wall-breach tactic (P1) sits between those two: it needs a spare volley
/// and a wall this volley can actually open (see `wall_breach_with`), and when
/// it holds it demotes the P2-2 pressure branch for this tower — a hole their
/// ring cannot un-ring is worth more than another round of station damage that
/// does not finish it. `station_killable` still outranks it, unchanged.
fn choose_enemy_targets_with(
    policy: &crate::brain::coach::Policy,
    turn: &Turn,
    tower: &Unit,
    projectiles: usize,
    sim: &mut Sim,
) -> Option<Vec<Pos>> {
    if station_killable(turn, sim) {
        return station_targets(turn, tower, projectiles, sim);
    }
    // Wall-breach tactic (P1). Checked BEFORE the sustained-pressure branch
    // below and only before that one: `station_killable` above is a rule of the
    // game and keeps its absolute priority. When a spare volley can actually
    // open a hole in their ring, chipping a station this round cannot kill is
    // the worse use of it — the wave behind that hole decides the half.
    let breach = wall_breach_with(weights(), turn, tower, sim);
    // Sustained station pressure (P2-2): the wave is already covered — the
    // caller's `spare_firepower` gate owns that ordering — and a volley into
    // the enemy station is permanent progress toward the win condition, which
    // outranks plinking operators. pk577297: 1490 damage in 43 rounds, half
    // won while the opponent cleared robots on the wrong side of the map.
    if !breach && policy.station_pressure && station_focus(turn, tower, sim) {
        return station_targets(turn, tower, projectiles, sim);
    }
    // Breaching is spent through the ordinary ranking rather than around it: a
    // wall is worth `wall_breach_value` instead of `wall_value` for this
    // volley, so reachability, the gatling cone, the visibility window and the
    // cross-tower damage reservation all keep working on it unchanged.
    let ranking = if breach {
        Weights {
            wall_value: weights().wall_breach_value,
            ..*weights()
        }
    } else {
        *weights()
    };
    let mut ranked: Vec<(i64, i64, Vec<Pos>)> = Vec::new(); // (score, id, reachable cells)
    for unit in turn.enemy.iter().filter(|unit| unit.alive()) {
        if sim.building_hp(unit) <= 0 {
            continue;
        }
        // The coach pulled the campaign off their base (our own station is the
        // one bleeding): skip it here too, or "收火" would only demote it from
        // first place to whatever the ranked order still gives a full-HP
        // station. A station we can actually destroy this round never reaches
        // this loop — `station_killable` above already took that shot, and that
        // is a rule of the game, not a preference of ours.
        if !policy.station_pressure && unit.kind == UnitKind::Station {
            continue;
        }
        let cells: Vec<Pos> = unit
            .footprint()
            .into_iter()
            .filter(|cell| in_range(tower, *cell))
            .collect();
        // `enemy_unit_value_with` never returns our own units — the loop walks
        // `turn.enemy` — so our own wall line is not a candidate here even when
        // a friendly wall is the closest thing in range.
        let score = enemy_unit_score_with(&ranking, turn, tower, unit, &cells, sim);
        if score > 0 {
            ranked.push((score, unit.id, cells));
        }
    }
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(
            a.2.first()
                .map(|p| (p.x, p.y))
                .cmp(&b.2.first().map(|p| (p.x, p.y))),
        )
    });

    let mut targets: Vec<Pos> = Vec::new();
    for (_, unit_id, cells) in ranked {
        if targets.len() >= projectiles {
            break;
        }
        if !turn.enemy.iter().any(|unit| unit.id == unit_id) {
            continue;
        }
        // Breach commit: a wall owns ONE cell, so the "unspent projectiles move
        // down the ranking" spill below would hand the rest of the volley to
        // the runner-up and leave the wall standing at half damage — a scratch,
        // not a breach (`a_wall_is_not_worth_a_volley_that_barely_scratches_it`
        // is the same judgement). When it was the breach gate that put a wall at
        // the top of this ranking, the whole volley goes into that cell;
        // `pad_targets` repeats it out to `projectiles`. Outside breach mode a
        // wall still spills like any other building.
        if breach
            && turn
                .enemy
                .iter()
                .any(|unit| unit.id == unit_id && unit.kind == UnitKind::Wall)
        {
            targets.push(cells[0]);
            sim.damage_building(unit_id, tower_volley_damage(tower));
            break;
        }
        let mut committed = 0i64;
        for cell in &cells {
            if targets.len() >= projectiles {
                break;
            }
            let mut trial = targets.clone();
            trial.push(*cell);
            if tower.kind == UnitKind::Gatling && !within_cone(tower.pos, &trial) {
                continue;
            }
            targets.push(*cell);
            committed += tower_shot_damage(tower);
        }
        sim.damage_building(unit_id, committed);
    }
    if targets.is_empty() {
        return None;
    }
    pad_targets(tower, &mut targets, projectiles);
    Some(targets)
}

/// Best 3×3 bomb impact: max simulated damage (100 per cell) + kill bonuses.
pub fn bomb_impact(turn: &Turn) -> Option<Pos> {
    let robots: Vec<&Robot> = turn
        .robots
        .iter()
        .filter(|robot| robot.health > 0)
        .collect();
    if robots.is_empty() {
        return None;
    }
    let mut best: Option<(i64, Pos)> = None;
    for robot in &robots {
        let impact = robot.pos;
        let mut value = 0i64;
        for victim in &robots {
            if chebyshev(impact, victim.pos) > 1 {
                continue;
            }
            value += hit_value(turn, victim, victim.health, 100);
        }
        if best.map(|(v, _)| value > v).unwrap_or(true) {
            best = Some((value, impact));
        }
    }
    best.map(|(_, pos)| pos)
}

/// Best dizzy impact: robots targeting us, weighted by proximity to our base.
pub fn dizzy_impact(turn: &Turn) -> Option<Pos> {
    let robots: Vec<&Robot> = turn
        .robots
        .iter()
        .filter(|robot| robot.health > 0 && !robot.dizzy)
        .collect();
    if robots.is_empty() {
        return None;
    }
    let mut best: Option<(i64, Pos)> = None;
    for robot in &robots {
        let impact = robot.pos;
        let mut value = 0i64;
        for victim in &robots {
            if chebyshev(impact, victim.pos) > 1 {
                continue;
            }
            value += victim.kind.score() * 5 + threat(turn, victim);
        }
        if best.map(|(v, _)| value > v).unwrap_or(true) {
            best = Some((value, impact));
        }
    }
    // Only worth it against a real cluster or an imminent threat.
    best.filter(|(value, _)| *value >= 40).map(|(_, pos)| pos)
}

/// Is a robot kind worth panicking about? Used by night item logic.
pub fn is_big_threat(kind: RobotKind) -> bool {
    matches!(kind, RobotKind::Large | RobotKind::Boss)
}

/// A robot this close to a tower is a direct danger to it even when it is not
/// (yet) marching on our base.
const LOCAL_PRESSURE_RADIUS: i32 = 5;

/// Total threat a tower can currently engage — used to decide which tower
/// fires first in the shared damage simulation and which tower gets first pick
/// of a controller.
///
/// Mere range coverage is NOT pressure: a level-3 rocket covers the whole map,
/// so counting every robot it could reach made the rocket the top-priority
/// tower on every board and starved the close-range guns that actually stop a
/// wave. Only robots that are a real danger (marching on us, or already next to
/// this tower) count.
pub fn threat_load(turn: &Turn, tower: &Unit) -> i64 {
    turn.robots
        .iter()
        .filter(|robot| robot.health > 0 && in_range(tower, robot.pos))
        .map(|robot| {
            let marching = threat(turn, robot);
            if marching > 0 {
                marching + robot.kind.score()
            } else if chebyshev(tower.pos, robot.pos) <= LOCAL_PRESSURE_RADIUS {
                robot.kind.score()
            } else {
                0
            }
        })
        .sum()
}

/// Full HP of a wall at the given level (for repair decisions).
pub fn wall_max_hp(level: i32) -> i64 {
    match level.max(1).min(3) {
        1 => 1000,
        2 => 1500,
        _ => 2000,
    }
}

/// A damaged wall adjacent to `role` worth repairing with a WallFixer, and
/// safe to approach (no robot within `safe_radius`).
///
/// DAY ONLY. `safe_radius` is measured from the WALL, and a wall under attack
/// is by definition one with robots beside it — so at night this predicate is
/// false for exactly the walls the night is about to lose. [`night_mend_target`]
/// is the night's version: it measures the danger against the OPERATOR, which is
/// the thing that can actually be killed.
pub fn repair_target(turn: &Turn, role: &Unit, safe_radius: i32) -> Option<Pos> {
    if role.count_item("WallFixer") == 0 {
        return None;
    }
    let robots_near = |pos: Pos| {
        turn.robots
            .iter()
            .any(|robot| robot.health > 0 && chebyshev(robot.pos, pos) < safe_radius)
    };
    turn.walls()
        .iter()
        .filter(|wall| wall.health < wall_max_hp(wall.level))
        .filter(|wall| chebyshev(role.pos, wall.pos) <= 1)
        .filter(|wall| !robots_near(wall.pos))
        .map(|wall| wall.pos)
        .min_by_key(|pos| {
            turn.walls()
                .iter()
                .find(|wall| wall.pos == *pos)
                .map(|wall| wall.health)
                .unwrap_or(i64::MAX)
        })
}

/// The wall a NIGHT mender should patch this round, or `None`.
///
/// Why the night needs its own rule rather than [`repair_target`]: issues
/// #131-#135 each lost the base on night 2 or 3 (`ourWallLost` 2465-15135 a
/// night) while the ring was never restored — the `safe_radius = 3` call the
/// spare duty made requires no robot within two cells of the WALL, and a wall
/// the wave is chewing on has robots beside it by construction. Every mend the
/// night did perform came from `cooldown_repair`, which needs a paired
/// controller standing beside a wall on a reload round AND a `WallFixer` in its
/// pack — and 表 2a of those five matches shows the first kit bought on day 2 at
/// round 147-154, i.e. after the night that mattered. Net: with no kit on the
/// board before night 2 and the day rule refusing every wall under fire, **no
/// wall HP could be restored on night 1 in any of the five.**
///
/// A `WallFixer` restores its target to FULL for 10 gold (`wall_max_hp` 1000 /
/// 1500 / 2000) — the cheapest HP on the board by a wide margin. So the safety
/// test is the one that can still be satisfied while the wall is under fire: no
/// live robot ADJACENT to the operator (it is not being meleed), the same
/// predicate `cooldown_repair` already uses for a paired controller. A wall
/// being chewed from two or three cells out is exactly the wall worth mending.
pub fn night_mend_target(turn: &Turn, role: &Unit) -> Option<Pos> {
    if role.count_item("WallFixer") == 0 {
        return None;
    }
    let meleed = turn
        .robots
        .iter()
        .any(|robot| robot.health > 0 && chebyshev(robot.pos, role.pos) <= 1);
    if meleed {
        return None;
    }
    turn.walls()
        .into_iter()
        .filter(|wall| chebyshev(role.pos, wall.pos) == 1)
        .filter(|wall| wall.health < wall_max_hp(wall.level))
        .min_by_key(|wall| (wall.health, wall.pos.x, wall.pos.y))
        .map(|wall| wall.pos)
}
