//! Daytime planning (70 rounds): workers run the economy loop
//! (mine → sell → build towers/walls → upgrades), the pioneer runs tasks,
//! the treasure hunt and shopping.

use std::collections::HashSet;

use crate::brain::{
    economy, night, stand_cells, task, tower_stand_cells, treasure, walk_or_remove_wall,
    walk_toward, Plan,
};
use crate::model::{
    chebyshev, footprint_distance, station_footprint, Turn, Unit, STONE, WEAPON_BUILD_COST,
};
use crate::protocol::{Pos, RoleCommand};
use crate::state::{BotState, TaskSession};

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

/// Stones a wall trip must be able to carry home before it is worth starting.
/// A trip that lands one stone is not a wall line, it is a lost day: issue #17
/// spent the whole of D1 walking to a vein on the far side of the map and put
/// up zero walls.
const MIN_WALL_LOAD: i64 = 4;
/// Rounds kept in hand on top of the walk home before a stone carrier gives up
/// on the vein — a blocked cell, a detour, a claim.
const WALL_TRIP_SLACK: i64 = 3;

/// The day-round by which this role must be standing beside its gun. A role
/// with no gun pair is due at dusk like everyone else.
fn gun_deadline(turn: &Turn, role: &Unit, pairs: &[(i64, i64)]) -> i64 {
    pairs
        .iter()
        .find(|(controller, _)| *controller == role.id)
        .and_then(|(_, tower)| turn.role_by_id(*tower))
        .map(|tower| preposition_round(chebyshev(role.pos, tower.pos)))
        .unwrap_or(economy::DUSK_ROUND)
}

/// Can a stone trip to `vein` still land a load before this role has to be at
/// its gun? Walk out, dig `MIN_WALL_LOAD`, walk back to the wall line: a vein
/// further than that is not a wall this day, it is a walk into the dusk.
/// Issue #17: "我方仅 14 次 collect，金币峰值仅 75（初始值）… 城墙体系彻底缺失
/// （0 墙）" — the crew crossed the map for stone it could not bring home, and
/// the day produced neither a wall nor a coin.
fn stone_trip_fits(
    turn: &Turn,
    role: &Unit,
    pairs: &[(i64, i64)],
    vein: Pos,
    gaps: &[Pos],
) -> bool {
    let back = gaps
        .iter()
        .map(|gap| chebyshev(vein, *gap) as i64)
        .min()
        .unwrap_or(0);
    let rounds = chebyshev(role.pos, vein) as i64 + MIN_WALL_LOAD + back + 1;
    rounds <= gun_deadline(turn, role, pairs) - turn.in_day_round
}

/// Has the vein stopped paying for the walk home? The load already in hand is
/// what the ring gets; a carrier that keeps digging is a carrier the night
/// finds outside the wall. Measured against the wall window (dusk plus the
/// seal grace), not the gun deadline: the seal is allowed to spend the last
/// rounds of the day on the ring, and a trip that is still placing walls then
/// is a trip that worked.
fn wall_trip_overdue(turn: &Turn, role: &Unit, gaps: &[Pos]) -> bool {
    let home = gaps
        .iter()
        .map(|gap| chebyshev(role.pos, *gap) as i64)
        .min()
        .unwrap_or(0);
    turn.in_day_round + home + 1 + WALL_TRIP_SLACK >= economy::DUSK_ROUND + SEAL_GRACE
}

/// Stones this role should carry before it walks out to the wall line.
fn stone_batch(turn: &Turn, gaps: usize) -> i64 {
    let want = if turn.day == 1 {
        RING_BATCH
    } else {
        STONE_BATCH
    };
    want.min(gaps as i64).max(1)
}
/// The radius-2 station ring has 20 cells. Day 1 is allowed to complete that
/// entire single-layer shell; later days retain the conservative repair/expand
/// budget so fortification cannot permanently starve the economy.
const D1_WALL_CAP: i64 = 20;
const LATER_WALL_CAP: i64 = 6;
/// Day-rounds after dusk during which a stone carrier may still walk out to
/// close the last hole in the ring. Long enough for a round trip from any
/// tower post, short enough that the gun is manned again well before night.
const SEAL_GRACE: i64 = 8;
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

/// How many ring cells this day's fortification budget covers.
///
/// Day 1 is the build-out: the whole shell is allowed, because an open ring is
/// what the robots walk through. Later days keep a maintenance budget so wall
/// work cannot permanently starve the economy — EXCEPT when the ring has been
/// closed before and is open now. That is a breach, not upkeep: the 6-cell
/// budget cannot even re-close a ring the night took 10 walls out of, and the
/// half-spent budget is paid for by the station (issue #21: -30 residual, base
/// 1500 → 20 HP).
fn wall_daily_cap(day: i64, ring_ever_complete: bool) -> i64 {
    if day == 1 || ring_ever_complete {
        D1_WALL_CAP
    } else {
        LATER_WALL_CAP
    }
}

/// Gold reserved for tower builds. We build 1-2 towers first and keep the
/// rest for wall repair kits, medicine and upgrades — never all three slots
/// at once (battle pk575557 spent 75g on three towers and had nothing left).
pub fn tower_build_reserve(tower_count: usize, gap_count: usize) -> i64 {
    ((2 - tower_count as i64).max(0)).min(gap_count as i64) * WEAPON_BUILD_COST
}

pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let mut plan = Plan::default();
    let mut claimed: HashSet<Pos> = HashSet::new();

    let tower_gaps = tower_gaps(turn, state);
    let pairs = night::stable_pairs(turn, state);
    update_wall_gate(turn, state, &pairs);
    let wall_gaps = wall_gaps(turn, state);
    // Ring integrity, remembered across days. An EMPTY gap list with walls
    // standing means the shell is closed — the door the economy cut this
    // morning is filtered out of `wall_gaps` while it is still light, so this
    // does not read a deliberate door as a hole. From then on `wall_daily_cap`
    // treats holes as breach repair (see `BotState::ring_ever_complete`).
    if wall_gaps.is_empty() && !turn.walls().is_empty() {
        state.ring_ever_complete = true;
    }
    let wall_cap = wall_daily_cap(turn.day, state.ring_ever_complete);
    // On D1 carry enough stone to finish the complete radius-2 shell. Later
    // days use a bounded maintenance budget. `stone_demand` counts the GAPS
    // still open, not the shortfall against what is already carried: stone in a
    // backpack is committed to those gaps, and treating it as "demand already
    // met" made the carrier sell the ring's own stone out from under itself.
    let wall_demand = (wall_gaps.len() as i64).min(wall_cap);
    // P0-3 门重封的石头保障：白天 `open_door` 切开的门在黄昏前不计入
    // `wall_gaps`（白天它是通道，不是缺口），但黄昏必须重封。门不计入采石
    // 需求时，一个"除门之外完好"的环会让全天 `want_stone=false`——没人备石，
    // 黄昏 step 3 的封门分支要求包里有石头，于是门整夜敞开（v1 §5.5 的机制性
    // 漏洞，与机器人打洞叠加后就是"墙环夜间重新开口"）。把门计入需求、但不
    // 计入白天的 build 目标（wall_gaps 的过滤不动）。D1 无门机制，不动。
    let open_doors = if turn.day > 1 {
        state
            .door_cells
            .iter()
            .filter(|door| !wall_gaps.contains(door))
            .count() as i64
    } else {
        0
    };
    let stone_demand = wall_demand + open_doors;
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
    let shared_wall_duty = turn.day == 1 && !wall_gaps.is_empty();
    let wall_work_done = wall_gaps.is_empty() && !shared_wall_duty;
    let buyer_id: Option<i64> = if budget.intent.is_empty() {
        None
    } else if wall_work_done && workers.len() >= 2 {
        workers.first().map(|unit| unit.id)
    } else {
        workers.last().map(|unit| unit.id)
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
            "wallGaps": wall_gaps.len(),
            "stoneDemand": stone_demand,
            "teamStone": economy::team_ores(turn, STONE),
            "mayBuild": economy::may_build_weapon(turn, state),
            "upgradeReachable": economy::upgrade_reachable(turn, state),
        }),
    );
    // With two workers, the LAST one is the dedicated economy worker: it skips
    // wall duty and focuses on mine → sell → shop, so the wall line never
    // monopolizes both workers. Day 1's open ring is the exception — a builder
    // can only spend stone it is carrying itself, so a "held back" stone in
    // the economy worker's pack is not a reserve, it is a hole in the ring,
    // and the only role that can close it is the one carrying it. That is
    // exactly the shape of issues #12/#13/#14: one worker swept 8 walls and
    // ran dry while the other stood on 17 stone it was neither allowed to
    // build with nor able to sell. While the D1 ring is still open both
    // workers fortify; the exemption returns as soon as only the gate is left,
    // and the mine→sell→shop loop owns the rest of the day.
    let economy_id = if workers.len() >= 2 {
        workers.last().map(|unit| unit.id)
    } else {
        None
    };
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
                "needs": budget.intent.iter().map(|need| need.name.as_str()).collect::<Vec<_>>(),
                "ready": budget.shopping.iter().map(|need| need.name.as_str()).collect::<Vec<_>>(),
            }),
        );
    }

    for worker in &workers {
        worker_day(
            turn,
            state,
            worker,
            &tower_gaps,
            &wall_gaps,
            stone_demand,
            &budget,
            buyer_id,
            economy_id,
            shared_wall_duty,
            &pairs,
            &mut claimed,
            &mut plan,
        );
    }

    if let Some(pioneer) = turn.pioneer() {
        pioneer_day(turn, state, pioneer, &pairs, &mut claimed, &mut plan);
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
        if plan.commands.contains_key(&role.id) {
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

    plan
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
fn fallback_toward_station(turn: &Turn, role: &Unit) -> Option<RoleCommand> {
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
    walk_or_remove_wall(turn, role, &stands, &mut HashSet::new())
}

#[allow(clippy::too_many_arguments)]
fn worker_day(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    tower_gaps: &[(Pos, String)],
    wall_gaps: &[Pos],
    stone_demand: i64,
    budget: &economy::Budget,
    buyer_id: Option<i64>,
    economy_id: Option<i64>,
    shared_wall_duty: bool,
    pairs: &[(i64, i64)],
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    // 1. Self-heal — a dead worker builds nothing, so this outranks all else.
    if let Some(cmd) = use_medicine(role) {
        plan.push(role.id, cmd);
        return;
    }
    // 2. Apply upgrade vouchers we already carry — using them the moment we
    //    hold them beats buying more or anything else (the night's firepower
    //    depends on it).
    if let Some(cmd) = voucher_flow(turn, role, claimed) {
        plan.push(role.id, cmd);
        return;
    }
    // 3. Once everyone has retreated at dusk, a stone carrier closes what is
    //    left of the ring — the intentionally-last gate first, then any other
    //    cell the sweep had to skip because a teammate was standing on it when
    //    it went past (that is the hole that left the ring at 19/20 with the
    //    stone already in a backpack). This is the only wall action allowed to
    //    outrank the hard pre-position lock, and the errand is bounded to the
    //    dusk window so a gun is never abandoned at nightfall.
    if state.wall_gate_sealed && role.count_item(STONE) > 0 {
        let mut gaps = wall_gaps.to_vec();
        gaps.sort_by_key(|site| chebyshev(role.pos, *site));
        if let Some(site) = gaps
            .into_iter()
            .find(|site| !wall_would_trap(turn, pairs, state, *site))
        {
            let gate = wall_gate(turn) == Some(site);
            if chebyshev(role.pos, site) == 1 {
                state.walls_built_today = state.walls_built_today.saturating_add(1);
                state.walled_cells_today.insert(site);
                crate::log::event(
                    if gate {
                        "wall_gate_build"
                    } else {
                        "wall_seal_build"
                    },
                    serde_json::json!({"round": turn.round_no, "role": role.id, "target": site}),
                );
                plan.push(role.id, RoleCommand::build(site, "wall"));
                return;
            }
            if turn.in_day_round < economy::DUSK_ROUND + SEAL_GRACE {
                if let Some(cmd) = build_or_walk(turn, role, site, "wall", claimed) {
                    claimed.insert(site);
                    plan.push(role.id, cmd);
                    return;
                }
            }
        }
    }
    // 3b. 黄昏封门（D2+，P0-3）：`open_door` 为白天经济切开的门，从黄昏起就是
    //    普通缺口。step 3 要等 `wall_gate_sealed` 旗标（散兵未归队时不封），
    //    step 5 受每日预算与批量闸限制——两条路都可能让门整夜敞开。带石工人从
    //    黄昏起直接把门砌上，不必等散兵：他们还有指定的 gate 可以归队。窗口
    //    延到白天最后一回合：封门若因绕路错过 SEAL_GRACE，门会整夜敞开（be 的
    //    复核意见 C）；操炮由夜间无条件召回兜底（night.rs 的红线不动）。
    if turn.day > 1
        && role.count_item(STONE) > 0
        && turn.in_day_round >= economy::DUSK_ROUND
        && turn.in_day_round < crate::model::DAY_ROUNDS
    {
        let mut doors: Vec<Pos> = state.door_cells.iter().copied().collect();
        doors.sort_by_key(|site| chebyshev(role.pos, *site));
        if let Some(site) = doors.into_iter().find(|site| {
            turn.is_land(*site)
                && !claimed.contains(site)
                // The designated gate is also the preferred door: if step 3
                // already sealed it this round, `door_cells` still lists it —
                // never wall a cell that already has our wall in it.
                && !turn.walls().iter().any(|wall| wall.pos == *site)
                && !wall_would_trap(turn, pairs, state, *site)
        }) {
            claimed.insert(site);
            if chebyshev(role.pos, site) == 1 {
                state.walls_built_today = state.walls_built_today.saturating_add(1);
                state.walled_cells_today.insert(site);
                state.door_cells.remove(&site);
                crate::log::event(
                    "door_reseal",
                    serde_json::json!({"round": turn.round_no, "role": role.id, "target": site}),
                );
                plan.push(role.id, RoleCommand::build(site, "wall"));
                return;
            }
            if let Some(cmd) = build_or_walk(turn, role, site, "wall", claimed) {
                plan.push(role.id, cmd);
            }
            return;
        }
    }
    // 4. Pre-position near the assigned tower. This outranks walls, weapons
    //    and the economy once the deadline hits: a tower with no operator by
    //    dusk is a weapon that never fires (battle pk575060 lost 17/20 night
    //    rounds to walking back). Once past the deadline the role locks to the
    //    tower — late-day economy is deliberately sacrificed for a manned gun.
    if let Some(tower_id) = pairs
        .iter()
        .find(|(controller, _)| *controller == role.id)
        .map(|(_, tower)| *tower)
    {
        if let Some(tower) = turn.role_by_id(tower_id) {
            let dist = chebyshev(role.pos, tower.pos);
            // The dedicated economy worker keeps the collect→sell→buy loop
            // running until the last day rounds (so gold never freezes during
            // tasks); everyone else retreats by dusk. The unconditional night
            // recall still guarantees arrival even if this lands late.
            // Dusk is a hard defense checkpoint for every operator, including
            // the economy worker. No collect/sell/buy action may delay a tower
            // assignment past its travel deadline.
            let deadline = preposition_round(dist);
            if turn.in_day_round >= deadline {
                // "Arrived" is a cell the gun can be OPERATED from, not merely
                // one within a chebyshev cell of it. A gun's diagonal
                // neighbours sit on the radius-2 wall ring: a controller that
                // parks on one of them is standing in the wall line, outside
                // the base. `update_wall_gate` then never sees "everyone has
                // retreated", the gate is never sealed, and since the gate seal
                // is the only wall action that outranks this lock the stone the
                // crew is carrying never becomes wall — the ring ends the day at
                // 0/20 with nine stone in two backpacks. It is also issue #15's
                // "操控者在墙的外侧": the gun is manned from in front of the
                // wall it is supposed to be behind. Walking on until a real
                // operating cell is reached (or, when none can be reached,
                // sheltering inside) is what lets the ring close around the
                // crew.
                let stands = tower_stand_cells(turn, tower.pos);
                if !stands.iter().any(|stand| *stand == role.pos) {
                    // Walk there — but never by demolishing the wall line. The
                    // ring is the day's whole product (issues #12/#13/#14: "420
                    // log lines and zero wall builds"), and a controller that
                    // cuts its way to a gun on the way in leaves a base that is
                    // open all night. The night recall in `night::plan` keeps
                    // its demolition escape hatch, where being locked out is
                    // fatal; here the fallback is to shelter inside, which is
                    // one of the three valid night duties (operate / heal /
                    // retreat) and the exact set the gate seal waits for.
                    if let Some(cmd) = walk_toward(turn, role, &stands, claimed) {
                        plan.push(role.id, cmd);
                        return;
                    }
                    if let Some(cmd) = retreat_inside(turn, role, claimed) {
                        plan.push(role.id, cmd);
                        return;
                    }
                }
                // Already at the post (or no walkable step to one): hold
                // position so a late mine/wall errand can't drag the role away
                // from the gun.
                return;
            }
        }
    }
    // 5. Build walls (stone) BEFORE weapons: the wall ring protects the base
    //    and the roles standing behind it. Capped to a minimal daily ring and
    //    skipped by the dedicated economy worker, so the wall line never
    //    monopolizes the whole day. Building also stops the moment any role
    //    could no longer reach its night weapon — the gate stays open until
    //    everyone has retreated inside, so we never wall ourselves out.
    let on_wall_duty = (Some(role.id) != economy_id || shared_wall_duty)
        && (state.walled_cells_today.len() as i64)
            < wall_daily_cap(turn.day, state.ring_ever_complete);
    // (shared_wall_duty is passed in from `plan`: day 1 keeps both workers on
    // the ring until it closes — see the comment there.)
    if role.count_item(STONE) > 0
        && !wall_gaps.is_empty()
        && on_wall_duty
        && roles_can_reach(turn, pairs)
    {
        let batch = stone_batch(turn, wall_gaps.len());
        let carrying = role.count_item(STONE) as i64;
        // One trip per batch. Dropping a wall every time we happen to walk past
        // a gap with a single stone in hand is what turned the day into a
        // mine↔ring shuttle: the build is only worth taking once the load is
        // complete, there is no more stone left to fetch, or nightfall is close
        // enough that another trip would not pay for itself.
        //
        // "Complete" is measured against the TEAM's stone, not this one pack:
        // two workers splitting a 20-cell ring hold 10 each and neither pack
        // ever reaches the batch, so a per-pack test waits for a load that no
        // single worker is supposed to carry and the ring is never started.
        // Once the team between them holds enough to fill every gap, the next
        // mine trip only pushes the build past dusk.
        //
        // And the walk home has the last word: once dusk plus the seal grace
        // is only a walk away, the load in hand is the load the ring gets.
        // Issue #17's crew was still digging at the far vein when the day ran
        // out — the stone arrived nowhere, and the ring kept zero walls.
        let team_stone = economy::team_ores(turn, STONE);
        let load_complete = carrying >= batch
            || team_stone >= wall_gaps.len() as i64
            || stone_demand <= 0
            || turn.in_day_round >= economy::DUSK_ROUND - 12
            || wall_trip_overdue(turn, role, wall_gaps);
        // Build immediately when already standing next to a safe gap — but only
        // once this trip's load is settled. The batch exists to avoid the
        // mine↔ring commute, so it decides whether it is worth WALKING OUT to
        // the wall line, never whether a stone already in hand gets used: when
        // there is no more stone to fetch (batch complete, no reachable ore
        // left, or dusk too close for another trip) waiting for a load means
        // never placing the stone at all, and the role walks off to a tower
        // with a full pack instead
        // (tests/combat.rs::worker_builds_wall_before_weapon).
        let more_stone_available =
            economy::choose_mine(turn, state, role.pos, stone_demand, &mut HashSet::new())
                .is_some();
        let adjacent_site = if load_complete || !more_stone_available {
            wall_gaps
                .iter()
                .find(|site| {
                    !claimed.contains(site)
                        && chebyshev(role.pos, **site) == 1
                        && !wall_would_trap(turn, pairs, state, **site)
                })
                .copied()
        } else {
            None
        };
        if let Some(site) = adjacent_site {
            claimed.insert(site);
            state.walls_built_today = state.walls_built_today.saturating_add(1);
            state.walled_cells_today.insert(site);
            crate::log::event(
                "wall_build",
                serde_json::json!({"role": role.id, "target": site, "stone": role.count_item(STONE)}),
            );
            plan.push(role.id, RoleCommand::build(site, "wall"));
            return;
        }
        // Otherwise commit to the wall line once we carry a batch of stone.
        if load_complete {
            for site in wall_gaps {
                if claimed.contains(site) || wall_would_trap(turn, pairs, state, *site) {
                    continue;
                }
                if let Some(cmd) = build_or_walk(turn, role, *site, "wall", claimed) {
                    claimed.insert(*site);
                    if cmd.action == "build" {
                        state.walls_built_today = state.walls_built_today.saturating_add(1);
                        state.walled_cells_today.insert(*site);
                        crate::log::event(
                            "wall_build",
                            serde_json::json!({"role": role.id, "target": *site, "stone": role.count_item(STONE)}),
                        );
                    }
                    plan.push(role.id, cmd);
                    return;
                }
            }
        }
    }
    // 6. Build weapons (gold) once the wall line is underway — but keep a
    //    gold reserve so the main weapon's level-2 upgrade is never starved
    //    (see economy::may_build_weapon).
    //
    //    P0-4 墙环在建时不折返建塔：第 1 天墙线还有缺口（= `shared_wall_duty`）
    //    且首塔已经立起来时，跳过第 2/3 塔，等环成型再建。首塔 gatling 豁免
    //    ——环需要石头，石头需要采矿，而采矿需要一个能守住矿点的开局。
    //
    //    没有这道闸，P0-2 的 `may_build_weapon` 会在 R3（墙环一砖未砌时）就
    //    把第 3 座塔放上去，当天的走路预算随即在"塔位 ↔ 石矿 ↔ 墙线"之间翻
    //    倍，`day1_sim` 的墙优先用例整组转红（实测：3 塔 0 墙）。有闸之后第
    //    2/3 塔等环成型——分析 §4 要的正是这条顺序。
    //
    //    闸门看的是"塔已经存在"这个回合快照事实，不是"这个回合谁打算建塔"。
    //    把队友本回合的建塔意向也算进来会更严格，实测会把"石头不可达"那种
    //    整天砌不出墙的地图变成整天只有一门炮（`day1_sim` 的
    //    stone_out_of_reach 板：远矿行军 + 收入冻结，issue #17 原样复发），
    //    因此按分析 §4 的原文只认已存在的塔；该图第 2/3 塔顺延到 D2，是分析
    //    已经接受的权衡。
    let ring_still_forming = turn.day == 1 && shared_wall_duty && !turn.towers().is_empty();
    if economy::may_build_weapon(turn, state) && !ring_still_forming {
        for (site, kind) in tower_gaps {
            if claimed.contains(site) {
                continue;
            }
            if let Some(cmd) = build_or_walk(turn, role, *site, kind, claimed) {
                claimed.insert(*site);
                plan.push(role.id, cmd);
                return;
            }
        }
    }
    // 7. Shopping (dedicated buyer) — upgrades come after survival. When
    //    nothing is affordable YET the buyer still sets off once the ore in its
    //    pack covers the price, so the purchase lands the round the sale does.
    //
    //    The sale comes first. This branch outranks step 9, so a buyer sent to
    //    the shop with an unsold pack never got to sell it: the shop refused
    //    the purchase (it is paid in gold, not ore), the pack rode back to the
    //    mine, and the round trip was spent twice over. A role with something
    //    to sell sells it and shops next round — the gold in hand is what
    //    `budget.shopping` is computed from, so this is also the only order in
    //    which the purchase can happen at all.
    if buyer_id == Some(role.id) && !economy::should_sell(turn, state, role, stone_demand) {
        if !budget.shopping.is_empty() {
            if let Some(cmd) = buyer_flow(turn, role, &budget.shopping, claimed) {
                plan.push(role.id, cmd);
                return;
            }
        } else if economy::buyer_must_preposition(turn, role, &budget.intent) {
            if let Some(cmd) = walk_to_shop(turn, role, claimed) {
                plan.push(role.id, cmd);
                return;
            }
        }
    }
    // 7b. Personal Medicine: only its carrier can drink it, so this is a
    //     per-role errand, after the team list has had its turn. For a
    //     critically wounded role it is the recovery path, and the walk is
    //     part of it.
    if let Some(cmd) = self_provision(turn, role, claimed, true) {
        plan.push(role.id, cmd);
        return;
    }
    // 7c. Cut a door in our own wall line. Everything the economy needs — ore,
    //     the vendor, the shop — is OUTSIDE the ring, and a ring with no door is
    //     a base nobody can leave: the day after the wall line closes, every
    //     role stands inside with an empty pack and issues no command at all
    //     (issue #14's frozen economy, in its final form). Runs before selling
    //     and mining because with the door shut there is no point trying either.
    if let Some(cmd) = open_door(turn, state, role, claimed) {
        plan.push(role.id, cmd);
        return;
    }
    // 8. Patch the wall line. Before selling and before mining: a 10-gold kit
    //    buys back 1000 HP of wall, and a wall left at 200 HP is the cell the
    //    next wave comes through. Issue #16 lost the ring that way — D2 rebuilt
    //    walls (16→19) but never restored one HP, the first night's damaged
    //    cells were the ones that failed on the second, and the base finished
    //    the match at 35 HP with two roles dead. The repair existed but only
    //    fired for a role already standing beside a damaged wall, and the
    //    mining step returns long before that: with stone still to dig, the
    //    worker never walks to the wall it should be mending.
    //
    //    One role is excused: the dedicated economy worker with a load to sell.
    //    It is the one keeping the collect→sell→buy loop running — the loop
    //    that pays for these kits in the first place — and it is also the role
    //    that walks to the shop and ends up holding them, so exempting it is
    //    not an option. Instead the sale goes first and the repair happens a
    //    round later, from the same errand: the wall is still mended today, and
    //    no round of the loop is spent on it.
    let repair_duty =
        Some(role.id) != economy_id || !economy::should_sell(turn, state, role, stone_demand);
    if repair_duty {
        if let Some(cmd) = repair_flow(turn, state, role, wall_gaps, claimed) {
            plan.push(role.id, cmd);
            return;
        }
    }
    // 9. Sell accumulated ore in one batch before collecting more. This keeps
    //    the collect→sell→buy loop moving instead of filling a 100-slot pack
    //    one item at a time while usable gold remains trapped in the backpack.
    if economy::should_sell(turn, state, role, stone_demand) {
        if let Some(cmd) = sell_flow(turn, state, role, stone_demand, claimed) {
            plan.push(role.id, cmd);
            return;
        }
    }
    // 10. Mine the nearest ore (stone first while walls are wanted). Mining
    //    pauses during dusk so the ore we hold is converted to gold instead.
    if turn.in_day_round < economy::DUSK_ROUND && !role.backpack_full() {
        // The dedicated economy worker keeps the collect→sell→buy loop funded
        // once the ring's own build-out is over: outside day 1 it digs ore the
        // vendor buys, never the stone the wall line is holding back. See
        // `economy::choose_sellable_mine`.
        let keep_gold_loop = Some(role.id) == economy_id && !shared_wall_duty;
        if let Some(cmd) = mine_flow(
            turn,
            state,
            role,
            stone_demand,
            keep_gold_loop,
            pairs,
            wall_gaps,
            claimed,
        ) {
            plan.push(role.id, cmd);
            return;
        }
    }
    // 11. Last-resort repair: the adjacency-only selector, reached only when
    //    step 8 declined — no kit in the pack, or no legal cell to stand on
    //    while walking to the wall it picked. Step 8 owns the repair policy
    //    now; this stays as the cheap no-move fallback beside a wall.
    if let Some(wall_pos) = crate::brain::combat::repair_target(turn, role, 0) {
        plan.push(role.id, RoleCommand::use_item_at("WallFixer", wall_pos));
        return;
    }
    // 12. Burn a carried robot-summon order only when everything else is done.
    if let Some(cmd) = burn_summon_order(state, role) {
        plan.push(role.id, cmd);
    }
}

fn pioneer_day(
    turn: &Turn,
    state: &mut BotState,
    pioneer: &Unit,
    pairs: &[(i64, i64)],
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
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
    let recalled = turn.in_day_round >= pioneer_recall_round(turn, pioneer);
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
                "dayRound": turn.in_day_round,
                "reason": "dusk_recall",
            }),
        );
        state.finish_task(false, "dusk_recall");
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
                // the ring and keeps the gate seal from ever completing.
                let stands = tower_stand_cells(turn, tower.pos);
                if !stands.iter().any(|stand| *stand == pioneer.pos) {
                    if let Some(cmd) = walk_toward(turn, pioneer, &stands, claimed) {
                        plan.push(pioneer.id, cmd);
                        return;
                    }
                    // Same fallback as the workers: a pioneer whose tower is
                    // walled off retreats inside rather than standing idle.
                    if let Some(cmd) = retreat_inside(turn, pioneer, claimed) {
                        plan.push(pioneer.id, cmd);
                        return;
                    }
                }
                // Already at the post (or no walkable step to one): hold there.
                return;
            }
        }
    }
    // 4. Accept a fresh task when a point is ready (walk there first). Not
    //    once the dusk recall has fired: a task accepted now is a task that
    //    keeps the pioneer at the point through the seal.
    if !recalled {
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
    if let Some(cmd) = retreat_inside(turn, pioneer, claimed) {
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
            .min_by_key(|task| chebyshev(pioneer.pos, task.pos))?;
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
            crate::log::event(
                "task_accept",
                serde_json::json!({"round": turn.round_no, "session": session_id, "point": candidate.pos, "taskType": candidate.task_type}),
            );
            self.task = TaskSession {
                active: true,
                session_id,
                accepted_round: turn.round_no,
                timeout_round: turn.round_no + timeout,
                point: Some(candidate.pos),
                task_type: candidate.task_type.clone(),
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
    let walk = crate::brain::interior_cells(turn)
        .iter()
        .map(|cell| chebyshev(pioneer.pos, *cell))
        .min()
        .unwrap_or(0) as i64;
    (economy::DUSK_ROUND - 1 - walk - PIONEER_RETREAT_SLACK).max(0)
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
fn max_hp(role: &Unit) -> Option<i64> {
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
fn self_provision(
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

fn at_shop(turn: &Turn, pos: Pos) -> bool {
    turn.weapon_shops()
        .iter()
        .flat_map(|shop| stand_cells(turn, *shop))
        .any(|stand| stand == pos)
}

/// Walk to the nearest weapon shop; None when no shop stand is known.
fn walk_to_shop(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
    let mut stands: Vec<Pos> = Vec::new();
    for shop in turn.weapon_shops() {
        stands.extend(stand_cells(turn, shop));
    }
    if stands.is_empty() {
        return None;
    }
    walk_toward(turn, role, &stands, claimed)
}

/// Buy the first needed item: walk to the weapon shop, then buy. Buying is
/// the buyer's sole job — a carried summon order must never preempt a voucher,
/// upgrade or repair purchase.
fn buyer_flow(
    turn: &Turn,
    role: &Unit,
    shopping: &[economy::Need],
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
    walk_toward(turn, role, &stands, claimed)
}

/// Use a carried robot-summon order against the enemy (harassment), one per
/// round, capped by the daily summon budget. Kept out of `buyer_flow` so a
/// voucher purchase is never delayed by it.
fn burn_summon_order(state: &mut BotState, role: &Unit) -> Option<RoleCommand> {
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
fn voucher_flow(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
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

fn build_or_walk(
    turn: &Turn,
    role: &Unit,
    target: Pos,
    name: &str,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if chebyshev(role.pos, target) == 1 && turn.is_land(target) {
        return Some(RoleCommand::build(target, name));
    }
    let stands = stand_cells(turn, target);
    walk_toward(turn, role, &stands, claimed)
}

/// Walk a role back inside the ring, onto a cell at footprint distance <= 1
/// from the station. Returns `None` when it is already inside or when nothing
/// inside is reachable at all, so a genuinely boxed-in role holds rather than
/// paces. "Reachable" now includes demolishing one of our own walls on the way
/// in (`walk_or_remove_wall`, P0-3): the ring was built to keep robots out, and
/// a role the crew accidentally sealed on the wrong side of it is a hole in the
/// gate seal for the whole night, which costs more than one wall cell.
fn retreat_inside(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
    let station = turn.station()?;
    let footprint = station_footprint(station.pos);
    if footprint_distance(role.pos, &footprint) <= 1 {
        return None;
    }
    let stands = crate::brain::interior_cells(turn);
    if stands.is_empty() {
        return None;
    }
    walk_or_remove_wall(turn, role, &stands, claimed)
}

/// Is there a walkable route from `start` to any of `stands`? A role already
/// standing on a stand cell counts as reachable (no move needed).
fn can_reach(turn: &Turn, start: Pos, stands: &[Pos]) -> bool {
    if stands.iter().any(|stand| *stand == start) {
        return true;
    }
    let blocked = turn.blocked_for(-1);
    crate::path::step_toward_stands(turn, start, stands, &blocked).is_some()
}

/// Stand cells of the tower a role is paired with for the coming night.
fn night_goal(turn: &Turn, pairs: &[(i64, i64)], role_id: i64) -> Option<Vec<Pos>> {
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
fn roles_can_reach(turn: &Turn, pairs: &[(i64, i64)]) -> bool {
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
fn wall_would_trap(turn: &Turn, pairs: &[(i64, i64)], state: &BotState, site: Pos) -> bool {
    let mut blocked = turn.blocked_for(-1);
    blocked.insert(site);
    for role in turn.controllable() {
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
        // ring cell at once — the gate included — which is how day 1 ended at
        // 19/20 with the last stone sitting in a backpack (issues #12/#13/#14).
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
fn pending_ring(turn: &Turn, state: &BotState, ignore: Pos) -> Vec<Pos> {
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let Some(gate) = wall_gate(turn) else {
        return Vec::new();
    };
    let walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    ring_cells(&footprint, 2)
        .into_iter()
        .filter(|pos| *pos != gate || state.wall_gate_sealed)
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

/// Walkable cells outside the wall ring — everything at footprint distance 3
/// or more: the mines, the vendors, the shops, the enemy half. "Can this role
/// get out?" is exactly "can it reach one of these?".
fn outside_cells(turn: &Turn, footprint: &[Pos]) -> Vec<Pos> {
    let mut cells: Vec<Pos> = Vec::new();
    for x in 0..turn.width {
        for y in 0..turn.height {
            let pos = Pos { x, y };
            if turn.is_land(pos) && footprint_distance(pos, footprint) >= 3 {
                cells.push(pos);
            }
        }
    }
    cells
}

/// Cut a door in our own wall line.
///
/// The ring is a closed box, and everything the economy runs on is outside it.
/// While the wall crew is still building, the ring has gaps and the question
/// never comes up; the moment the last cell is filled, every role inside is
/// walled away from the ore with an empty pack and the whole day produces no
/// command at all. That is issue #14 — "R18 gold 105, then 216 rounds
/// untouched" — in its final form, and it repeats every day after the ring
/// closes.
///
/// So a role that is inside, has an errand outside and can reach neither cuts
/// its own way out. The opening is recorded in `door_cells` and `wall_gaps`
/// stops offering it for the rest of the day, or the wall crew would re-seal it
/// under the digger's feet every round (demolish, rebuild, demolish) and the
/// day would be spent oscillating over one cell. At dusk `door_cells` stops
/// being honoured, the cell becomes an ordinary gap again and the seal crew
/// fills it: the door is a daytime fixture, the ring must be closed at night.
fn open_door(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    // Past dusk the wall line is closing, not opening.
    if turn.in_day_round >= economy::DUSK_ROUND || turn.walls().is_empty() {
        return None;
    }
    // Day 1 is the fortification day. The ring goes up from the outside in and
    // the crew walks in and out of the gaps it leaves; a door cut into that
    // line is a hole the day does not get back (measured: five cells lost, the
    // ring finished at R32 and burrowed through at R48-R57). From day 2 the
    // ring stands and the economy has to live behind it.
    if turn.day <= 1 {
        return None;
    }
    let station = turn.station()?;
    let footprint = station.footprint();
    // Already outside, or standing on the wall line itself: nothing to cut.
    if footprint_distance(role.pos, &footprint) > 1 {
        return None;
    }
    // A way out already exists — the ring is incomplete, the door is open, or
    // this role never was trapped. Never demolish a wall we do not have to.
    let blocked = turn.blocked_for(role.id);
    let outside = outside_cells(turn, &footprint);
    if crate::path::step_toward_stands(turn, role.pos, &outside, &blocked).is_some() {
        return None;
    }
    // One door per day, and one is enough. A teammate's cut from THIS round is
    // invisible in the turn's occupancy (the wall only comes down when the
    // judger applies the command), so without this check every trapped role
    // cuts its own hole in the same round — two stones spent, two cells to
    // re-seal at dusk (measured in the day-2 simulation: two `door_open`
    // events on the same round).
    if !state.door_cells.is_empty() {
        return None;
    }
    // Cut the nearest cell of our own ring — but NOT the designated gate. The
    // gate is the cell the D1 crew seals from OUTSIDE while the ring goes up;
    // once the towers stand on the inner band they can cover the gate's entire
    // inside approach, so a door cut there can never be re-sealed from within
    // (the day-2 simulation: towers on the band row, gate unreachable, open
    // all night). Any other ring cell is inside-reachable by construction —
    // the role cutting it is standing on a stand of it right now — which is
    // exactly what the dusk reseal needs.
    let gate = wall_gate(turn);
    let target = turn
        .walls()
        .into_iter()
        .map(|wall| wall.pos)
        .filter(|pos| chebyshev(role.pos, *pos) == 1)
        .filter(|pos| footprint_distance(*pos, &footprint) == 2)
        .min_by_key(|pos| (gate == Some(*pos), pos.x, pos.y))?;
    if !claimed.insert(target) {
        return None; // a teammate is already cutting this round
    }
    state.door_cells.insert(target);
    crate::log::event(
        "door_open",
        serde_json::json!({
            "round": turn.round_no,
            "role": role.id,
            "target": target,
            "gate": gate == Some(target),
        }),
    );
    Some(RoleCommand::remove(target))
}

/// Walk to the nearest mine and collect. Stone is preferred while the wall
/// line still needs the load this role is carrying; distance always beats ore
/// value.
#[allow(clippy::too_many_arguments)]
fn mine_flow(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    stone_demand: i64,
    keep_gold_loop: bool,
    pairs: &[(i64, i64)],
    wall_gaps: &[Pos],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    // Fetch stone only while the TEAM is short of the gaps still open.
    // `stone_demand` is the gap count, not this role's shortfall: two workers
    // splitting a 20-cell ring hold 10 each, so a per-pack test keeps both of
    // them digging stone long after the ring has all the stone it can use,
    // and the sellable ore that funds the rest of the day never gets mined.
    //
    // The dedicated economy worker never joins that queue outside day 1: it is
    // the role that has to keep carrying ore the vendor will buy, and stone is
    // the one ore the vendor is refused. See
    // `economy::choose_sellable_mine` for the freeze that cost issue #18 the
    // whole match.
    let want_stone =
        !keep_gold_loop && stone_demand > 0 && economy::team_ores(turn, STONE) < stone_demand;
    let pick = if keep_gold_loop {
        economy::choose_sellable_mine(turn, state, role.pos, claimed)?
    } else {
        economy::choose_mine(
            turn,
            state,
            role.pos,
            if want_stone { stone_demand } else { 0 },
            claimed,
        )?
    };
    // Stone is what the ring wants, but only a trip that can carry a load home
    // before this role must be at its gun is a wall trip. When no vein is that
    // close, the demand is not "stone" any more — it is an unwinnable walk, and
    // the day is worth more spent on ore that sells (issue #17's frozen purse).
    let stone_errand = want_stone && pick.1 == STONE;
    let (mine, ore) = if stone_errand && !stone_trip_fits(turn, role, pairs, pick.0, wall_gaps) {
        economy::choose_mine(turn, state, role.pos, 0, claimed)?
    } else {
        pick
    };
    // On a stone errand the trip is the point: an ore picked up on the way
    // there is a detour that fills the pack with the wrong load, and the stone
    // is never reached. Everywhere else the pass-by mine is free money — and
    // that includes the day when the ring's stone turned out to be out of
    // reach, because the errand is ore that sells by then, not stone.
    let strict = stone_errand && ore == STONE;
    // Collect an adjacent mine of the preferred ore directly. A mine a
    // teammate has merely RESERVED as a walking target is still collectable by
    // a role already standing next to it: the reservation decides who walks
    // there, not who may dig. Without the second look the role stands on the
    // ore with an empty pack, refuses to collect because someone else claimed
    // the cell, cannot walk to it either (it is already on a stand, so the
    // path search returns "no step"), and produces no command at all for the
    // rest of the day — issue #13's idle role, caused by a claim.
    //
    // While the ring is still short of stone that courtesy stops at stone.
    // `nearest_adjacent_mine` also accepts a vein of ANY ore, which is right
    // for the general miner — a windfall is a windfall — but on the stone
    // errand it is how the wall line loses its day: with the stone 22 cells out
    // (issue #17) every carrier that brushed the iron on the way stopped there
    // and mined four ore instead, twice over, and the day ended 8 collects and
    // 0 walls with gold frozen at the 75 it started with. We came for stone;
    // the ore under our feet can wait for the walk home.
    let adjacent = if strict {
        adjacent_mine_of(turn, role, STONE)
    } else {
        nearest_adjacent_mine(turn, role, &ore, claimed)
            .or_else(|| nearest_adjacent_mine(turn, role, &ore, &HashSet::new()))
    };
    if let Some(mine_pos) = adjacent {
        claimed.insert(mine_pos);
        return Some(RoleCommand::collect(mine_pos));
    }
    // Which vein the day is being spent on, and why. The wall line is the one
    // errand whose failure is invisible until the night (issues #12-#17 all
    // opened with "0 墙"), so the pick, the ring's demand and whether the trip
    // can still be paid for in walls are worth a line in the log.
    crate::log::event(
        "mine_pick",
        serde_json::json!({
            "round": turn.round_no,
            "role": role.id,
            "wantStone": want_stone,
            "stoneErrand": stone_errand,
            "ore": ore,
            "mine": mine,
        }),
    );
    let stands = stand_cells(turn, mine);
    let cmd = walk_toward(turn, role, &stands, claimed)?;
    claimed.insert(mine);
    Some(cmd)
}

/// Walk to a vendor and sell the most valuable ore stack.
fn sell_flow(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    stone_demand: i64,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    let mut stands: Vec<Pos> = Vec::new();
    for vendor in turn.vendors() {
        stands.extend(stand_cells(turn, vendor));
    }
    if stands.is_empty() {
        return None;
    }
    if stands.iter().any(|pos| *pos == role.pos) {
        let cmd = economy::sell_command(turn, state, role, stone_demand)?;
        // WORKFLOW_REQUEST §7.1 asks whether the economy is income-starved or
        // spend-blocked, and the only way to tell is the sell side against
        // `round.gold`: a run of rounds at a constant gold with no `sell` in
        // them is a mining problem, while sells that land with the gold pinned
        // afterwards is a spending one. Until now nothing was logged here at
        // all, so the recipe matched zero lines.
        crate::log::event("sell", crate::log::ledger_record(turn, role, &cmd));
        return Some(cmd);
    }
    walk_toward(turn, role, &stands, claimed)
}

/// The wall this role should mend first, if any.
///
/// The rank puts the walls holding an opening first — a gap the ring has not
/// filled yet, or the daytime door. That is where the line is already broken
/// and where the next robot comes through; a wall at 300 HP beside a gap is a
/// breach waiting for the night, while the same wall on a closed stretch can
/// wait a round. Then the weakest wall, then the nearest.
pub fn repair_target(turn: &Turn, state: &BotState, role: &Unit, wall_gaps: &[Pos]) -> Option<Pos> {
    let beside_opening = |pos: Pos| {
        state
            .door_cells
            .iter()
            .any(|door| chebyshev(*door, pos) <= 1)
            || wall_gaps.iter().any(|gap| chebyshev(*gap, pos) <= 1)
    };
    turn.walls()
        .into_iter()
        .filter(|wall| wall.health < crate::brain::combat::wall_max_hp(wall.level))
        .min_by_key(|wall| {
            (
                !beside_opening(wall.pos),
                wall.health,
                chebyshev(role.pos, wall.pos),
                wall.pos.x,
                wall.pos.y,
            )
        })
        .map(|wall| wall.pos)
}

/// Walk to the wall that most needs mending and patch it.
///
/// Issue #16: the ring was rebuilt but never repaired. `WallFixer` costs 10
/// gold and restores its target to FULL HP (任务书: "目标坐标所在围墙回满血"),
/// which is by far the cheapest HP on the board — cheaper than a wall upgrade
/// voucher (20 gold for +500) and cheaper than a whole new wall. The kit was in
/// the shopping list, but the repair itself only ever fired for a role that
/// happened to be standing next to a damaged wall at the end of the day, and
/// mining returns several steps earlier, so a worker with stone to dig never
/// got there. Repair is therefore an ERRAND: pick the worst wall, walk to it,
/// mend it.
fn repair_flow(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    wall_gaps: &[Pos],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if role.count_item("WallFixer") == 0 {
        return None;
    }
    let target = repair_target(turn, state, role, wall_gaps)?;
    if chebyshev(role.pos, target) == 1 {
        return Some(RoleCommand::use_item_at("WallFixer", target));
    }
    // Walk to the wall's inner side: the ring's outside is where the robots
    // are, and a worker mending from out there is a worker the night can pick
    // off. `stand_cells` may offer cell that is itself a wall (the neighbouring
    // ring cells); `walk_toward` drops those, since walls block movement.
    let stands = stand_cells(turn, target);
    walk_toward(turn, role, &stands, claimed)
}

/// A mine of the preferred ore (or any valuable ore) we already stand next to.
/// A mine of exactly `ore` this role is already standing next to.
///
/// The narrow sibling of `nearest_adjacent_mine`, for the errands where the
/// wrong ore is not a windfall but a lost day.
fn adjacent_mine_of(turn: &Turn, role: &Unit, ore: &str) -> Option<Pos> {
    turn.all_mines()
        .into_iter()
        .filter(|(pos, kind)| kind == ore && chebyshev(role.pos, *pos) == 1)
        .map(|(pos, _kind)| pos)
        .min_by_key(|pos| (pos.x, pos.y))
}

fn nearest_adjacent_mine(
    turn: &Turn,
    role: &Unit,
    preferred_ore: &str,
    claimed: &HashSet<Pos>,
) -> Option<Pos> {
    let mut best: Option<(i64, Pos)> = None;
    for (pos, ore) in turn.all_mines() {
        if claimed.contains(&pos) || chebyshev(role.pos, pos) != 1 {
            continue;
        }
        let value = if ore == preferred_ore { 0 } else { 1 };
        if best.map(|(v, _)| value < v).unwrap_or(true) {
            best = Some((value, pos));
        }
    }
    best.map(|(_, pos)| pos)
}

// ---------------------------------------------------------------------------
// Build site planning
// ---------------------------------------------------------------------------

/// Desired tower cells (ring at distance 1 from the station footprint),
/// paired with the weapon kind that should stand there. Cells already
/// holding one of our towers, blacklisted cells and non-land are excluded.
pub fn tower_gaps(turn: &Turn, state: &BotState) -> Vec<(Pos, String)> {
    if turn.towers().len() >= 3 {
        return Vec::new();
    }
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let occupied: HashSet<Pos> = turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    let center = Pos {
        x: turn.width / 2,
        y: turn.height / 2,
    };

    let mut cells: Vec<Pos> = ring_cells(&footprint, 1)
        .into_iter()
        .filter(|pos| turn.is_land(*pos) && !occupied.contains(pos))
        .collect();
    cells.sort_by_key(|pos| (chebyshev(*pos, center), pos.x, pos.y));

    let mut have = [0i32; 3]; // gatling, railgun, rocket
    for tower in turn.towers() {
        match tower.kind {
            crate::model::UnitKind::Gatling => have[0] += 1,
            crate::model::UnitKind::Railgun => have[1] += 1,
            crate::model::UnitKind::Rocket => have[2] += 1,
            _ => {}
        }
    }
    // Build order: gatling controls the near lane, then railgun exploits lined-up
    // waves without being stopped by the front robot. Rocket remains the third
    // slot: it is still completed early, but only after the two dependable
    // no-cooldown weapons can defend the base.
    let build_order: [(usize, &str); 3] = [(0, "gatling"), (1, "railgun"), (2, "rocket")];
    // Cells that a standing weapon must be able to be OPERATED from. Three guns
    // packed onto adjacent ring cells steal each other's only standing room —
    // the middle one then has no adjacent free cell at all, and a weapon nobody
    // can man is a weapon that never fires (issues #12/#13/#14: "炮塔全程无人
    // 操控"). Reserve every existing tower's operating cells up front and refuse
    // a new site that would take one, or that would have fewer than two of its
    // own left once the neighbours are built out.
    let mut reserved: HashSet<Pos> = HashSet::new();
    for tower in turn.towers() {
        reserved.extend(tower_stand_cells(turn, tower.pos));
    }
    // Cell occupancy splits in two for the corridor question. A unit standing
    // somewhere is transient — it will move — but the station and a built
    // tower are there for good, and only those can strand a corridor cell.
    // Reading a teammate's position as permanent is what let the rocket land
    // on the one cell that cut the base interior in half.
    let mut permanent: HashSet<Pos> = footprint.iter().copied().collect();
    for tower in turn.towers() {
        permanent.insert(tower.pos);
    }
    let mut gaps: Vec<(Pos, String)> = Vec::new();
    let mut used: HashSet<Pos> = HashSet::new();
    for (have_idx, kind) in build_order {
        if have[have_idx] > 0 {
            continue;
        }
        let mut taken = occupied.clone();
        taken.extend(used.iter().copied());
        taken.extend(reserved.iter().copied());
        let mut fixed = permanent.clone();
        fixed.extend(used.iter().copied());
        // Operability is a PREFERENCE, never a veto. Two guns on a corner base
        // eat most of the five ring cells it has, and a strict "two standing
        // cells or nothing" test then rejects the third site outright — the
        // base ends the day with two guns while the third slot waits for a
        // cell that will never free up. A gun with a single standing cell still
        // fires; a gun that was never built never does, and `gatling -> railgun
        // -> rocket` is a hard requirement. So the strict search runs first and
        // the fallback is exactly the pre-existing test: any free, non-
        // blacklisted ring cell. The fallback can therefore never site FEWER
        // guns than the plain search did.
        let pick = |require_operable: bool| -> Option<Pos> {
            cells.iter().copied().find(|pos| {
                !used.contains(pos)
                    && !state.blacklisted_builds.contains(&(*pos, kind.to_string()))
                    && (!require_operable
                        || (!reserved.contains(pos)
                            && operating_cells(turn, *pos, &taken).len() >= 2
                            && !strands_corridor(turn, &footprint, &fixed, *pos)))
            })
        };
        let site = pick(true).or_else(|| pick(false));
        if let Some(pos) = site {
            used.insert(pos);
            reserved.extend(operating_cells(turn, pos, &taken));
            gaps.push((pos, kind.to_string()));
        }
    }
    gaps
}

/// Would putting a tower on `site` strand a ROLE in the ring-1 corridor?
///
/// The band between the station and the wall is the only walkable corridor
/// inside the base, and a weapon sits on it. Two guns two cells apart leave the
/// cell between them with no ring-1 neighbour at all, and a role standing there
/// can reach nothing for the rest of the day: no gun, no gate, no way out. That
/// is issue #13's "0 角色站桩闲置", and it also stops the ring from ever
/// closing, because `wall_would_trap` sees the stranded role as the reason not
/// to seal.
///
/// The test is about occupants, not geometry: an empty pocket costs nothing
/// (nobody is in it, and anyone who walks in can walk back out the way they
/// came), while vetoing every geometric pocket rejects the third gun's only
/// site from some base positions and leaves the base with two towers.
fn strands_corridor(turn: &Turn, footprint: &[Pos], taken: &HashSet<Pos>, site: Pos) -> bool {
    let band: Vec<Pos> = ring_cells(footprint, 1);
    let free: Vec<Pos> = band
        .iter()
        .copied()
        .filter(|pos| *pos != site && !taken.contains(pos))
        .collect();
    turn.controllable().iter().any(|role| {
        band.contains(&role.pos)
            && !free
                .iter()
                .any(|other| *other != role.pos && chebyshev(*other, role.pos) == 1)
    })
}

/// Cells a weapon standing at `site` could be operated from, treating `taken`
/// as occupied. Mirrors `tower_stand_cells` for a tower that does not exist
/// yet, so a build site can be accepted or rejected before any gold is spent.
fn operating_cells(turn: &Turn, site: Pos, taken: &HashSet<Pos>) -> Vec<Pos> {
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let all: Vec<Pos> = crate::model::neighbours(site)
        .into_iter()
        .filter(|pos| turn.is_land(*pos) && !taken.contains(pos))
        .collect();
    let inner: Vec<Pos> = all
        .iter()
        .copied()
        .filter(|pos| footprint_distance(*pos, &footprint) <= 1)
        .collect();
    if inner.len() >= 2 {
        inner
    } else {
        all
    }
}

fn wall_gate(turn: &Turn) -> Option<Pos> {
    let station = turn.station()?;
    let footprint = station.footprint();
    let xmax = footprint.iter().map(|pos| pos.x).max()?;
    let ymin = footprint.iter().map(|pos| pos.y).min()?;
    Some(Pos {
        x: xmax + 2,
        y: ymin - 1,
    })
}

fn update_wall_gate(turn: &Turn, state: &mut BotState, pairs: &[(i64, i64)]) {
    // The seal checkpoint is dusk on EVERY day, not just the first. The ring is
    // re-opened each morning by `open_door` (the ore is outside it), so without
    // a daily seal the wall line the crew spends the day rebuilding is a wall
    // line with a hole in it.
    if state.wall_gate_sealed || turn.in_day_round < economy::DUSK_ROUND {
        return;
    }
    let Some(station) = turn.station() else {
        return;
    };
    let footprint = station.footprint();
    match gate_open_record(turn, pairs, &footprint) {
        None => {
            state.wall_gate_sealed = true;
            crate::log::event(
                "wall_gate_seal",
                serde_json::json!({"round": turn.round_no, "dayRound": turn.in_day_round}),
            );
        }
        Some(record) => crate::log::event("wall_gate_open", record),
    }
}

/// The `wall_gate_open` record — who the dusk seal is still waiting on, or
/// `None` when nobody is and the gate may close.
///
/// The decision and the record are the same computation on purpose. The record
/// used to name the class ("controllers_not_retreated") and never the culprit,
/// so the fifteen open rounds of the dusk window — which is the whole window,
/// i.e. a gate that never sealed at all — could not be attributed from the log
/// to anyone. That question cost issue #15 a match and a half, and its answer
/// was "the pioneer is standing at a task point", which is only visible if the
/// record carries positions.
///
/// `away` is `[id, x, y]` for every controllable role that is neither home nor
/// on the operating cells of the tower it will man tonight; `stuck` names the
/// subset that cannot walk to its post at all. The two are different failures:
/// a role on its way in is a gate that seals a round or two later, while a role
/// walled off from its gun is a gate that never seals, and it is the case the
/// wall crew is supposed to make impossible (`wall_would_trap`). Both lists are
/// empty when there is nothing to say, and `log::event` prunes the empty one
/// out, so a round with a single role walking home costs one short line.
///
/// `footprint` is passed in rather than looked up because a missing station is
/// not "everyone is home": the caller has already decided what to do about a
/// base that is gone, and this function must not answer it by accident.
pub fn gate_open_record(
    turn: &Turn,
    pairs: &[(i64, i64)],
    footprint: &[Pos],
) -> Option<serde_json::Value> {
    let mut away: Vec<serde_json::Value> = Vec::new();
    let mut stuck: Vec<i64> = Vec::new();
    for role in turn.controllable() {
        if footprint_distance(role.pos, footprint) <= 1 {
            continue; // home: this one is not what the seal is waiting on
        }
        let stands = night_goal(turn, pairs, role.id);
        if stands
            .as_ref()
            .map_or(false, |stands| stands.contains(&role.pos))
        {
            continue; // already on the operating cells of the tower it mans
        }
        away.push(serde_json::json!([role.id, role.pos.x, role.pos.y]));
        if stands.map_or(false, |stands| !crate::brain::can_reach_any(turn, role, &stands)) {
            stuck.push(role.id);
        }
    }
    if away.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "round": turn.round_no,
        "away": away,
        "stuck": stuck,
    }))
}

/// Desired D1 wall cells: one radius-2 shell around the station. One gate cell
/// remains omitted while any controller is outside; after the dusk retreat
/// checkpoint `update_wall_gate` explicitly admits that final seal cell.
pub fn wall_gaps(turn: &Turn, state: &BotState) -> Vec<Pos> {
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let Some(gate) = wall_gate(turn) else {
        return Vec::new();
    };

    let mut cells = ring_cells(&footprint, 2);
    cells.sort_by_key(|pos| (std::cmp::Reverse(chebyshev(*pos, gate)), pos.x, pos.y));

    let existing_walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    let occupied: HashSet<Pos> = turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    cells
        .into_iter()
        .filter(|pos| state.wall_gate_sealed || *pos != gate)
        // A cell the economy cut open this morning is a door, not a hole: the
        // gap list must not send the wall crew to re-seal it under the digger's
        // feet all day. From dusk the door stops being honoured and the cell is
        // an ordinary gap again, so the ring is closed before the robots come.
        .filter(|pos| turn.in_day_round >= economy::DUSK_ROUND || !state.door_cells.contains(pos))
        .filter(|pos| {
            turn.is_land(*pos)
                && !existing_walls.contains(pos)
                && !occupied.contains(pos)
                && !state
                    .blacklisted_builds
                    .contains(&(*pos, "wall".to_string()))
        })
        .collect()
}

fn ring_cells(footprint: &[Pos], radius: i32) -> Vec<Pos> {
    let xs: Vec<i32> = footprint.iter().map(|pos| pos.x).collect();
    let ys: Vec<i32> = footprint.iter().map(|pos| pos.y).collect();
    let (xmin, xmax) = (
        *xs.iter().min().unwrap_or(&0),
        *xs.iter().max().unwrap_or(&0),
    );
    let (ymin, ymax) = (
        *ys.iter().min().unwrap_or(&0),
        *ys.iter().max().unwrap_or(&0),
    );
    let mut cells = Vec::new();
    for x in xmin - radius..=xmax + radius {
        for y in ymin - radius..=ymax + radius {
            let pos = Pos { x, y };
            if footprint.contains(&pos) {
                continue;
            }
            if footprint_distance(pos, footprint) == radius {
                cells.push(pos);
            }
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
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
            wall_would_trap(&turn, &[], &state, pos(21, 20)),
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
            !wall_would_trap(&turn, &[], &state, pos(20, 20)),
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
