//! Shop: the buyer's craft — medicine, provisioning, the shop walks, summon
//! orders, vouchers and the FIXED BUY WHITELIST of comment 1 §3 (issue #221
//! phase 5b).
//!
//! The pioneer is the buyer ([`crate::brain::role::pioneer`]'s shop step): the
//! legacy intent-list shopping and its pair-based trip budgets are gone, and
//! [`fixed_buy_list`] below is the whole purchasing policy — 清单外不买. The
//! only two entries that do not rank in the list are the special cases the
//! comment names: the WallFixer surplus conversion (capped per day) and the
//! Medicine a wounded role buys for itself ([`self_provision`]).

use std::collections::HashSet;

use super::base_layout;
use crate::brain::{combat, economy, stand_cells, walk_toward};
use crate::model::{chebyshev, Turn, Unit};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// Heal when badly hurt. A dead controller builds nothing and mans nothing,
/// so this outranks every other action. Threshold: below 30% HP.
pub(crate) fn use_medicine(role: &Unit) -> Option<RoleCommand> {
    let Some(max_hp) = max_hp(role) else {
        return None;
    };
    if role.health * 10 < max_hp * 3 && role.count_item("Medicine") > 0 {
        return Some(RoleCommand::use_item("Medicine"));
    }
    None
}

/// Max HP of a controllable role, or None when the unit is not one.
pub(crate) fn max_hp(role: &Unit) -> Option<i64> {
    match role.kind {
        crate::model::UnitKind::Worker => Some(220),
        crate::model::UnitKind::Pioneer => Some(200),
        _ => None,
    }
}

/// Buy a Medicine for THIS role. Medicine cannot be handed to a team-mate, so
/// a team-level purchase only ever equips the buyer — this is the errand that
/// equips everyone else, and for a role already at the shop it never costs a
/// detour.
///
/// A critically wounded role walks there. That is the whole recovery path, and
/// issue #22 measured what its absence costs: 20012 came out of the first night
/// at 20 HP, and because nothing in the game restores health except a Medicine
/// the wound never healed — HP frozen for 235 rounds, `night_withdraw` pulling
/// it off tower 20020 every night it was threatened, the gun silent behind it
/// and the withdrawn controller with no errand that could ever change its
/// state. A controller below the night withdrawal threshold has already lost
/// its gun; walking to the shop is the only action left that can give it back,
/// so unlike the pre-night top-up this errand is worth the trip. It still obeys
/// the hard dusk lock-in above (step 4 runs first), so a Medicine run can never
/// cost a manned tower.
pub(crate) fn self_provision(
    turn: &Turn,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
    may_travel: bool,
) -> Option<RoleCommand> {
    if role.count_item("Medicine") > 0 {
        return None;
    }
    let max_hp = max_hp(role)?;
    let hurt = role.health * 10 < max_hp * 8;
    let ready = turn.in_day_round >= economy::DUSK_ROUND - economy::READINESS_LEAD;
    if !hurt && !ready {
        return None;
    }
    let price = turn
        .weapon_shop
        .get("Medicine")
        .copied()
        .unwrap_or(i64::MAX);
    if price <= 0 || price == i64::MAX || turn.gold < price {
        return None;
    }
    if at_shop(turn, role.pos) {
        return Some(RoleCommand::buy("Medicine", 1));
    }
    // Below the night withdrawal threshold the role has nothing else to lose:
    // a potion is the only way back onto its gun. Above it the shop trip is not
    // worth abandoning the day's errand for, so the purchase waits until the
    // role happens to be there (the buyer, or the pre-night top-up above).
    let critical = role.health * 10 < max_hp * super::fight::WITHDRAW_HEALTH_TENTHS;
    if !critical || !may_travel {
        return None;
    }
    crate::log::event(
        "medicine_errand",
        serde_json::json!({"round": turn.round_no, "role": role.id, "health": role.health}),
    );
    walk_to_shop(turn, role, claimed)
}

pub(crate) fn at_shop(turn: &Turn, pos: Pos) -> bool {
    turn.weapon_shops()
        .iter()
        .flat_map(|shop| stand_cells(turn, *shop))
        .any(|stand| stand == pos)
}

/// Walk to the nearest weapon shop; None when no shop stand is known.
pub(crate) fn walk_to_shop(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
    let mut stands: Vec<Pos> = Vec::new();
    for shop in turn.weapon_shops() {
        stands.extend(stand_cells(turn, shop));
    }
    if stands.is_empty() {
        return None;
    }
    walk_toward(turn, role, &stands, claimed)
}

/// THE FIXED BUY WHITELIST of comment 1 §3 (issue #221 phase 5b), in the
/// comment's own order — 清单外不买, nothing outside this list is ever bought:
///
///   1. 基地券, and the base's health shortfall lifts it to the very front
///      (基地血量不足优先);
///   2. 武器券1, sized by the L1 weapons standing (按当前L1武器数);
///   3. 正面墙券1, sized by the L1 walls on the enemy-facing front;
///   4. 武器券2, sized by the L2 weapons;
///   5. 正面墙券2, sized by the front's L2 walls;
///   6. 侧面墙券1, sized by the L1 walls on the top/bottom rows;
///   7. 基地券1;  8. 基地券2 — the station's level ladder;
///   9. LargeRobotSummonOrder — 买完立即使用 (the pioneer burns it the next
///      round, [`burn_summon_order`]);
///  10. Bomb;  11. BossRobotSummonOrder.
///
/// The two special cases deliberately do NOT rank in the list: the WallFixer
/// surplus conversion ([`wall_fixer_surplus`], capped 5/10 by day) and the
/// Medicine a wounded role buys for itself ([`self_provision`]). Affordability
/// is not decided here — the list is the WANT, exactly like the old
/// `Budget::intent`; [`buy_first_affordable`] at the counter spends it against
/// the gold B has sold so far, first affordable entry wins.
pub(crate) fn fixed_buy_list(turn: &Turn) -> Vec<economy::Need> {
    fn need(name: &str, num: i64, rank: i32, reason: &'static str) -> economy::Need {
        economy::Need {
            name: name.to_string(),
            num,
            priority: rank,
            latest_round: 0,
            value: 0,
            combat_per_gold: 0.0,
            reason,
        }
    }
    let mut out: Vec<economy::Need> = Vec::new();
    let mut rank = 0;
    let mut push = |name: &str, num: i64, reason: &'static str, out: &mut Vec<economy::Need>| {
        if num > 0 {
            rank += 1;
            out.push(need(name, num, rank, reason));
        }
    };
    // 1. 基地券 — a station below its level's HP table is the emergency the
    //    comment puts first: the upgrade is the only heal the base has.
    if let Some(station) = turn.station() {
        if station.alive() && station.health < combat::station_max_hp(station.level) {
            let voucher = match station.level {
                1 => Some("StationUpgradeVoucher1"),
                2 => Some("StationUpgradeVoucher2"),
                _ => None,
            };
            if let Some(name) = voucher {
                push(name, 1, "station_hurt", &mut out);
            }
        }
    }
    // 2-6. Weapons and walls, by the standing count of the level each voucher
    //    upgrades. Front (朝向敌人的一面) before side — the comment's order.
    let l1_guns = turn
        .towers()
        .iter()
        .filter(|tower| tower.alive() && tower.level == 1)
        .count() as i64;
    let l2_guns = turn
        .towers()
        .iter()
        .filter(|tower| tower.alive() && tower.level == 2)
        .count() as i64;
    let front = base_layout::front_wall_cells(turn);
    let side = base_layout::side_wall_cells(turn);
    let walls_on = |cells: &[Pos], level: i32| {
        turn.walls()
            .iter()
            .filter(|wall| wall.alive() && wall.level == level && cells.contains(&wall.pos))
            .count() as i64
    };
    push("WeaponUpgradeVoucher1", l1_guns, "weapon_l1", &mut out);
    push(
        "WallUpgradeVoucher1",
        walls_on(&front, 1),
        "front_wall_l1",
        &mut out,
    );
    push("WeaponUpgradeVoucher2", l2_guns, "weapon_l2", &mut out);
    push(
        "WallUpgradeVoucher2",
        walls_on(&front, 2),
        "front_wall_l2",
        &mut out,
    );
    push("WallUpgradeVoucher1", walls_on(&side, 1), "side_wall_l1", &mut out);
    // 7/8. The station's level ladder, independent of the health emergency.
    if let Some(station) = turn.station() {
        match station.level {
            1 => push("StationUpgradeVoucher1", 1, "station_ladder", &mut out),
            2 => push("StationUpgradeVoucher2", 1, "station_ladder", &mut out),
            _ => {}
        }
    }
    // 9-11. The wave items, in the comment's order.
    push("LargeRobotSummonOrder", 1, "summon_large", &mut out);
    push(
        "Bomb",
        (2 - economy::stock_of(turn, "Bomb")).max(0),
        "bomb",
        &mut out,
    );
    push("BossRobotSummonOrder", 1, "summon_boss", &mut out);
    // Only what the shop actually sells: an entry with no price is a want no
    // gold can ever satisfy, and it would hold the `buy_deadline` sync hostage.
    out.retain(|need| turn.weapon_shop.contains_key(&need.name));
    out
}

/// The WallFixer surplus conversion (comment 1 §3's special case, 不参与排序):
/// leftover economy turns into repair kits up to a TEAM stock of 5 through day
/// 5 and 10 from day 6 — the later nights chew the ring harder, and by then
/// the base fund matters less than the wall holding it. `None` at the cap.
pub(crate) fn wall_fixer_surplus(turn: &Turn) -> Option<economy::Need> {
    let cap = if turn.day <= 5 { 5 } else { 10 };
    let num = cap - economy::stock_of(turn, "WallFixer");
    (num > 0).then(|| economy::Need {
        name: "WallFixer".to_string(),
        num,
        priority: i32::MAX,
        latest_round: 0,
        value: 0,
        combat_per_gold: 0.0,
        reason: "surplus_fixer",
    })
}

/// At the counter: buy the FIRST AFFORDABLE entry of the whitelist, and only
/// when the ranked list has nothing affordable this round, convert the surplus
/// into WallFixer kits. Affordability is measured against the SPENDABLE gold —
/// the purse minus [`economy::spendable_reserve`], so the third-tower fund and
/// the treasure offering stay untouchable exactly as the legacy budget kept
/// them (P0-4, P2-2).
pub(crate) fn buy_first_affordable(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    list: &[economy::Need],
) -> Option<RoleCommand> {
    let spendable = (turn.gold - economy::spendable_reserve(turn, state)).max(0);
    let free_slots = role.capacity.saturating_sub(role.backpack.len() as i64);
    let mut buyable = list.iter().filter_map(|need| {
        let price = turn.weapon_shop.get(&need.name).copied()?;
        let num = need.num.min(free_slots).min(spendable / price.max(1));
        (price > 0 && num > 0).then(|| (need.name.clone(), num))
    });
    let (name, num) = buyable
        .next()
        .or_else(|| {
            // 盈余转换: the ranked list is out of reach this round; kits are.
            let fixer = wall_fixer_surplus(turn)?;
            let price = turn.weapon_shop.get(&fixer.name).copied()?;
            let num = fixer.num.min(free_slots).min(spendable / price.max(1));
            (price > 0 && num > 0).then(|| (fixer.name.clone(), num))
        })?;
    let cmd = RoleCommand::buy(&name, num);
    crate::log::event("buy", crate::log::ledger_record(turn, role, &cmd));
    Some(cmd)
}

/// Use a carried robot-summon order against the enemy (harassment), one per
/// round, capped by the daily summon budget. Kept out of `buyer_flow` so a
/// voucher purchase is never delayed by it.
pub(crate) fn burn_summon_order(state: &mut BotState, role: &Unit) -> Option<RoleCommand> {
    const ORDERS: [&str; 4] = [
        "BossRobotSummonOrder",
        "LargeRobotSummonOrder",
        "MiddleRobotSummonOrder",
        "SmallRobotSummonOrder",
    ];
    for order in ORDERS {
        if role.count_item(order) > 0 && state.summon_orders_today < 10 {
            state.consume_summon_order();
            state.harass_done_today = true;
            return Some(RoleCommand::use_item(order));
        }
    }
    None
}

/// Walk to a building matching a carried voucher and apply it.
pub(crate) fn voucher_flow(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
    for voucher in economy::held_vouchers(role) {
        let Some(target) = economy::voucher_target(turn, role, &voucher) else {
            continue;
        };
        if chebyshev(role.pos, target) <= 1 {
            return Some(RoleCommand::use_item_at(&voucher, target));
        }
        let stands = stand_cells(turn, target);
        return walk_toward(turn, role, &stands, claimed);
    }
    None
}
