//! Weapon ballistics models and target selection.
//!
//! Gatling: `level` bullets, every target pair must sit inside one 90° cone;
//! each bullet damages the first robot along its trajectory (10 dmg).
//! Railgun: single target, energy pierces along the line, each robot takes
//! min(remaining energy, current hp).
//! Rocket: `level` missiles, 20 center + 10 splash (8 neighbours = half of
//! center), impacts stack, 3-round cooldown (checked by the caller).
//!
//! All three consult a shared per-round HP simulation (`SimHp`) so towers
//! processed later do not overkill robots an earlier tower already claimed.
//! ASSUMPTION (see 需求分析 Q5): trajectories are intercepted by ROBOTS only;
//! walls and buildings do not block bullet/rail paths.

use std::collections::HashMap;

use crate::model::{chebyshev, footprint_distance, station_footprint, Unit, UnitKind, Turn, Robot, RobotKind, FULL_MAP_RANGE};
use crate::protocol::Pos;

const KILL_BONUS_PER_SCORE: i64 = 20;

/// Robot id → HP remaining in this round's firing simulation.
pub type SimHp = HashMap<i64, i64>;

pub fn init_sim(turn: &Turn) -> SimHp {
    turn.robots
        .iter()
        .filter(|robot| robot.health > 0)
        .map(|robot| (robot.id, robot.health))
        .collect()
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

/// Expected value of `dmg` damage on `robot`.
fn hit_value(turn: &Turn, robot: &Robot, hp_before: i64, dmg: i64) -> i64 {
    let mut value = dmg.min(hp_before) * 2;
    let threat = threat(turn, robot);
    if hp_before - dmg <= 0 {
        value += robot.kind.score() * KILL_BONUS_PER_SCORE + threat;
    } else {
        value += threat / 4;
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

/// Choose attack target positions for a ready tower, degrading `sim` with the
/// damage this tower is expected to deal. Returns None when nothing to shoot.
pub fn choose_attack(turn: &Turn, tower: &Unit, sim: &mut SimHp) -> Option<Vec<Pos>> {
    let projectiles = crate::model::tower_projectiles(tower.kind, tower.level.max(1)) as usize;
    if projectiles == 0 {
        return None;
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
            return Some(targets);
        }
    }
    // Fallback: opportunistic shots at visible enemy units (destroying the
    // enemy station wins the half). Prefer the enemy station footprint.
    choose_enemy_targets(turn, tower, projectiles)
}

fn robot_at<'a>(robots: &[&'a Robot], pos: Pos) -> Option<&'a Robot> {
    robots.iter().copied().find(|robot| robot.pos == pos)
}

/// First robot (by simulated HP > 0) along the trajectory tower→target.
fn first_on_line<'r>(robots: &[&'r Robot], sim: &SimHp, from: Pos, to: Pos) -> Option<&'r Robot> {
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
    sim: &mut SimHp,
) -> Option<Vec<Pos>> {
    let candidates: Vec<&&Robot> = robots.iter().filter(|robot| in_range(tower, robot.pos)).collect();
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
            let Some(victim) = first_on_line(robots, sim, tower.pos, robot.pos) else { continue };
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
                    let hp = sim.get(&victim.id).copied().unwrap_or(0);
                    sim.insert(victim.id, (hp - 10).max(0));
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

fn choose_railgun(turn: &Turn, tower: &Unit, robots: &[&Robot], sim: &mut SimHp) -> Option<Vec<Pos>> {
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
            let Some(victim) = robot_at(robots, cell) else { continue };
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
        let hp = sim.get(&id).copied().unwrap_or(0);
        sim.insert(id, (hp - dmg).max(0));
    }
    Some(vec![target])
}

fn choose_rocket(
    turn: &Turn,
    tower: &Unit,
    robots: &[&Robot],
    missiles: usize,
    sim: &mut SimHp,
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

fn splash_value(turn: &Turn, impact: Pos, robots: &[&Robot], sim: &SimHp) -> i64 {
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

fn apply_splash(impact: Pos, robots: &[&Robot], sim: &mut SimHp) {
    for robot in robots {
        let d = chebyshev(impact, robot.pos);
        if d > 1 {
            continue;
        }
        let raw = if d == 0 { 20 } else { 10 };
        let hp = sim.get(&robot.id).copied().unwrap_or(0);
        sim.insert(robot.id, (hp - raw).max(0));
    }
}

fn choose_enemy_targets(turn: &Turn, tower: &Unit, projectiles: usize) -> Option<Vec<Pos>> {
    let mut units: Vec<&Unit> = turn
        .enemy
        .iter()
        .filter(|unit| unit.alive() && unit.footprint().iter().any(|cell| in_range(tower, *cell)))
        .collect();
    // With no robot in range, disable the opponent's firepower before spending
    // shots on walls. Visible humans come first because they operate every
    // weapon; towers follow, then the station and finally walls.
    units.sort_by_key(|unit| match unit.kind {
        UnitKind::Pioneer | UnitKind::Worker => 0,
        UnitKind::Gatling | UnitKind::Railgun | UnitKind::Rocket => 1,
        UnitKind::Station => 2,
        UnitKind::Wall => 3,
        UnitKind::Unknown => 4,
    });
    let mut targets: Vec<Pos> = Vec::new();
    for unit in units {
        for cell in unit.footprint() {
            if in_range(tower, cell) {
                targets.push(cell);
            }
        }
    }
    if targets.is_empty() {
        return None;
    }
    targets.truncate(projectiles.max(1));
    while targets.len() < projectiles {
        let last = *targets.last().unwrap();
        targets.push(last);
    }
    // Cone legality for gatling.
    if tower.kind == UnitKind::Gatling && !within_cone(tower.pos, &targets) {
        targets.truncate(1);
        while targets.len() < projectiles {
            let last = *targets.last().unwrap();
            targets.push(last);
        }
    }
    Some(targets)
}

/// Best 3×3 bomb impact: max simulated damage (100 per cell) + kill bonuses.
pub fn bomb_impact(turn: &Turn) -> Option<Pos> {
    let robots: Vec<&Robot> = turn.robots.iter().filter(|robot| robot.health > 0).collect();
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

/// Total threat a tower can currently engage — used to decide which tower
/// fires first in the shared damage simulation.
pub fn threat_load(turn: &Turn, tower: &Unit) -> i64 {
    turn.robots
        .iter()
        .filter(|robot| robot.health > 0 && in_range(tower, robot.pos))
        .map(|robot| threat(turn, robot) + robot.kind.score())
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
