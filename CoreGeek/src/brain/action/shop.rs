//! Shop: the buyer's craft — medicine, provisioning, the shop/vendor walks,
//! the buyer flow, trip-budget arithmetic, summon orders and vouchers
//! (issue #221 phase 2d).
//!
//! Pure move out of `day.rs`, unchanged except visibility. Phase 5 replaces
//! the intent-list shopping with the comment-1 fixed buy whitelist (base →
//! weapon → front wall → … → LargeRobotSummonOrder → Bomb → Boss) and the
//! WallFixer surplus caps; the trip-budget guards become the pioneer's
//! person-relative round arithmetic.

use std::collections::HashSet;

use crate::brain::day::walk_home;
use crate::brain::{economy, stand_cells, walk_toward};
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
    let critical = role.health * 10 < max_hp * crate::brain::night::WITHDRAW_HEALTH_TENTHS;
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

/// Walk to the nearest vendor stand: the SALE leg of a merged outing, walked
/// before the shop leg because the counter is paid in gold and the pack is
/// where the gold is. Same shape as `walk_to_shop` — the day has one way of
/// walking a role to a stand and this is not a second one.
pub(crate) fn walk_to_vendor(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
    let mut stands: Vec<Pos> = Vec::new();
    for vendor in turn.vendors() {
        stands.extend(stand_cells(turn, vendor));
    }
    if stands.is_empty() {
        return None;
    }
    walk_toward(turn, role, &stands, claimed)
}

/// Buy the first needed item: walk to the weapon shop, then buy. Buying is
/// the buyer's sole job — a carried summon order must never preempt a voucher,
/// upgrade or repair purchase.
pub(crate) fn buyer_flow(
    turn: &Turn,
    role: &Unit,
    shopping: &[economy::Need],
    pairs: &[(i64, i64)],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    let need = shopping.first()?;
    let mut stands: Vec<Pos> = Vec::new();
    for shop in turn.weapon_shops() {
        stands.extend(stand_cells(turn, shop));
    }
    if stands.is_empty() {
        return None;
    }
    if stands.iter().any(|pos| *pos == role.pos) {
        let price = turn
            .weapon_shop
            .get(&need.name)
            .copied()
            .unwrap_or(i64::MAX);
        let free_slots = role.capacity.saturating_sub(role.backpack.len() as i64);
        let affordable = if price > 0 { turn.gold / price } else { 0 };
        let num = need.num.min(free_slots).min(affordable);
        if num > 0 {
            crate::log::event(
                "buy",
                crate::log::ledger_record(turn, role, &RoleCommand::buy(&need.name, num)),
            );
            return Some(RoleCommand::buy(&need.name, num));
        }
        return None; // wait for gold or backpack space
    }
    // NOT WITHOUT TIME TO FINISH (issues #156-#160).
    //
    // The shop is the farthest errand on the board and this is the one walk on
    // it that has to end back at a POST: the buyer is the dedicated economy
    // worker, which is also a tower controller with a dusk deadline
    // (`preposition_round`). Nothing used to compare the two, so the walk was
    // started whenever the shopping list turned non-empty and step 4's lock-in
    // then turned the role around wherever it happened to be when
    // `in_day_round + dist_to_post` ran out.
    //
    // Measured, pk590730 (day 2): the purse jumps to 130 at day-round 23 — 表 2c
    // prints exactly that — the buyer walks twelve rounds toward the shop,
    // reaching (28,24), four cells short of the counter, and the lock turns it
    // around. 表 2a for that match has no voucher in it at all, 表 2b prints 75
    // rounds with an affordable voucher at the head of the list, and the base
    // falls on night 2 with score_3 at 10 of a possible 550. Reproduced in
    // `tests/day1_sim.rs` before this branch existed: the day ends with 144 gold
    // in the purse and not one `buy`.
    //
    // So the errand is time-boxed here instead: it may only be started if the
    // whole round trip — out to a stand, one round at the counter, back to the
    // post — still lands before dusk, which is the same budget step 4 measures
    // the lock-in against. A trip that fits at the start still fits at the
    // counter (walking out is what shrinks it), and step 4 waives the lock-in
    // for exactly that window, so a started trip is completed. A trip that does
    // not fit is not started: the role keeps today's work instead of spending
    // a dozen rounds being turned around with the purse unspent.
    if !shop_errand_fits(turn, role, pairs) || role.backpack_full() {
        crate::log::event(
            "shop_trip",
            serde_json::json!({
                "round": turn.round_no,
                "role": role.id,
                "decision": "no_time",
                "inDayRound": turn.in_day_round,
                "trip": shop_round_trip(turn, role, pairs),
                "need": need.name,
            }),
        );
        return None;
    }
    walk_toward(turn, role, &stands, claimed)
}

/// Rounds the buyer needs to complete a shop errand from where it stands: walk
/// out to a stand, spend one round at the counter, walk back to its post.
/// `None` when no shop stand exists on the board at all.
///
/// The geometry is the Chebyshev one the rest of the day planner budgets in
/// (`economy::shop_travel`, `walk_home`), so this agrees with the deadline
/// arithmetic rather than with a second estimate of it. It is deliberately not
/// a path length: a detour around the ring only makes the estimate optimistic,
/// and the number it feeds is a bound on when to STOP, not a promise of
/// arrival — the night recall still owns the post.
pub(crate) fn shop_round_trip(turn: &Turn, role: &Unit, pairs: &[(i64, i64)]) -> Option<i64> {
    let post = pairs
        .iter()
        .find(|(controller, _)| *controller == role.id)
        .and_then(|(_, tower)| turn.role_by_id(*tower))
        .map(|tower| tower.pos);
    let mut best: Option<i64> = None;
    for shop in turn.weapon_shops() {
        for stand in stand_cells(turn, shop) {
            let back = match post {
                Some(post) => chebyshev(stand, post) as i64,
                None => walk_home(turn, stand),
            };
            let trip = chebyshev(role.pos, stand) as i64 + 1 + back;
            best = Some(best.map_or(trip, |best: i64| best.min(trip)));
        }
    }
    best
}

/// Can the buyer still be back behind the wire before dusk?
///
/// See [`buyer_flow`] for what the answer decides. The bound is `DUSK_ROUND`
/// itself and not `HARD_SEAL_ROUND`: a trip that ends inside the dusk window is
/// the trip that holds the gate open (`wall_gate_open` naming the buyer on
/// every round of it), which is the hole `dusk_recall_round` exists to close.
pub(crate) fn shop_errand_fits(turn: &Turn, role: &Unit, pairs: &[(i64, i64)]) -> bool {
    shop_round_trip(turn, role, pairs)
        .map(|trip| turn.in_day_round + trip <= economy::DUSK_ROUND)
        .unwrap_or(false)
}

/// Is there a purchase to make this round, and can this role still make it?
///
/// One predicate for the three places that have to agree about the shop errand
/// (issues #156-#160), because the defect was exactly that they did not:
///
///   * step 7 uses it to let the errand outrank the sale. The buyer is the
///     dedicated economy worker, whose pack is full of SELLABLE ore by
///     construction — that is its job — so `should_sell` was true for most of
///     the day and the shop branch never ran. The one round it did run was the
///     round after a sale, which is the round the buyer is standing at the
///     VENDOR, the far corner of the board from the shop, with the pre-position
///     lock already firing.
///   * step 4 uses it to hold the lock-in off the walk it would otherwise cut
///     short (see the call site).
///   * [`buyer_flow`] uses it to refuse a walk that cannot be finished.
///
/// `budget.shopping` non-empty is the whole of "there is a purchase to make":
/// that list is computed from the gold in hand, so a sale is not what stands
/// between the buyer and the counter — the walk is. A full backpack is the one
/// thing that can still make the errand pointless (the goods need a slot), and
/// the sale that empties it is the next step of the same day.
pub(crate) fn shop_trip_worth_taking(
    turn: &Turn,
    role: &Unit,
    pairs: &[(i64, i64)],
    shopping: &[economy::Need],
) -> bool {
    !shopping.is_empty() && !role.backpack_full() && shop_errand_fits(turn, role, pairs)
}

/// The same decision, named — the `shopTrip` column of the `shopping` event.
///
/// The three "no" answers are the three ways the errand dies, and they call for
/// different fixes: `no_time` is a scheduling problem (this batch), `pack_full`
/// is a slot problem, and `sale_first` is the ore in the pack being worth more
/// than the trip it would displace. Without the split the next batch reads them
/// as one number again.
pub(crate) fn shop_trip_decision(
    turn: &Turn,
    role: &Unit,
    pairs: &[(i64, i64)],
    shopping: &[economy::Need],
) -> &'static str {
    if shopping.is_empty() {
        return "nothing_affordable";
    }
    if role.backpack_full() {
        return "pack_full";
    }
    if !shop_errand_fits(turn, role, pairs) {
        return "no_time";
    }
    "walk"
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
