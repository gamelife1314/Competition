//! Weapon ballistics models and target selection.
//!
//! Gatling: `level` bullets, every target pair must sit inside one 90° cone;
//! each bullet damages the first robot along its trajectory (10 dmg).
//! Railgun: single target, energy pierces along the line, each robot takes
//! min(remaining energy, current hp).
//! Rocket: `level` missiles, 20 center + 10 splash (8 neighbours), impacts
//! stack, 3-round cooldown.

use std::collections::HashMap;

use crate::model::{chebyshev, Unit, UnitKind, Turn, Robot, RobotKind, FULL_MAP_RANGE};
use crate::protocol::Pos;

const KILL_BONUS_PER_SCORE: i64 = 20;
const THREAT_BONUS: i64 = 8;

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

fn robot_hp_map<'a>(robots: &[&'a Robot]) -> HashMap<Pos, i64> {
    let mut map = HashMap::new();
    for robot in robots {
        map.insert(robot.pos, robot.health);
    }
    map
}

fn kill_bonus(robot: &Robot, hp_after: i64, our_team: &str) -> i64 {
    let mut bonus = 0;
    if hp_after <= 0 {
        bonus += robot.kind.score() * KILL_BONUS_PER_SCORE;
    }
    if robot.target_team == our_team {
        bonus += THREAT_BONUS;
    }
    bonus
}

fn in_range(tower: &Unit, pos: Pos) -> bool {
    let range = tower.range_of_attack();
    range >= FULL_MAP_RANGE || chebyshev(tower.pos, pos) <= range
}

/// Choose attack target positions for a ready tower. Returns None when
/// nothing is worth shooting at.
pub fn choose_attack(turn: &Turn, tower: &Unit) -> Option<Vec<Pos>> {
    let projectiles = crate::model::tower_projectiles(tower.kind, tower.level.max(1)) as usize;
    if projectiles == 0 {
        return None;
    }
    let robots: Vec<&Robot> = turn.robots.iter().filter(|robot| robot.health > 0).collect();
    let targets = match tower.kind {
        UnitKind::Gatling => choose_gatling(turn, tower, &robots, projectiles),
        UnitKind::Railgun => choose_railgun(turn, tower, &robots),
        UnitKind::Rocket => choose_rocket(turn, tower, &robots, projectiles),
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

fn choose_gatling(turn: &Turn, tower: &Unit, robots: &[&Robot], bullets: usize) -> Option<Vec<Pos>> {
    let mut sim = robot_hp_map(robots);
    let candidates: Vec<&&Robot> = robots.iter().filter(|robot| in_range(tower, robot.pos)).collect();
    if candidates.is_empty() {
        return None;
    }
    let mut chosen: Vec<Pos> = Vec::new();
    while chosen.len() < bullets {
        let mut best: Option<(i64, Pos)> = None;
        for robot in &candidates {
            if !within_cone(tower.pos, &[&chosen[..], &[robot.pos]].concat()) {
                continue;
            }
            // Bullet hits the first robot along the trajectory.
            let hit = first_robot_on_line(tower.pos, robot.pos, &sim);
            let Some((hit_pos, hp)) = hit else { continue };
            let dmg = 10.min(hp);
            let mut value = dmg * 3;
            if hp - dmg <= 0 {
                if let Some(victim) = robots.iter().find(|r| r.pos == hit_pos) {
                    value += kill_bonus(victim, hp - dmg, &turn.team_type);
                }
            }
            if best.map(|(v, _)| value > v).unwrap_or(true) {
                best = Some((value, robot.pos));
            }
        }
        match best {
            Some((_, pos)) => {
                // Apply simulated damage for subsequent picks.
                if let Some((hit_pos, hp)) = first_robot_on_line(tower.pos, pos, &sim) {
                    let dmg = 10.min(hp);
                    sim.insert(hit_pos, (hp - dmg).max(0));
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

fn first_robot_on_line(from: Pos, to: Pos, sim: &HashMap<Pos, i64>) -> Option<(Pos, i64)> {
    for cell in line_cells(from, to) {
        if cell == from {
            continue;
        }
        if let Some(hp) = sim.get(&cell) {
            if *hp > 0 {
                return Some((cell, *hp));
            }
        }
    }
    None
}

fn choose_railgun(turn: &Turn, tower: &Unit, robots: &[&Robot]) -> Option<Vec<Pos>> {
    let energy0 = if tower.attack_power > 0 {
        tower.attack_power
    } else {
        match tower.level.max(1) {
            1 => 10,
            2 => 20,
            _ => 30,
        }
    };
    let by_pos: HashMap<Pos, &&Robot> =
        robots.iter().map(|robot| (robot.pos, robot)).collect();
    let mut best: Option<(i64, i32, Pos)> = None;
    for robot in robots {
        if !in_range(tower, robot.pos) {
            continue;
        }
        let mut energy = energy0;
        let mut value = 0i64;
        for cell in line_cells(tower.pos, robot.pos) {
            if cell == tower.pos {
                continue;
            }
            if let Some(victim) = by_pos.get(&cell) {
                if energy <= 0 {
                    break;
                }
                let hp = victim.health;
                let dmg = energy.min(hp);
                energy -= dmg;
                value += dmg * 2;
                if hp - dmg <= 0 {
                    value += kill_bonus(victim, hp - dmg, &turn.team_type);
                }
            }
        }
        if value <= 0 {
            continue;
        }
        let dist = chebyshev(tower.pos, robot.pos);
        let better = match best {
            Some((bv, bd, _)) => value > bv || (value == bv && dist < bd),
            None => true,
        };
        if better {
            best = Some((value, dist, robot.pos));
        }
    }
    best.map(|(_, _, pos)| vec![pos])
}

fn choose_rocket(turn: &Turn, tower: &Unit, robots: &[&Robot], missiles: usize) -> Option<Vec<Pos>> {
    let mut sim = robot_hp_map(robots);
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
            let value = splash_value(turn, *impact, &sim, robots);
            if best.map(|(v, _)| value > v).unwrap_or(true) {
                best = Some((value, *impact));
            }
        }
        match best {
            Some((value, pos)) => {
                if value <= 0 && !impacts.is_empty() {
                    break;
                }
                apply_splash(pos, &mut sim);
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

fn splash_value(turn: &Turn, impact: Pos, sim: &HashMap<Pos, i64>, robots: &[&Robot]) -> i64 {
    let mut value = 0i64;
    for robot in robots {
        let d = chebyshev(impact, robot.pos);
        if d > 1 {
            continue;
        }
        let raw = if d == 0 { 20 } else { 10 };
        let hp = sim.get(&robot.pos).copied().unwrap_or(0);
        if hp <= 0 {
            continue;
        }
        let dmg = raw.min(hp);
        value += dmg * 2;
        if hp - dmg <= 0 {
            value += kill_bonus(robot, hp - dmg, &turn.team_type);
        }
    }
    value
}

fn apply_splash(impact: Pos, sim: &mut HashMap<Pos, i64>) {
    for (pos, hp) in sim.iter_mut() {
        let d = chebyshev(impact, *pos);
        if d > 1 {
            continue;
        }
        let raw = if d == 0 { 20 } else { 10 };
        *hp = (*hp - raw).max(0);
    }
}

fn choose_enemy_targets(turn: &Turn, tower: &Unit, projectiles: usize) -> Option<Vec<Pos>> {
    let mut targets: Vec<Pos> = Vec::new();
    // Enemy station first (2x2 footprint cells), then other visible units.
    for unit in turn.enemy.iter().filter(|unit| unit.kind == UnitKind::Station) {
        for cell in unit.footprint() {
            if in_range(tower, cell) {
                targets.push(cell);
            }
        }
    }
    for unit in turn.enemy.iter().filter(|unit| {
        unit.kind != UnitKind::Station && unit.alive() && in_range(tower, unit.pos)
    }) {
        targets.push(unit.pos);
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
            let dmg = 100.min(victim.health);
            value += dmg * 2;
            if victim.health - dmg <= 0 {
                value += kill_bonus(victim, victim.health - dmg, &turn.team_type);
            }
        }
        if best.map(|(v, _)| value > v).unwrap_or(true) {
            best = Some((value, impact));
        }
    }
    best.map(|(_, pos)| pos)
}

/// Best dizzy impact: robots targeting us, weighted by proximity to our base.
pub fn dizzy_impact(turn: &Turn) -> Option<Pos> {
    let base = turn.station().map(|station| station.pos);
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
            value += victim.kind.score() * 5;
            if victim.target_team == turn.team_type {
                value += 15;
                if let Some(base) = base {
                    let d = chebyshev(base, victim.pos);
                    value += ((12 - d).max(0) * 3) as i64;
                }
            }
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
