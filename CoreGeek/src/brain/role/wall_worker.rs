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
//! 7. **the buyer's errand** — TRANSITIONAL: `round_context` still nominates
//!    A as the team's buyer; phase 5 moves purchasing to the pioneer's fixed
//!    whitelist and this step dies;
//! 8. **summon orders** — burn a carried order when nothing else wants the
//!    round.
//!
//! # Transitional state (phase 4b-2)
//!
//! The NIGHT still belongs to the legacy `night::plan_owned`: the
//! single-operator L-shape model (design D14-D16) lands in 4b-3. The operator
//! cell this mainline pre-positions on is already the post that model uses —
//! it is a valid stand for all three weapon sites, so the legacy pairing
//! finds A where the night wants it.

use std::collections::HashSet;

use super::economy_worker::{pick_vein, NEAR_VENDOR_ROUNDS, VEIN_COLLECT_LIMIT};
use super::super::action::base_layout;
use super::super::action::build::{build_or_walk, repair_flow};
use super::super::action::geometry::wall_layer;
use super::super::action::mine::stone_covered;
use super::super::action::sell::sell_flow;
use super::super::action::shop::{
    burn_summon_order, buyer_flow, self_provision, shop_round_trip, shop_trip_worth_taking,
    use_medicine, voucher_flow, walk_to_shop,
};
use super::super::day::{
    self, roles_can_reach, stone_batch, wall_trip_overdue, wall_would_trap, RoundCtx,
    HARD_SEAL_ROUND,
};
use super::super::route::trip_rounds;
use super::super::{economy, stand_cells, walk_toward, Plan};
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
        // The four exceptions, each worth the round it spends and each ending
        // at or next to the base — the legacy lock's counter/errand/ring-tail
        // exemptions, plus the cash-out band that replaces the legacy dusk
        // sell deadline (design D2).
        let ring_tail = role.count_item(STONE) > 0
            && !ctx.wall_gaps.is_empty()
            && turn.in_day_round < economy::DUSK_ROUND;
        let at_counter = turn
            .vendors()
            .iter()
            .any(|vendor| chebyshev(role.pos, *vendor) == 1)
            && economy::sellable_ores(turn, role, ctx.stone_demand) > 0;
        let on_shop_errand = Some(role.id) == ctx.buyer_id
            && shop_trip_worth_taking(turn, role, &ctx.pairs, &ctx.budget.shopping);
        if on_shop_errand {
            // Logged only on the rounds the guard actually gives way — the
            // legacy finding: the three or four rounds a match where the
            // buyer is past its deadline with the counter still reachable.
            crate::log::event(
                "shop_trip",
                serde_json::json!({
                    "round": turn.round_no,
                    "role": role.id,
                    "decision": "protected",
                    "inDayRound": turn.in_day_round,
                    "trip": shop_round_trip(turn, role, &ctx.pairs),
                    "lockRound": post.map(|post| {
                        (economy::DUSK_ROUND
                            - trip_rounds(turn, role.pos, post)
                            - POST_MARGIN)
                            .max(0)
                    }),
                }),
            );
        }
        let cashout = sell_trigger(turn, state, role, ctx) == Some("dusk_cashout");
        if !ring_tail && !at_counter && !on_shop_errand && !cashout {
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
    // Step 7 — the buyer's errand (TRANSITIONAL: `round_context` nominates A;
    // phase 5 moves purchasing to the pioneer's fixed whitelist and this step
    // dies with the nomination).
    if Some(role.id) == ctx.buyer_id {
        if !ctx.budget.shopping.is_empty()
            && shop_trip_worth_taking(turn, role, &ctx.pairs, &ctx.budget.shopping)
        {
            if let Some(cmd) =
                buyer_flow(turn, role, &ctx.budget.shopping, &ctx.pairs, claimed)
            {
                return Some(cmd);
            }
        } else if ctx.budget.shopping.is_empty()
            && economy::buyer_must_preposition(turn, role, &ctx.budget.intent)
        {
            if let Some(cmd) = walk_to_shop(turn, role, claimed) {
                return Some(cmd);
            }
        }
    }
    // Step 8 — a carried summon order burns only when nothing else wants the
    // round.
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
    if chebyshev(role.pos, vein) == 1 {
        *state.vein_hits.entry(vein).or_insert(0) += 1;
        return Some(RoleCommand::collect(vein));
    }
    let stands = stand_cells(turn, vein);
    let cmd = walk_toward(turn, role, &stands, claimed)?;
    claimed.insert(vein);
    // Which vein the quota is being spent on, and why — the legacy
    // `mine_pick` fields, so the wall line's failure mode stays audible in
    // the log (issues #12-#17 all opened with "0 墙").
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
}
