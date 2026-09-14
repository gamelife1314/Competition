//! Economy: deadline-budgeted shopping, travel-cost-aware selling, voucher
//! usage and mine selection.
//!
//! The shopping list is built as a set of funding GOALS (`intent_list`) that
//! each carry the latest day-round at which the purchase must be complete.
//! That list survives being unaffordable — the buyer still sets off toward the
//! shop before the gold arrives, so the purchase lands the round the money
//! does instead of the round after another cross-map trip.

use std::collections::HashSet;

use crate::brain::coach::Harass;
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
/// Rounds before dusk at which the third tower's fund comes under guard
/// (issue #9: the rocket never arrived on the first night). With the ordering
/// rule gone (P0-2) the guard is purely financial — it holds the 25 gold the
/// third gun costs, so small purchases cannot spend the purse below it.
pub const FALLBACK_LEAD: i64 = 20;

/// Enemy station HP at or below which the base counts as "low" (P2-3).
///
/// A base is 1500 HP at level 1, so this is the last 40%: the point at which a
/// boss wave is not harassment but a finisher, because the score2 amplifier and
/// the win condition are the same thing — the enemy station going down.
pub const ENEMY_STATION_LOW_HP: i64 = 600;

/// Two enemy towers this close together are a cluster (P2-3).
///
/// A BOSS wave is area damage against a base; against towers packed inside one
/// blast radius it is the only order that pays for itself twice.
pub const ENEMY_TOWER_CLUSTER: i32 = 3;

/// Rounds before `DUSK_ROUND - FALLBACK_LEAD` at which the build reserve starts
/// guarding the 25 gold the third tower costs (P0-4). The guard exists so the
/// gold is still there when the window opens — consumables and wall vouchers
/// used to be able to spend the purse below 25 in the very rounds it was about
/// to open, which is how the rocket pad never got laid (issues #22/#23:
/// "第 3 塔从未建造", `tower_plan mayBuild=false` 全天).
pub const GUARD_LEAD: i64 = 6;

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
    //
    //    SAME TIER AS THE WEAPON VOUCHER, and that is the whole fix (issues
    //    #161-#170). The two vouchers cost the same 100 gold (任务书 4.6.3) and
    //    the purse in those ten matches peaked at 130-155 (表 2c) — enough for
    //    one and never for both. At priority 1 the station voucher was behind
    //    the weapon voucher for every round of every match, so it was head of
    //    the list exactly ONCE per match (round 1, before a tower exists and the
    //    weapon voucher is not yet queued) and unaffordable in that one round.
    //    表 2a across all ten reports: not a single `StationUpgradeVoucher1`
    //    bought, while nine of the ten bases fell, `scoreAttr.survival` sat at
    //    0-30 against a 应得 of 10-100, and the round score froze with the base
    //    (163 counted 152 of the 424 kill points it had earned, 167 64 of 309).
    //
    //    `survival_value` already ranks this pair correctly — the station is
    //    credited 2250 effective HP per 100 gold against the weapon's 1500
    //    (`effective_hp`: "the station is the loss condition itself, so its HP
    //    counts half again as much") — and a priority number was overriding it.
    //    In the same tier the recorded ranking decides, which is what this
    //    block's own docstring says it does.
    //
    //    The previous tier was set when the guns were killing nothing: the
    //    batches behind `clear_gap_order_with` (issues #8/#18/#19/#20, #131-#135)
    //    read kill=0 and base-fell-D2, so firepower was the binding constraint
    //    and a bigger base behind dead guns bought nothing. This batch reads the
    //    opposite — kill 70/143/174/187/248 in 170, 225 in 167, 309 in 163's
    //    day 4 — the guns work, and the base is what ends the half. The
    //    firepower-first reorder is not deleted: `clear_gap_order_with` still
    //    demotes the station to 5 the moment `firepower_gap` is positive, which
    //    is the firepower case stated exactly.
    if let Some(station) = turn.station() {
        let voucher = match station.level {
            1 => "StationUpgradeVoucher1",
            2 => "StationUpgradeVoucher2",
            _ => "",
        };
        if !voucher.is_empty() && stock_of(turn, voucher) == 0 {
            needs.push(upgrade_need(turn, voucher, 1, 0, latest(6)));
        }
    }

    // 4. Night consumables: pre-stocked before the first night instead of only
    //    reacting to an injury. A controller that drops below 30% at night with
    //    no Medicine simply dies, and a dead controller mans no weapon.
    //    P1-4 续航包：one bottle PER CONTROLLER — the night withdrawal rule
    //    almost never fires when every controller carries a potion, and the
    //    potion is also what clears the hysteresis holdout (night.rs).
    let injured = turn.controllable().iter().any(|role| {
        let max_hp = if role.kind == UnitKind::Worker {
            220
        } else {
            200
        };
        role.health * 10 < max_hp * 8
    });
    let controllers = turn.controllable().len() as i64;
    let medicine = stock_of(turn, "Medicine");
    let want_medicine = if injured || readiness {
        controllers.max(1)
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
    // The `walls.len() >= RING_WALLS_FOR_UPGRADE` gate that used to stand here
    // is why the first `WallFixer` of every match in issues #131-#135 was bought
    // on day 2 (表 2a: round 147, 148, 149, 151, 154 — and one of the five never
    // bought one at all) while `ourWallLost` on night 1 alone ran 2465-6410 and
    // the base behind the ring fell on night 2 or 3 in four of them. A ring that
    // exists is a ring the night can breach; six standing cells was a proxy for
    // "the ring is real" that only became true half a day after the ring started
    // taking damage. A `WallFixer` mends a wall, and with no wall on the board it
    // is 10 gold for nothing — that is the whole test. The quantity is unchanged
    // (2 kits before the first complete ring, 4 after) and so are the priorities,
    // so the 25-gold third-tower reserve and the 100-gold weapon voucher are
    // exactly as protected as they were; a 2-kit day-1 stock is 20 gold.
    let want_fixers = if damaged_walls > 0 {
        damaged_walls.min(6)
    } else if readiness && !walls.is_empty() {
        // P1-4 续航包：2–4 kits a day. A ring that has been closed before is
        // the ring the night tears open (issue #21: 17→7) — and the kits are
        // the only wall HP available during the night itself.
        if state.ring_ever_complete {
            4
        } else {
            2
        }
    } else {
        0
    };
    if fixers < want_fixers {
        // A wall at half HP is a breach waiting for the night, and 10 gold
        // buys its full 1000-3000 HP back (任务书: WallFixer "目标坐标所在围墙回
        // 满血") — the cheapest HP in the game. Issue #16 could not afford one
        // on D2 and lost the ring: a chipped wall can wait behind Medicine, a
        // half-destroyed one cannot. The tie with Medicine keeps Medicine
        // first (stable sort, and Medicine is listed above), so a hurt role
        // still drinks before the wall is patched.
        let critical = walls
            .iter()
            .any(|wall| wall.health * 2 < crate::brain::combat::wall_max_hp(wall.level));
        needs.push(Need {
            name: "WallFixer".into(),
            num: want_fixers - fixers,
            priority: if critical { 2 } else { 3 },
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
    //
    // The coach owns how hard we harass (brain/coach.rs): `Off` = the last
    // summons bought nothing on the enemy side, `Rich` = they demonstrably bit.
    // `Rhythm` is the shipped behaviour, so both gates below stay byte-for-byte
    // what they were — the policy only widens or closes them.
    let harass = state.coach.policy().harass;
    // Harassment: boss wave on the enemy, but only once our own defense
    // stands (all three towers) and we are rich.
    //
    // P2-3 压制窗口: the same order, armed by a stronger reason. A boss wave is
    // worth buying when the enemy base is nearly down (the win condition) or
    // when their towers sit inside one blast radius, and in that window it
    // outranks every other HARASS order on the list — the thin-ring finisher at
    // 8 and this one's own ordinary 9. It does not jump the consumables and the
    // wall repairs above it (2–4): the night's medicine is still the night's.
    // Two things are deliberately unchanged: the 500-gold gate — `gold` is
    // already net of the build reserve below, so this can never divert a coin
    // our own defence is holding — and the three-tower precondition, because a
    // summons we cannot defend behind is a wave paid for twice. The one thing
    // the window does relax is `harass_done_today`: a small order fired at dawn
    // must not use up the day's one chance at the boss wave the evening's
    // window just opened.
    let suppression = boss_suppression_window(turn);
    if harass != Harass::Off
        && gold >= 500
        && turn.towers().len() >= 3
        && (suppression || !state.harass_done_today)
    {
        needs.push(Need {
            name: "BossRobotSummonOrder".into(),
            num: 1,
            priority: if suppression { 7 } else { 9 },
            latest_round: 0,
            value: 0,
            reason: if suppression {
                "boss_suppression"
            } else {
                "harass"
            },
        });
    }

    // P2-3 召唤令节奏: a boss wave is a half-ender against a wall-less enemy
    // base — 对手无墙时 BOSS 令可直接终结半场 (v1). The rich gate above stays;
    // this cheaper trigger arms only while the enemy ring is thin enough for
    // the extra wave to matter (enemy walls are globally visible, 接口文档
    // 1.4), and tops the stock up to two so the pressure is a rhythm across
    // nights instead of one spike the daily 10-use cap never reaches anyway.
    let enemy_walls = turn
        .enemy
        .iter()
        .filter(|unit| unit.kind == UnitKind::Wall && unit.alive())
        .count();
    if harass != Harass::Off
        && enemy_walls <= harass.thin_wall_limit()
        && turn.towers().len() >= 3
        && gold >= 300
        && stock_of(turn, "BossRobotSummonOrder") < harass.stock_cap()
        && !state.harass_done_today
    {
        needs.push(Need {
            name: "BossRobotSummonOrder".into(),
            num: 1,
            priority: 8,
            latest_round: 0,
            value: 0,
            reason: "harass_finisher",
        });
    }

    // P1-1: tonight's clear gap may reorder this list. The committed default is
    // OFF; it is armed from outside either by the local A/B dial
    // (`CG_TUNE_CLEAR_GAP=1`) or by the coach, which arms it itself the first
    // night our station takes hits (brain/coach.rs — the dial was waiting on an
    // A/B report the intranet workflow cannot run).
    let gap_funding = crate::brain::combat::weights().clear_gap_drive != 0
        || state.coach.policy().gap_funding;
    clear_gap_order_with(gap_funding, turn, &mut needs);
    needs
}

/// Is the enemy in a state a boss wave can finish (P2-3)?
///
/// Two triggers, both read from the protocol rather than guessed: their base is
/// below [`ENEMY_STATION_LOW_HP`], or two of their towers stand within
/// [`ENEMY_TOWER_CLUSTER`] of each other. Either one means the wave is aimed at
/// something that cannot absorb it.
///
/// A station we cannot see is NOT a low station — `enemy_station()` returning
/// `None` means out of vision, not destroyed — so the HP test fails closed and
/// only the tower test can fire on a hidden base. The towers are globally
/// visible (接口文档 1.4), which is why they carry the trigger.
pub fn boss_suppression_window(turn: &Turn) -> bool {
    if turn
        .enemy_station()
        .map(|station| station.health <= ENEMY_STATION_LOW_HP)
        .unwrap_or(false)
    {
        return true;
    }
    let towers: Vec<Pos> = turn
        .enemy
        .iter()
        .filter(|unit| unit.kind.is_tower() && unit.alive())
        .map(|tower| tower.pos)
        .collect();
    towers.iter().enumerate().any(|(index, tower)| {
        towers
            .iter()
            .skip(index + 1)
            .any(|other| chebyshev(*tower, *other) <= ENEMY_TOWER_CLUSTER)
    })
}

/// P1-1: when tonight's estimated wave out-HPs our guns (`firepower_gap` >
/// 0), firepower funding outranks everything long-term: the station upgrade
/// drops below the consumables and harassment is suppressed outright — a
/// bigger base behind too few guns is how the D2–D3 nights were lost
/// (issues #8/#18/#19/#20). Weapon/wall vouchers and the night sustain pack
/// keep their priorities; the P0-4 third-tower guard already owns the build
/// side of the same gap.
///
/// Dial-gated (`CG_TUNE_CLEAR_GAP`, default OFF): the committed fixed order
/// stands until the multi-opponent A/B report shows the reorder winning
/// (Improve.kimi.md §7, P1-1 row). `drive` is taken explicitly so tests can
/// exercise both branches without touching the process-wide dial.
pub fn clear_gap_order_with(drive: bool, turn: &Turn, needs: &mut Vec<Need>) {
    if !drive || crate::brain::combat::firepower_gap(turn) <= 0 {
        return;
    }
    for need in needs.iter_mut() {
        if matches!(
            need.name.as_str(),
            "StationUpgradeVoucher1" | "StationUpgradeVoucher2"
        ) {
            need.priority = 5;
        }
    }
    needs.retain(|need| need.name != "BossRobotSummonOrder");
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
    // The station upgrade wins its ties. `value` alone leaves one: the level-2
    // -> 3 station voucher (150 gold, `effective_hp` 2250) and the level-1
    // weapon voucher (100 gold, 1500) both compute to exactly 1500 effective HP
    // per 100 gold, so which of them led was decided by the order `intent_list`
    // happened to push them in. A tie is not a reason to let the loss condition
    // wait behind a gun: the station is the only asset with no repair item
    // (`WallFixer` mends a wall, `Medicine` mends a unit, nothing mends the
    // base) and the only one whose destruction ends the half outright
    // (任务书 ch.7) — and it takes the round's gold out of reach with it.
    intents.sort_by_key(|need| {
        (
            need.priority,
            std::cmp::Reverse(need.value),
            std::cmp::Reverse(need.name.starts_with("StationUpgradeVoucher")),
        )
    });
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
        // Wall vouchers count as infrastructure only while the tower fund is
        // not being guarded: a guarded 25 gold is the third tower's, and a
        // 20-gold wall voucher is exactly the purchase that used to spend it
        // (P0-4; issues #22/#23 froze at 5–25 with the rocket pad never laid).
        // Weapon and station vouchers keep the free pass — a purse that
        // actually reaches 100 gold turns the guard off by itself
        // (`upgrade_reachable`), so they can never drain the guarded fund.
        let wall_voucher = need.name.starts_with("WallUpgradeVoucher");
        let floor = if (is_upgrade(&need.name) && !wall_voucher)
            || (urgent && is_night_readiness(&need.name))
        {
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

/// Gold the team could raise today: coins in hand plus every ore in a backpack
/// the vendor would ACTUALLY TAKE, valued at the current price. The economy's
/// real spending power — used to tell a goal that is merely not-yet-affordable
/// from one that is out of reach entirely.
///
/// Stone counts for nothing here. It is the wall line's raw material, and
/// `sell_command` refuses to sell it while the ring still wants it, so pricing
/// it as cash read spending power out of a backpack the shop will not honour.
/// Issue #22 paid for that twice in one match. The ring's stone read as "the
/// 100-gold upgrade is still reachable", which held the third tower back and
/// the match was fought with two guns for the fourth battle running; and
/// `buyer_must_preposition` marched the only economic worker to the shop on
/// stone it could not sell, where it camped while the purse froze at 5 gold and
/// the whole match produced exactly one sale. Only `gold` and the ore the
/// vendor buys can pay a shop, so only those are counted.
pub fn liquid_gold(turn: &Turn) -> i64 {
    let mut liquid = turn.gold;
    for role in turn.controllable() {
        for ore in ORES {
            if ore == STONE {
                continue;
            }
            let count = role.count_item(ore) as i64;
            if count > 0 {
                liquid += turn.vendor_prices.get(ore).copied().unwrap_or(1).max(1) * count;
            }
        }
    }
    liquid
}

/// Should the buyer set off for the shop even though nothing on the list is
/// affordable yet?
///
/// One case, and only one: *funded*. The deadline is within one trip and the
/// ore already in our backpacks — ore the vendor will actually buy, see
/// [`liquid_gold`] — covers the price, so leaving now means the purchase lands
/// the round the sale does instead of a cross-map walk later.
///
/// Deliberately NOT "any deadline is within one trip": that parked the only
/// economic worker at a far shop for half the day for a 100-gold voucher the
/// team was never going to afford, which is how the day ended with two towers,
/// no walls and a frozen purse (issues #12/#14). The narrower "last chance"
/// version of the same mistake — leave now because there is no time left for a
/// second trip — survived that fix and cost issue #22 the match: with a shop 20
/// rounds away and 5 gold in the purse it fired on day-round 11, every day, and
/// the one role that could have earned the 100 gold stood at the counter for
/// the remaining 44 rounds of the day. A goal nothing can pay for is not a
/// reason to stop earning; the buyer keeps the collect→sell→buy loop running
/// and sets off the round the pack covers the price.
pub fn buyer_must_preposition(turn: &Turn, role: &Unit, needs: &[Need]) -> bool {
    let travel = shop_travel(turn, role.pos);
    let liquid = liquid_gold(turn);
    needs.iter().any(|need| {
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
        // A deadline the walk can no longer meet is a dead goal, not a reason
        // to set off (issues #12/#14).
        if turn.in_day_round + travel > need.latest_round {
            return false;
        }
        liquid >= price * need.num
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
    if travel <= NEAR_VENDOR
        || purchase_urgent(turn, state, role)
        || funding_next_weapon(turn, role)
    {
        return SELL_BATCH;
    }
    (SELL_BATCH + (travel - NEAR_VENDOR) / 2).min(MAX_SELL_BATCH)
}

/// Is this role sitting on a load the team's next weapon is waiting for?
///
/// A far vendor is worth one bigger load only when the load can afford to
/// wait. While the team cannot yet pay for its next gun the gold is the
/// bottleneck, not the walk, and the amortized batch is the thing standing
/// between the ore and the build: issue #15's economy produced its first coin
/// at r=27 — by which time the window for the second and third weapon had
/// already closed — with a worker parked on a full pack waiting for a load
/// worth the trip. Nothing to sell means nothing to decide, so an empty pack
/// leaves the batch alone.
fn funding_next_weapon(turn: &Turn, role: &Unit) -> bool {
    turn.towers().len() < 3 && turn.gold < WEAPON_BUILD_COST && total_ores(role) > 0
}

/// Whether we may spend 25g building another weapon this round.
///
/// One door, and it is the purse: three towers is the cap, 25 gold is the
/// price. The two-tower rule that used to stand in front of it — hold the whole
/// purse for WeaponUpgradeVoucher1 until the main gun reaches level 2, and let
/// the third tower through only once the upgrade was provably out of reach —
/// was the ordering bug behind five straight matches fought with two guns
/// (docs/FAILURE-ANALYSIS-2026-09-14.md §2.2):
///
/// * the upgrade costs 100 gold and income froze at 5–25, so the door it was
///   guarding never opened either;
/// * "out of reach" was measured in `liquid_gold` — cash plus every sellable
///   ore in a backpack — and a worker that picks up iron on the way to the
///   stone vein always carried ≥100 of it, so `upgrade_reachable` was true all
///   day and the fallback never fired. The tower was held hostage to a purchase
///   that could not happen.
///
/// The upgrade keeps its own priority and always did: `intent_list` lists
/// WeaponUpgradeVoucher1 at priority 0 and `tower_build_reserve` still reserves
/// the 1-2 tower fund, so the voucher is bought the moment the collect→sell→buy
/// loop can pay for it. What it no longer gets is a veto over the third gun.
pub fn may_build_weapon(turn: &Turn, _state: &BotState) -> bool {
    turn.towers().len() < 3 && turn.gold >= WEAPON_BUILD_COST
}

/// Can the team still put 100 gold together before dusk? A carried voucher
/// counts; otherwise the liquid wealth already on the board.
pub fn upgrade_reachable(turn: &Turn, _state: &BotState) -> bool {
    stock_of(turn, "WeaponUpgradeVoucher1") > 0 || liquid_gold(turn) >= WEAPON_VOUCHER1_PRICE
}

/// Should the 25-gold third-tower fund be guarded from the shopping list?
/// True once the build window is near AND the level-2 upgrade is out of reach
/// — an upgrade the team can still pay for is the better purchase and may
/// spend the purse (the issue #13 lesson); a third tower or an already-upgraded
/// gun leaves nothing to guard.
pub fn third_tower_guard(turn: &Turn, state: &BotState) -> bool {
    let towers = turn.towers();
    if towers.len() != 2 || towers.iter().any(|tower| tower.level >= 2) {
        return false;
    }
    if upgrade_reachable(turn, state) {
        return false;
    }
    turn.in_day_round >= DUSK_ROUND - FALLBACK_LEAD - GUARD_LEAD
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

/// Ore in this backpack the vendor would actually take. Stone is held back
/// while the wall line still wants it, so counting it as sellable sent the
/// carrier on a cross-map walk to a sale that `sell_command` then refused —
/// the trip was spent, the stone stayed, and the wall never got built.
pub fn sellable_ores(turn: &Turn, role: &Unit, stone_demand: i64) -> i64 {
    let stone = role.count_item(STONE) as i64;
    let team_stone = team_ores(turn, STONE);
    let surplus = (team_stone - stone_demand - STONE_BUFFER).max(0);
    total_ores(role) - stone + surplus.min(stone)
}

/// Should this role head to the vendor?
pub fn should_sell(turn: &Turn, state: &BotState, role: &Unit, stone_demand: i64) -> bool {
    let ores = sellable_ores(turn, role, stone_demand);
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
    // Only SELLABLE ore counts: stone the ring still needs is not "one more
    // stack", it is the wall line's raw material, and walking a full pack of
    // it to a vendor is how day 1 ended up with two towers, no ring and a
    // frozen purse (issues #12/#13/#14). Committed stone instead makes the
    // role take the wall step below, which is where it was headed anyway.
    let force_sell = ores >= 15;
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
        // Stone is sold down to the ring's own reserve, never through it. The
        // test above decides WHETHER stone is sellable at all; this decides HOW
        // MUCH, and the whole stack used to leave with the vendor — including
        // the cells the day still owes the ring. It matters most on the day
        // whose only stone demand IS a gap: day 2 mines stone because the door
        // counts in `stone_demand`, the dusk cash-out then sells every stone in
        // the pack, and the door cut that morning is still open at nightfall
        // because no carrier has one left to close it with (`sellable_ores`
        // already counts the surplus this way; the sale now agrees with it).
        let count = if ore == STONE {
            count.min((team_ores(turn, STONE) - stone_demand - STONE_BUFFER).max(0))
        } else {
            count
        };
        if count <= 0 {
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

/// The nearest mine whose ore the vendor will actually take.
///
/// `choose_mine` answers "what does the day need", and while the ring is short
/// of stone the answer is always stone. Every load that errand brings home is
/// then held back by `sellable_ores` (stone is the wall line's raw material,
/// not income), so a worker kept on it never earns a coin. The errand is not a
/// phase that passes, either: `build` spends the pack, so
/// `team_ores(STONE) < stone_demand` is re-established by the very act of
/// closing a gap and can hold from the first gap of day 2 to the last round of
/// the match. Issue #18's purse froze at the 75 gold day 1 had spent on towers
/// and stayed at exactly 0 for the remaining 300 rounds — base level 1, no
/// voucher ever bought, while the opponent "通过 sell 操作回血" — and this is
/// the half of that loop that was missing.
///
/// This is the selector for the ONE role whose job is collect→sell→buy (the
/// dedicated economy worker). The ring still gets its diggers: every other
/// worker is on `choose_mine` and stone duty, and day 1 keeps the whole crew on
/// the ring until it closes, because the ring is what makes the rest of the
/// match affordable ([`crate::brain::day`]'s `shared_wall_duty`). Stone is the
/// fallback here: with no other vein left to dig, income is impossible anyway
/// and idling is worse.
pub fn choose_sellable_mine(
    turn: &Turn,
    state: &BotState,
    role_pos: Pos,
    claimed: &HashSet<Pos>,
) -> Option<(Pos, String)> {
    // Nearest first, equal distance broken by higher vendor value — the same
    // rule `choose_mine` uses once it has no stone to fetch.
    let mut best: Option<(i64, std::cmp::Reverse<i64>, i32, i32, Pos, String)> = None;
    for (pos, ore) in turn.all_mines() {
        if ore == STONE || state.ore_on_outage(&ore, turn.day) || claimed.contains(&pos) {
            continue;
        }
        let price = turn.vendor_prices.get(&ore).copied().unwrap_or(1);
        let key = (
            // Through the day's entrance, exactly as `choose_mine` measures it:
            // the two selectors feed the same trip, and pricing them differently
            // made the economy worker and the wall crew disagree about which
            // vein was near.
            crate::brain::route::trip_rounds(turn, role_pos, pos),
            std::cmp::Reverse(price),
            pos.x,
            pos.y,
            pos,
            ore,
        );
        if best.as_ref().map(|current| key < *current).unwrap_or(true) {
            best = Some(key);
        }
    }
    if let Some((_, _, _, _, pos, ore)) = best {
        return Some((pos, ore));
    }
    choose_mine(turn, state, role_pos, 0, claimed)
}

/// Pick the mine for this worker: stones first while wall demand is unmet,
/// otherwise the closest mine of any ore.
///
/// **Distance is measured through the ring, not across it** (joint route/order
/// planner). `chebyshev(role_pos, mine)` is the straight line, and once the
/// shell is up that line goes through a wall: a vein three cells east of the
/// base is three rounds away only if the day's entrance happens to be on the
/// east side, and fifteen if it is on the west. The old metric therefore sent
/// the crew to the geometrically nearest vein and paid for it in the walk home
/// — the owner's *"如果朝哪个地方采矿，朝这个方向给开个口方便他进来，减少回合
/// 浪费"*, read from the other end. `route::trip_rounds` is that same walk with
/// the opening priced in, so the pick and the entrance agree by construction.
///
/// Distance still beats value — a short walk keeps the build loop moving faster
/// than a high-value ore on the far side of the map — but it is the real
/// distance now.
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
    let walk = |pos: Pos| crate::brain::route::trip_rounds(turn, role_pos, pos);
    // Stones for the wall line: nearest stone mine wins (coordinate tiebreak
    // keeps the pick deterministic across HashMap iteration order). When no
    // stone is reachable, fall back to the nearest mine of any ore rather
    // than idling.
    if stone_demand > 0 {
        if let Some((pos, ore)) = options
            .iter()
            .filter(|(_pos, ore)| ore == STONE)
            .min_by_key(|(pos, _ore)| (walk(*pos), pos.x, pos.y))
        {
            return Some((*pos, ore.clone()));
        }
    }
    // Nearest mine first; equal distance broken by higher vendor value.
    options.into_iter().min_by_key(|(pos, ore)| {
        let price = turn.vendor_prices.get(ore).copied().unwrap_or(1);
        (walk(*pos), std::cmp::Reverse(price), pos.x, pos.y)
    })
}
