//! The wall worker (Worker A) mainline — issue #221 phase 4b, spec: comment 1
//! §4/§5.
//!
//! The person comment 1 describes: builds and mans the three weapons, mines
//! the day's stone quota, builds the wall in one direction in the fixed order
//! with the back four cells left as the permanent entrance, repairs breaches,
//! and spends only the rounds those duties leave over on surplus ore and
//! selling. Every deadline is measured against A's OWN walk to the operator
//! cell — the single post adjacent to all three weapon sites — not against a
//! paired tower's distance (design D1-D3: the scheduler is organized by
//! person, so the person's post sets the budget).
//!
//! ONE priority chain, walked every day round:
//!
//! 0. **HP first** — drink Medicine; buy one when hurt or topping up for the
//!    night (`self_provision`'s readiness window); use a carried voucher;
//! 1. **post guard** — once A's own walk to the operator cell no longer fits
//!    before dusk, every far errand ends and the guns get manned. Only four
//!    things outrank it, and each ends at or next to the base: a wall line
//!    still holding stone (the ring's tail), a vendor counter A already
//!    stands at, a shop errand that finishes before dusk, and the dusk
//!    cash-out sell band. Adjacent gaps are laid on the way in; at the post a
//!    tower that can be built from it is one command, not an errand —
//!    otherwise holding IS the plan;
//! 2. **weapons** — a tower build when the gold and the slot exist, before
//!    any wall (a wall with no gun behind it delays the night, it does not
//!    stop it);
//! 3. **the wall quota** — batch the load, walk the line, lay gaps in
//!    comment 1 §4's fixed order, trap veto until the hard seal;
//! 4. **repair** — WallFixer on the weakest breach;
//! 5. **sell** — only when a trigger fires (pack full, the buy deadline, the
//!    peddler near, the dusk cash-out band): 卖矿越晚越好;
//! 6. **mine** — quota stone first on a single-vein latch (10 collects,
//!    comment 1 §5); surplus ore only while the trip still gets A back to the
//!    post before dusk;
//! 7. **summon orders** — burn a carried order when nothing else wants the
//!    round. (The buyer's errand died in phase 5b: the pioneer is the team's
//!    only buyer, so A never purchases anything.)
//!
//! # Night (phase 4b-3, design D14-D16)
//!
//! [`plan_night`] is the single-operator L-shape: ONE post — the operator
//! cell, adjacent by construction to all three weapon sites — from which A
//! fires the whole salvo (attacks are tower-addressed commands that name the
//! controller, so three guns cost A no personal command), mends stone on the
//! reload rounds, walks back to the post while robots live, and mines the
//! swept board (comment 1 §3.4: 所有己方机器人消灭后出去采矿). The pioneer
//! stands wall-repair duty inside the ring ([`crate::brain::role::pioneer`],
//! Q5's default — not A's backup gunner; `BotState::pioneer_night_backup` is
//! the switch), and B mines all night outside it on its own mainline. The
//! legacy pairing loop is deleted.

use std::collections::HashSet;

use super::economy_worker::{
    night_safe, pick_vein, NEAR_VENDOR_ROUNDS, NIGHT_ROBOT_CLEARANCE, VEIN_COLLECT_LIMIT,
};
use super::super::action::base_layout;
use super::super::action::build::{build_or_walk, repair_flow};
use super::super::action::fight::{
    cooldown_repair, hostile_wave, night_medicine, update_withdraw_holdout, withdrawing,
};
use super::super::action::geometry::wall_layer;
use super::super::action::mine::stone_covered;
use super::super::action::sell::sell_flow;
use super::super::action::shop::{burn_summon_order, self_provision, use_medicine, voucher_flow};
use super::super::day::{
    self, roles_can_reach, stone_batch, wall_trip_overdue, wall_would_trap, RoundCtx,
    HARD_SEAL_ROUND,
};
use super::super::route::trip_rounds;
use super::super::{
    break_out, combat, economy, interior_cells, shelter, stand_cells, walk_or_remove_wall,
    walk_toward, Plan,
};
use crate::model::{chebyshev, Turn, Unit, STONE};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// Slack kept on top of A's walk to the operator cell before a deadline
/// counts as missed — the same three rounds of detour allowance the legacy
/// `preposition_round` held, re-based on the person instead of the pair.
pub(crate) const POST_MARGIN: i64 = 3;

/// The last rounds before the post deadline in which the dusk cash-out fires:
/// ore whose "sell and still reach the post" trip fits inside the band is
/// sold now, because after the band the post guard owns A anyway and the ore
/// would ride into the night unsold.
const CASHOUT_LEAD: i64 = 3;

/// Plan Worker A's day round.
///
/// `ctx_owned` is the context roster [`day::round_context`] was computed
/// with — the pairing and the trap vetoes all read the same list, and in
/// this phase it is still `[B]` alone.
pub(crate) fn plan_day(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    ctx: &RoundCtx,
    ctx_owned: &[i64],
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    if role.alive() {
        if let Some(cmd) = mainline(turn, state, role, ctx, ctx_owned, claimed) {
            plan.push(role.id, cmd);
        }
    }
    // The per-person backstop, identical to the branch the legacy `plan_owned`
    // closing loop ran for this role when every step above declined (its two
    // hold-skip exemptions — task point, altar — are pioneer-only). Declining
    // is fine, standing still is not: issue #13's "0 角色站桩闲置". The
    // fallback ignores `claimed` by design, and it declines inside the ring,
    // so holding at the operator cell stays a hold.
    if !plan.commands.contains_key(&role.id) {
        if let Some(cmd) = day::fallback_toward_station(turn, role) {
            plan.push(role.id, cmd);
        }
    }
}

/// The single priority chain. Returns at most one command — the first guard
/// that fires owns the round.
fn mainline(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    ctx: &RoundCtx,
    ctx_owned: &[i64],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    // Step 0 — HP first (comment 1 §5: 有药吃, 有钱买, 无钱先采矿 — the last
    // is the fall-through, not a branch). `self_provision` unconditional, the
    // legacy 7b semantics: it covers the hurt buy AND the pre-night readiness
    // top-up, and declines by itself while Medicine is held or unaffordable.
    if let Some(cmd) = use_medicine(role) {
        return Some(cmd);
    }
    if let Some(cmd) = self_provision(turn, role, claimed, true) {
        return Some(cmd);
    }
    if let Some(cmd) = voucher_flow(turn, role, claimed) {
        return Some(cmd);
    }
    // Step 1 — the post guard (design D1-D3's person-relative replacement of
    // the pair-based pre-position lock): the deadline is A's OWN walk to the
    // operator cell, the one stand adjacent to all three weapon sites.
    let post = base_layout::operator_cell(turn);
    let deadline_hit = post.is_some_and(|post| {
        turn.in_day_round + trip_rounds(turn, role.pos, post) + POST_MARGIN
            >= economy::DUSK_ROUND
    });
    if deadline_hit {
        // The three exceptions, each worth the round it spends and each ending
        // at or next to the base — the legacy lock's counter/ring-tail
        // exemptions, plus the cash-out band that replaces the legacy dusk
        // sell deadline (design D2). The shop-errand exemption died with the
        // buyer nomination (phase 5b): the pioneer owns the counter now.
        let ring_tail = role.count_item(STONE) > 0
            && !ctx.wall_gaps.is_empty()
            && turn.in_day_round < economy::DUSK_ROUND;
        let at_counter = turn
            .vendors()
            .iter()
            .any(|vendor| chebyshev(role.pos, *vendor) == 1)
            && economy::sellable_ores(turn, role, ctx.stone_demand) > 0;
        let cashout = sell_trigger(turn, state, role, ctx) == Some("dusk_cashout");
        if !ring_tail && !at_counter && !cashout {
            // Lay what the walk home passes: a gap adjacent this round is a
            // wall placed, not a round lost.
            if let Some(cmd) = place_adjacent_wall(turn, state, role, ctx, ctx_owned, claimed) {
                return Some(cmd);
            }
            if let Some(post) = post {
                if role.pos != post {
                    // `None` (no route) hands the round to the backstop,
                    // which keeps the demolition escape hatch.
                    return walk_toward(turn, role, &[post], claimed);
                }
                // At the post: a tower buildable from this very cell is one
                // command, not an errand — the operator cell is adjacent to
                // all three sites. Otherwise hold; holding at the post is the
                // plan, not idleness (the backstop declines inside too).
                return build_tower(turn, state, role, ctx, claimed, true);
            }
            // No station on the board: no post to hold, carry on.
        }
    }
    // Step 2 — weapons before walls (comment 1 §5: 建造并维护三个武器).
    if let Some(cmd) = build_tower(turn, state, role, ctx, claimed, false) {
        return Some(cmd);
    }
    // Step 3 — the day's wall quota in the fixed one-direction order.
    if let Some(cmd) = build_wall_step(turn, state, role, ctx, ctx_owned, claimed) {
        return Some(cmd);
    }
    // Step 4 — repair what the night took (WallFixer-gated inside).
    if let Some(cmd) = repair_flow(turn, state, role, &ctx.wall_gaps, claimed) {
        return Some(cmd);
    }
    // Step 5 — surplus into gold. 卖矿越晚越好 (comment 1 §5): the trigger
    // set below is the whole definition of "late enough".
    if let Some(trigger) = sell_trigger(turn, state, role, ctx) {
        crate::log::event(
            "economy_trigger",
            serde_json::json!({
                "round": turn.round_no,
                "role": role.id,
                "trigger": trigger,
            }),
        );
        if economy::sellable_ores(turn, role, ctx.stone_demand) <= 0 {
            // Nothing the vendor takes (or a full pack of it): dump the
            // cheapest droppable and keep the loop turning.
            return economy::discard_command(turn, state, role, ctx.stone_demand)
                .or_else(|| sell_flow(turn, state, role, ctx.stone_demand, claimed));
        }
        return sell_flow(turn, state, role, ctx.stone_demand, claimed);
    }
    // Step 6 — mine: the quota stone first, surplus ore only on a trip that
    // still gets A back to the post before dusk.
    if turn.in_day_round < economy::DUSK_ROUND {
        if role.backpack_full() {
            if let Some(cmd) = economy::discard_command(turn, state, role, ctx.stone_demand) {
                return Some(cmd);
            }
        } else if let Some(cmd) = mine_step(turn, state, role, ctx, claimed) {
            return Some(cmd);
        }
    }
    // Step 7 — a carried summon order burns only when nothing else wants the
    // round. A buys nothing any more: phase 5b made the pioneer the team's
    // only buyer and deleted the nomination this step used to ride on.
    burn_summon_order(state, role)
}

/// Build an empty weapon slot. `adjacent_only` is the post-guard form: the
/// operator cell is adjacent to all three sites by construction
/// ([`base_layout`]), so a late tower is paid for without ever leaving the
/// post; the full form walks to the site.
fn build_tower(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    ctx: &RoundCtx,
    claimed: &mut HashSet<Pos>,
    adjacent_only: bool,
) -> Option<RoleCommand> {
    if !economy::may_build_weapon(turn, state) || turn.in_day_round >= economy::DUSK_ROUND {
        return None;
    }
    for (site, name) in &ctx.tower_gaps {
        if claimed.contains(site) {
            continue;
        }
        if adjacent_only && (chebyshev(role.pos, *site) != 1 || !turn.is_land(*site)) {
            continue;
        }
        if let Some(cmd) = build_or_walk(turn, role, *site, name, claimed) {
            claimed.insert(*site);
            return Some(cmd);
        }
    }
    None
}

/// May this gap be closed right now? The trap veto holds until
/// `HARD_SEAL_ROUND`; past it the night outranks a straggler's way home —
/// the same override the legacy dusk seal and B's stone delivery run.
fn safe_gap(
    turn: &Turn,
    ctx: &RoundCtx,
    state: &BotState,
    ctx_owned: &[i64],
    site: Pos,
) -> bool {
    !wall_would_trap(turn, &ctx.pairs, state, site, ctx_owned)
        || turn.in_day_round >= HARD_SEAL_ROUND
}

/// The placement itself, with the day's tally and the log line the wall
/// count is audited by (the legacy `wall_build` event minus the at-post
/// flag, which the fixed post made meaningless).
fn place_wall(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    site: Pos,
    claimed: &mut HashSet<Pos>,
) -> RoleCommand {
    claimed.insert(site);
    state.walls_built_today = state.walls_built_today.saturating_add(1);
    state.walled_cells_today.insert(site);
    crate::log::event(
        "wall_build",
        serde_json::json!({
            "role": role.id,
            "target": site,
            "layer": wall_layer(turn, site),
            "stone": role.count_item(STONE),
        }),
    );
    RoleCommand::build(site, "wall")
}

/// Lay a wall on a gap adjacent this round, in comment 1 §4's fixed order —
/// the one-command form of the wall step, used wherever the round is already
/// spoken for (the post guard's walk in) or the load is complete.
fn place_adjacent_wall(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    ctx: &RoundCtx,
    ctx_owned: &[i64],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if role.count_item(STONE) == 0 {
        return None;
    }
    ctx.wall_gaps
        .iter()
        .copied()
        .find(|site| {
            !claimed.contains(site)
                && chebyshev(role.pos, *site) == 1
                && safe_gap(turn, ctx, state, ctx_owned, *site)
        })
        .map(|site| place_wall(turn, state, role, site, claimed))
}

/// The day's wall quota (comment 1 §4/§5): batch the load, walk the line,
/// lay in the fixed order. The legacy shared wall duty's arithmetic, kept
/// whole — the batch release, the reserve floor, the trap veto — with the
/// `on_wall_duty` roster gate gone: there is one wall worker now, and the
/// quota is its own.
fn build_wall_step(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    ctx: &RoundCtx,
    ctx_owned: &[i64],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if ctx.wall_gaps.is_empty() || role.count_item(STONE) == 0 {
        return None;
    }
    if (state.walled_cells_today.len() as i64) >= ctx.wall_cap {
        return None;
    }
    // Nobody may be sealed out of their own base by the line going up
    // (P0-3); the hard-seal override lives in `safe_gap`, per cell.
    if !roles_can_reach(turn, &ctx.pairs) {
        return None;
    }
    let gaps = ctx.wall_gaps.len() as i64;
    let batch = stone_batch(turn, ctx.wall_gaps.len());
    let team_stone = economy::team_ores(turn, STONE);
    // The load is complete when the pack covers the batch, when the team
    // pool already covers every visible gap (more digging is waste), when
    // the day owes nothing, or when the day is too short for another
    // dig round-trip.
    let load_complete = role.count_item(STONE) as i64 >= batch
        || team_stone >= gaps
        || ctx.stone_demand <= 0
        || turn.in_day_round >= economy::DUSK_ROUND - 12
        || wall_trip_overdue(turn, role, &ctx.wall_gaps);
    let reserve_met = team_stone >= ctx.stone_demand;
    // Place what is adjacent when no more stone is diggable, or when both
    // the load and the reserve are complete: a walk to a farther cell would
    // only delay a placement available this very round.
    let more_stone = pick_vein(turn, state, role, true, 0, claimed, true).is_some();
    if !more_stone || (load_complete && reserve_met) {
        if let Some(cmd) = place_adjacent_wall(turn, state, role, ctx, ctx_owned, claimed) {
            return Some(cmd);
        }
    }
    // Commit to the line once the load is complete AND the team reserve is
    // met: first safe gap in the fixed order, claim only what the walk
    // actually committed to (the legacy pattern — a claim left behind by a
    // failed walk would hide the gap from every later step this round).
    if load_complete && reserve_met {
        for site in &ctx.wall_gaps {
            if claimed.contains(site) || !safe_gap(turn, ctx, state, ctx_owned, *site) {
                continue;
            }
            if let Some(cmd) = build_or_walk(turn, role, *site, "wall", claimed) {
                claimed.insert(*site);
                if cmd.action == "build" {
                    state.walls_built_today = state.walls_built_today.saturating_add(1);
                    state.walled_cells_today.insert(*site);
                    crate::log::event(
                        "wall_build",
                        serde_json::json!({
                            "role": role.id,
                            "target": *site,
                            "layer": wall_layer(turn, *site),
                            "stone": role.count_item(STONE),
                        }),
                    );
                }
                return Some(cmd);
            }
        }
    }
    None
}

/// A's sell triggers — comment 1 §6's set, cut to the wall worker's own:
/// T4 the full pack, T1 the team's buy deadline, T3 the peddler within
/// [`NEAR_VENDOR_ROUNDS`], and the dusk cash-out — the "sell and still
/// reach the post" band, which is the person-relative form of the legacy
/// dusk sell deadline (design D2: the post, not the pair's tower, is what
/// the day has to end at). T2/T5 are B's team-support triggers and stay B's.
fn sell_trigger(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    ctx: &RoundCtx,
) -> Option<&'static str> {
    // T4 — backpack full: MUST sell (or dump what cannot be sold).
    if economy::free_slots(role) <= 0 {
        return Some("backpack_full");
    }
    if economy::sellable_ores(turn, role, ctx.stone_demand) <= 0 {
        return None;
    }
    // T1 — the buyer's synced deadline: the ore must be gold before the
    // counter is staffed, or the purchase waits a whole loop.
    if let Some(deadline) = state.buy_deadline {
        if turn.round_no + economy::vendor_travel(turn, role.pos) >= deadline {
            return Some("buy_deadline");
        }
    }
    // T3 — the peddler is already within 3 rounds: sell on the way past.
    if economy::vendor_travel(turn, role.pos) <= NEAR_VENDOR_ROUNDS {
        return Some("near_vendor");
    }
    // The dusk cash-out: the last rounds in which a sell trip still gets A
    // back to the operator cell before dark. Outside the band the trip is
    // either comfortably early (the quota owns the day) or already impossible
    // (step 1's post guard owns A), and the ore rides one more loop.
    if let Some(post) = base_layout::operator_cell(turn) {
        let rounds_left = (economy::DUSK_ROUND - turn.in_day_round).max(0);
        let via_vendor = turn
            .vendors()
            .iter()
            .map(|vendor| {
                economy::vendor_travel(turn, role.pos) + trip_rounds(turn, *vendor, post)
            })
            .min();
        if let Some(via) = via_vendor {
            if via + POST_MARGIN <= rounds_left
                && rounds_left - (via + POST_MARGIN) <= CASHOUT_LEAD
            {
                return Some("dusk_cashout");
            }
        }
    }
    None
}

/// The mine half of A's loop: the day's quota stone first (comment 1 §5:
/// 当日配额 = 建墙需求), surplus ore after — but only on a trip that still
/// gets A back to the operator cell before dusk.
fn mine_step(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    ctx: &RoundCtx,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    let stone_short = !stone_covered(turn, ctx.stone_demand);
    let (vein, ore) = latched_vein(turn, state, role, ctx, claimed)
        .or_else(|| {
            let pick = if stone_short {
                pick_vein(turn, state, role, true, 0, claimed, true)
            } else {
                pick_vein(turn, state, role, false, 0, claimed, false)
                    .filter(|(pos, _)| surplus_trip_fits(turn, role, *pos))
            };
            pick.map(|(pos, ore)| {
                state.wall_vein_latch = Some(pos);
                (pos, ore)
            })
        })?;
    run_vein(turn, state, role, vein, &ore, stone_short, claimed)
}

/// The shared mine executor — both the day step and the swept-night step
/// end here: collect when adjacent, else walk, announcing the pick with the
/// legacy `mine_pick` record (the fields issues #12-#17 read, so the wall
/// line's failure mode stays audible in the log).
fn run_vein(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    vein: Pos,
    ore: &str,
    stone_short: bool,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if chebyshev(role.pos, vein) == 1 {
        *state.vein_hits.entry(vein).or_insert(0) += 1;
        return Some(RoleCommand::collect(vein));
    }
    let stands = stand_cells(turn, vein);
    let cmd = walk_toward(turn, role, &stands, claimed)?;
    claimed.insert(vein);
    crate::log::event(
        "mine_pick",
        serde_json::json!({
            "round": turn.round_no,
            "role": role.id,
            "wantStone": stone_short,
            "stoneErrand": stone_short && ore == STONE,
            "ore": ore,
            "mine": vein,
        }),
    );
    Some(cmd)
}

/// A's latched vein, revalidated every round (the same contract as B's, a
/// separate field — see [`BotState::wall_vein_latch`]): still on the map,
/// not mined out, not claimed this round, not blacked out — and still the
/// right ORE for the errand: a stone latch dies the moment the team pool
/// covers the demand; an ore latch dies when stone goes short again or the
/// surplus trip no longer fits before the post deadline.
fn latched_vein(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    ctx: &RoundCtx,
    claimed: &HashSet<Pos>,
) -> Option<(Pos, String)> {
    let pos = state.wall_vein_latch?;
    if state.vein_hits.get(&pos).copied().unwrap_or(0) >= VEIN_COLLECT_LIMIT {
        return None;
    }
    if claimed.contains(&pos) {
        return None;
    }
    let (_, ore) = turn.all_mines().into_iter().find(|(p, _)| *p == pos)?;
    if state.ore_on_outage(&ore, turn.day) {
        return None;
    }
    let stone_short = !stone_covered(turn, ctx.stone_demand);
    if ore == STONE {
        if !stone_short {
            return None;
        }
    } else if stone_short || !surplus_trip_fits(turn, role, pos) {
        return None;
    }
    Some((pos, ore))
}

/// Can a surplus ore trip still get A back to the operator cell before
/// dusk? The guard that makes 卖矿越晚越好 safe: A earns only while the walk
/// out and the walk home fit inside the post deadline — the person-relative
/// replacement for the legacy `gun_deadline` arithmetic (design D1).
fn surplus_trip_fits(turn: &Turn, role: &Unit, vein: Pos) -> bool {
    base_layout::operator_cell(turn).is_some_and(|post| {
        turn.in_day_round
            + trip_rounds(turn, role.pos, vein)
            + trip_rounds(turn, vein, post)
            + POST_MARGIN
            <= economy::DUSK_ROUND
    })
}

/// Plan Worker A's night round — the single-operator L-shape (issue #221
/// phase 4b-3, design D14-D16, comment 1 §3.4: 晚上操作武器…所有己方机器人
/// 消灭后出去采矿).
///
/// ONE post — the operator cell, adjacent by construction to all three
/// weapon sites — operates the whole L. The chain:
///
/// 0. **survival** — a controller about to die is bait, not a gunner:
///    withdraw inside the ring, hold there, guns dark (issue #20);
/// 1. **the night potion** — the legacy firing filter kept towers silent in
///    a round the controller drank, so healing outranks the whole salvo;
/// 2. **the whole salvo** — every gun A can fire this round, most-pressured
///    first on one shared damage simulation (the legacy loop's cross-tower
///    overkill prevention, now one controller for all of them). Attacks are
///    TOWER-addressed commands naming the controller, so a full salvo costs
///    A no personal command. A reload round the gun cannot fire anyway is a
///    masonry round (`cooldown_repair`), never traded against a shot;
/// 3. **recall** — no gun fired and A is off-post while robots live: walk
///    back on the legacy three-rung ladder (claims, no-claims, `break_out`);
/// 4. **the swept-night mine** — no hostile robot left alive: A goes out
///    and mines (D15/D16's reversal of the no-night-mining rule);
/// 5. **hold** — no threat, no ore, no walk: the post IS the plan.
///
/// Every path emits the legacy `night_debug` record so 表 6a/6b keep reading
/// the same schema.
pub(crate) fn plan_night(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    if !role.alive() {
        return;
    }
    // The P1-4 hysteresis set survives the pairing loop that used to read
    // it: maintained here, it keeps a once-downed operator off the guns
    // until it has healed or the board has been quiet for a spell.
    update_withdraw_holdout(turn, state);
    let mut towers = turn.towers();
    towers.sort_by_cached_key(|tower| std::cmp::Reverse(combat::threat_load(turn, tower)));

    // Step 0 — survival before position. `withdrawing` is false for a
    // Medicine carrier and the holdout branch re-checks the potion, so a
    // healer always reaches step 1 instead of fleeing.
    let wounded = withdrawing(turn, role)
        || (role.count_item("Medicine") == 0 && state.withdraw_holdout.contains(&role.id));
    if wounded {
        night_debug(
            turn,
            towers
                .iter()
                .map(|tower| {
                    serde_json::json!({
                        "tower": tower.id,
                        "controller": role.id,
                        "reason": "controller_withdrawn",
                        "hp": role.health,
                    })
                })
                .collect(),
        );
        if !interior_cells(turn).contains(&role.pos) {
            shelter(turn, role, claimed, plan);
        }
        return; // already inside the ring: hold, do not pace
    }
    // Step 1 — the night potion: towers wait for the drink.
    if let Some(cmd) = night_medicine(turn, role) {
        night_debug(
            turn,
            towers
                .iter()
                .map(|tower| {
                    serde_json::json!({
                        "tower": tower.id,
                        "controller": role.id,
                        "reason": "controller_healing",
                    })
                })
                .collect(),
        );
        plan.push(role.id, cmd);
        return;
    }

    // Step 2 — the whole salvo.
    let policy = state.coach.policy();
    let mut sim = combat::init_sim(turn);
    let firing: Vec<(i64, i64)> = towers
        .iter()
        .filter(|tower| tower.cooldown == 0 && chebyshev(role.pos, tower.pos) <= 1)
        .map(|tower| (tower.id, role.id))
        .collect();
    let aims = combat::plan_round(&policy, turn, &firing);
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut fired_any = false;
    let mut mend: Option<RoleCommand> = None;
    for tower in &towers {
        let adjacent = chebyshev(role.pos, tower.pos) <= 1;
        let mut reason = "controller_walking"; // refined by steps 3/4 below
        let mut fired = 0usize;
        let mut enemy_fire = false;
        if adjacent && tower.cooldown == 0 {
            match combat::choose_attack_kind_aimed(
                &policy,
                turn,
                tower,
                &mut sim,
                aims.get(&tower.id).copied(),
            ) {
                Some((targets, kind)) => {
                    fired = targets.len();
                    enemy_fire = kind == combat::TargetKind::EnemyAssets;
                    if enemy_fire {
                        // Coach credit: damage on the enemy base is not
                        // evidence about our summons.
                        state.coach.note_enemy_fire();
                    }
                    plan.push(tower.id, RoleCommand::attack(role.id, targets));
                    fired_any = true;
                    reason = "fired";
                }
                None if combat::spare_firepower(turn) => reason = "no_target_in_range",
                None => {
                    // Every robot hunting us is out of every live tower's
                    // reach: hold the opportunistic fire (issue #7's 有余力时)
                    // and spend the round on the wall instead.
                    reason = "no_target_reserved_for_robots";
                    if mend.is_none() {
                        if let Some(cmd) = cooldown_repair(turn, role) {
                            mend = Some(cmd);
                            reason = "mending";
                        }
                    }
                }
            }
        } else if adjacent {
            // Reload is masonry time (issues #126-#130).
            reason = "cooldown";
            if mend.is_none() {
                if let Some(cmd) = cooldown_repair(turn, role) {
                    mend = Some(cmd);
                    reason = "mending";
                }
            }
        }
        let mut row = serde_json::json!({
            "tower": tower.id,
            "controller": role.id,
            "reason": reason,
        });
        if fired > 0 {
            row["fired"] = serde_json::json!(fired);
            if enemy_fire {
                row["enemyAssets"] = serde_json::json!(true);
            }
        }
        rows.push(row);
    }
    if fired_any {
        // The salvo IS the round: the operator holds (legacy discipline — no
        // walking and no masonry in a round a gun fired, and holding costs
        // nothing because attacks are tower-addressed).
        night_debug(turn, rows);
        return;
    }
    if let Some(cmd) = mend {
        night_debug(turn, rows);
        plan.push(role.id, cmd);
        return;
    }

    // Step 3 — recall (issues #121-#130): no gun fired and hostile robots
    // live. The legacy three-rung ladder, so a teammate's committed move
    // never freezes the walk and a pocket never holds A until dawn.
    let post = base_layout::operator_cell(turn);
    if hostile_wave(turn) {
        let at_post = post.is_some_and(|post| role.pos == post);
        if let (Some(post), false) = (post, at_post) {
            let stands = [post];
            let mut moved = false;
            let mut digging = false;
            let mut recall_site = None;
            if let Some(cmd) = walk_or_remove_wall(turn, role, &stands, claimed) {
                plan.push(role.id, cmd);
                moved = true;
            } else {
                let mut ignored = HashSet::new();
                if let Some(cmd) = walk_or_remove_wall(turn, role, &stands, &mut ignored) {
                    plan.push(role.id, cmd);
                    moved = true;
                } else if let Some(cmd) = break_out(turn, role, &stands, &mut ignored) {
                    digging = cmd.action == "remove";
                    plan.push(role.id, cmd);
                    moved = true;
                }
            }
            if !moved {
                recall_site = Some((
                    role.pos,
                    stands.len(),
                    turn.walls()
                        .into_iter()
                        .filter(|wall| chebyshev(role.pos, wall.pos) == 1)
                        .count(),
                ));
            }
            let reason = if digging {
                "controller_digging"
            } else if moved {
                "controller_walking"
            } else {
                "controller_stuck"
            };
            for row in rows.iter_mut() {
                if row["reason"] == "controller_walking" {
                    row["reason"] = serde_json::json!(reason);
                    if let Some((pos, stands, walls)) = recall_site {
                        row["stuck"] = serde_json::json!([pos.x, pos.y, stands, walls]);
                    }
                }
            }
            night_debug(turn, rows);
            return;
        }
        // At the post with silent guns (or no station left on the board):
        // holding IS the plan — the day backstop would refuse inside the
        // ring anyway.
        night_debug(turn, rows);
        return;
    }

    // Step 4 — the swept-night mine (comment 1 §3.4: 所有己方机器人消灭后
    // 出去采矿). The moment a hostile robot lives again step 3 owns A, and
    // that walk back silences no gun the trip itself would not have.
    if let Some(cmd) = night_mine_step(turn, state, role, claimed) {
        for row in rows.iter_mut() {
            if row["reason"] == "controller_walking" {
                row["reason"] = serde_json::json!("swept_mining");
            }
        }
        night_debug(turn, rows);
        plan.push(role.id, cmd);
        return;
    }
    // Step 5 — hold.
    night_debug(turn, rows);
}

/// The legacy per-round night record, same shape: 表 6a/6b read the reasons
/// from this schema, so the L-shape model reports in it too.
fn night_debug(turn: &Turn, rows: Vec<serde_json::Value>) {
    crate::log::event(
        "night_debug",
        serde_json::json!({"round": turn.round_no, "robots": turn.robots.len(), "pairs": rows}),
    );
}

/// The mine half of the swept night (comment 1 §3.4). The day step's quota
/// discipline unchanged — stone first while the ring is short, the
/// single-vein latch (D19), the ten-collect cap — with two night
/// differences: the surplus ore has no dusk deadline to fit (the guns, not
/// the clock, bound the trip; a surviving robot pulls A straight back to
/// the post next round), and every pick runs through the D15 robot
/// clearance so A never mines inside a robot's zone even mid-sweep.
fn night_mine_step(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    let demand = day::stone_demand_of(turn, state);
    if role.backpack_full() {
        return economy::discard_command(turn, state, role, demand);
    }
    let stone_short = !stone_covered(turn, demand);
    let (vein, ore) = night_latched_vein(turn, state, claimed, stone_short)
        .or_else(|| {
            // The quota stone first — but NOT through `pick_vein`'s stone
            // pass: its deliverability gate measures dusk, and at night it
            // correctly refuses stone for B (stone does not sell, and B
            // never builds). A's night stone feeds tomorrow morning's wall
            // line, so it gets its own picker; when no stone survives the
            // filters the round falls back to the sellable ore, mirroring
            // B's `choose_vein` second pass.
            let pick = if stone_short {
                night_stone_pick(turn, state, role, claimed).or_else(|| {
                    pick_vein(turn, state, role, false, NIGHT_ROBOT_CLEARANCE, claimed, false)
                })
            } else {
                pick_vein(turn, state, role, false, NIGHT_ROBOT_CLEARANCE, claimed, false)
            };
            pick.map(|(pos, ore)| {
                state.wall_vein_latch = Some(pos);
                (pos, ore)
            })
        })?;
    run_vein(turn, state, role, vein, &ore, stone_short, claimed)
}

/// The stone half of the swept-night mine: `pick_vein`'s outage, claim,
/// ten-collect and D15 robot-clearance filters, ranked by plain walk
/// distance (nearest wins, coordinates break ties) instead of a dusk
/// deadline that does not exist after dark.
fn night_stone_pick(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    claimed: &HashSet<Pos>,
) -> Option<(Pos, String)> {
    let mut best: Option<(i64, i32, i32, Pos)> = None;
    for (pos, ore) in turn.all_mines() {
        if ore != STONE
            || state.ore_on_outage(&ore, turn.day)
            || claimed.contains(&pos)
            || state.vein_hits.get(&pos).copied().unwrap_or(0) >= VEIN_COLLECT_LIMIT
            || !night_safe(turn, pos, NIGHT_ROBOT_CLEARANCE)
        {
            continue;
        }
        let trip = trip_rounds(turn, role.pos, pos).max(1);
        let is_better = best
            .as_ref()
            .map_or(true, |current| (trip, pos.x, pos.y) < (current.0, current.1, current.2));
        if is_better {
            best = Some((trip, pos.x, pos.y, pos));
        }
    }
    best.map(|(_, _, _, pos)| (pos, STONE.to_owned()))
}

/// The night contract of [`BotState::wall_vein_latch`]: the same
/// revalidation the day's [`latched_vein`] runs — on the map, not mined out,
/// not claimed, not blacked out, a stone latch released when the pool
/// covers the demand, an ore latch released when stone goes short — minus
/// the dusk-deadline release, which does not exist after dark.
fn night_latched_vein(
    turn: &Turn,
    state: &BotState,
    claimed: &HashSet<Pos>,
    stone_short: bool,
) -> Option<(Pos, String)> {
    let pos = state.wall_vein_latch?;
    if state.vein_hits.get(&pos).copied().unwrap_or(0) >= VEIN_COLLECT_LIMIT {
        return None;
    }
    if claimed.contains(&pos) {
        return None;
    }
    let (_, ore) = turn.all_mines().into_iter().find(|(p, _)| *p == pos)?;
    if state.ore_on_outage(&ore, turn.day) {
        return None;
    }
    if ore == STONE {
        if !stone_short {
            return None;
        }
    } else if stone_short {
        return None;
    }
    Some((pos, ore))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32, y: i32) -> Pos {
        Pos { x, y }
    }

    fn unit(id: i64, kind: &str, at: Pos, pack: Vec<&str>) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": kind,
            "health": 1000, "level": 1, "backPackCapability": 100, "backpack": pack
        })
    }

    fn zone(x: i32, y: i32, kind: &str) -> serde_json::Value {
        serde_json::json!({"pos": {"x": x, "y": y}, "neutralType": kind})
    }

    /// The day-1 board: station at (10,24) on the 41×32 map, `roundNo`
    /// controlling the day round (day 1: `in_day_round = roundNo - 1`).
    fn board(
        round_no: i64,
        roles: Vec<serde_json::Value>,
        zones: Vec<serde_json::Value>,
        gold: i64,
    ) -> Turn {
        let mut all = vec![unit(10001, "station", pos(10, 24), vec![])];
        all.extend(roles);
        let payload = serde_json::json!({
            "roundNo": round_no,
            "mapInfo": {"width": 41, "height": 32, "zones": zones},
            "teamOur": {
                "type": "challenger", "goldNum": gold, "totalScore": 0,
                "playerTasks": [], "roles": all
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
        });
        let req: crate::protocol::Request =
            serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    /// Drive A (10002) through the real scheduler seam: context first, then
    /// the mainline, then read back the one command the plan holds for A.
    fn plan_for(turn: &Turn, state: &mut BotState, owned: &[i64]) -> Option<RoleCommand> {
        let ctx = day::round_context(turn, state, owned);
        let mut claimed = HashSet::new();
        let mut plan = Plan::default();
        let role = turn.role_by_id(10002).expect("worker A exists");
        plan_day(turn, state, role, &ctx, owned, &mut claimed, &mut plan);
        plan.commands.get(&10002).cloned()
    }

    fn target(cmd: &RoleCommand) -> Pos {
        cmd.targetPos.as_ref().expect("a target")[0]
    }

    /// Comment 1 §4: the line goes up in ONE direction. A carrier beside
    /// four different gaps lays the one the fixed order lists first — the
    /// front column's foot, not the nearest, not the handiest.
    #[test]
    fn the_wall_goes_up_in_the_fixed_order() {
        let stone = vec!["stone"; 6];
        let turn = board(
            5,
            vec![unit(10002, "worker", pos(12, 22), stone)],
            vec![],
            0,
        );
        let mut state = BotState::default();
        let cmd = plan_for(&turn, &mut state, &[]).expect("a wall is placed");
        assert_eq!(cmd.action, "build");
        assert_eq!(target(&cmd), pos(13, 21), "front column first");
        assert_eq!(cmd.name.as_deref(), Some("wall"));
        assert_eq!(state.walls_built_today, 1);
        assert!(state.walled_cells_today.contains(&pos(13, 21)));
    }

    /// Comment 1 §5: 每个矿只能采集10次 — A digs ONE vein out on a latch, and
    /// the latch is A's own field: B's stays untouched. A mined-out vein
    /// releases the latch and the quota moves to the next stone.
    #[test]
    fn the_quota_latches_one_stone_vein_and_releases_at_ten() {
        let zones = vec![zone(14, 24, "stone"), zone(16, 24, "stone")];
        let turn = board(5, vec![unit(10002, "worker", pos(13, 24), vec![])], zones, 0);
        let mut state = BotState::default();
        let cmd = plan_for(&turn, &mut state, &[]).expect("adjacent stone is dug");
        assert_eq!(cmd.action, "collect");
        assert_eq!(target(&cmd), pos(14, 24), "the ROI pick takes the nearer vein");
        assert_eq!(state.wall_vein_latch, Some(pos(14, 24)));
        assert_eq!(state.vein_hits.get(&pos(14, 24)).copied(), Some(1));
        assert_eq!(state.vein_latch, None, "B's latch is a separate field");

        // The vein is spent: the latch releases and the quota walks to the
        // next stone instead of standing on an empty one.
        let mut state = BotState::default();
        state.wall_vein_latch = Some(pos(14, 24));
        state.vein_hits.insert(pos(14, 24), VEIN_COLLECT_LIMIT);
        let cmd = plan_for(&turn, &mut state, &[]).expect("the next vein is walked to");
        assert_eq!(cmd.action, "move");
        assert_eq!(state.wall_vein_latch, Some(pos(16, 24)));
    }

    /// Design D1: the deadline is the PERSON's walk. With the post 13 rounds
    /// away and 5 rounds of day left, every far errand is over and A is
    /// walking to the operator cell — no pair, no tower, no pre-position
    /// table involved.
    #[test]
    fn the_post_deadline_is_the_persons_own_walk() {
        let turn = board(
            51, // in_day 50: 50 + 13 + POST_MARGIN >= 55
            vec![unit(10002, "worker", pos(20, 10), vec![])],
            vec![],
            0,
        );
        let mut state = BotState::default();
        let cmd = plan_for(&turn, &mut state, &[]).expect("A walks to the post");
        assert_eq!(cmd.action, "move");
        let post = base_layout::operator_cell(&turn).expect("the operator cell");
        assert_eq!(post, pos(9, 23));
        assert!(
            (chebyshev(target(&cmd), post) as i64) < trip_rounds(&turn, pos(20, 10), post),
            "the step closes on the operator cell"
        );
    }

    /// Issue #207 §5: the ring's tail outranks the post. A carrier beside an
    /// open gap with the day still short keeps laying walls past the point
    /// where the post guard would otherwise recall it.
    #[test]
    fn the_ring_tail_outranks_the_post_guard() {
        let stone = vec!["stone"; 6];
        let turn = board(
            51, // in_day 50: deadline hit (50 + 5 + 3 >= 55)
            vec![unit(10002, "worker", pos(14, 26), stone)],
            vec![],
            0,
        );
        let mut state = BotState::default();
        let cmd = plan_for(&turn, &mut state, &[]).expect("the carrier keeps building");
        assert_eq!(cmd.action, "build");
        assert_eq!(target(&cmd), pos(13, 25), "still in the fixed order");
    }

    /// The trap veto is per-cell and the hard seal overrides it: before
    /// `HARD_SEAL_ROUND` the pocketed role's only way home stays open, at
    /// and after it the ring closes whatever the cost (P0-3 → issues
    /// #121-#125).
    #[test]
    fn the_hard_seal_overrides_the_trap_veto() {
        // One worker in a pocket of our own walls whose only opening is
        // (21,20) — the day.rs P0-3 fixture, with A as the carrier beside it.
        let pocket = |round_no: i64| {
            let mut roles: Vec<serde_json::Value> = [
                pos(19, 19),
                pos(19, 20),
                pos(19, 21),
                pos(20, 19),
                pos(20, 21),
                pos(21, 19),
                pos(21, 21),
            ]
            .iter()
            .enumerate()
            .map(|(index, at)| unit(20000 + index as i64, "wall", *at, vec![]))
            .collect();
            // A stands BESIDE the opening, not on it: a carrier's own body
            // blocks the cell it stands on (`blocked_for`), which would seal
            // the pocket before the hypothetical wall and turn the veto into
            // the "already unreachable" pass at `wall_would_trap`.
            roles.push(unit(10002, "worker", pos(22, 20), vec!["stone"; 6]));
            roles.push(unit(10003, "worker", pos(20, 20), vec![])); // idle, pocketed
            board(round_no, roles, vec![], 0)
        };
        let before = pocket(61); // in_day 60 < HARD_SEAL_ROUND
        let mut state = BotState::default();
        let ctx = day::round_context(&before, &mut state, &[]);
        assert!(
            !safe_gap(&before, &ctx, &state, &[], pos(21, 20)),
            "the one cell that lets the idle role home is refused before the hard seal"
        );
        let after = pocket(67); // in_day 66 >= HARD_SEAL_ROUND
        let mut state = BotState::default();
        let ctx = day::round_context(&after, &mut state, &[]);
        assert!(
            safe_gap(&after, &ctx, &state, &[], pos(21, 20)),
            "past the hard seal the ring closes anyway"
        );
    }

    /// P0-4: the moment 25 gold is in hand the tower outranks the wall — A
    /// standing beside BOTH builds the gun, not the ring.
    #[test]
    fn the_tower_outranks_the_wall() {
        let turn = board(
            5,
            vec![unit(10002, "worker", pos(8, 22), vec!["stone"; 6])],
            vec![],
            30,
        );
        let mut state = BotState::default();
        let cmd = plan_for(&turn, &mut state, &[]).expect("the tower is built");
        assert_eq!(cmd.action, "build");
        assert_eq!(target(&cmd), pos(9, 22), "the L's first site");
        assert_eq!(cmd.name.as_deref(), Some("rocket"));
    }

    /// 卖矿越晚越好, bounded by the post: surplus ore is only mined while the
    /// round trip still gets A back to the operator cell before dusk. The
    /// quota board is a closed ring (demand 0), so every collect here is
    /// surplus — and the same vein flips from worth-taking to refused as the
    /// day runs down.
    #[test]
    fn surplus_mining_is_bound_by_the_post_trip() {
        let ring: Vec<serde_json::Value> = base_layout::wall_build_order(&board(5, vec![], vec![], 0))
            .iter()
            .enumerate()
            .map(|(index, at)| unit(20000 + index as i64, "wall", *at, vec![]))
            .collect();
        let mut roles = ring;
        roles.push(unit(10002, "worker", pos(10, 20), vec![]));
        let zones = vec![zone(20, 10, "copper")];

        // in_day 20: 20 + 10 out + 13 home + 3 <= 55 — the trip fits.
        let turn = board(21, roles.clone(), zones.clone(), 0);
        let mut state = BotState::default();
        plan_for(&turn, &mut state, &[]);
        assert_eq!(
            state.wall_vein_latch,
            Some(pos(20, 10)),
            "early in the day the surplus vein is worth the walk"
        );

        // in_day 40: 40 + 10 + 13 + 3 = 66 > 55 — the post owns the day.
        let turn = board(41, roles, zones, 0);
        let mut state = BotState::default();
        plan_for(&turn, &mut state, &[]);
        assert_eq!(state.wall_vein_latch, None, "the same vein no longer fits");
    }

    /// The dusk cash-out band: the sell trip fires exactly in the last
    /// rounds where "sell and still reach the post" fits — comfortably
    /// early it stays ore, too late the post guard already owns A.
    #[test]
    fn the_cashout_fires_only_inside_its_band() {
        // Vendor at (4,23): travel 4 from A at (9,23) (to the vendor's stand
        // at (5,23)), 5 from the vendor to the post — via = 9. The trigger
        // fires while via + margin <= rounds_left AND the slack over that
        // floor is <= CASHOUT_LEAD, i.e. rounds_left in 12..=15.
        let make = |round_no: i64| {
            board(
                round_no,
                vec![unit(10002, "worker", pos(9, 23), vec!["copper"; 2])],
                vec![zone(4, 23, "vendor")],
                0,
            )
        };
        let mut state = BotState::default();
        let early = make(21); // in_day 20, rounds_left 35
        let ctx = day::round_context(&early, &mut state, &[]);
        let role = early.role_by_id(10002).expect("A exists");
        assert_eq!(sell_trigger(&early, &state, role, &ctx), None);

        let mut state = BotState::default();
        let band = make(44); // in_day 43, rounds_left 12 == via + margin
        let ctx = day::round_context(&band, &mut state, &[]);
        let role = band.role_by_id(10002).expect("A exists");
        assert_eq!(
            sell_trigger(&band, &state, role, &ctx),
            Some("dusk_cashout")
        );

        let mut state = BotState::default();
        let late = make(51); // in_day 50, rounds_left 5 < via + margin
        let ctx = day::round_context(&late, &mut state, &[]);
        let role = late.role_by_id(10002).expect("A exists");
        assert_eq!(sell_trigger(&late, &state, role, &ctx), None);
    }

    /// The quota STOPS at the demand: with the team pool already holding
    /// every stone the ring owes, a worker standing between a stone vein and
    /// a copper vein digs the copper — stone is not merchandise, and the
    /// day's quota is the only reason A ever digs it.
    #[test]
    fn a_covered_quota_turns_a_to_the_ore() {
        let turn = board(
            5,
            vec![
                unit(10002, "worker", pos(14, 24), vec![]),
                unit(10003, "worker", pos(20, 20), vec!["stone"; 16]),
            ],
            vec![zone(14, 25, "stone"), zone(14, 23, "copper")],
            0,
        );
        let mut state = BotState::default();
        let cmd = plan_for(&turn, &mut state, &[10003]).expect("A digs the copper");
        assert_eq!(cmd.action, "collect");
        assert_eq!(target(&cmd), pos(14, 23), "the stone vein is skipped");
        assert_eq!(state.wall_vein_latch, Some(pos(14, 23)));
    }

    // ---- Night fixtures (phase 4b-3) ----

    fn tower(id: i64, kind: &str, at: Pos, cooldown: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": kind,
            "health": 1000, "level": 1, "attackPower": 10, "attackRange": 10,
            "cooldown": cooldown, "backPackCapability": 0, "backpack": []
        })
    }

    fn robot(id: i64, at: Pos, kind: &str, health: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": kind,
            "health": health, "abnormalState": "", "targetTeam": "challenger"
        })
    }

    /// A night board: `roundNo` 71+ is day 1's night (`in_day_round >= 70`).
    /// The L's three sites are (9,22) rocket, (10,22) railgun, (9,24)
    /// gatling, and the operator cell (9,23) is adjacent to all three.
    fn night_board(
        round_no: i64,
        roles: Vec<serde_json::Value>,
        zones: Vec<serde_json::Value>,
        robots: Vec<serde_json::Value>,
    ) -> Turn {
        let mut all = vec![unit(10001, "station", pos(10, 24), vec![])];
        all.extend(roles);
        let payload = serde_json::json!({
            "roundNo": round_no,
            "mapInfo": {"width": 41, "height": 32, "zones": zones},
            "teamOur": {
                "type": "challenger", "goldNum": 0, "totalScore": 0,
                "playerTasks": [], "roles": all
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": robots},
        });
        let req: crate::protocol::Request =
            serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    /// Drive A (10002) through the night mainline and return the WHOLE plan:
    /// the attacks are keyed by tower id, A's personal command by 10002.
    fn night_plan_for(turn: &Turn, state: &mut BotState) -> Plan {
        let mut claimed = HashSet::new();
        let mut plan = Plan::default();
        let role = turn.role_by_id(10002).expect("worker A exists");
        plan_night(turn, state, role, &mut claimed, &mut plan);
        plan
    }

    /// Design D16's L-shape: ONE operator fires the whole three-gun salvo in
    /// a single round. Attacks are tower-addressed commands naming the
    /// controller, so the salvo costs A no personal command at all — the
    /// operator stands at the post with an empty slot while all three guns
    /// fire.
    #[test]
    fn one_operator_fires_the_whole_l() {
        let turn = night_board(
            75,
            vec![
                unit(10002, "worker", pos(9, 23), vec![]), // the operator cell
                tower(30001, "rocket", pos(9, 22), 0),
                tower(30002, "railgun", pos(10, 22), 0),
                tower(30003, "gatling", pos(9, 24), 0),
            ],
            vec![],
            vec![
                robot(90001, pos(12, 23), "largeRobot", 500),
                robot(90002, pos(11, 22), "largeRobot", 500),
                robot(90003, pos(11, 24), "largeRobot", 500),
            ],
        );
        assert!(!turn.is_day);
        let mut state = BotState::default();
        let plan = night_plan_for(&turn, &mut state);
        for id in [30001i64, 30002, 30003] {
            let cmd = plan.commands.get(&id).expect("every gun of the L fires");
            assert_eq!(cmd.action, "attack");
            assert_eq!(cmd.controllerId.as_deref(), Some("10002"));
        }
        assert!(
            plan.commands.get(&10002).is_none(),
            "the salvo costs the operator no personal command"
        );
    }

    /// D14/§3.4: while robots live, the night belongs to the guns — an
    /// off-post A walks back instead of running errands. The copper vein
    /// beside A must not steal the round: the swept-night mine is step 4,
    /// and step 4 needs a swept board.
    #[test]
    fn a_live_wave_recalls_the_operator() {
        let turn = night_board(
            75,
            vec![
                unit(10002, "worker", pos(20, 10), vec![]),
                tower(30001, "rocket", pos(9, 22), 0),
            ],
            vec![zone(21, 10, "copper")],
            vec![robot(90001, pos(12, 23), "largeRobot", 500)],
        );
        let mut state = BotState::default();
        let plan = night_plan_for(&turn, &mut state);
        let cmd = plan.commands.get(&10002).expect("A walks home");
        assert_eq!(cmd.action, "move");
        let post = base_layout::operator_cell(&turn).expect("the operator cell");
        assert!(
            (chebyshev(target(cmd), post) as i64) < trip_rounds(&turn, pos(20, 10), post),
            "the step closes on the post"
        );
        assert!(
            plan.commands.get(&30001).is_none(),
            "the gun is out of reach and stays silent"
        );
    }

    /// Comment 1 §3.4: 所有己方机器人消灭后出去采矿 — the swept night sends A
    /// out to the quota stone (the ring still owes 16 cells and the pool
    /// holds none), on the single-vein latch and clear of any robot zone
    /// (vacuous here, but the picker runs the same D15 filter).
    #[test]
    fn the_swept_night_sends_a_to_the_quota_stone() {
        let turn = night_board(
            75,
            vec![
                unit(10002, "worker", pos(9, 23), vec![]),
                tower(30001, "rocket", pos(9, 22), 0),
            ],
            vec![zone(14, 24, "stone"), zone(16, 24, "copper")],
            vec![],
        );
        let mut state = BotState::default();
        let plan = night_plan_for(&turn, &mut state);
        let cmd = plan.commands.get(&10002).expect("A mines the swept night");
        assert_eq!(cmd.action, "move");
        assert_eq!(
            state.wall_vein_latch,
            Some(pos(14, 24)),
            "quota stone first, on the latch — the copper does not tempt it"
        );
        assert!(plan.commands.get(&30001).is_none(), "nothing to shoot at");
    }

    /// Issue #20 / P1-4: a dying operator is bait, not a gunner — the guns
    /// stay dark and an A already inside the ring holds instead of pacing.
    #[test]
    fn a_dying_operator_holds_inside_instead_of_manning() {
        let mut a = unit(10002, "worker", pos(9, 23), vec![]);
        a["health"] = serde_json::json!(40); // under 30% of the 220 worker max
        let turn = night_board(
            75,
            vec![a, tower(30001, "rocket", pos(9, 22), 0)],
            vec![],
            vec![robot(90001, pos(11, 24), "smallRobot", 100)],
        );
        let mut state = BotState::default();
        let plan = night_plan_for(&turn, &mut state);
        assert!(plan.commands.get(&30001).is_none(), "the gun stays dark");
        assert!(
            plan.commands.get(&10002).is_none(),
            "already inside: hold, do not pace"
        );
        assert!(
            state.withdraw_holdout.contains(&10002),
            "the hysteresis set keeps A off the guns while the wound is fresh"
        );
    }

    /// Issues #126-#130: a reload round with no WallFixer and no masonry is
    /// a hold round — the operator at the post does not pace and does not
    /// errand while the wave lives.
    #[test]
    fn a_reloading_gun_holds_the_operator_at_the_post() {
        let turn = night_board(
            75,
            vec![
                unit(10002, "worker", pos(9, 23), vec![]),
                tower(30001, "rocket", pos(9, 22), 5),
            ],
            vec![zone(14, 24, "stone")],
            vec![robot(90001, pos(12, 23), "largeRobot", 500)],
        );
        let mut state = BotState::default();
        let plan = night_plan_for(&turn, &mut state);
        assert!(plan.commands.get(&30001).is_none(), "the gun is reloading");
        assert!(
            plan.commands.get(&10002).is_none(),
            "at the post with a live wave: holding IS the plan"
        );
    }
}
