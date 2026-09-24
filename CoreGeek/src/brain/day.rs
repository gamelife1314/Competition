//! Daytime planning (70 rounds) — what is left of the legacy dispatch
//! (issue #221 phase 4b): the round context both worker mainlines read, the
//! team prompt slot, the pioneer's day and the closing backstop. The worker
//! loop is gone — A lives in [`crate::brain::role::wall_worker`], B in
//! [`crate::brain::role::economy_worker`] — and phase 5 moves the pioneer out
//! too and deletes this file.

use std::collections::HashSet;

use crate::brain::{
    economy, news, stand_cells, task, tower_stand_cells, treasure,
    walk_or_remove_wall, walk_toward, Plan,
};
use crate::model::{
    chebyshev, footprint_distance, station_footprint, Turn, Unit, STONE, WEAPON_BUILD_COST,
};
use crate::protocol::{Pos, RoleCommand};
use crate::state::{BotState, TaskSession};

// The mine/sell executors moved to `action::{mine,sell}` (issue #221 phase 2b);
// the build/tower/wall-geometry cluster moved to `action::{build,tower_site,
// geometry}` (phase 2c). Everything the test crates used to import from here is
// re-exported under its old name until phase 5 flips the paths once.
use super::action::geometry::{ring_open_cells, wall_layer};
use super::action::mine::stone_covered;
use super::action::shop::{
    self_provision, shop_round_trip, shop_trip_decision, voucher_flow,
};
pub(crate) use super::action::shop::use_medicine;
pub use super::action::build::repair_target;
pub use super::action::geometry::{
    primary_wall_gaps, tower_build_reserve, wall_daily_cap, wall_gaps, D1_WALL_CAP, LATER_WALL_CAP,
};
pub use super::action::tower_site::tower_gaps;

/// First day round (in_day_round) when a controller must drop everything and
/// walk to its tower, so it arrives adjacent (chebyshev <= 1) BEFORE dusk
/// (round 55). Dusk is the sell/upgrade window; a tower still operator-less
/// by then is a gun that never fires — battle pk575060 spent 17/20 night
/// rounds walking back. `dist - 1` moves are needed (one per round) plus
/// three rounds of slack for blocked cells, detours and the wall line.
fn preposition_round(dist: i32) -> i64 {
    (economy::DUSK_ROUND - 1 - dist as i64 - 3).max(0)
}
/// Stones to carry before walking out to the wall line on a maintenance day.
const STONE_BATCH: i64 = 6;
/// Day 1's wall line is one complete ring, and the mine↔ring commute costs more
/// rounds than the mining does. Carrying the whole remaining demand in a single
/// trip is what closes the ring before the dusk lock-in; the one-stone dribble
/// (mine one, walk back, build one) is how the base ended up with two towers, no
/// wall and a frozen purse in issues #12/#13/#14.
const RING_BATCH: i64 = 20;

/// Rounds kept in hand on top of the walk home before a stone carrier gives up
/// on the vein — a blocked cell, a detour, a claim.
const WALL_TRIP_SLACK: i64 = 3;

/// Has the vein stopped paying for the walk home? The load already in hand is
/// what the ring gets; a carrier that keeps digging is a carrier the night
/// finds outside the wall. Measured against the wall window (dusk plus the
/// seal grace), not the gun deadline: the seal is allowed to spend the last
/// rounds of the day on the ring, and a trip that is still placing walls then
/// is a trip that worked. Read by the wall worker's batch release (phase 4b).
pub(crate) fn wall_trip_overdue(turn: &Turn, role: &Unit, gaps: &[Pos]) -> bool {
    let home = gaps
        .iter()
        .map(|gap| chebyshev(role.pos, *gap) as i64)
        .min()
        .unwrap_or(0);
    turn.in_day_round + home + 1 + WALL_TRIP_SLACK >= economy::DUSK_ROUND + SEAL_GRACE
}

/// Stones this role should carry before it walks out to the wall line. Read
/// by the wall worker's batch release (phase 4b).
pub(crate) fn stone_batch(turn: &Turn, gaps: usize) -> i64 {
    let want = if turn.day == 1 {
        RING_BATCH
    } else {
        STONE_BATCH
    };
    want.min(gaps as i64).max(1)
}
/// Day-rounds after dusk during which a stone carrier may still walk out to
/// close the last hole in the ring. Long enough for a round trip from any
/// tower post, short enough that the gun is manned again well before night.
pub(crate) const SEAL_GRACE: i64 = 8;
/// The day-round past which the gate is sealed whether or not the crew is home.
///
/// `SEAL_GRACE` is the window the seal is *allowed* to spend; this is the point
/// at which it stops being allowed to spend more. Across issues #121-#125 the
/// gate never sealed on any day after the first in ANY of the five matches —
/// `wall_gate_open` for 5, 7, 10, 11 and 15 of the fifteen dusk rounds, the
/// last of those being the whole window, i.e. the ring kept a robot-sized hole
/// every night of the match. The cause is that the seal waits for every role,
/// and a role fourteen to twenty cells out at dusk cannot arrive in time; the
/// wait then outlives the day, and the night planner never revisits the flag,
/// so the hole is permanent.
///
/// A straggler left outside is recoverable — the night recall's
/// `walk_or_remove_wall` hatch cuts back through our own ring — while an open
/// ring is not: the wall is the only thing between the waves and the station,
/// and `score_3` is 10×day for every day the station stands (550 over ten).
/// Sealing with three day-rounds to spare is also what leaves the stone carrier
/// time to actually place the gate cell.
pub(crate) const HARD_SEAL_ROUND: i64 = economy::DUSK_ROUND + 11;
/// Slack on top of the walk home before the pioneer's dusk recall fires, for a
/// blocked cell or a detour. Mirrors the three rounds `preposition_round` keeps.
const PIONEER_RETREAT_SLACK: i64 = 2;
/// Day-rounds that must still be available before a task point is worth
/// accepting.
///
/// 任务书 5.3: "在任务执行结束后，再次接取任务需等待 30 个回合刷新时间" — and the
/// dusk recall ends every session the pioneer is still holding when it fires.
/// So a session accepted with fewer rounds left than a session needs is not a
/// cheap attempt, it is that task point sold for 30 rounds: the recall walks
/// the pioneer off the point, the judger ends the task, and the point is gone
/// through the whole of the next morning. A working session costs four to six
/// rounds (prompt → `llmResp` → the held-back command → verdict → submit), so
/// twelve leaves room for one failed cycle and a second try.
const TASK_MIN_ATTEMPT_ROUNDS: i64 = 12;
/// Rounds a session may spend without producing a single submission before the
/// pioneer goes back to the wall line.
///
/// Issue #15's lesson was that the opponent dropped failing tasks immediately
/// and "把开拓者投入防御"; a session that has run three full LLM cycles (each
/// costs prompt → answer → command → verdict, so five rounds apiece) with
/// nothing submitted has produced no evidence that the fourth is the one. The
/// pioneer is one of the three controllers the wall gate seal waits for, and
/// issue #26 lost its base to nine rounds of `wall_gate_open`. Only a session
/// with NOTHING submitted qualifies: one wrong answer is evidence the loop is
/// working, and `MAX_WRONG_ANSWERS` already governs that case.
const MAX_STERILE_ROUNDS: i64 = 15;

pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    plan_owned(turn, state, &[], &mut HashSet::new())
}

/// The day plan with the issue-#221 roster seam: every unit id in `owned` is
/// dispatched by a role mainline ([`crate::brain::role`]) instead of this
/// planner — the backstop skips it (no shopping errand, no fallback walk) and
/// the caller merges the plans. `owned == &[]` is exactly the old `plan`,
/// which is what the test crates still drive. `claimed` is shared with the
/// mainlines so two people never reserve the same vein or build site.
pub(crate) fn plan_owned(
    turn: &Turn,
    state: &mut BotState,
    owned: &[i64],
    mut claimed: &mut HashSet<Pos>,
) -> Plan {
    let ctx = round_context(turn, state, owned);
    let mut plan = Plan::default();
    plan_roles(turn, state, &ctx, owned, &mut claimed, &mut plan);
    plan
}

/// The round's shared reads for the day planner (issue #221 phase 4a): every
/// number the legacy `plan_owned` computed before dispatching any role. The
/// role mainlines ([`crate::brain::role`]) and the legacy dispatch
/// ([`plan_roles`]) read the SAME instance, so demand, budget and pairing can
/// never disagree between the two schedulers inside one round.
pub(crate) struct RoundCtx {
    pub(crate) tower_gaps: Vec<(Pos, String)>,
    pub(crate) wall_gaps: Vec<Pos>,
    pub(crate) stone_demand: i64,
    pub(crate) wall_cap: i64,
    pub(crate) budget: economy::Budget,
    pub(crate) buyer_id: Option<i64>,
    pub(crate) pairs: Vec<(i64, i64)>,
}

/// Compute the [`RoundCtx`]. Split out of `plan_owned` (issue #221 phase 4a):
/// the side effects — the ring-completion memory, the buy deadline and the
/// per-round telemetry — run here exactly once, in the order they always ran.
/// `owned` is the CONTEXT roster: the units invisible to the pairing, the trap
/// vetoes and the buyer pick. It stays `[B]` in phase 4a — the wall worker's
/// day still delegates to the legacy dispatch, so the context must keep
/// counting A.
pub(crate) fn round_context(turn: &Turn, state: &mut BotState, owned: &[i64]) -> RoundCtx {
    let tower_gaps = tower_gaps(turn, state);
    // Owned-aware: units dispatched by a role mainline (the economy worker)
    // are NOT in these pairs. B stays outside the ring after dark on purpose
    // (comment 1 §6), so B's position must not veto the last wall as a "trap" —
    // issues #111/#112/#115 sealed the ring with everyone inside; the new B is
    // deliberately not everyone.
    let pairs = super::action::fight::stable_pairs_owned(turn, state, owned);
    let wall_gaps = wall_gaps(turn, state);
    // Ring integrity, remembered across days. An EMPTY gap list with walls
    // standing means the shell is closed — the permanent entrance is not part
    // of the count (comment 1 §4: the back four cells are open by design), so
    // this never reads the entrance as a hole. From then on `wall_daily_cap`
    // treats holes as breach repair (see `BotState::ring_ever_complete`).
    //
    // Measured on the PRIMARY ring alone. The second layer (P2-1) is a later,
    // partial addition which sits in `wall_gaps` too; letting it answer this
    // question would mean the ring's own breach-repair budget never latched,
    // and a ring the night tore open would be repaired on the six-cell
    // maintenance budget that cannot re-close it (issue #21).
    let primary_open = ring_open_cells(turn, state);
    if primary_open == 0 && !turn.walls().is_empty() {
        state.ring_ever_complete = true;
    }
    let wall_cap = wall_daily_cap(turn.day, state.ring_ever_complete, primary_open);
    // On D1 carry enough stone to finish the complete radius-2 shell. Later
    // days use a bounded maintenance budget. `stone_demand` counts the GAPS
    // still open, not the shortfall against what is already carried: stone in a
    // backpack is committed to those gaps, and treating it as "demand already
    // met" made the carrier sell the ring's own stone out from under itself.
    // There is no gate term and no door term any more (design D17): the
    // permanent entrance is never built, so every cell of `wall_gaps` is a cell
    // the day intends to wall, and the demand is exactly that list, capped.
    let wall_demand = (wall_gaps.len() as i64).min(wall_cap);
    let stone_demand = wall_demand;
    // Gold reserved for finishing the tower build-out is untouchable by the
    // shopping list — defenses come before consumables, but only for the 1-2
    // towers we actually build (never all three slots at once).
    let mut build_reserve = tower_build_reserve(turn.towers().len(), tower_gaps.len());
    // P0-4 第三塔资金守护：fallback 窗口临近且升级不可达时，给第三塔守住 25 金。
    // 没有资金守护时，小额采购（药/修墙包/墙券）会把金币常年压在 5–24，
    // fallback 的 `gold >= 25` 永远凑不齐（#22/#23 连续 5+ 场第三塔缺席）。
    let guard = !tower_gaps.is_empty() && economy::third_tower_guard(turn, state);
    if guard {
        build_reserve = build_reserve.max(WEAPON_BUILD_COST);
    }
    // Dusk cash-out (issues #201-#205): once dusk arrives, drop the build
    // reserve so every remaining gold is spendable on weapon upgrades and
    // towers. Hoarding gold past DUSK_ROUND buys nothing — the night is spent
    // fighting, not shopping — so the reserve that protected the third-tower
    // fund all afternoon is released the moment that fund's window closes.
    if turn.in_day_round >= economy::DUSK_ROUND {
        build_reserve = 0;
    }
    // P2-2 宝藏线的献祭金：祭坛坐标、献祭物品、开启日、4 次上限全都实现了，却一次
    // 也跑不起来——因为购物单会把金币全部花在升级券上，开拓者走到商店时钱包是空
    // 的，只能站在柜台前等，等到开启日过去。这里把"下一件献祭物的钱"从购物单里
    // 扣住（有上限、且只扣钱包里真有的钱，见 treasure::gold_reserve）。
    let treasure_reserve = treasure::gold_reserve(turn, state);
    build_reserve = build_reserve.max(treasure_reserve);
    let budget = economy::budget(turn, state, build_reserve);

    // Buyer assignment: a dedicated WORKER so voucher purchases are never
    // preempted by a task accept or the treasure hunt. The pioneer stays free
    // for tasks/treasure. An unaffordable intent still nominates a buyer: the
    // deadline budgeter needs someone walking toward the shop before the gold
    // arrives.
    //
    // P1-2 买家与经济工拆分: once the wall work is done the FIRST worker takes
    // the shop trips and the dedicated economy worker (the last one) stays on
    // the collect→sell loop. With one role doing both, every purchase cost the
    // mine→vendor→shop triangle — 30-40 rounds per buy, one or two buys a day,
    // and the purse idled at the counter while ore waited in the backpack
    // (issues #20/#22). While the ring is still being built the wall worker has
    // no time for errands, so the old same-role fallback holds; with one worker
    // alive it holds too.
    let workers = turn.workers();
    // Day 1: both workers fortify while the ring is short of the stone the day
    // owes it, and the economy worker is released the moment the TEAM's packs
    // hold that stone. We need gold for towers AND upgrade vouchers — both
    // workers on stone all day = zero income (issue #18's freeze), so the day
    // has to buy its ring and then go earn.
    //
    // WHAT PAYS FOR THE WALL NOW: the stone already in the crew's packs. The
    // release is `stone_covered` — the very bound both worker mainlines use to
    // decide who digs stone (see `action::mine`) — so "the ring has its stone"
    // cannot mean two different things in two places. The old trigger was `wall_gaps.len() > 4`,
    // i.e. release once the ring is all but closed, and on the day-1 board that
    // lands at R41 with dusk at 55 and the nearest sellable vein eight to ten
    // rounds out: the released worker reached the ore exactly as the recall
    // took it home, and the day produced 21 stone and nothing else (0 iron,
    // 0 copper, 0 sales, purse frozen at 25). Releasing on the stone the day
    // owes frees that worker ~25 rounds earlier. The first worker keeps the
    // wall duty, and digs any stone the day turns out to still owe (a failed
    // build, a robot's hole), so the ring is never left short to pay for the
    // ore.
    let shared_wall_duty = turn.day == 1
        && workers.len() >= 2
        && !stone_covered(turn, stone_demand);
    // The flag's only surviving reader is the buyer pick below (phase 4b
    // deleted the worker loop that dispatched on it): while the D1 ring is
    // still short of stone, wall work is NOT done, so the shop errand stays
    // with the fallback nominee instead of moving to the first worker.
    let wall_work_done = wall_gaps.is_empty() && !shared_wall_duty;
    let buyer_id: Option<i64> = if budget.intent.is_empty() {
        None
    } else {
        let prefer = if wall_work_done && workers.len() >= 2 {
            workers.first()
        } else {
            workers.last()
        };
        // A buyer owned by a role mainline cannot be sent on the legacy trip;
        // the errand falls to the first worker this planner still commands.
        prefer
            .map(|unit| unit.id)
            .filter(|id| !owned.contains(id))
            .or_else(|| {
                workers
                    .iter()
                    .map(|unit| unit.id)
                    .find(|id| !owned.contains(id))
            })
    };
    // Tower plan telemetry: whether a third weapon is being held back for the
    // 100-gold upgrade, and whether that upgrade is still reachable, is the
    // decision the deadline budgeter exists to make explainable.
    crate::log::event(
        // Written every day round, deliberately: this is the evidence for
        // WORKFLOW_REQUEST §7.1's first question — whether `mayBuild` was ever
        // true while `towers < 3` — and that question is a *ratio* over rounds,
        // which a record emitted only on change could not answer. What it no
        // longer carries is `dayRound` (derivable from `round`) and
        // `fallbackRound` (`DUSK_ROUND - FALLBACK_LEAD`, the same constant on
        // all 1400 of them).
        "tower_plan",
        serde_json::json!({
            "round": turn.round_no,
            "towers": turn.towers().len(),
            "gaps": tower_gaps.len(),
            "reserve": build_reserve,
            "guard": guard,
            // P2-1: how many of `wallGaps` are the second layer. Without the
            // split, a day that spends its wall budget on ring 3 reads exactly
            // like a day that failed to close ring 2.
            "secondLayer": wall_gaps
                .iter()
                .filter(|site| wall_layer(turn, **site) == 3)
                .count(),
            "treasureReserve": treasure_reserve,
            "wallGaps": wall_gaps.len(),
            "stoneDemand": stone_demand,
            "teamStone": economy::team_ores(turn, STONE),
            "mayBuild": economy::may_build_weapon(turn, state),
            "upgradeReachable": economy::upgrade_reachable(turn, state),
        }),
    );
    if !budget.intent.is_empty() {
        // Economy intent vs outcome: the head of the list is what we WANT, the
        // gold check and the buyer's distance say whether it is reachable this
        // round. A frozen economy (gold stuck, buyer never arriving) is then
        // visible in the log instead of only in the final score.
        let head = budget.head().expect("intent is non-empty");
        let price = turn.weapon_shop.get(&head.name).copied().unwrap_or(-1);
        let buyer_dist = buyer_id
            .and_then(|id| turn.role_by_id(id).map(|role| role.pos))
            .map(|pos| economy::shop_travel(turn, pos))
            .unwrap_or(-1);
        // Cross-person sync (issue #221, comment 1 §6/§7): the economy worker
        // must turn its ore into gold BEFORE the buyer stands at the counter,
        // or the purchase waits a whole loop. Transitional source: the legacy
        // buyer's own walk; phase 5 moves the computation to the pioneer's
        // "buy + return + use" budget, which is what the comment describes.
        state.buy_deadline = (buyer_dist >= 0).then_some(turn.round_no + buyer_dist);
        crate::log::event(
            "shopping",
            serde_json::json!({
                // The round is the join key: WORKFLOW_REQUEST §7.3's ledger
                // table is about the *ratio* of unaffordable rounds and what
                // gold was doing in them, and without this the event could only
                // be counted, never aligned with `round.gold`.
                "round": turn.round_no,
                "buyer": buyer_id,
                "gold": turn.gold,
                "reserve": build_reserve,
                "need": head.name,
                "needNum": head.num,
                "price": price,
                "reason": head.reason,
                "deadline": head.latest_round,
                "affordable": !budget.shopping.is_empty(),
                "buyerShopDist": buyer_dist,
                // WHY THE BUYER IS NOT AT THE COUNTER (issues #156-#160). The
                // purchase is the whole errand and the two numbers that decide
                // it — the round trip and the round the buyer has to be home by
                // — were nowhere in the log, so "75 rounds affordable, no
                // purchase" (表 2b/2a of pk590730) could only be read as a
                // mystery. `shopTrip` is the decision the buyer's mainline
                // step will take this round: `walk` (the errand is worth
                // taking), `no_time`
                // (it cannot be finished before dusk), `pack_full` (no slot for
                // the goods) or `sale_first` (there is ore to sell and the trip
                // is not worth taking yet). `trip` is the round-trip estimate in
                // rounds and `lockRound` the pre-position deadline it is
                // measured against.
                "shopTrip": buyer_id
                    .and_then(|id| turn.role_by_id(id))
                    .map(|role| shop_trip_decision(turn, role, &pairs, &budget.shopping))
                    .unwrap_or("no_buyer"),
                "trip": buyer_id
                    .and_then(|id| turn.role_by_id(id))
                    .and_then(|role| shop_round_trip(turn, role, &pairs)),
                "lockRound": buyer_id
                    .and_then(|id| turn.role_by_id(id))
                    .map(|role| {
                        pairs
                            .iter()
                            .find(|(controller, _)| *controller == role.id)
                            .and_then(|(_, tower)| turn.role_by_id(*tower))
                            .map(|tower| preposition_round(chebyshev(role.pos, tower.pos)))
                            .unwrap_or(economy::DUSK_ROUND)
                    }),
                "needs": budget.intent.iter().map(|need| need.name.as_str()).collect::<Vec<_>>(),
                "ready": budget.shopping.iter().map(|need| need.name.as_str()).collect::<Vec<_>>(),
            }),
        );
    } else {
        state.buy_deadline = None;
    }

    RoundCtx {
        tower_gaps,
        wall_gaps,
        stone_demand,
        wall_cap,
        budget,
        buyer_id,
        pairs,
    }
}

/// Dispatch the roles the legacy day planner still owns: the team prompt
/// slot, the pioneer and the closing backstop. Split out of `plan_owned`
/// (issue #221 phase 4a); phase 4b deleted the worker loop — both workers
/// run their own mainlines ([`crate::brain::role`]) around this call, the
/// wall worker's BEFORE (its legacy claim priority over the pioneer and B is
/// preserved) and the economy worker's AFTER.
///
/// `skip` is the DISPATCH exclusion: ids a role mainline commands, which the
/// backstop must not second-guess with a walk of its own.
pub(crate) fn plan_roles(
    turn: &Turn,
    state: &mut BotState,
    ctx: &RoundCtx,
    skip: &[i64],
    mut claimed: &mut HashSet<Pos>,
    mut plan: &mut Plan,
) {
    // The day's news/treasure prompt is a team-level channel, not a role
    // command — it costs the pioneer no movement. Moving it out of
    // `pioneer_day` so a dead pioneer does not silence the LLM ask: the news
    // read and treasure altar inference still run when the pioneer has died,
    // because the prompt slot is a property of the ROUND, not of whichever
    // role is alive to take it.
    // Merged prompt: when both news and treasure need asking, send one prompt
    // with both questions (user request: "用一个 prompt 向大模型发起两个提问").
    // Falls back to individual prompts when only one consumer needs the slot.
    if !plan_merged_prompt(turn, state, &mut plan) {
        news::plan_prompt(turn, state, &mut plan);
        treasure::plan_ask(turn, state, &mut plan);
    }

    if let Some(pioneer) = turn.pioneer() {
        pioneer_day(turn, state, pioneer, &ctx.pairs, &mut claimed, &mut plan);
    }

    // Backstop. Every step above can decline: the target is claimed by a
    // teammate, the path closed behind the role, the mine it is standing next
    // to was reserved by someone else, the ring is between it and everything.
    // Declining is fine, standing still is not — a controller with no command
    // at all is issue #13's "0 角色站桩闲置", and it is also what pins a role
    // outside the ring so the gate seal never completes. A role that is neither
    // on a gun nor inside shelters by closing on the station, which is always
    // legal and always useful: it is the cell the gate seal waits for and the
    // corridor every post is reached from.
    //
    // Claims are deliberately ignored here. A reservation is etiquette for
    // choosing between two targets; it must never be a reason to do nothing.
    for role in turn.controllable() {
        if skip.contains(&role.id) || plan.commands.contains_key(&role.id) {
            continue;
        }
        // …except a pioneer holding a task. It is not idle: it is standing
        // exactly where 任务书 5.3 requires it to stand. That rule lists
        // "离开己方任务点周围一格内" among the four ways a self-evolution task
        // ENDS, and `plan_pioneer` emits no command for most of the session
        // (waiting for the LLM, for the sandbox verdict, for the submission
        // verdict), so this backstop used to walk the pioneer home the round
        // after it accepted. The judger ended the task on that first step and
        // refused every later `executeCmd` with "[JUDGER_ERROR] executeCmd
        // 仅在自进化任务执行期间可用" — issue #17's two sessions, both burned
        // to zero while the opponent took 345 from four completed tasks.
        if holds_task_point(state, role) {
            continue;
        }
        // …or a pioneer holding the altar for the treasure's opening day
        // (P2-1). `holds_altar`'s deliberate no-command wait is not idleness
        // either; walking it home here would restart the two-step oscillation
        // the predicate exists to end.
        if role.kind == crate::model::UnitKind::Pioneer
            && treasure::holds_altar(turn, state, role)
        {
            continue;
        }
        if let Some(cmd) = fallback_toward_station(turn, role) {
            plan.push(role.id, cmd);
        }
    }
}

/// Is this role pinned to a task point this round?
///
/// Only the pioneer can hold a self-evolution task (`validate` admits
/// `acceptTask` for pioneers alone), and only while the task is live with a
/// point to hold. A session whose point was never delivered still freezes the
/// role: it is mid-accept, and stepping away would end the task before it
/// started.
fn holds_task_point(state: &BotState, role: &Unit) -> bool {
    state.task.active && role.kind == crate::model::UnitKind::Pioneer
}

/// Last resort for a role that produced no command: close on the station.
/// Returns `None` when holding position is the right answer (already at a gun
/// or already inside), so the deliberate dusk lock-in is never overridden.
pub(crate) fn fallback_toward_station(turn: &Turn, role: &Unit) -> Option<RoleCommand> {
    let station = turn.station()?;
    let footprint = station.footprint();
    if footprint_distance(role.pos, &footprint) <= 1 {
        return None; // already home: standing still here is the plan
    }
    if turn
        .towers()
        .iter()
        .any(|tower| chebyshev(role.pos, tower.pos) <= 1)
    {
        return None; // on post: holding the gun outranks everything
    }
    let stands = crate::brain::interior_cells(turn);
    if stands.is_empty() {
        return None;
    }
    // Demolish our own wall if that is the only way home — the same escape
    // hatch the night recall keeps (P0-3). A role sealed out holds still
    // forever otherwise: `walk_toward` finds no route, this returns nothing,
    // and since it is `away` from home the dusk seal never fires either, so the
    // ring stays open all night with the role on the wrong side of it.
    //
    // The ladder rather than `walk_or_remove_wall` alone, because a role the
    // backstop reaches has already been declined by every step above it, and
    // the demolition hatch still refuses when no SINGLE cut reopens the route
    // (issues #176-#185: 20011 on (24,16) for twelve straight dusk rounds).
    walk_home_or_reroute(turn, role, &stands, &mut HashSet::new())
}

/// Merged prompt: when BOTH the news read and the treasure ask want the
/// round's prompt slot, send a single prompt that asks both questions (issue
/// #207 §3: 「要尽快向大模型发起民间传闻的解读以及对官方消息的解读，合并在一个
/// 回合中，用一个 prompt 向大模型发起两个提问」). One round-trip instead of two,
/// and a second question that costs no extra slot.
///
/// Returns `true` if a merged prompt was sent, `false` if either consumer
/// doesn't need asking (in which case the individual `news::plan_prompt` /
/// `treasure::plan_ask` calls handle it as before, unchanged).
///
/// BOTH is the whole rule, and each half is the consumer's OWN predicate —
/// `news::plan_prompt`'s (`wants_reading` + today's text on the board) and
/// `treasure::plan_ask`'s (`Idle` + two legends + day ≥ 2). Merge only when the
/// two asks would otherwise BOTH go out this round, and the merge can never
/// stand a consumer down: with one of them wanting the slot the call above
/// falls through to the same two functions in the same order they were in
/// before this existed.
///
/// The budget is asked for on the news's purpose — the highest ranked and the
/// one never stood down. Asking for the treasure's purpose as well would
/// REFUSE the merge on every day whose news is still unread (`news_read_reserved`
/// stands the treasure down precisely then), which is the one round the merge
/// exists for; and it would refuse it for a reason that does not apply, since
/// the altar's question rides on the news's own ask and spends no second slot.
fn plan_merged_prompt(turn: &Turn, state: &mut BotState, plan: &mut Plan) -> bool {
    if plan.prompt.is_some() {
        return false;
    }
    // Both consumers must want the slot.
    let news_wants = state.news.wants_reading()
        && state.official_seen.get(&turn.day).is_some();
    let treasure_wants = state.treasure.phase == crate::state::TreasurePhase::Idle
        && state.treasure.legends.len() >= 2
        && turn.day >= 2;
    if !(news_wants && treasure_wants) {
        return false;
    }
    // Check the LLM budget via the News purpose (highest priority — never
    // stood down).
    if !state.request_prompt(crate::state::PromptPurpose::News, turn) {
        return false;
    }
    // Build the merged prompt.
    let news_text = state.official_seen.get(&turn.day).cloned().unwrap_or_default();
    let keyword = news::price_outlook(turn.day, &news_text);
    let news_correction = state.news.correction.clone();
    let legends: Vec<(i64, String)> = state.treasure.legends.iter().cloned().collect();
    let treasure_wrong_items = state.treasure.wrong_item_rounds > 0;
    let treasure_time_feedback = state.treasure.time_feedback;

    crate::log::event(
        "merged_prompt_ask",
        serde_json::json!({
            "day": turn.day,
            "round": turn.round_no,
            "keyword": keyword.len(),
            "legends": legends.len(),
        }),
    );
    plan.prompt = Some(news::build_merged_prompt(
        turn.day,
        &news_text,
        &keyword,
        news_correction.as_deref(),
        &legends,
        treasure_wrong_items,
        treasure_time_feedback,
    ));
    // Set BOTH phases to AskedLlm so both response handlers process the answer.
    state.news.attempts += 1;
    state.news.phase = news::ReadPhase::AskedLlm {
        round: turn.round_no,
    };
    state.treasure.time_feedback = false;
    state.treasure.phase = crate::state::TreasurePhase::AskedLlm {
        round: turn.round_no,
    };
    true
}

fn pioneer_day(
    turn: &Turn,
    state: &mut BotState,
    pioneer: &Unit,
    pairs: &[(i64, i64)],
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    // News/treasure prompt planning has been moved to `plan()` above the
    // `pioneer_day` call, so a dead pioneer does not silence the LLM ask.
    // The prompt slot is a property of the ROUND, not of whichever role is
    // alive to take it (issue #218: merged prompt never sent because pioneer
    // died on D1, leaving news/treasure LLM asks stranded for the whole game).

    // 0. Dusk recall. The gate seal waits for EVERY role to be inside the ring,
    //    and the pioneer is the one role whose work — task points, treasure,
    //    the shop — is always outside it. Issue #15: `wall_gate_open` for
    //    fifteen straight rounds — the whole dusk window, so the gate never
    //    sealed — while the
    //    pioneer accepted a fresh task at r=58 and again at r=69, one round
    //    before nightfall; the gate cell was never walled, the ring kept a
    //    robot-sized hole all night, and the controllers standing behind it
    //    were picked off one at a time. The recall round is measured from where
    //    the pioneer IS, so it tightens as the day goes on and an errand
    //    accepted at r=40 is not still being walked at r=54.
    //
    //    This subsumes the older "abort at dusk when a tower would go unmanned"
    //    checkpoint: it fires strictly earlier (the walk home is already
    //    subtracted) and for every pioneer, paired or not.
    //    Latched (`dusk_committed`): this deadline is measured from where the
    //    pioneer IS, so re-testing it each round used to let a pioneer that had
    //    walked one cell closer fall back out of the recall, accept a task, and
    //    walk straight back out — the same two-cell oscillation §2.4 measured on
    //    the workers, on the one role whose work is always outside the ring.
    let recalled = dusk_committed(state, turn, pioneer, pioneer_recall_round(turn, pioneer));
    // Is the altar's window this round's business (P2-3)? One computation, read
    // in the three places 「顺序上不能固定」 moves: the session it takes the
    // pioneer off, the altar step it puts above the task accept, and the task
    // accept it stands down. `window_due` is the treasure's own predicate —
    // measured in the pioneer's walk, see `treasure::window_due` — and `!recalled`
    // is the unchanged dusk constraint on top of it: the window does not outrank
    // being home before nightfall.
    let altar_due = !recalled && treasure::window_due(turn, state, pioneer);
    // A session that has produced nothing at all after `MAX_STERILE_ROUNDS` is
    // not converging, and the pioneer is worth more on the wall line than on a
    // point nothing is coming out of (issue #15's lesson; issue #26 lost the
    // base to a gate no controller had retreated through). Checked before the
    // recall so the log names the real reason a session ended.
    if state.task.active
        && state.task.best_answer.is_empty()
        && state.task.submitted_round.is_none()
        && turn.round_no - state.task.accepted_round >= MAX_STERILE_ROUNDS
    {
        crate::log::event(
            "task_defense_abort",
            serde_json::json!({
                "round": turn.round_no,
                "session": state.task.session_id,
                "task_kind": state.task.kind.as_str(),
                "dayRound": turn.in_day_round,
                "reason": "sterile",
            }),
        );
        state.finish_task(false, "sterile");
    }
    if state.task.active && recalled {
        crate::log::event(
            "task_defense_abort",
            serde_json::json!({
                "round": turn.round_no,
                "session": state.task.session_id,
                "task_kind": state.task.kind.as_str(),
                "dayRound": turn.in_day_round,
                "reason": "dusk_recall",
            }),
        );
        state.finish_task(false, "dusk_recall");
    }
    // 1. THE ONE-SHOT RACE (P2-3). The altar is the one thing on the board that
    //    does not come back: 任务书 5.2 — 一张地图宝藏只有一个, a legal summon
    //    consumes the offering whatever the outcome, and a round spent elsewhere
    //    is a round the opponent can take it in. A self-evolution session is the
    //    opposite: 5.3 puts the point back after a 30-round refresh and pays its
    //    own reward. So a session still running when the window opens is
    //    ABANDONED for the altar rather than finished — the pioneer is leaving
    //    the point either way (「离开己方任务点周围一格内」 ends the task), and the
    //    only choice left is whether it leaves for the treasure or for nothing.
    if state.task.active && altar_due {
        crate::log::event(
            "task_defense_abort",
            serde_json::json!({
                "round": turn.round_no,
                "session": state.task.session_id,
                "task_kind": state.task.kind.as_str(),
                "dayRound": turn.in_day_round,
                "reason": "treasure_race",
            }),
        );
        state.finish_task(false, "treasure_race");
    }
    if state.task.active {
        if let Some(cmd) = task::plan_pioneer(turn, state, pioneer, plan) {
            plan.push(pioneer.id, cmd);
            return;
        }
        // Nothing to send this round — which is most rounds. HOLD THE POINT:
        // 任务书 5.3 ends the task on "离开己方任务点周围一格内", so standing
        // still is the job, and the one move still worth making is the one that
        // puts the pioneer back inside that cell after something nudged it off.
        // The `plan` backstop is blocked from walking it home by
        // `holds_task_point`; without that pairing the pioneer was walked off
        // the point the round after it accepted and issue #17's sessions died
        // with every `executeCmd` refused.
        if let Some(point) = state.task.point {
            if chebyshev(pioneer.pos, point) > 1 {
                let stands = stand_cells(turn, point);
                if let Some(cmd) = walk_toward(turn, pioneer, &stands, claimed) {
                    plan.push(pioneer.id, cmd);
                }
            }
        }
        return;
    }
    // 2. Self-heal.
    if let Some(cmd) = use_medicine(pioneer) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 3. Pre-position near the assigned tower once the deadline hits: the
    //    pioneer walks to its tower BEFORE accepting a new task or chasing
    //    treasure, so it is not stranded far away when night falls. Once the
    //    deadline passes the role locks to the tower (wall demolition stays
    //    worker-only, so only walk_toward is used here).
    if let Some(tower_id) = pairs
        .iter()
        .find(|(controller, _)| *controller == pioneer.id)
        .map(|(_, tower)| *tower)
    {
        if let Some(tower) = turn.role_by_id(tower_id) {
            let dist = chebyshev(pioneer.pos, tower.pos);
            if turn.in_day_round >= preposition_round(dist) {
                // As for the workers: the post is a cell the gun can be
                // operated from, and a diagonal neighbour of a gun is a cell of
                // the radius-2 wall ring — a pioneer parked there is outside
                // the ring it is supposed to shelter behind.
                let stands = tower_stand_cells(turn, tower.pos);
                // If the pioneer is already inside the ring (dist <= 1 from
                // the station), hold there — walking out to an outer-ring
                // stand cell would undo the dusk latch.
                let station = turn.station();
                let inside = station.map_or(false, |s| {
                    footprint_distance(pioneer.pos, &s.footprint()) <= 1
                });
                if !inside && !stands.iter().any(|stand| *stand == pioneer.pos) {
                    let walked = walk_toward(turn, pioneer, &stands, claimed);
                    if let Some(cmd) = walked {
                        plan.push(pioneer.id, cmd);
                        return;
                    }
                    // Same fallback as the workers: a pioneer whose tower is
                    // walled off retreats inside rather than standing idle.
                    if let Some(cmd) = retreat_inside(turn, state, pioneer, claimed) {
                        plan.push(pioneer.id, cmd);
                        return;
                    }
                }
                // Already at the post (or inside the ring): hold there.
                return;
            }
        }
    }
    // 3b. THE ALTAR, while its window is live (P2-3). This step exists because
    //     the order used to be fixed: the task accept (step 4) sat above the
    //     treasure (step 6) and `next_task_point` returns a command the moment a
    //     point is acceptable, so a pioneer that took a task on the altar's
    //     opening day never summoned at all. It did not arrive a round late, it
    //     never went. 「顺序上不能固定」 — while the window is due, the altar is
    //     the errand and the task point waits.
    //
    //     Below the tower lock on purpose. `preposition_round` (step 3) is what
    //     puts an operator behind a gun before nightfall, and that is a hard
    //     constraint: the night recall is its backstop, not its plan. The race
    //     is also won in the morning — `window_due` measures the walk against
    //     the daylight that is left, so a window still worth running is one the
    //     pioneer reaches long before its post comes due — and a pioneer that
    //     has already locked to its gun has a window it cannot win anyway.
    if altar_due {
        if let Some(cmd) = treasure::plan_pioneer(turn, state, pioneer, claimed, plan) {
            plan.push(pioneer.id, cmd);
            return;
        }
        if treasure::holds_altar(turn, state, pioneer) {
            return;
        }
    }
    // 4. Accept a fresh task when a point is ready (walk there first). Not
    //    once the dusk recall has fired: a task accepted now is a task that
    //    keeps the pioneer at the point through the seal. And not while the
    //    altar's window is due (step 3b): a session is fifteen rounds at a point
    //    the window is about to make the wrong place to stand.
    if !recalled && !altar_due {
        if let Some(cmd) = state.next_task_point(turn, pioneer, claimed) {
            plan.push(pioneer.id, cmd);
            return;
        }
    }
    // 5. Vouchers in the backpack. Our own buildings, so this stays available
    //    after the recall — it is an errand on the way home, not out of it.
    if let Some(cmd) = voucher_flow(turn, pioneer, claimed) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 5b. Personal Medicine: while already at the shop (no detour), and — for a
    //     pioneer below the night withdrawal threshold, which is otherwise
    //     stuck at that health forever — the walk there too, but only before
    //     the dusk recall. After it the pioneer belongs at its gun.
    if let Some(cmd) = self_provision(turn, pioneer, claimed, !recalled) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 6. Treasure hunt. Outdoors, so the same cutoff as the task point.
    if !recalled {
        if let Some(cmd) = treasure::plan_pioneer(turn, state, pioneer, claimed, plan) {
            plan.push(pioneer.id, cmd);
            return;
        }
        // P2-1: waiting beside the altar for the opening day is a deliberate
        // HOLD, not idleness. The treasure planner returns no command for that
        // wait, and the loiter/retreat steps below would each drag the pioneer
        // a cell away so the treasure step has to drag it back the next — a
        // two-step oscillation that can spend the whole opening window
        // commuting (and that keeps the gate seal waiting on a role that is
        // never home).
        if treasure::holds_altar(turn, state, pioneer) {
            return;
        }
    }
    // 7. Loiter next to a task point so we catch refreshes immediately — until
    //    the recall round, after which loitering IS the thing being recalled.
    if !recalled {
        loiter_at_task_point(turn, state, pioneer, claimed, plan);
        if plan.commands.contains_key(&pioneer.id) {
            return;
        }
    }
    // 7b. No task point to wait at either: run the economy. With nothing to
    //     fight for the pioneer used to walk to the station and stand there for
    //     the rest of the day — 47 of day 1's 70 rounds, which is issue #13's
    //     "角色站桩闲置" in its purest form, and a third pair of hands the wall
    //     line and the purse never got. It has the same pack and the same arms
    //     as a worker; the only thing it must not do is stop being a pioneer
    //     when a task point appears, so this sits below the task, voucher,
    //     treasure and loiter steps.
    //
    //     It mines for VALUE, not for the ring: a builder can only spend the
    //     stone in its own pack and the pioneer has no wall step, so stone in
    //     its hands would be the "held back" load that left the ring at 19/20.
    //     Ore it can sell is gold it can spend on towers and upgrades.
    //
    //     Never past the pre-position deadline (step 3 above runs first), so an
    //     errand can still not cost the pioneer its gun at dusk.
    // 7b. Nothing to fight for: shelter inside the ring. NOT the mine→sell loop
    //     — that was tried and it is worse than useless: `validate` admits
    //     `collect` and `sell` for Workers only, so a pioneer handed the
    //     economy produced a command every round that the judger dropped on the
    //     floor. The role looked busy to the planner and stood rooted to the
    //     spot on the board — 47 of day 1's 70 rounds at (17,22) with an empty
    //     pack, which is exactly the "角色站桩闲置" of issue #13. What a pioneer
    //     can actually do with a free afternoon is get behind the wall line
    //     before the robots come, and that is also what lets the gate seal.
    // 8. Nothing to do at all — no task point, no treasure, no voucher. Retreat
    //    inside the ring instead of standing in the open: a lone role parked
    //    outside the wall line is a free kill for the robots, and it is also
    //    what stops `update_wall_gate` from ever sealing (the gate waits for
    //    everyone to be inside). Issue #13's "0 角色站桩闲置" is exactly this —
    //    a role with no task, no treasure and no post, silent for the rest of
    //    the day while a gun stands unmanned nearby.
    if let Some(cmd) = retreat_inside(turn, state, pioneer, claimed) {
        plan.push(pioneer.id, cmd);
    }
}

/// Accept or walk toward the first valid task point.
impl BotState {
    pub fn next_task_point(
        &mut self,
        turn: &Turn,
        pioneer: &Unit,
        claimed: &mut HashSet<Pos>,
    ) -> Option<RoleCommand> {
        let candidate = turn
            .player_tasks
            .iter()
            .filter(|task| task.is_valid && task.cooldown_rounds == 0)
            // A point whose execution window the judger already shut stays
            // shut for this pioneer: re-accepting it only re-enters the same
            // refusal. See `BotState::task_refusals`.
            .filter(|task| {
                !self
                    .task_refusals
                    .get(&task.pos)
                    .map_or(false, |until| turn.round_no <= *until)
            })
            // P1-4: the kind outranks the distance. 推理 + 传闻 first, 自进化
            // next, everything else last; distance only decides inside a lane.
            // Until this existed the pioneer took whatever point was nearest,
            // which on a board carrying two kinds is a coin toss between the
            // day's reasoning and its sandbox. The kind is also stamped on the
            // session, so the log says which lane the rounds went to.
            .min_by_key(|task| {
                (
                    crate::brain::task::classify(&task.task_type).rank(),
                    chebyshev(pioneer.pos, task.pos),
                    task.pos.x,
                    task.pos.y,
                )
            })?;
        if chebyshev(pioneer.pos, candidate.pos) <= 1 {
            // The clock gates the ACCEPT, never the approach: a point accepted
            // with fewer rounds left than a session needs is a point sold for
            // its 30-round cooldown (see `TASK_MIN_ATTEMPT_ROUNDS`), but
            // walking toward a point is free and the morning is the only time
            // the pioneer has. Measured from where the pioneer IS, so standing
            // out at the point already costs the walk home — which is the same
            // deadline the recall will enforce.
            if turn.in_day_round + TASK_MIN_ATTEMPT_ROUNDS > pioneer_recall_round(turn, pioneer) {
                crate::log::event(
                    "task_accept_deferred",
                    serde_json::json!({
                        "round": turn.round_no,
                        "dayRound": turn.in_day_round,
                        "point": candidate.pos,
                        "recall": pioneer_recall_round(turn, pioneer),
                    }),
                );
                return None;
            }
            let timeout = if candidate.timeout_rounds > 0 {
                candidate.timeout_rounds
            } else {
                250
            };
            self.task_session_seq = self.task_session_seq.saturating_add(1);
            let session_id = self.task_session_seq;
            let kind = crate::brain::task::classify(&candidate.task_type);
            crate::log::event(
                "task_accept",
                serde_json::json!({
                    "round": turn.round_no,
                    "session": session_id,
                    "point": candidate.pos,
                    "taskType": candidate.task_type,
                    "task_kind": kind.as_str(),
                    "kindRank": kind.rank(),
                }),
            );
            self.task = TaskSession {
                active: true,
                session_id,
                accepted_round: turn.round_no,
                timeout_round: turn.round_no + timeout,
                point: Some(candidate.pos),
                task_type: candidate.task_type.clone(),
                kind,
                ..Default::default()
            };
            return Some(RoleCommand::accept_task());
        }
        let stands = stand_cells(turn, candidate.pos);
        walk_toward(turn, pioneer, &stands, claimed)
    }
}

/// Day-round from which the pioneer stops taking on anything outside the wall
/// line and heads home: `DUSK_ROUND` less the walk back, less a small slack.
///
/// See the dusk recall in `pioneer_day` for why this exists. The walk is
/// measured from the pioneer's CURRENT cell, so the deadline is a rolling one
/// — the further out it has drifted, the earlier it has to turn around, and an
/// errand it cannot finish and still be home by dusk is never started.
fn pioneer_recall_round(turn: &Turn, pioneer: &Unit) -> i64 {
    let walk = walk_home(turn, pioneer.pos);
    (economy::DUSK_ROUND - 1 - walk - PIONEER_RETREAT_SLACK).max(0)
}

/// Rounds of walking from `from` to the nearest cell inside the ring.
pub(crate) fn walk_home(turn: &Turn, from: Pos) -> i64 {
    crate::brain::interior_cells(turn)
        .iter()
        .map(|cell| chebyshev(from, *cell))
        .min()
        .unwrap_or(0) as i64
}

/// Has this role's dusk commitment fired? Latching is the whole point: the
/// deadline above is measured from where the role IS, so re-testing it every
/// round lets a role that has walked one cell closer fall back out of the
/// commitment, take an economy errand, and walk straight back out — the
/// two-cell oscillation of §2.4. One commitment per role per day; the set is
/// cleared at day rollover (`BotState::observe`).
fn dusk_committed(state: &mut BotState, turn: &Turn, role: &Unit, deadline: i64) -> bool {
    if state.dusk_home.contains(&role.id) {
        return true;
    }
    if turn.in_day_round < deadline {
        return false;
    }
    state.dusk_home.insert(role.id);
    true
}

fn loiter_at_task_point(
    turn: &Turn,
    state: &BotState,
    pioneer: &Unit,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    let mut stands: Vec<Pos> = Vec::new();
    for task in &turn.player_tasks {
        // Loitering beside a refused point is the same loop one step slower:
        // the pioneer camps the window it may not use instead of taking the
        // treasure or the walk home. See `BotState::task_refusals`.
        if state
            .task_refusals
            .get(&task.pos)
            .map_or(false, |until| turn.round_no <= *until)
        {
            continue;
        }
        stands.extend(stand_cells(turn, task.pos));
    }
    if stands.is_empty() {
        return;
    }
    if stands.iter().any(|pos| *pos == pioneer.pos) {
        return; // already loitering
    }
    if let Some(cmd) = walk_toward(turn, pioneer, &stands, claimed) {
        plan.push(pioneer.id, cmd);
    }
}

/// Walk a role back inside the ring, onto a cell at footprint distance <= 1
/// from the station. Returns `None` when it is already inside or when nothing
/// inside is reachable at all, so a genuinely boxed-in role holds rather than
/// paces. "Reachable" now includes demolishing one of our own walls on the way
/// in (`walk_or_remove_wall`, P0-3): the ring was built to keep robots out, and
/// a role the crew accidentally sealed on the wrong side of it is a role the
/// night cannot use, which costs more than one wall cell. With the permanent
/// entrance (design D17) the wrong side is a pathing accident, not a daily
/// certainty, and the entrance column is always walkable.
///
/// `_state` is vestigial: the shelter list used to be filtered off the day's
/// gate approach cells (`gate_clear_stands`), and the gate died with D17.
fn retreat_inside(turn: &Turn, _state: &BotState, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
    let station = turn.station()?;
    let footprint = station_footprint(station.pos);
    if footprint_distance(role.pos, &footprint) <= 1 {
        return None;
    }
    let stands = crate::brain::interior_cells(turn);
    if stands.is_empty() {
        return None;
    }
    walk_home_or_reroute(turn, role, &stands, claimed)
}

/// Walk a role home under a dusk deadline: the second rung of the night
/// recall's ladder, which the day side never got (issues #176-#185).
///
/// [`walk_or_remove_wall`] has a deliberate HOLD branch: when `walk_toward`
/// finds nothing but a route exists once this round's claims are ignored, it
/// issues NO command, on the reasoning that a claim is an intent and the
/// claimer moves on. That reasoning holds mid-day and fails at a deadline,
/// because at a deadline the claimer does not move on — it is walking home too
/// and stops on the cell it claimed. Two roles whose only route crosses each
/// other's claimed cell then freeze each other, and neither issues a command
/// again.
///
/// The measurement is exactly that, and the log says so in its own column. A
/// role frozen on one cell for eleven or twelve straight dusk rounds is listed
/// by `wall_gate_open` with an EMPTY `stuck` list, and `stuck` is populated by
/// `can_reach_any` — which is `step_toward_stands` over `blocked_for(-1)`, the
/// pathfinder with every claim dropped. An empty `stuck` therefore *is* the
/// hold branch's guard: the route home exists, the role is not walled off, it
/// simply never takes a step. In 179 the dusk window names 20012 on (28,10)
/// for eleven rounds; 178 does it twice over, 20011 on (24,16) for twelve and
/// 20012 for eleven, and `wall_gate_forced` then seals the ring with BOTH of
/// them outside — that match also carries the batch's worst `tower_unpaired`
/// (78), the guns of the two roles the ring closed on. 176 and 182 are the same
/// shape, and eight of the ten reports leave the gate open for 5-12 of the
/// fifteen dusk rounds.
///
/// So under a deadline the hold is not a trade, it is the loss: survival is
/// `10xday` for every day the station stands (任务书 ch.6, 550 over ten) and the
/// ring is what the station stands behind.
///
/// This is the SAME rung `night.rs` already runs for its recall — try the
/// claims, then try ignoring them — for the same defect ("126's 20040 for 11
/// rounds at (27,13), 129's 20020 for 14 at (32,10). Both guns were silent for
/// the whole night"). The night side got it in #126-#130 and the day side did
/// not. `break_out` is deliberately NOT part of this: its other half cuts a
/// wall, and by day a wall is the asset being defended rather than the
/// obstacle — a role that is genuinely walled off keeps
/// `walk_or_remove_wall`'s demolition hatch, which only ever cuts a cell whose
/// removal reopens the route.
///
/// `claimed` is filled on the first rung and left alone afterwards, so a role
/// that takes the ignoring-claims rung does not reserve the cell against a
/// teammate's legal move.
pub fn walk_home_or_reroute(
    turn: &Turn,
    role: &Unit,
    stands: &[Pos],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if let Some(cmd) = walk_or_remove_wall(turn, role, stands, claimed) {
        return Some(cmd);
    }
    // The round's reservations dropped. With no claims in the set, `walk_toward`
    // and the claim-free pathfinder agree, so a role the hold branch would have
    // parked instead takes the step it can already legally take.
    let mut ignored = HashSet::new();
    walk_or_remove_wall(turn, role, stands, &mut ignored)
}

/// Is there a walkable route from `start` to any of `stands`? A role already
/// standing on a stand cell counts as reachable (no move needed).
pub(crate) fn can_reach(turn: &Turn, start: Pos, stands: &[Pos]) -> bool {
    if stands.iter().any(|stand| *stand == start) {
        return true;
    }
    let blocked = turn.blocked_for(-1);
    crate::path::step_toward_stands(turn, start, stands, &blocked).is_some()
}

/// Stand cells of the tower a role is paired with for the coming night.
pub(crate) fn night_goal(turn: &Turn, pairs: &[(i64, i64)], role_id: i64) -> Option<Vec<Pos>> {
    let tower_id = pairs
        .iter()
        .find(|(controller, _)| *controller == role_id)
        .map(|(_, tower)| *tower)?;
    let tower = turn.role_by_id(tower_id)?;
    Some(tower_stand_cells(turn, tower.pos))
}

/// Where a role has to be able to get before the ring closes around it: the
/// operating cells of the tower it mans tonight, or — for a role with no gun to
/// man — the inside of the ring itself.
///
/// The second case is the whole of issue #15's "idle role walled out". With
/// two towers and three roles, the odd role out has no `night_goal` at all, so
/// both safety checks used to `continue` past it: it could be mining outside,
/// the crew could close the last ring cell behind it, and its home — the inner
/// band `update_wall_gate` measures "everyone is inside" against — became
/// unreachable. The gate then never sealed (`wall_gate_open` for all fifteen
/// rounds of the dusk window, 5c naming the role), and the base spent the night
/// behind an open wall. An idle role's home is as load-bearing as a
/// controller's gun, so it is checked the same way.
fn night_home(turn: &Turn, pairs: &[(i64, i64)], role_id: i64) -> Vec<Pos> {
    night_goal(turn, pairs, role_id).unwrap_or_else(|| crate::brain::interior_cells(turn))
}

/// Every controllable role must still be able to reach its night post — its
/// gun, or the inside of the ring when it has no gun. If any can't, wall
/// building must stop: the gate stays open until everyone is inside, so we
/// never seal a role outside the ring.
pub(crate) fn roles_can_reach(turn: &Turn, pairs: &[(i64, i64)]) -> bool {
    for role in turn.controllable() {
        let home = night_home(turn, pairs, role.id);
        if home.is_empty() {
            continue; // nowhere to be: not a verdict this check can make
        }
        if !can_reach(turn, role.pos, &home) {
            return false;
        }
    }
    true
}

/// Would placing a wall at `site` cut any role off from its weapon, or seal a
/// gap the ring still has to fill away from the workers who can fill it?
/// Simulate the wall and re-run both reachability checks. Together with the
/// far-side-first build order, this guarantees the ring is only ever closed
/// after everyone has retreated inside — and that the last stone can still
/// reach the last gap.
pub(crate) fn wall_would_trap(
    turn: &Turn,
    pairs: &[(i64, i64)],
    state: &BotState,
    site: Pos,
    owned: &[i64],
) -> bool {
    let mut blocked = turn.blocked_for(-1);
    blocked.insert(site);
    for role in turn.controllable() {
        // A unit dispatched by its own mainline is OUTSIDE ON PURPOSE (comment
        // 1 §6: the economy worker does not come home at dusk). Standing beyond
        // this wall is that unit's plan, not an accident the wall caused, so
        // it never gets a veto — otherwise the ring's last cell waits forever
        // for a worker who is never coming back (the seal board finished at
        // 19/20 with the door open and the worker's own stones two maps away).
        if owned.contains(&role.id) {
            continue;
        }
        // A role with no gun to man is measured against the inside of the ring
        // (see `night_home`): "no tower" is not "no home", and the hole this
        // wall would cut is in ITS way home, not only in a controller's.
        let home = night_home(turn, pairs, role.id);
        if home.is_empty() {
            continue;
        }
        if home.iter().any(|stand| *stand == role.pos) {
            continue; // already at the weapon / already home: nothing to trap
        }
        // A role that cannot reach its post even WITHOUT this wall is not
        // what the wall would trap. Counting it anyway vetoes every remaining
        // ring cell at once — which is how day 1 ended at 19/20 with the last
        // stone sitting in a backpack (issues #12/#13/#14).
        if !crate::brain::can_reach_any(turn, role, &home) {
            continue;
        }
        if crate::path::step_toward_stands(turn, role.pos, &home, &blocked).is_none() {
            return true;
        }
    }
    // The same question for the wall line itself. A ring cell is only
    // buildable from a cell next to it, and a cell a teammate was standing on
    // when the sweep went past is exactly the one that stays open. Closing the
    // ring over the top of it leaves that hole reachable from the outside
    // only, with the stone on the wrong side of the wall — day 1 finished
    // 19/20 with two stones stuck in a backpack exactly that way.
    let can_build = |role: &Unit, gap: Pos, blocked: &HashSet<Pos>| -> bool {
        if chebyshev(role.pos, gap) == 1 {
            return true;
        }
        let stands = stand_cells(turn, gap);
        crate::path::step_toward_stands(turn, role.pos, &stands, blocked).is_some()
    };
    let before = turn.blocked_for(-1);
    for gap in pending_ring(turn, state, site) {
        let reachable = |blocked: &HashSet<Pos>| {
            turn.controllable()
                .iter()
                .filter(|role| !owned.contains(&role.id))
                .any(|role| can_build(role, gap, blocked))
        };
        // A gap nobody could reach even before this wall is not the wall's
        // doing, and vetoing on its account would leave the ring open forever.
        if reachable(&before) && !reachable(&blocked) {
            return true;
        }
    }
    false
}

/// Ring cells that are still to be filled, IGNORING whether a teammate happens
/// to be standing on one. `wall_gaps` drops an occupied cell so we never build
/// under a unit, but that same filter hides the cell from the build-order
/// safety check — and a ring cell a role was standing on when the sweep went
/// past is precisely the one that ends up walled off from the inside, with the
/// stone on the wrong side. Day 1 finished 19/20 exactly that way.
///
/// The list is the fixed build order (comment 1 §4), so the four permanent
/// entrance cells are not in it and never were "pending": the gate clause this
/// function used to carry died with the gate (design D17).
pub(crate) fn pending_ring(turn: &Turn, state: &BotState, ignore: Pos) -> Vec<Pos> {
    let walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    super::action::base_layout::wall_build_order(turn)
        .into_iter()
        .filter(|pos| {
            *pos != ignore
                && turn.is_land(*pos)
                && !walls.contains(pos)
                && !state
                    .blacklisted_builds
                    .contains(&(*pos, "wall".to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Build site planning
// ---------------------------------------------------------------------------

/// The stone the day still owes: wall gaps inside today's budget, and nothing
/// else — the door and gate terms died with the gate (design D17), and the
/// permanent entrance is never walled. Same arithmetic `plan` runs inline,
/// recomputed from pure queries so a role mainline (issue #221) can ask it
/// without threading `plan`'s locals. Reads the STORED `ring_ever_complete`,
/// which `plan` may set a round later — a one-round lag on the cap latch,
/// never on the gap count.
pub(crate) fn stone_demand_of(turn: &Turn, state: &BotState) -> i64 {
    let wall_gaps = wall_gaps(turn, state);
    let primary_open = ring_open_cells(turn, state);
    let wall_cap = wall_daily_cap(turn.day, state.ring_ever_complete, primary_open);
    (wall_gaps.len() as i64).min(wall_cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::action::geometry::ring_cells;
    use crate::model::{neighbours, ORES};

    fn pos(x: i32, y: i32) -> Pos {
        Pos { x, y }
    }

    fn unit(id: i64, kind: &str, at: Pos) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": kind,
            "health": 1000, "level": 1, "backPackCapability": 100, "backpack": []
        })
    }

    fn board(roles: Vec<serde_json::Value>) -> Turn {
        let payload = serde_json::json!({
            "roundNo": 5,
            "mapInfo": {"width": 41, "height": 32, "zones": []},
            "teamOur": {
                "type": "challenger", "goldNum": 0, "totalScore": 0,
                "playerTasks": [], "roles": roles
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
        });
        let req: crate::protocol::Request =
            serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    /// One worker in a pocket of our own walls whose only opening is `(21,20)`,
    /// plus the station. Nothing pairs it to a gun: this is the "idle role" of
    /// P0-3, the third role on a two-tower board. `seal` closes the opening.
    fn pocketed_idle_role(seal: bool) -> Turn {
        let mut roles = vec![unit(10001, "station", pos(10, 24))];
        let mut cells = vec![
            pos(19, 19),
            pos(19, 20),
            pos(19, 21),
            pos(20, 19),
            pos(20, 21),
            pos(21, 19),
            pos(21, 21),
        ];
        if seal {
            cells.push(pos(21, 20));
        }
        for (index, at) in cells.iter().enumerate() {
            roles.push(unit(20000 + index as i64, "wall", *at));
        }
        roles.push(unit(10002, "worker", pos(20, 20)));
        board(roles)
    }

    /// P0-3: a role with no gun to man still has a home — the inside of the
    /// ring — and a wall that would cut it off from that home is a wall the
    /// crew may not build. Both safety checks used to `continue` past such a
    /// role, which is how an idle worker ended a day sealed outside the ring
    /// with the gate open behind it (docs/FAILURE-ANALYSIS-2026-09-14.md §3.3).
    #[test]
    fn a_wall_that_seals_an_idle_role_out_is_refused() {
        let turn = pocketed_idle_role(false);
        let state = BotState::default();
        let idle = turn.role_by_id(10002).expect("the idle role exists");
        assert!(
            night_goal(&turn, &[], idle.id).is_none(),
            "test setup: this role mans no gun tonight"
        );
        // The pocket has exactly one opening and the role is not standing on
        // its home band, so today it can still get home — which is what makes
        // the wall on that opening the crew's doing and not the role's problem.
        assert!(
            crate::brain::can_reach_any(&turn, idle, &crate::brain::interior_cells(&turn)),
            "test setup: the role must be able to reach home BEFORE the wall"
        );
        assert!(
            roles_can_reach(&turn, &[]),
            "test setup: nobody is cut off yet, so the wall step is running"
        );
        assert!(
            wall_would_trap(&turn, &[], &state, pos(21, 20), &[]),
            "the one cell that lets the idle role home was about to be walled over"
        );
    }

    /// …and once that cell IS wall — by the crew, by a robot, or by the crew's
    /// own earlier mistake — the whole wall step stands down instead of
    /// building somewhere else with a role sealed out of its own base.
    #[test]
    fn an_idle_role_already_sealed_out_stops_the_wall_line() {
        let sealed = pocketed_idle_role(true);
        assert!(
            !roles_can_reach(&sealed, &[]),
            "wall building went ahead with a role sealed out of its own base"
        );
    }

    /// The same predicate on a board where nobody is cut off: an idle role
    /// standing inside is not a veto, and neither is a wall far from it.
    #[test]
    fn a_wall_nobody_is_cut_off_by_is_allowed() {
        let turn = board(vec![
            unit(10001, "station", pos(10, 24)),
            unit(10002, "worker", pos(12, 24)),
        ]);
        let state = BotState::default();
        assert!(
            roles_can_reach(&turn, &[]),
            "an idle role standing inside the base is not a veto"
        );
        assert!(
            !wall_would_trap(&turn, &[], &state, pos(20, 20), &[]),
            "a wall in the open, far from every role's way home, traps nobody"
        );
    }

    #[test]
    fn ring_distance_one_of_footprint() {
        let footprint = station_footprint(pos(10, 24));
        let ring = ring_cells(&footprint, 1);
        assert_eq!(ring.len(), 12); // 4x4 outer minus 2x2 footprint
        assert!(ring
            .iter()
            .all(|cell| footprint_distance(*cell, &footprint) == 1));
    }

    #[test]
    fn radius_two_ring_is_one_complete_layer() {
        let footprint = station_footprint(pos(10, 24));
        let ring = ring_cells(&footprint, 2);
        assert_eq!(ring.len(), 20);
        assert!(ring
            .iter()
            .all(|cell| footprint_distance(*cell, &footprint) == 2));
        assert!(ring
            .iter()
            .all(|cell| footprint_distance(*cell, &footprint) != 3));
        let _ = neighbours(pos(0, 0));
        let _ = ORES;
    }
}
