//! Economy: shopping lists, selling, voucher usage, mine selection.

use std::collections::HashSet;

use crate::model::{chebyshev, Unit, UnitKind, Turn, ORES, STONE, WEAPON_BUILD_COST};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// Sell a stack once this many ore accumulate — small enough that gold flows
/// every few rounds instead of sitting in a full backpack until dusk.
pub const SELL_BATCH: i64 = 4;
pub const STONE_BUFFER: i64 = 2;

/// First day round (in_day_round) of the "dusk" window: mining stops and every
/// ore is converted to gold so the night is spent upgrading, not digging.
pub const DUSK_ROUND: i64 = 55;

/// Gold kept out of new weapon builds so the main weapon's level-2 upgrade
/// (WeaponUpgradeVoucher1, 100g) is never starved by a third level-1 weapon.
pub const WEAPON_UPGRADE_RESERVE: i64 = 25;

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

/// Whether we may spend 25g building another weapon this round. Build 1-2
/// weapons first (a gold reserve must survive), then stop until the main
/// weapon reaches level 2 — a level-2 weapon out-values a third level-1
/// weapon, and the gold is better spent on WeaponUpgradeVoucher1.
pub fn may_build_weapon(turn: &Turn) -> bool {
    let towers = turn.towers();
    if towers.len() < 2 {
        return turn.gold >= WEAPON_BUILD_COST;
    }
    towers.iter().any(|tower| tower.level >= 2) && turn.gold >= WEAPON_BUILD_COST + WEAPON_UPGRADE_RESERVE
}

/// What we should buy right now, ordered by priority and filtered by gold.
/// `reserve` is gold set aside for tower builds (25/each) — spending it on
/// consumables would stall the defense build-out.
pub fn shopping_list(turn: &Turn, state: &BotState, reserve: i64) -> Vec<Need> {
    let mut needs: Vec<Need> = Vec::new();
    let gold = (turn.gold - reserve).max(0);

    // Weapon upgrade vouchers: biggest defensive win per gold. One at a time —
    // upgrade the main weapon first, then reassess.
    for voucher in ["WeaponUpgradeVoucher1", "WeaponUpgradeVoucher2"] {
        let want_level = if voucher.ends_with('1') { 1 } else { 2 };
        let need_count = turn
            .towers()
            .iter()
            .filter(|tower| tower.level == want_level)
            .count() as i64;
        let have_count = stock_of(turn, voucher);
        let deficit = (need_count - have_count).max(0).min(1);
        if deficit > 0 {
            needs.push(Need {
                name: voucher.into(),
                num: deficit,
                priority: 1,
            });
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
    // Night consumables: only once the defense is solid (every weapon slot
    // filled → no gold still reserved for builds) and gold is plentiful.
    let defense_solid = reserve == 0;
    if defense_solid && stock_of(turn, "Bomb") < 2 && gold >= 150 {
        needs.push(Need {
            name: "Bomb".into(),
            num: (2 - stock_of(turn, "Bomb")).min(gold / 100),
            priority: 3,
        });
    }
    if defense_solid && stock_of(turn, "DizzyWeapon") < 1 && gold >= 150 {
        needs.push(Need { name: "DizzyWeapon".into(), num: 1, priority: 3 });
    }
    // Wall repair kits when the wall line took damage overnight (10g each).
    let damaged_walls = turn
        .walls()
        .iter()
        .filter(|wall| wall.health < crate::brain::combat::wall_max_hp(wall.level))
        .count() as i64;
    if damaged_walls > 0 && stock_of(turn, "WallFixer") < damaged_walls.min(6) {
        needs.push(Need {
            name: "WallFixer".into(),
            num: (damaged_walls.min(6) - stock_of(turn, "WallFixer")).min(gold / 10),
            priority: 3,
        });
    }
    // Medicine only when someone is actually hurt (keep a spare so a second
    // injury doesn't force a fresh 10g trip).
    let injured = turn.controllable().iter().any(|role| {
        let max_hp = if role.kind == UnitKind::Worker { 220 } else { 200 };
        role.health * 10 < max_hp * 8
    });
    if injured && stock_of(turn, "Medicine") < 2 {
        needs.push(Need { name: "Medicine".into(), num: 2 - stock_of(turn, "Medicine"), priority: 2 });
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

    // Gold filter. Two budgets: upgrade vouchers are defensive infrastructure
    // and may spend the full gold hoard; everything else (consumables) must
    // leave `reserve` untouched so the tower build-out never stalls.
    let mut remaining = turn.gold;
    let mut out = Vec::new();
    needs.sort_by_key(|need| need.priority);
    for need in needs {
        if need.num <= 0 {
            continue;
        }
        let price = turn.weapon_shop.get(&need.name).copied().unwrap_or(i64::MAX);
        if price == i64::MAX {
            continue;
        }
        let cost = price.saturating_mul(need.num);
        let floor = if is_upgrade(&need.name) { 0 } else { reserve };
        if cost <= remaining && remaining.saturating_sub(cost) >= floor {
            remaining -= cost;
            out.push(need);
        }
    }
    out
}

/// Upgrade vouchers (weapon/station/wall) are infrastructure, not
/// consumables — the build reserve must not block them.
fn is_upgrade(name: &str) -> bool {
    name.contains("Voucher")
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
    // Already at the vendor: never walk away with a half-sold backpack —
    // convert every sellable ore before leaving (stones stay held for walls).
    let at_vendor = turn.vendors().iter().any(|vendor| chebyshev(role.pos, *vendor) == 1);
    if at_vendor {
        for ore in ORES {
            if role.count_item(ore) == 0 {
                continue;
            }
            if ore == STONE && team_ores(turn, STONE) <= stone_demand + STONE_BUFFER {
                continue;
            }
            return true;
        }
        return false; // only held-back stone remains: nothing to sell here
    }
    // Dusk: stop stockpiling — convert every ore to gold before nightfall.
    if turn.in_day_round >= DUSK_ROUND {
        return true;
    }
    let half_full = role.capacity > 0 && role.backpack.len() as i64 * 2 >= role.capacity;
    // A pack of 15+ items forces a vendor run regardless of ore type — a
    // nearly-full miner that keeps digging for "one more stack" stalls gold.
    let force_sell = role.backpack.len() as i64 >= 15;
    if role.backpack_full() || ores >= SELL_BATCH || half_full || force_sell {
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

/// Pick the nearest mine for this worker: stones first while wall demand is
/// unmet, otherwise the closest mine of any ore. Distance always beats value
/// — a short walk keeps the build loop moving faster than a high-value ore
/// on the far side of the map.
pub fn choose_mine(
    turn: &Turn,
    state: &BotState,
    role_pos: Pos,
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
    // Stones for the wall line: nearest stone mine wins (coordinate tiebreak
    // keeps the pick deterministic across HashMap iteration order). When no
    // stone is reachable, fall back to the nearest mine of any ore rather
    // than idling.
    if stone_demand > 0 {
        if let Some((pos, ore)) = options
            .iter()
            .filter(|(_pos, ore)| ore == STONE)
            .min_by_key(|(pos, _ore)| (chebyshev(role_pos, *pos), pos.x, pos.y))
        {
            return Some((*pos, ore.clone()));
        }
    }
    // Nearest mine first; equal distance broken by higher vendor value.
    options
        .into_iter()
        .min_by_key(|(pos, ore)| {
            let price = turn.vendor_prices.get(ore).copied().unwrap_or(1);
            (chebyshev(role_pos, *pos), std::cmp::Reverse(price), pos.x, pos.y)
        })
}
