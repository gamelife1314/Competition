//! Economy: deadline-budgeted shopping, travel-cost-aware selling, voucher
//! usage and mine selection.
//!
//! The shopping list is built as a set of funding GOALS (`intent_list`) that
//! each carry the latest day-round at which the purchase must be complete.
//! That list survives being unaffordable — the buyer still sets off toward the
//! shop before the gold arrives, so the purchase lands the round the money
//! does instead of the round after another cross-map trip.

use std::collections::HashSet;

use crate::model::{chebyshev, Turn, Unit, UnitKind, ORES, STONE, WEAPON_BUILD_COST};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// Sell a stack once this many ore accumulate when the vendor trip is cheap —
/// small enough that gold flows every few rounds instead of sitting in a full
/// backpack until dusk.
pub const SELL_BATCH: i64 = 4;
/// Upper bound for the far-vendor batch: past this the backpack size and the
/// dusk cash-out matter more than amortizing one walk.
pub const MAX_SELL_BATCH: i64 = 12;
/// Vendor trips no longer than this are cheap enough that the base batch is
/// always used — shuttling a small stack often beats carrying a heavy pack.
pub const NEAR_VENDOR: i64 = 6;
/// Trip cost assumed when the vendor's stands are unknown (never blocks a
/// sale; only widens the batch).
const ASSUMED_TRAVEL: i64 = 8;
pub const STONE_BUFFER: i64 = 2;

/// First day round (in_day_round) of the "dusk" window: mining stops and every
/// ore is converted to gold so the night is spent upgrading, not digging.
pub const DUSK_ROUND: i64 = 55;

/// Gold kept out of new weapon builds so the main weapon's level-2 upgrade
/// (WeaponUpgradeVoucher1) is never starved by a third level-1 weapon.
pub const WEAPON_UPGRADE_RESERVE: i64 = 25;
/// Shop price of WeaponUpgradeVoucher1 — the funding goal of the main weapon.
pub const WEAPON_VOUCHER1_PRICE: i64 = 100;

/// A ring this size is worth upgrading: +500 HP per 20/30 gold is the cheapest
/// survival on the board, but a voucher with no wall to spend it on is dead
/// gold.
const RING_WALLS_FOR_UPGRADE: usize = 6;
/// Rounds before dusk at which night consumables must be in a backpack.
pub const READINESS_LEAD: i64 = 25;
/// Rounds before dusk at which a night consumable may dip into the tower build
/// reserve — a night with no Medicine is where controllers die.
const READINESS_URGENT: i64 = 10;
/// Rounds before dusk at which an unreachable weapon upgrade stops blocking the
/// third tower (issue #9: the rocket never arrived on the first night).
pub const FALLBACK_LEAD: i64 = 20;

#[derive(Debug, Clone)]
pub struct Need {
    pub name: String,
    pub num: i64,
    pub priority: i32, // lower = more urgent
    /// Latest day-round by which the purchase must be complete; the walk to
    /// the shop is already subtracted. 0 means "no deadline".
    pub latest_round: i64,
    /// Effective HP bought per 100 gold — the marginal-survival ranking used
    /// inside a priority bucket. 0 for consumables.
    pub value: i64,
    /// Why the item is on the list (telemetry only).
    pub reason: &'static str,
}

/// The day's purchase plan: `intent` is everything we want with its deadline
/// (affordable or not), `shopping` is the affordable subset to act on now.
#[derive(Debug, Clone, Default)]
pub struct Budget {
    pub intent: Vec<Need>,
    pub shopping: Vec<Need>,
    pub reserve: i64,
}

impl Budget {
    pub fn head(&self) -> Option<&Need> {
        self.shopping.first().or_else(|| self.intent.first())
    }
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
    turn.controllable()
        .iter()
        .map(|role| role.count_item(ore) as i64)
        .sum()
}

/// Rounds of walking from `from` to the nearest stand of any of `targets`.
fn travel_to(turn: &Turn, from: Pos, targets: &[Pos]) -> i64 {
    targets
        .iter()
        .flat_map(|target| crate::brain::stand_cells(turn, *target))
        .map(|stand| chebyshev(from, stand) as i64)
        .min()
        .unwrap_or(ASSUMED_TRAVEL)
}

/// Rounds of walking from `from` to the nearest vendor stand (0 when already
/// standing on one). This is the trip cost that sets the sell batch.
pub fn vendor_travel(turn: &Turn, from: Pos) -> i64 {
    travel_to(turn, from, &turn.vendors())
}

/// Rounds of walking from `from` to the nearest weapon-shop stand.
pub fn shop_travel(turn: &Turn, from: Pos) -> i64 {
    travel_to(turn, from, &turn.weapon_shops())
}

/// Rounds the dedicated buyer (the last worker) needs to reach the shop. Used
/// to schedule every purchase deadline; falls back to a conservative estimate
/// when no shop has been seen yet.
pub fn buyer_shop_travel(turn: &Turn) -> i64 {
    match turn.workers().last() {
        Some(buyer) => shop_travel(turn, buyer.pos),
        None => ASSUMED_TRAVEL,
    }
}

/// Latest day-round at which a role must set off for the vendor so its whole
/// backpack can still be cashed out before nightfall. Replaces the flat dusk
/// constant: a role 10 rounds from the vendor has to leave 10 rounds earlier.
pub fn sell_deadline(turn: &Turn, role: &Unit) -> i64 {
    (DUSK_ROUND - 1 - vendor_travel(turn, role.pos)).max(0)
}

/// Effective HP each upgrade buys, used to rank candidates by marginal
/// survival per gold instead of by a fixed ladder.
///
/// Walls are pure HP (level 1→2 and 2→3 are both +500). The weapon voucher
/// also buys a range/damage step — a level-2 gun kills about twice as fast —
/// which is credited as extra effective HP. The station is the loss condition
/// itself, so its HP counts half again as much.
fn effective_hp(name: &str) -> i64 {
    match name {
        "WallUpgradeVoucher1" | "WallUpgradeVoucher2" => 500,
        "StationUpgradeVoucher1" | "StationUpgradeVoucher2" => 2250,
        "WeaponUpgradeVoucher1" => 1500,
        "WeaponUpgradeVoucher2" => 2000,
        _ => 0,
    }
}

/// Effective HP bought per 100 gold. 0 when the price is unknown.
fn survival_value(name: &str, price: i64) -> i64 {
    let hp = effective_hp(name);
    if hp == 0 || price <= 0 || price == i64::MAX {
        return 0;
    }
    hp * 100 / price
}

fn upgrade_need(turn: &Turn, name: &str, num: i64, priority: i32, latest_round: i64) -> Need {
    let price = turn.weapon_shop.get(name).copied().unwrap_or(i64::MAX);
    Need {
        name: name.to_string(),
        num,
        priority,
        latest_round,
        value: survival_value(name, price),
        reason: "upgrade",
    }
}

/// Everything we want to buy, with a deadline per item, regardless of whether
/// we can afford it yet.
pub fn intent_list(turn: &Turn, state: &BotState, reserve: i64) -> Vec<Need> {
    let mut needs: Vec<Need> = Vec::new();
    let travel = buyer_shop_travel(turn);
    // A purchase must be complete before dusk: walking to the shop costs
    // `travel` rounds and the gold itself needs a few more rounds to be earned.
    let latest = |margin: i64| (DUSK_ROUND - 1 - travel - margin).max(0);
    let readiness = turn.in_day_round >= DUSK_ROUND - READINESS_LEAD;
    let gold = (turn.gold - reserve).max(0);

    // 1. Weapon upgrade vouchers: the single biggest firepower step (+500 HP,
    //    +2 range, double damage on the main gun) and the gate to level 3.
    //    Funded before anything else — the larger purchases below are only
    //    considered once this one is in hand, already applied, or out of reach.
    for voucher in ["WeaponUpgradeVoucher1", "WeaponUpgradeVoucher2"] {
        let want_level = if voucher.ends_with('1') { 1 } else { 2 };
        let need_count = turn
            .towers()
            .iter()
            .filter(|tower| tower.level == want_level)
            .count() as i64;
        let deficit = (need_count - stock_of(turn, voucher)).max(0).min(1);
        if deficit > 0 {
            needs.push(upgrade_need(turn, voucher, deficit, 0, latest(6)));
        }
    }

    // 2. Wall upgrades: 500 HP for 20 (level 1→2) or 30 gold (2→3). They only
    //    queue once a ring actually stands, and — until the weapon deadline —
    //    only once the 100-gold weapon voucher can no longer be starved by
    //    them (in hand, already applied, affordable alongside, or too late).
    let walls = turn.walls();
    if walls.len() >= RING_WALLS_FOR_UPGRADE {
        let weapon_path_clear = stock_of(turn, "WeaponUpgradeVoucher1") > 0
            || turn.towers().iter().all(|tower| tower.level >= 2)
            || turn.gold >= WEAPON_VOUCHER1_PRICE
            || turn.in_day_round >= latest(6);
        if weapon_path_clear {
            for (voucher, want_level) in [("WallUpgradeVoucher1", 1), ("WallUpgradeVoucher2", 2)] {
                // One voucher covers exactly one wall, so queue at most two per
                // trip; the next round re-counts what is still at that level.
                let want = walls
                    .iter()
                    .filter(|wall| wall.level == want_level)
                    .count()
                    .min(2) as i64;
                let have = stock_of(turn, voucher);
                if want > have {
                    needs.push(upgrade_need(turn, voucher, want - have, 1, latest(4)));
                }
            }
        }
    }

    // 3. Station upgrade: survival score (score3 caps at 550). Ranked against
    //    the walls by HP per gold rather than by fiat.
    if let Some(station) = turn.station() {
        let voucher = match station.level {
            1 => "StationUpgradeVoucher1",
            2 => "StationUpgradeVoucher2",
            _ => "",
        };
        if !voucher.is_empty() && stock_of(turn, voucher) == 0 {
            needs.push(upgrade_need(turn, voucher, 1, 1, latest(6)));
        }
    }

    // 4. Night consumables: pre-stocked before the first night instead of only
    //    reacting to an injury. A controller that drops below 30% at night with
    //    no Medicine simply dies, and a dead controller mans no weapon.
    let injured = turn.controllable().iter().any(|role| {
        let max_hp = if role.kind == UnitKind::Worker {
            220
        } else {
            200
        };
        role.health * 10 < max_hp * 8
    });
    let medicine = stock_of(turn, "Medicine");
    let want_medicine = if injured {
        2
    } else if readiness {
        1
    } else {
        0
    };
    if medicine < want_medicine {
        needs.push(Need {
            name: "Medicine".into(),
            num: want_medicine - medicine,
            priority: 2,
            latest_round: latest(1),
            value: 0,
            reason: "night_readiness",
        });
    }

    // 5. Wall repair kits: damaged walls first, otherwise a small pre-night
    //    stock so a breach can be patched without a shopping trip.
    let damaged_walls = walls
        .iter()
        .filter(|wall| wall.health < crate::brain::combat::wall_max_hp(wall.level))
        .count() as i64;
    let fixers = stock_of(turn, "WallFixer");
    let want_fixers = if damaged_walls > 0 {
        damaged_walls.min(6)
    } else if readiness && walls.len() >= RING_WALLS_FOR_UPGRADE {
        2
    } else {
        0
    };
    if fixers < want_fixers {
        needs.push(Need {
            name: "WallFixer".into(),
            num: want_fixers - fixers,
            priority: 3,
            latest_round: latest(0),
            value: 0,
            reason: "wall_repair",
        });
    }

    // 6. Area items, only once the defense is solid (every weapon slot filled
    //    → no gold still reserved for builds) and gold is plentiful.
    let defense_solid = reserve == 0;
    if defense_solid && stock_of(turn, "Bomb") < 2 && gold >= 150 {
        needs.push(Need {
            name: "Bomb".into(),
            num: 2 - stock_of(turn, "Bomb"),
            priority: 4,
            latest_round: latest(0),
            value: 0,
            reason: "night_item",
        });
    }
    if defense_solid && stock_of(turn, "DizzyWeapon") < 1 && gold >= 150 {
        needs.push(Need {
            name: "DizzyWeapon".into(),
            num: 1,
            priority: 4,
            latest_round: latest(0),
            value: 0,
            reason: "night_item",
        });
    }

    // (Treasure sacrifice items are bought exclusively by the pioneer inside
    // treasure.rs — summonTreasure requires the items in the pioneer's pack.)
    // Harassment: boss wave on the enemy, but only once our own defense
    // stands (all three towers) and we are rich.
    if gold >= 500 && turn.towers().len() >= 3 && !state.harass_done_today {
        needs.push(Need {
            name: "BossRobotSummonOrder".into(),
            num: 1,
            priority: 9,
            latest_round: 0,
            value: 0,
            reason: "harass",
        });
    }

    needs
}

/// The affordable subset of `intent_list`, in purchase order.
///
/// Two budgets: upgrade vouchers are defensive infrastructure and may spend
/// the whole gold hoard; consumables must leave `reserve` untouched so the
/// tower build-out never stalls — except in the last `READINESS_URGENT`
/// rounds before dusk, when a night without medicine costs more than a tower
/// slot. A need that only partly fits is bought partly, not skipped.
pub fn shopping_list(turn: &Turn, state: &BotState, reserve: i64) -> Vec<Need> {
    let mut intents = intent_list(turn, state, reserve);
    intents.sort_by_key(|need| (need.priority, std::cmp::Reverse(need.value)));
    let urgent = turn.in_day_round >= DUSK_ROUND - READINESS_URGENT;
    let mut remaining = turn.gold;
    let mut out = Vec::new();
    for need in intents {
        if need.num <= 0 {
            continue;
        }
        let price = turn
            .weapon_shop
            .get(&need.name)
            .copied()
            .unwrap_or(i64::MAX);
        if price <= 0 || price == i64::MAX {
            continue;
        }
        let floor = if is_upgrade(&need.name) || (urgent && is_night_readiness(&need.name)) {
            0
        } else {
            reserve
        };
        let affordable = (remaining - floor).max(0) / price;
        let num = need.num.min(affordable);
        if num <= 0 {
            continue;
        }
        remaining -= price * num;
        out.push(Need { num, ..need });
    }
    out
}

/// The day's purchase plan: intent (with deadlines) plus what is affordable.
pub fn budget(turn: &Turn, state: &BotState, reserve: i64) -> Budget {
    Budget {
        intent: intent_list(turn, state, reserve),
        shopping: shopping_list(turn, state, reserve),
        reserve,
    }
}

/// Rounds of slack the buyer waits at the shop before a deadline, so the
/// purchase lands the round the gold does rather than the round after it.
const PREPOSITION_LEAD: i64 = 3;

/// Should the buyer set off for the shop even though nothing on the list is
/// affordable yet? True once the earliest deadline is within one shop trip
/// plus `PREPOSITION_LEAD`: arriving early means the purchase lands the round
/// the gold does, instead of the round after another cross-map walk. Early in
/// the day this is false, so the buyer never camps at the shop instead of
/// mining.
pub fn buyer_must_preposition(turn: &Turn, role: &Unit, needs: &[Need]) -> bool {
    let travel = shop_travel(turn, role.pos);
    needs.iter().any(|need| {
        need.num > 0
            && need.latest_round > 0
            && turn.in_day_round + travel + PREPOSITION_LEAD >= need.latest_round
    })
}

/// Is a funding goal close enough to its deadline that ore must be cashed in
/// immediately? Waiting for a larger, cheaper batch would miss the purchase.
fn purchase_urgent(turn: &Turn, state: &BotState, role: &Unit) -> bool {
    let travel = vendor_travel(turn, role.pos);
    intent_list(turn, state, 0).iter().any(|need| {
        if !(need.num > 0 && need.latest_round > 0) {
            return false;
        }
        let price = turn
            .weapon_shop
            .get(&need.name)
            .copied()
            .unwrap_or(i64::MAX);
        if price <= 0 || price == i64::MAX {
            return false;
        }
        price * need.num > turn.gold && turn.in_day_round + travel + 1 >= need.latest_round
    })
}

/// Upgrade vouchers (weapon/station/wall) are infrastructure, not
/// consumables — the build reserve must not block them.
fn is_upgrade(name: &str) -> bool {
    name.contains("Voucher")
}

/// Night supplies a controller may die without.
fn is_night_readiness(name: &str) -> bool {
    matches!(name, "Medicine" | "WallFixer")
}

/// How many ore to accumulate before walking to the vendor. A trip costs
/// `travel` rounds of walking, so a distant vendor is worth one bigger load; a
/// vendor within `NEAR_VENDOR` rounds — or an imminent purchase deadline — is
/// worth cashing in at the base batch so the gold arrives while it can still
/// be spent. The dusk cash-out (`sell_deadline`) always wins: no batch may
/// push a sale past nightfall.
pub fn sell_batch(turn: &Turn, state: &BotState, role: &Unit) -> i64 {
    let travel = vendor_travel(turn, role.pos);
    if travel <= NEAR_VENDOR || purchase_urgent(turn, state, role) {
        return SELL_BATCH;
    }
    (SELL_BATCH + (travel - NEAR_VENDOR) / 2).min(MAX_SELL_BATCH)
}

/// Whether we may spend 25g building another weapon this round. Build 1-2
/// weapons first (a gold reserve must survive), then stop until the main
/// weapon reaches level 2 — a level-2 weapon out-values a third level-1
/// weapon, and the gold is better spent on WeaponUpgradeVoucher1.
pub fn may_build_weapon(turn: &Turn, state: &BotState) -> bool {
    let towers = turn.towers();
    if towers.len() < 2 {
        return turn.gold >= WEAPON_BUILD_COST;
    }
    if towers.iter().any(|tower| tower.level >= 2) {
        return turn.gold >= WEAPON_BUILD_COST + WEAPON_UPGRADE_RESERVE;
    }
    // Two level-1 towers with neither upgraded: the gold is normally held for
    // the level-2 step. But when that step is clearly out of reach before dusk
    // (issue #9: the rocket never arrived and the first night had two guns),
    // buy the 25-gold rocket instead of hard-waiting for a 100-gold voucher.
    third_tower_fallback(turn, state)
}

/// The weapon upgrade is unreachable in time, so a third level-1 weapon beats
/// an empty gold hoard. Only fires once the upgrade deadline is in sight.
fn third_tower_fallback(turn: &Turn, state: &BotState) -> bool {
    turn.gold >= WEAPON_BUILD_COST
        && turn.in_day_round >= DUSK_ROUND - FALLBACK_LEAD
        && !upgrade_reachable(turn, state)
}

/// Can the team still put 100 gold together before dusk? A carried voucher
/// counts; otherwise liquid gold plus the ore already in backpacks at today's
/// vendor prices.
pub fn upgrade_reachable(turn: &Turn, state: &BotState) -> bool {
    if stock_of(turn, "WeaponUpgradeVoucher1") > 0 {
        return true;
    }
    let mut liquid = turn.gold;
    for role in turn.controllable() {
        for ore in ORES {
            let count = role.count_item(ore) as i64;
            if count == 0 {
                continue;
            }
            let base = state.base_prices.get(ore).copied().unwrap_or(1).max(1);
            let price = turn.vendor_prices.get(ore).copied().unwrap_or(base);
            liquid += price * count;
        }
    }
    liquid >= WEAPON_VOUCHER1_PRICE
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
    let at_vendor = turn
        .vendors()
        .iter()
        .any(|vendor| chebyshev(role.pos, *vendor) == 1);
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
    // Dusk cash-out: the walk must start early enough to arrive before night.
    if turn.in_day_round >= sell_deadline(turn, role) {
        return true;
    }
    let half_full = role.capacity > 0 && role.backpack.len() as i64 * 2 >= role.capacity;
    // A pack of 15+ items forces a vendor run regardless of ore type — a
    // nearly-full miner that keeps digging for "one more stack" stalls gold.
    let force_sell = role.backpack.len() as i64 >= 15;
    if role.backpack_full() || ores >= sell_batch(turn, state, role) || half_full || force_sell {
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
pub fn sell_command(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    stone_demand: i64,
) -> Option<RoleCommand> {
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
    options.into_iter().min_by_key(|(pos, ore)| {
        let price = turn.vendor_prices.get(ore).copied().unwrap_or(1);
        (
            chebyshev(role_pos, *pos),
            std::cmp::Reverse(price),
            pos.x,
            pos.y,
        )
    })
}
