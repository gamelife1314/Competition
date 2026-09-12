//! Command sanitizer: the judger counts malformed/illegal commands as team
//! anomalies (5 strikes → disqualified). Execution failures (collision, no
//! target at the impact point) are safe, but *structural* violations are not.
//! Anything questionable is dropped: doing nothing beats a strike.

use std::collections::{BTreeMap, HashMap};

use crate::brain::combat;
use crate::model::{chebyshev, UnitKind, Turn};
use crate::protocol::{Pos, RoleCommand};

pub fn sanitize(turn: &Turn, commands: HashMap<i64, RoleCommand>) -> BTreeMap<String, RoleCommand> {
    let mut out = BTreeMap::new();
    let mut remaining_gold = turn.gold;
    let mut weapon_count = turn.towers().len();
    let mut ordered: Vec<(i64, RoleCommand)> = commands.into_iter().collect();
    ordered.sort_by_key(|(id, _)| *id);

    for (id, cmd) in ordered {
        let Some(cmd) = sanitize_one(turn, id, &cmd) else { continue };
        let cost = match cmd.action.as_str() {
            "buy" => {
                let Some(name) = cmd.name.as_ref() else { continue };
                let price = turn.weapon_shop.get(name).copied().unwrap_or(i64::MAX);
                price.saturating_mul(cmd.num.unwrap_or(1))
            }
            "build" if cmd.name.as_deref() != Some("wall") => {
                if weapon_count >= 3 {
                    continue;
                }
                crate::model::WEAPON_BUILD_COST
            }
            _ => 0,
        };
        if cost > remaining_gold {
            continue;
        }
        remaining_gold -= cost;
        if cmd.action == "build" && cmd.name.as_deref() != Some("wall") {
            weapon_count += 1;
        }
        out.insert(id.to_string(), cmd);
    }
    out
}

fn sanitize_one(turn: &Turn, id: i64, cmd: &RoleCommand) -> Option<RoleCommand> {
    // The command key must be one of our living units.
    let actor = turn.role_by_id(id)?;
    if !actor.alive() {
        return None;
    }
    let action = cmd.action.as_str();

    // Structural requirements per action.
    match action {
        "move" => {
            if !actor.kind.is_controllable() {
                return None;
            }
            let target = single_target(cmd)?;
            if !turn.in_bounds(target) || chebyshev(actor.pos, target) != 1 {
                return None;
            }
        }
        "attack" => {
            if turn.is_day || !actor.kind.is_tower() || actor.cooldown > 0 {
                return None;
            }
            let controller_id = cmd.controllerId.as_ref()?.parse::<i64>().ok()?;
            let controller = turn.role_by_id(controller_id)?;
            if !controller.kind.is_controllable() || !controller.alive() {
                return None;
            }
            if chebyshev(controller.pos, actor.pos) > 1 {
                return None;
            }
            let targets = cmd.targetPos.as_ref()?;
            if targets.is_empty() {
                return None;
            }
            let projectiles = crate::model::tower_projectiles(actor.kind, actor.level.max(1)) as usize;
            if actor.kind == UnitKind::Railgun && targets.len() != 1 {
                return None;
            }
            if targets.len() != projectiles {
                return None;
            }
            let range = actor.range_of_attack();
            for target in targets {
                if !turn.in_bounds(*target) {
                    return None;
                }
                if chebyshev(actor.pos, *target) > range {
                    return None;
                }
            }
            // Gatling: all targets must fit inside one 90° cone.
            if actor.kind == UnitKind::Gatling && !combat::within_cone(actor.pos, targets) {
                return None;
            }
        }
        "sell" => {
            if !actor.kind.is_controllable() {
                return None;
            }
            let name = cmd.name.as_ref()?;
            if !crate::model::ORES.contains(&name.as_str()) {
                return None;
            }
            let num = cmd.num.unwrap_or(1);
            if num <= 0 || actor.count_item(name) < num as usize {
                return None;
            }
        }
        "buy" => {
            if !actor.kind.is_controllable() {
                return None;
            }
            let name = cmd.name.as_ref()?;
            let num = cmd.num.unwrap_or(1);
            if num <= 0 || !turn.weapon_shop.contains_key(name) {
                return None;
            }
            let price = turn.weapon_shop.get(name).copied().unwrap_or(i64::MAX);
            let free_slots = actor.capacity.saturating_sub(actor.backpack.len() as i64);
            if price.saturating_mul(num) > turn.gold || num > free_slots {
                return None;
            }
        }
        "build" => {
            if actor.kind != UnitKind::Worker || !turn.is_day {
                return None;
            }
            let target = single_target(cmd)?;
            let name = cmd.name.as_ref()?;
            if !turn.in_bounds(target) || chebyshev(actor.pos, target) != 1 {
                return None;
            }
            if !turn.is_land(target) {
                return None;
            }
            match name.as_str() {
                "wall" => {
                    if actor.count_item(crate::model::STONE) < 1 {
                        return None;
                    }
                }
                "gatling" | "railgun" | "rocket" => {
                    if turn.gold < crate::model::WEAPON_BUILD_COST || turn.towers().len() >= 3 {
                        return None;
                    }
                    // Rebuilding on an occupied weapon cell silently destroys
                    // the existing tower. Tactical planners must choose a free
                    // slot; the validator makes that invariant fail-safe.
                    if turn.towers().iter().any(|tower| tower.pos == target) {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        "remove" => {
            if actor.kind != UnitKind::Worker {
                return None;
            }
            let target = single_target(cmd)?;
            if chebyshev(actor.pos, target) != 1 {
                return None;
            }
            // Must actually be one of our walls.
            if !turn
                .walls()
                .iter()
                .any(|wall| wall.pos == target)
            {
                return None;
            }
        }
        "acceptTask" => {
            if actor.kind != UnitKind::Pioneer {
                return None;
            }
        }
        "submitAnswer" => {
            if actor.kind != UnitKind::Pioneer {
                return None;
            }
            let answer = cmd.taskAnswer.as_ref()?;
            if answer.trim().is_empty() {
                return None;
            }
        }
        "summonTreasure" => {
            if actor.kind != UnitKind::Pioneer {
                return None;
            }
            let target = single_target(cmd)?;
            if chebyshev(actor.pos, target) != 1 {
                return None;
            }
            let items = cmd.item.as_ref()?;
            if items.is_empty() {
                return None;
            }
            // Items are a multiset: the backpack must hold the FULL count of
            // each distinct name (a legal summon consumes them all).
            for item in items {
                let required = items.iter().filter(|other| *other == item).count();
                if actor.count_item(item) < required {
                    return None;
                }
            }
        }
        "use" => {
            if !actor.kind.is_controllable() {
                return None;
            }
            let name = cmd.name.as_ref()?;
            if actor.count_item(name) < 1 {
                return None;
            }
            match name.as_str() {
                // Items that require a target position within 1 cell.
                "WallFixer" => {
                    let target = single_target(cmd)?;
                    if chebyshev(actor.pos, target) != 1 {
                        return None;
                    }
                }
                // Upgrade vouchers: target must be our matching building at
                // the right level, within 1 cell.
                "WeaponUpgradeVoucher1" | "WeaponUpgradeVoucher2" | "StationUpgradeVoucher1"
                | "StationUpgradeVoucher2" | "WallUpgradeVoucher1" | "WallUpgradeVoucher2" => {
                    let target = single_target(cmd)?;
                    if chebyshev(actor.pos, target) != 1 {
                        return None;
                    }
                    if !voucher_target_ok(turn, name, target) {
                        return None;
                    }
                }
                "DizzyWeapon" | "Bomb" => {
                    let target = single_target(cmd)?;
                    if !turn.in_bounds(target) {
                        return None;
                    }
                }
                // Medicine, summon orders and task items need no position.
                _ => {}
            }
        }
        "drop" => {
            if !actor.kind.is_controllable() {
                return None;
            }
            let name = cmd.name.as_ref()?;
            if actor.count_item(name) < 1 {
                return None;
            }
        }
        "collect" => {
            if actor.kind != UnitKind::Worker {
                return None;
            }
            let target = single_target(cmd)?;
            if chebyshev(actor.pos, target) != 1 {
                return None;
            }
            match turn.zones.get(&target).map(String::as_str) {
                Some("stone") | Some("iron") | Some("copper") => {}
                _ => return None,
            }
        }
        _ => return None, // unknown action code → would be an anomaly
    }
    Some(cmd.clone())
}

fn single_target(cmd: &RoleCommand) -> Option<Pos> {
    let targets = cmd.targetPos.as_ref()?;
    if targets.len() != 1 {
        return None;
    }
    Some(targets[0])
}

fn voucher_target_ok(turn: &Turn, voucher: &str, target: Pos) -> bool {
    let want_level = if voucher.ends_with('1') { 1 } else { 2 };
    let target_unit = turn.ours.iter().find(|unit| {
        unit.footprint().contains(&target)
    });
    let Some(unit) = target_unit else { return false };
    if unit.level != want_level {
        return false;
    }
    match voucher {
        "WeaponUpgradeVoucher1" | "WeaponUpgradeVoucher2" => unit.kind.is_tower(),
        "StationUpgradeVoucher1" | "StationUpgradeVoucher2" => unit.kind == UnitKind::Station,
        "WallUpgradeVoucher1" | "WallUpgradeVoucher2" => unit.kind == UnitKind::Wall,
        _ => false,
    }
}
