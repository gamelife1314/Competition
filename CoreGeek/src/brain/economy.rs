//! Economy: shopping lists, selling, voucher usage, mine selection.

use std::collections::HashSet;

use crate::model::{chebyshev, Unit, UnitKind, Turn, ORES, STONE};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

pub const SELL_BATCH: i64 = 20;
pub const STONE_BUFFER: i64 = 2;

#[derive(Debug, Clone)]
pub struct Need {
    pub name: String,
    pub num: i64,
    pub priority: i32, // lower = more urgent
}

pub fn stock_of(turn: &Turn, name: &str) -> i64 {
    turn.controllable()
        .iter()
        .map(|role| role.count_item(name) as i64)
        .sum()
}

fn total_ores(role: &Unit) -> i64 {
    ORES.iter().map(|ore| role.count_item(ore) as i64).sum()
}

pub fn team_ores(turn: &Turn, ore: &str) -> i64 {
    turn.controllable().iter().map(|role| role.count_item(ore) as i64).sum()
}

/// What we should buy right now, ordered by priority and filtered by gold.
/// `reserve` is gold set aside for tower builds (25/each) — spending it on
/// consumables would stall the defense build-out.
pub fn shopping_list(turn: &Turn, state: &BotState, reserve: i64) -> Vec<Need> {
    let mut needs: Vec<Need> = Vec::new();
    let gold = (turn.gold - reserve).max(0);

    // Weapon upgrade vouchers: biggest defensive win per gold.
    for tower in turn.towers() {
        let (voucher, ok) = match tower.level {
            1 => ("WeaponUpgradeVoucher1", true),
            2 => ("WeaponUpgradeVoucher2", true),
            _ => ("", false),
        };
        if ok && stock_of(turn, voucher) == 0 {
            needs.push(Need { name: voucher.into(), num: 1, priority: 1 });
        }
    }
    // Station upgrade: survival score.
    if let Some(station) = turn.station() {
        let voucher = match station.level {
            1 => "StationUpgradeVoucher1",
            2 => "StationUpgradeVoucher2",
            _ => "",
        };
        if !voucher.is_empty() && stock_of(turn, voucher) == 0 {
            needs.push(Need { name: voucher.into(), num: 1, priority: 2 });
        }
    }
    // Night consumables: stock up once upgrades are affordable.
    if stock_of(turn, "Bomb") < 2 && gold >= 150 {
        needs.push(Need {
            name: "Bomb".into(),
            num: (2 - stock_of(turn, "Bomb")).min(gold / 100),
            priority: 3,
        });
    }
    if stock_of(turn, "DizzyWeapon") < 1 && gold >= 150 {
        needs.push(Need { name: "DizzyWeapon".into(), num: 1, priority: 3 });
    }
    // Wall repair kits when the wall line took damage overnight (10g each).
    let damaged_walls = turn
        .walls()
        .iter()
        .filter(|wall| wall.health < crate::brain::combat::wall_max_hp(wall.level))
        .count() as i64;
    if damaged_walls > 0 && stock_of(turn, "WallFixer") < damaged_walls.min(4) {
        needs.push(Need {
            name: "WallFixer".into(),
            num: (damaged_walls.min(4) - stock_of(turn, "WallFixer")).min(gold / 10),
            priority: 3,
        });
    }
    // Medicine only when someone is actually hurt.
    let injured = turn.controllable().iter().any(|role| {
        let max_hp = if role.kind == UnitKind::Worker { 220 } else { 200 };
        role.health * 10 < max_hp * 8
    });
    if injured && stock_of(turn, "Medicine") < 1 {
        needs.push(Need { name: "Medicine".into(), num: 1, priority: 2 });
    }
    // (Treasure sacrifice items are bought exclusively by the pioneer inside
    // treasure.rs — summonTreasure requires the items in the pioneer's pack.)
    // Harassment: boss wave on the enemy, but only once our own defense
    // stands (all three towers) and we are rich.
    if gold >= 500
        && turn.towers().len() >= 3
        && !state.harass_done_today
        && state.summon_orders_today < 10
    {
        needs.push(Need { name: "BossRobotSummonOrder".into(), num: 1, priority: 9 });
    }

    // Gold filter, cheapest-first within priority is unnecessary: keep order,
    // accumulate cost.
    let mut remaining = gold;
    let mut out = Vec::new();
    needs.sort_by_key(|need| need.priority);
    for need in needs {
        if need.num <= 0 {
            continue;
        }
        let price = turn.weapon_shop.get(&need.name).copied().unwrap_or(i64::MAX);
        let cost = price.saturating_mul(need.num);
        if cost <= remaining && price != i64::MAX {
            remaining -= cost;
            out.push(need);
        }
    }
    out
}

/// Find a building the voucher in `role`'s backpack can upgrade.
pub fn voucher_target(turn: &Turn, role: &Unit, voucher: &str) -> Option<Pos> {
    let want_level = if voucher.ends_with('1') { 1 } else { 2 };
    let kind_ok = |unit: &Unit| -> bool {
        match voucher {
            "WeaponUpgradeVoucher1" | "WeaponUpgradeVoucher2" => unit.kind.is_tower(),
            "StationUpgradeVoucher1" | "StationUpgradeVoucher2" => unit.kind == UnitKind::Station,
            "WallUpgradeVoucher1" | "WallUpgradeVoucher2" => unit.kind == UnitKind::Wall,
            _ => false,
        }
    };
    turn.ours
        .iter()
        .filter(|unit| unit.alive() && kind_ok(unit) && unit.level == want_level)
        .map(|unit| unit.pos)
        .filter(|pos| chebyshev(role.pos, *pos) >= 0)
        .min_by_key(|pos| chebyshev(role.pos, *pos))
}

pub fn held_vouchers(role: &Unit) -> Vec<String> {
    const VOUCHERS: [&str; 6] = [
        "WeaponUpgradeVoucher1",
        "WeaponUpgradeVoucher2",
        "StationUpgradeVoucher1",
        "StationUpgradeVoucher2",
        "WallUpgradeVoucher1",
        "WallUpgradeVoucher2",
    ];
    VOUCHERS
        .iter()
        .filter(|voucher| role.count_item(voucher) > 0)
        .map(|voucher| voucher.to_string())
        .collect()
}

/// Should this role head to the vendor?
pub fn should_sell(turn: &Turn, state: &BotState, role: &Unit, stone_demand: i64) -> bool {
    let ores = total_ores(role);
    if ores == 0 {
        return false;
    }
    if role.backpack_full() || ores >= SELL_BATCH {
        return true;
    }
    // Surplus stones: the wall line is complete, convert dead weight to gold.
    if stone_demand <= 0 && role.count_item(STONE) >= 8 {
        return true;
    }
    // Price spike: sell the spiked ore immediately (>= 2x baseline).
    for ore in ORES {
        let count = role.count_item(ore) as i64;
        if count <= 0 {
            continue;
        }
        if ore == STONE && team_ores(turn, STONE) <= stone_demand + STONE_BUFFER {
            continue;
        }
        let base = state.base_prices.get(ore).copied().unwrap_or(1).max(1);
        let now = turn.vendor_prices.get(ore).copied().unwrap_or(base);
        if now >= base * 2 && count >= 5 {
            return true;
        }
    }
    false
}

/// Sell the most valuable stack (one ore kind per round). Stones are held
/// back while wall demand is unmet.
pub fn sell_command(turn: &Turn, state: &BotState, role: &Unit, stone_demand: i64) -> Option<RoleCommand> {
    let mut best: Option<(i64, &str, i64)> = None; // (value, ore, count)
    for ore in ORES {
        let count = role.count_item(ore) as i64;
        if count <= 0 {
            continue;
        }
        if ore == STONE && team_ores(turn, STONE) <= stone_demand + STONE_BUFFER {
            continue;
        }
        let base = state.base_prices.get(ore).copied().unwrap_or(1).max(1);
        let price = turn.vendor_prices.get(ore).copied().unwrap_or(base);
        let value = price * count + if price >= base * 2 { 50 } else { 0 };
        if best.map(|(v, _, _)| value > v).unwrap_or(true) {
            best = Some((value, ore, count));
        }
    }
    best.map(|(_, ore, count)| RoleCommand::sell(ore, count))
}

/// Pick a mine for this worker: stones first while wall demand unmet,
/// otherwise the highest-value ore that is not on outage.
pub fn choose_mine(
    turn: &Turn,
    state: &BotState,
    stone_demand: i64,
    claimed: &HashSet<Pos>,
) -> Option<(Pos, String)> {
    let mut options: Vec<(Pos, String)> = Vec::new();
    for (pos, ore) in turn.all_mines() {
        if state.ore_on_outage(&ore, turn.day) {
            continue;
        }
        if claimed.contains(&pos) {
            continue;
        }
        options.push((pos, ore));
    }
    if options.is_empty() {
        return None;
    }
    if stone_demand > 0 {
        // Deterministic pick (zones live in a HashMap): lowest coordinate
        // wins so workers do not flip-flop between equal stone mines.
        if let Some((pos, ore)) = options
            .iter()
            .filter(|(_pos, ore)| ore == STONE)
            .min_by_key(|(pos, _ore)| (pos.x, pos.y))
        {
            return Some((*pos, ore.clone()));
        }
    }
    // Highest vendor value first; coordinate tiebreak keeps it stable.
    options
        .into_iter()
        .max_by_key(|(pos, ore)| {
            let price = turn.vendor_prices.get(ore).copied().unwrap_or(1);
            (price, -(pos.x as i64 + pos.y as i64), -(pos.x as i64))
        })
}
