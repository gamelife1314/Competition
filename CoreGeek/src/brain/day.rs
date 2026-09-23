//! Daytime planning (70 rounds): workers run the economy loop
//! (mine → sell → build towers/walls → upgrades), the pioneer runs tasks,
//! the treasure hunt and shopping.

use std::collections::HashSet;

use crate::brain::{
    economy, news, night, route, stand_cells, task, tower_stand_cells, treasure,
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
use super::action::build::{
    build_or_walk, gate_clear_stands, open_door, repair_flow, update_wall_gate, wall_gate,
};
use super::action::geometry::{ring_cells, ring_open_cells, wall_layer};
use super::action::mine::{mine_flow, stone_covered};
use super::action::shop::{
    burn_summon_order, buyer_flow, self_provision, shop_round_trip, shop_trip_decision,
    shop_trip_worth_taking, voucher_flow, walk_to_shop, walk_to_vendor,
};
pub(crate) use super::action::shop::use_medicine;
use super::action::sell::sell_flow;
pub use super::action::build::{gate_open_record, repair_target};
pub use super::action::geometry::{
    far_edge_cutoff, far_shoulder, primary_wall_gaps, ring_gap_split, tower_build_reserve,
    wall_daily_cap, wall_gaps, D1_WALL_CAP, LATER_WALL_CAP,
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

/// Stones a wall trip must be able to carry home before it is worth starting.
/// A trip that lands one stone is not a wall line, it is a lost day: issue #17
/// spent the whole of D1 walking to a vein on the far side of the map and put
/// up zero walls.
pub(crate) const MIN_WALL_LOAD: i64 = 4;
/// Rounds kept in hand on top of the walk home before a stone carrier gives up
/// on the vein — a blocked cell, a detour, a claim.
const WALL_TRIP_SLACK: i64 = 3;

/// The day-round by which this role must be standing beside its gun. A role
/// with no gun pair is due at dusk like everyone else.
pub(crate) fn gun_deadline(turn: &Turn, role: &Unit, pairs: &[(i64, i64)]) -> i64 {
    pairs
        .iter()
        .find(|(controller, _)| *controller == role.id)
        .and_then(|(_, tower)| turn.role_by_id(*tower))
        .map(|tower| preposition_round(chebyshev(role.pos, tower.pos)))
        .unwrap_or(economy::DUSK_ROUND)
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
/// Day-rounds after dusk during which a stone carrier may still walk out to
/// close the last hole in the ring. Long enough for a round trip from any
/// tower post, short enough that the gun is manned again well before night.
const SEAL_GRACE: i64 = 8;
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
/// Day-rounds before dusk from which a role with no gun to man stops taking
/// errands outside the ring. One ordinary walk home (a worker is four to eight
/// cells out at the vein or the wall line) plus slack — the mirror of
/// `SEAL_GRACE`, which is the same budget on the far side of dusk.
const DUSK_RETREAT_LEAD: i64 = 8;
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

/// How many ring stones the dedicated economy worker carries and lays on day 1
/// before its shift changes to earning.
///
/// The ring is 20 cells plus the gate, and the day has to buy it AND earn with
/// it: 「挖矿必须进行，必须赚钱」. The wall worker carries the rest — its pack
/// holds 100 (`backPackCapability`, 任务书 3.2) against the 21 stones the day
/// owes, so nothing about the ring's stone needs a second carrier. What the
/// second carrier buys is TIME, and only on the cells it lays itself: measured
/// on the day-1 board, the two workers' 21 placements take the whole afternoon
/// (R17 to R56) because both walk the same ring route and only the one standing
/// beside the gap may build. A worker that carries six and lays six is out of
/// that queue by mid-morning with the rest of the day ahead of it; a worker that
/// carries eleven is in it until dusk, which is exactly the day the owner
/// filed. Six is the share the day's remaining rounds can pay for: leaving the
/// ring work at ~R30 puts the nearest sellable vein (eight to ten rounds out) in
/// reach before the dusk recall, and hands the wall worker the 15 stones it can
/// still lay before the gate seals.
const ECONOMY_D1_STONE_SHARE: i64 = 6;

/// Rounds one carrier needs per ring cell on the day-1 sweep.
///
/// The build order walks the ring, so a cell costs a step to the site and then
/// the placement: measured on the day-1 board, the sweep lays one cell every
/// other round from R17 to R56. Used by `worker_day`'s `ring_fits_without_me`,
/// which is what stops the economy worker standing down while the cells it
/// would leave behind no longer fit in the afternoon.
const ROUNDS_PER_RING_CELL: i64 = 2;

pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let mut plan = Plan::default();
    let mut claimed: HashSet<Pos> = HashSet::new();

    // THE ENTRANCE IS CHOSEN BEFORE ANYTHING ELSE IS PLANNED.
    //
    // Every walk this day is priced through it: which mine is worth working,
    // which ring cell is built next, whether a role inside can reach the ore at
    // all. It is latched once per day (`observe` clears it at rollover) so the
    // hole cannot move under a crew that is building around it.
    if state.gate_cell.is_none() {
        // The gap list is not readable yet — it is defined in terms of the
        // entrance being chosen here — so the daybreak pick is made with "stone
        // if any cell of the shell still carries no wall", which is true on
        // every day the wall line has work to do, and false on the day the ring
        // stands complete, when the only errand left is ore that sells.
        let demand = if route::open_ring_cells(turn, 2).is_empty() {
            0
        } else {
            1
        };
        state.gate_cell = route::entrance(turn, state, demand);
        crate::log::event(
            "gate_planned",
            serde_json::json!({
                "round": turn.round_no,
                "day": turn.day,
                "gate": state.gate_cell.or_else(|| route::legacy_entrance(turn)),
                "planned": state.gate_cell.is_some(),
            }),
        );
    }

    let tower_gaps = tower_gaps(turn, state);
    let pairs = night::stable_pairs(turn, state);
    update_wall_gate(turn, state, &pairs);
    let wall_gaps = wall_gaps(turn, state);
    // Ring integrity, remembered across days. An EMPTY gap list with walls
    // standing means the shell is closed — the door the economy cut this
    // morning is filtered out of `wall_gaps` while it is still light, so this
    // does not read a deliberate door as a hole. From then on `wall_daily_cap`
    // treats holes as breach repair (see `BotState::ring_ever_complete`).
    //
    // Measured on the PRIMARY ring alone. The second layer (P2-1) is a later,
    // partial addition which sits in `wall_gaps` too; letting it answer this
    // question would mean the ring's own breach-repair budget never latched,
    // and a ring the night tore open would be repaired on the six-cell
    // maintenance budget that cannot re-close it (issue #21).
    let (ring_owed, ring_shoulder) = ring_gap_split(turn, state);
    let primary_open = ring_owed.len();
    if primary_open == 0 && !turn.walls().is_empty() {
        state.ring_ever_complete = true;
    }
    let wall_cap = wall_daily_cap(turn.day, state.ring_ever_complete, primary_open);
    // On D1 carry enough stone to finish the complete radius-2 shell. Later
    // days use a bounded maintenance budget. `stone_demand` counts the GAPS
    // still open, not the shortfall against what is already carried: stone in a
    // backpack is committed to those gaps, and treating it as "demand already
    // met" made the carrier sell the ring's own stone out from under itself.
    // ...and the far shoulder is counted only when it IS the day's work
    // (issue #206 §6). 「有条件全部建造好」 does not mean "dig for a cell you were
    // told you may skip": `wall_gaps` returns the arc while any of it is open
    // and the shoulder only once none of it is, so this line is the arc's own
    // count for as long as a robot could walk in through it, and the shoulder's
    // count only on a day the arc is already complete — which is precisely the
    // day that can afford it.
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
    // THE GATE'S OWN STONE. `wall_demand` counts the gaps the sweep still owes
    // the ring, and the gate is deliberately not one of them — it is the hole
    // the day works through. But at dusk the gate becomes a wall like any
    // other, and a wall needs a stone: the old accounting stopped the
    // collecting one stone short, so the seal round found every pack empty and
    // the day's own entrance stayed open all night — the exact hole
    // issues #121-#125 measured (`wall_gate_open` for the whole dusk window).
    // The gate's stone is therefore counted exactly the way the door's is
    // above: in the demand all afternoon, never in the day's build list. The
    // `sell_command`/`should_sell` reserve reads the same demand, so the stone
    // survives the dusk cash-out; once the gate is actually walled the demand
    // drops the extra stone and the purse is free of it.
    let gate_stone = match wall_gate(turn, state) {
        Some(gate)
            if turn.is_land(gate)
                && !state.door_cells.contains(&gate)
                && !turn.walls().iter().any(|wall| wall.pos == gate) =>
        {
            1
        }
        _ => 0,
    };
    let stone_demand = wall_demand + open_doors + gate_stone;
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
    // release is `stone_covered` — the very bound `mine_flow` uses to decide
    // who digs (see it there) — so "the ring has its stone" cannot mean two
    // different things in two places. The old trigger was `wall_gaps.len() > 4`,
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
    // THE RING HAS THE LAST WORD (issue #207 §5(iii)). The release above is a
    // loan against a ring that is on schedule, and the schedule is arithmetic:
    // one carrier lays a ring cell every other round (a step to the site, then
    // the placement — measured over the day-1 sweep), so `worker_day`'s
    // `ring_fits_without_me` takes the earner back the moment the cells it
    // would leave behind no longer fit in the afternoon. This flag is the other
    // half of the bound, and the cruder one: the day is out of wall work it can
    // be sent to while the ring is still open.
    //
    // The debt is counted on the ring itself, not on `wall_gaps`: the sweep's
    // list drops the gate, the economy's door, a cell a teammate is standing on
    // and a cell whose only approach is blocked, and those dropped cells are
    // exactly the ones that end a day at 16/20 (the day-1 board closed its last
    // four from the dusk seal alone, R56 → R65, with the whole crew idle inside
    // the ring and 3 iron in a backpack). What the night finds open is the debt
    // that matters, so the debt is what this reads.
    let ring_debt = ring_open_cells(turn, state) as i64;
    let ring_at_risk = turn.day == 1 && ring_debt > 0 && wall_gaps.is_empty();
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
    // THE FAR SHOULDER, ONCE A ROUND (issue #206 §6).
    //
    // 「围墙不一定得全部建造起来，后边的门，背向机器人的方向可以开着」 is a rule
    // about what the day OWES, and the whole point of it is that a day which
    // stops at the enemy-facing arc is not a day that failed. That verdict is
    // invisible in a wall count: `wallGaps: 0` above reads the same whether the
    // ring is whole or the shoulder was written off, and a batch that cannot
    // tell those apart cannot tell "the owner's rule is working" from "the
    // crew stopped early".
    //
    // Written every day round for the same reason `tower_plan` is: "how many
    // rounds was the shoulder left open, and on how many of them was it because
    // the arc was still going up" is a ratio over rounds, and a record that only
    // fires on change cannot answer it. `reason` is the verdict, `open` is the
    // set the day is leaving for last, and `owed` is what it is doing instead.
    let far_reason = if turn.in_day_round >= far_edge_cutoff() {
        // Past the cutoff the shoulder is required, so it is in `ring_owed` and
        // not in `open` — the ring has to hold the night.
        "dusk_required"
    } else if !ring_owed.is_empty() {
        // The arc first: while a cell a robot can walk in through is open, the
        // shoulder is not even in the day's build list.
        "enemy_first"
    } else if !ring_shoulder.is_empty() {
        // The arc is finished and this is the day's remaining wall work — the
        // 「有条件全部建造好」 half, on the day that can afford it.
        "spare_rounds"
    } else {
        "none"
    };
    crate::log::event(
        "wall_far_edge",
        serde_json::json!({
            "round": turn.round_no,
            "day": turn.day,
            "owed": ring_owed.len(),
            "open": ring_shoulder.iter().map(|cell| crate::log::xy(*cell)).collect::<Vec<_>>(),
            "reason": far_reason,
            "teamStone": economy::team_ores(turn, STONE),
            "stoneDemand": stone_demand,
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
                // WHY THE BUYER IS NOT AT THE COUNTER (issues #156-#160). The
                // purchase is the whole errand and the two numbers that decide
                // it — the round trip and the round the buyer has to be home by
                // — were nowhere in the log, so "75 rounds affordable, no
                // purchase" (表 2b/2a of pk590730) could only be read as a
                // mystery. `shopTrip` is the decision `worker_day` will take
                // this round: `walk` (the errand is worth taking), `no_time`
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
    }

    for worker in &workers {
        worker_day(
            turn,
            state,
            worker,
            &tower_gaps,
            &wall_gaps,
            stone_demand,
            wall_cap,
            &budget,
            buyer_id,
            economy_id,
            shared_wall_duty,
            ring_at_risk,
            &pairs,
            &mut claimed,
            &mut plan,
        );
    }

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
    //
    // The ladder rather than `walk_or_remove_wall` alone, because a role the
    // backstop reaches has already been declined by every step above it, and
    // the demolition hatch still refuses when no SINGLE cut reopens the route
    // (issues #176-#185: 20011 on (24,16) for twelve straight dusk rounds).
    walk_home_or_reroute(turn, role, &stands, &mut HashSet::new())
}

#[allow(clippy::too_many_arguments)]
fn worker_day(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    tower_gaps: &[(Pos, String)],
    wall_gaps: &[Pos],
    stone_demand: i64,
    // Today's fortification budget, computed once in `plan` — the same number
    // the day's `wall_demand` is measured against, so the day's stone and the
    // day's wall work can never disagree about how much ring today is worth.
    wall_cap: i64,
    budget: &economy::Budget,
    buyer_id: Option<i64>,
    economy_id: Option<i64>,
    shared_wall_duty: bool,
    ring_at_risk: bool,
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
        // The intentionally-last gate FIRST: it is the hole the whole day was
        // planned around and the one cell the night must not find open. The
        // old distance-only sort sealed a nearer skipped cell instead and left
        // the entrance itself for a stone that was never reserved (the gate's
        // stone is in the demand now — see `plan`).
        let gate = wall_gate(turn, state);
        let mut gaps = wall_gaps.to_vec();
        gaps.sort_by_key(|site| (Some(*site) != gate, chebyshev(role.pos, *site)));
        if let Some(site) = gaps.into_iter().find(|site| {
            !wall_would_trap(turn, pairs, state, *site)
                || turn.in_day_round >= HARD_SEAL_ROUND
        }) {
            let gate = gate == Some(site);
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
            // The seal cell may be walked to until the day ends, not only until
            // `SEAL_GRACE`. The grace bounds the ordinary wall sweep, which has
            // the whole afternoon; this step only ever runs once the flag is up,
            // and across issues #121-#125 that was late or never. Three of the
            // five matches had a stone carrier on the wrong side of a sealed
            // gate when the day ran out.
            if turn.in_day_round < crate::model::DAY_ROUNDS {
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
        // THE DESIGNATED GATE IS A DOOR TOO (issues #121-#125).
        //
        // This step only ever saw `door_cells`, and on day 2+ that set is
        // usually EMPTY: `open_door` returns early when the role can already
        // reach the outside, which it can — through the gate the previous
        // night's seal built and the morning `open_door`/rebuild left open. So
        // the one hole the ring actually has was the one cell this step never
        // offered, and it stayed open all night in every match of the batch.
        // Step 3 covers it only once `wall_gate_sealed` is set, which is
        // exactly the flag a straggler keeps down.
        let mut doors: Vec<Pos> = state.door_cells.iter().copied().collect();
        if let Some(gate) = wall_gate(turn, state) {
            if !doors.contains(&gate) {
                doors.push(gate);
            }
        }
        // Total order, not distance alone: `door_cells` is a HashSet, so two
        // doors at the same distance would be tried in hash-iteration order and
        // the day's plan would differ between two runs on the same board. The
        // coordinate tiebreak is the one the rest of the planner uses.
        doors.sort_by_key(|site| (chebyshev(role.pos, *site), site.x, site.y));
        // Doors are economy cuts, not structural gaps. A door that would trap
        // a controller outside is left open: the controller walks through it
        // instead of demolishing a wall cell that the last round can never
        // re-wall (measured in day1_sim: R198 sealed the door at (12,26),
        // the trapped controller demolished (11,26) at R198 and R200, and the
        // R200 removal was never re-walled). Step 3's forced seal still
        // closes structural gaps; doors are the one opening that is never
        // forced.
        if let Some(site) = doors.into_iter().find(|site| {
            turn.is_land(*site)
                && !claimed.contains(site)
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
            // assignment past its travel deadline — with ONE measured
            // exception: a role already AT the vendor's counter sells before
            // it comes home. The deadline subtracts the walk and three rounds
            // of detours but not the sale itself, and a seller yanked one hop
            // from the counter walks home with a full pack UNSOLD — the whole
            // errand wasted and the day frozen (the day-2 board: iron rode
            // home twice). One round at the counter moves the arrival from
            // day-round ≤51 to ≤52, still before dusk; the lock fires the
            // round after the pack is sold, and the night recall bounds the
            // rest. The checkpoint stands — it is the lock's own arithmetic
            // that now includes the counter round.
            let deadline = preposition_round(dist);
            let at_counter = turn
                .vendors()
                .iter()
                .any(|vendor| chebyshev(role.pos, *vendor) == 1)
                && economy::should_sell(turn, state, role, stone_demand);
            // THE SECOND COUNTER EXCEPTION, and it is the same one (issues
            // #156-#160): a buyer already on a shop errand that still finishes
            // before dusk is worth more at the counter than at the post, and
            // the lock-in is what has been turning it around four cells short
            // of it. `shop_errand_fits` is the same predicate `buyer_flow`
            // starts the walk with, so the two cannot disagree: a trip that was
            // allowed to start is a trip this lock waits for, and one that was
            // not allowed to start never gets this far.
            //
            // The arrival it allows is bounded by dusk, not by
            // `preposition_round`'s three rounds of detour slack — so the post
            // is still manned before nightfall, and the unconditional night
            // recall (night.rs) owns the rest. What it buys is the purchase
            // itself: pk590730's 130 gold never reached the counter, and a
            // 1500-HP base at level 2 is what the night was missing.
            let on_shop_errand =
                Some(role.id) == buyer_id && shop_trip_worth_taking(turn, role, pairs, &budget.shopping);
            // Logged only on the rounds the lock actually gives way, which is
            // the whole finding: the three or four rounds a match where the
            // buyer is past its deadline with the counter still reachable. A
            // record on every shopping round would be forty lines a day of the
            // same facts.
            if on_shop_errand && turn.in_day_round >= deadline {
                crate::log::event(
                    "shop_trip",
                    serde_json::json!({
                        "round": turn.round_no,
                        "role": role.id,
                        "decision": "protected",
                        "inDayRound": turn.in_day_round,
                        "trip": shop_round_trip(turn, role, pairs),
                        "lockRound": deadline,
                    }),
                );
            }
            // THE RING'S TAIL OUTRANKS THE POST, AND IT IS BOUNDED BY THE HARD
            // DEADLINE (issue #207 §5). The lock above is measured against the
            // PRE-POSITION round, which is dusk minus the walk minus the detour
            // slack — on the day-1 board that is round 46, nine rounds before
            // dusk. A stone carrier locked there stops being a carrier: the two
            // cells the sweep could still reach sat bare until the gate seal
            // released them at dusk, and the ring closed at R65 — 「第一天只挖
            // 石头」's own ring, with the dusk at 55. The exception is the same
            // one step 3 already takes for the gate, and it is bounded the same
            // way: a role carrying stone for gaps that are still open keeps the
            // wall sweep available to it until `HARD_SEAL_ROUND`, and the
            // moment the gaps are filled (or the hard deadline passes) this
            // falls through and the lock has the last word. The gun is not
            // abandoned — `night::plan`'s unconditional recall is the backstop,
            // and the sweep's errand is inside the ring.
            let ring_tail = role.count_item(STONE) > 0
                && !wall_gaps.is_empty()
                && turn.in_day_round < economy::DUSK_ROUND;
            if turn.in_day_round >= deadline && !at_counter && !on_shop_errand && !ring_tail {
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
                let stands = gate_clear_stands(turn, state, stands);
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
                    if let Some(cmd) = retreat_inside(turn, state, role, claimed) {
                        plan.push(role.id, cmd);
                        return;
                    }
                }
                // Already at the post (or sheltering inside): the day's walking
                // is done, but the building need not be. On a slow board the
                // ring's tail lands exactly here — the carrier arrives beside
                // the last open cells with stone in its pack, and the bare
                // lock used to make it hold that stone for six idle rounds
                // while the shell waited for the seal step (the distant-vein
                // board: the bottom row sat open R48–R55 and the tail landed
                // two rounds late). An adjacent placement costs no walk and
                // drags nobody off a gun; the reserve floor still applies, so
                // this can never spend the gate's own stone.
                if role.count_item(STONE) > 0
                    && economy::team_ores(turn, STONE) >= stone_demand
                {
                    if let Some(site) = wall_gaps
                        .iter()
                        .find(|site| {
                            !claimed.contains(site)
                                && chebyshev(role.pos, **site) == 1
                                && !wall_would_trap(turn, pairs, state, **site)
                        })
                        .copied()
                    {
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
                                "atPost": true,
                            }),
                        );
                        plan.push(role.id, RoleCommand::build(site, "wall"));
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
    // 5. Build weapons (gold) FIRST — firepower is the priority. A tower costs
    //    25 gold and takes one round to place. Offense-first: every tower is a
    //    gun that kills NPCs for score, which is how the base survives.
    //    Day 1 must build at least 2 towers so both workers have a weapon to
    //    operate at night — a worker with no tower is dead weight during the
    //    assault. The wall line (step 6 below) is secondary: 2 towers with no
    //    walls beats 1 tower with a complete ring.
    if economy::may_build_weapon(turn, state) {
        // Dusk guard: after dusk, skip weapon building entirely. A weapon
        // that isn't placed by dusk can wait until tomorrow — a worker
        // stranded outside the ring at nightfall loses the gate seal and
        // the night walks in (dusk_gate tests: role 10002 stuck at
        // (13,23) building railgun every round R66-70 instead of going
        // home).
        let past_dusk = turn.in_day_round >= economy::DUSK_ROUND;
        if !past_dusk {
            for (site, kind) in tower_gaps {
                if claimed.contains(site) {
                    continue;
                }
                // Day 1: build all 3 towers immediately — firepower is the
                // day's first priority. Every worker needs a weapon to
                // operate at night; a worker with no tower is dead weight.
                if let Some(cmd) = build_or_walk(turn, role, *site, kind, claimed) {
                    claimed.insert(*site);
                    plan.push(role.id, cmd);
                    return;
                }
            }
        }
    }
    // 6. Build walls (stone) AFTER weapons: the wall ring is a nice-to-have
    //    that protects the base, but firepower kills enemies for score.
    //    Capped to a minimal daily ring and skipped by the dedicated economy
    //    worker, so the wall line never monopolizes the whole day. Building
    //    also stops the moment any role could no longer reach its night
    //    weapon — the gate stays open until everyone has retreated inside,
    //    so we never wall ourselves out.
    // DAY 1 IS THE SAME SPLIT, ONE SHIFT EARLIER. Day 1 used to keep the
    // economy worker on the stone queue until the ring was all but closed
    // (`wall_gaps.len() > 4`), and on the day-1 board the two workers spent
    // every daylight round of the day digging and laying 21 stone between
    // them: 21 stone dug, 20 walls up at R56 with dusk at 55, and not one iron
    // or copper in either pack — the purse never moved off 25 and the whole day
    // bought nothing (「第一天只挖石头」). The wall does not need the second
    // carrier for its stone, only for its cells: the first worker carries what
    // the day owes once this one has laid its share, and what this one carries
    // it still lays itself — [`ECONOMY_D1_STONE_SHARE`] stones, placed by the
    // `economy_unload_stone` branch below before the shift changes. From then
    // on it is the earner: `keep_gold_loop` takes it off the stone queue
    // altogether, so it fetches no more stone and digs only ore the vendor buys
    // (`choose_sellable_mine`).
    let economy_share_laid = Some(role.id) == economy_id
        && turn.day == 1
        && role.count_item(STONE) as i64 >= ECONOMY_D1_STONE_SHARE;
    // THE SHIFT CHANGE IS BOUNDED BY THE RING'S OWN REMAINING WORK. The last
    // carrier to leave the wall line is the one that decides whether the day
    // ends with a closed ring, and the rule above releases it on "my share is
    // carried" alone — which says nothing about whether the cells still bare
    // fit in the afternoon that is left. Measured on the distant-vein board
    // (the ring's stone eight cells further out): the earner stood down at R45
    // with six cells and ten rounds left, the solo carrier laid one cell every
    // other round as the build order walks the ring, and the ring closed at 65
    // against a bound of 63 — the gate had sealed at 61, so the last two cells
    // had to wait for the seal step. One carrier lays `ROUNDS_PER_RING_CELL`
    // rounds a cell (a step to the site, then the placement), so the day can
    // spare this role exactly when the gaps it would leave behind still fit.
    let ring_fits_without_me = (wall_gaps.len() as i64) * ROUNDS_PER_RING_CELL
        <= (economy::DUSK_ROUND - turn.in_day_round).max(0);
    // The shift change in one predicate, asked in three places below (the wall
    // step, the carried stone, the gold loop): the economy worker EARNS while
    // the ring is on schedule, and is a carrier whenever it is not.
    let economy_earns = Some(role.id) == economy_id
        && (!shared_wall_duty || economy_share_laid)
        && !ring_at_risk
        && ring_fits_without_me;
    let on_wall_duty = (Some(role.id) != economy_id || shared_wall_duty || ring_at_risk)
        && (state.walled_cells_today.len() as i64) < wall_cap;
    // Economy worker carrying stone when released from wall duty on Day 1:
    // let it place the stone it's carrying before switching to mining. A
    // worker with a pack full of stone can't mine ore, and standing idle
    // with stone is the exact freeze issue #12 describes. Day 2+ the economy
    // worker sells/ shops instead — wall repair is the first worker's job.
    let economy_unload_stone = Some(role.id) == economy_id
        && (!shared_wall_duty || economy_share_laid || ring_at_risk)
        && turn.day == 1
        && role.count_item(STONE) > 0
        && !wall_gaps.is_empty();
    // (shared_wall_duty is passed in from `plan`: day 1 keeps both workers on
    // the ring until it closes — see the comment there.)
    if role.count_item(STONE) > 0
        && !wall_gaps.is_empty()
        && (on_wall_duty || economy_unload_stone)
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
        // THE SEAL RESERVE IS A FLOOR ON THE SWEEP. `stone_demand` counts every
        // cell the dusk still owes — the gaps, the doors the economy cut, and
        // the gate itself — so a build may only spend stone while the team
        // still covers all of it. Without the floor the last placement of the
        // sweep zeroes the pool, and the gate — the one cell guaranteed to need
        // a stone at dusk — finds every pack empty (issues #121-#125: the gate
        // open for the whole dusk window in every match of the batch).
        //
        // It is a floor on the WAIT, not on the last stone in hand: a carrier
        // with nowhere left to dig is not waiting for anything, and the
        // disjunction below says so (see the `adjacent_site` guard).
        let reserve_met = team_stone >= stone_demand;
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
            economy::choose_mine(turn, state, role, stone_demand, &mut HashSet::new())
                .is_some();
        // A carrier with nowhere left to dig is not waiting for anything: the
        // stone it holds is the only stone there is, and holding it back means
        // never placing it at all (`combat.rs::worker_builds_wall_before_weapon`
        // — one worker, six stone, no vein on the board). The reserve governs
        // the WAIT for a batch, never the last stone in hand.
        let adjacent_site = if !more_stone_available || (load_complete && reserve_met) {
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
                serde_json::json!({
                    "role": role.id,
                    "target": site,
                    "layer": wall_layer(turn, site),
                    "stone": role.count_item(STONE),
                }),
            );
            plan.push(role.id, RoleCommand::build(site, "wall"));
            return;
        }
        // Otherwise commit to the wall line once we carry a batch of stone.
        if load_complete && reserve_met {
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
                            serde_json::json!({
                                "role": role.id,
                                "target": *site,
                                "layer": wall_layer(turn, *site),
                                "stone": role.count_item(STONE),
                            }),
                        );
                    }
                    plan.push(role.id, cmd);
                    return;
                }
            }
        }
    }
    // 6b. Has this role's day outside the ring ended? (P1.) The steps above have
    //     had their turn — a role carrying stone to a legal gap built it, and a
    //     role that could pay for a gun built that — and everything from here
    //     down is an errand OUTSIDE the ring: the shop trip, the vendor, the
    //     mine. A role outside the ring at dusk is the hole the gate cannot
    //     close; see `dusk_recall_round` for the measured shape (15/15 open
    //     rounds in #111 day 1, #112 both days, #115 day 1).
    //
    //     Two steps are deliberately left above the lock-in, because each is
    //     worth a round and each ends beside the base: the wall repair (step 8,
    //     the ring is at the base) and the sale (step 9, the ore in the pack
    //     becomes the gold the next day's towers are bought with). Only the
    //     WALKS are cut — a carried Medicine is still drunk at step 1.
    //
    //     Gunners are excluded: step 4 (`preposition_round`) already locks them
    //     and repeating it here would only shadow a deadline that works.
    let committed = !pairs.iter().any(|(controller, _)| *controller == role.id)
        && dusk_committed(state, turn, role, dusk_recall_round(turn, role));
    // 6d. THE MERGED OUTING (issue #206 §2, 「一趟买齐」). One trip out of the
    //     ring carries the whole errand list — the vein, the vendor, the shop —
    //     because the pack is what makes it possible and the walk is what makes
    //     it worth doing: a worker's pack is 100 slots and the pioneer's 40
    //     (任务书 3.2, read off `backPackCapability`, never assumed here), and
    //     every separate departure pays the walk again.
    //
    //     `economy::outing` lays the trip out and prices it in rounds against
    //     the daylight left; what the day takes from it is its ORDER. When the
    //     trip both sells and buys, the sale is walked first — step 7 outranks
    //     step 9, so a buyer holding ore and a shopping list set off for the
    //     shop with the load still in its pack, bought nothing (the counter is
    //     paid in gold, not ore), and walked the pack back out to the mine.
    //     That is the parking `buyer_must_preposition` was taught to avoid, one
    //     level up: the goal is payable, so the earner goes — but it goes to the
    //     VENDOR first and the shop is the next stop on the same trip.
    //
    //     When the day cannot fit the whole trip before the dusk recall, the
    //     route gives up its last stop instead — the shopping — and this step
    //     then routes nothing at all, leaving step 7 exactly as it was. A short
    //     afternoon costs a voucher, never the sale.
    //
    //     Only the trip's FIRST claim is enforced here. The digging, the
    //     counter and the shop are the day's own flows (7, 9, 10), each
    //     re-picking its venue from where the role actually stands.
    let mut sell_first = false;
    if !committed
        && !turn.vendors().is_empty()
        && (buyer_id == Some(role.id) || economy_id == Some(role.id))
    {
        if let Some(outing) = economy::outing(turn, state, role, &budget.shopping, stone_demand) {
            if outing.sells() && outing.buys() {
                crate::log::event(
                    "merged_outing",
                    serde_json::json!({
                        "round": turn.round_no,
                        "role": role.id,
                        "stops": outing.stops.len(),
                        "rounds": outing.rounds,
                        "daylight": outing.daylight,
                    }),
                );
                sell_first = true;
                if economy::vendor_travel(turn, role.pos) > 0 {
                    match walk_to_vendor(turn, role, claimed) {
                        Some(cmd) => {
                            plan.push(role.id, cmd);
                            return;
                        }
                        // No legal step toward the vendor after all — the route
                        // gives way rather than parking the role on the spot.
                        None => sell_first = false,
                    }
                }
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
    //
    //    A role whose dusk commitment has fired does not shop. The shop is the
    //    farthest errand on the board (its stands are the ones `buyerShopDist`
    //    measures in the teens), and the round trip is what parked the loose
    //    role outside the ring for the whole dusk window in the measurement
    //    behind `dusk_recall_round`. It sells what it carries and goes in.
    if committed {
        // Dusk cash-out (issues #201-#205): the buyer gets one last weapon
        // purchase through the dusk seal window. Gold left unspent at dusk is
        // gold that buys nothing all night — the reserve is already 0, so any
        // affordable weapon upgrade voucher in the shopping list is spent now
        // rather than hoarded. The trip must still fit inside the seal grace
        // (DUSK_ROUND + SEAL_GRACE) so the buyer is back before nightfall.
        //
        // Multi-worker dusk buy: during the cash-out window any worker — not
        // just the dedicated buyer — that is already standing at a shop stand
        // may purchase. The dedicated buyer still gets priority for the walk;
        // this only fires for a worker that happens to be at the counter
        // (e.g. the buyer itself returned, or another worker passed by).
        // Gold sitting in the purse at nightfall is the failure mode this
        // prevents.
        let is_buyer = buyer_id == Some(role.id);
        let at_shop = turn.weapon_shops().iter().any(|shop| {
            chebyshev(role.pos, *shop) <= 1
        });
        if !sell_first && !budget.shopping.is_empty() && (is_buyer || at_shop) {
            // The buyer may still walk to the shop; other workers only buy
            // if already standing at the counter (no new walks for non-buyers
            // — those workers need to get behind the ring for the seal).
            let can_walk = is_buyer
                && shop_round_trip(turn, role, pairs)
                    .map(|trip| {
                        turn.in_day_round + trip <= economy::DUSK_ROUND + SEAL_GRACE
                    })
                    .unwrap_or(false);
            if can_walk || at_shop {
                if let Some(cmd) = buyer_flow(turn, role, &budget.shopping, pairs, claimed) {
                    plan.push(role.id, cmd);
                    return;
                }
            }
        }
        // fall through: steps 8 and 9 still run, everything below them is
        // replaced by the lock-in.
    } else if !sell_first
        && buyer_id == Some(role.id)
        && (shop_trip_worth_taking(turn, role, pairs, &budget.shopping)
            || !economy::should_sell(turn, state, role, stone_demand))
    {
        if !budget.shopping.is_empty() {
            if let Some(cmd) = buyer_flow(turn, role, &budget.shopping, pairs, claimed) {
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
    //     part of it. Same rule for the committed role: drinking a carried
    //     Medicine needs no walk (step 1) and is untouched, buying one does.
    if !committed {
        if let Some(cmd) = self_provision(turn, role, claimed, true) {
            plan.push(role.id, cmd);
            return;
        }
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
    // 9b. The lock-in. Steps 8 and 9 have had their turn — a wall mended and a
    //     pack sold are both worth a round and both end beside the base — and
    //     everything from here down (the mine, the last-resort repair, the
    //     summon order) is an errand that leaves the ring and holds the gate
    //     open. See `dusk_recall_round` for what that costs.
    if committed {
        lock_in_for_dusk(turn, state, role, claimed, plan);
        return;
    }
    // 10. Mine the nearest ore (stone first while walls are wanted). Mining
    //    pauses during dusk so the ore we hold is converted to gold instead.
    //
    //    9c. Which is also why the pack-full branch is here and not beside the
    //    sale: a full pack is a role that cannot dig, and step 9 has just
    //    declined to sell it. `economy::discard_command` owns what may leave —
    //    only the cheapest ore carried, only when a vein worth more is still
    //    within reach of the afternoon, and never stone the ring is holding
    //    back — and it runs inside the same dusk guard, because the cash-out
    //    window converts ore to gold rather than throwing it away. With nothing
    //    worth dropping it returns None and the role is exactly as it was.
    if turn.in_day_round < economy::DUSK_ROUND {
        // The dedicated economy worker keeps the collect→sell→buy loop funded
        // once the ring's own build-out is over: outside day 1 it digs ore the
        // vendor buys, never the stone the wall line is holding back. See
        // `economy::choose_sellable_mine`.
        //
        // (`economy_earns` is the same predicate the wall step reads: the day-1
        // shift change is one rule, asked in two places, and the stone-first
        // bound lives inside it — see `plan`'s `ring_at_risk`.)
        let keep_gold_loop = economy_earns;
        if role.backpack_full() {
            if let Some(cmd) = economy::discard_command(turn, state, role, stone_demand) {
                plan.push(role.id, cmd);
                return;
            }
        } else if let Some(cmd) = mine_flow(
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
                // the ring and keeps the gate seal from ever completing.
                let stands = tower_stand_cells(turn, tower.pos);
                let stands = gate_clear_stands(turn, state, stands);
                // If the pioneer is already inside the ring (dist <= 1 from
                // the station), hold there — walking to a stand cell that
                // `gate_clear_stands` filtered to the outer ring would take
                // it back outside, breaking the dusk latch.
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

/// Day-round from which a role with NO gun to man must stop working outside the
/// ring and be inside it.
///
/// `preposition_round` covers a controller that has a tower, and the pioneer has
/// its own recall. A role with neither — the odd one out on a three-role board
/// with two guns, which is what day 1 always is (see `ring_still_forming`) —
/// had no deadline at all: `worker_day` falls straight through the pre-position
/// step, and every step below it (walls, buyer, shop, vendor, mine) is an errand
/// outside the ring.
///
/// Measured on a three-role board with one gun and the ring open: the role with
/// no gun walked to the weapon shop and issued `buy Medicine` on every single
/// round from r52 to r70 while `wall_gate_open` named it — the record that
/// issues #111/#112/#115 carry for the whole dusk window (15/15 open rounds,
/// base destroyed on night 2). The gate is the last ring cell and it does not
/// close while any role is outside, so one role's shopping trip is the hole the
/// night walks through.
///
/// The lead is FLAT, not measured from where the role currently is — the one
/// place this differs from `pioneer_recall_round`. A walk-based deadline reads
/// tighter but costs the whole afternoon: a worker eighteen cells out at the
/// far vein is told to stop at in-day 34 and hold for twenty rounds, and the day
/// then produces neither a coin nor a tower (measured on the
/// `stone_out_of_reach` board: the purse never moved off its opening 75 and the
/// gun count stayed at zero). `DUSK_RETREAT_LEAD` is one ordinary walk home plus
/// slack; a role caught further out than that still arrives inside the dusk
/// window (rounds 55-69), and arriving at 62 is the seal happening — which is
/// the thing that was never happening at all.
fn dusk_recall_round(turn: &Turn, role: &Unit) -> i64 {
    let flat = economy::DUSK_ROUND - DUSK_RETREAT_LEAD;
    // ...except that a FLAT lead is only ever right for a role that is one
    // ordinary walk from home, and issues #121-#125 show the other case is the
    // common one. In #122 and #124 a worker was still fourteen-plus cells out
    // when the dusk window opened and walked one cell per round for the whole
    // of it — `wall_gate_open` named it from day-round 56 to 70, the last day
    // round, and the gate never sealed. The lead is still flat for everyone it
    // fits; the floor below only moves the roles it demonstrably does not fit,
    // and it moves them just far enough to arrive INSIDE the window rather than
    // after it. Anchoring on `DUSK_ROUND + SEAL_GRACE` and not on `DUSK_ROUND`
    // is what keeps the afternoon: an eighteen-cell role turns around at
    // day-round 45, not at 34.
    let walked = economy::DUSK_ROUND + SEAL_GRACE - walk_home(turn, role.pos);
    flat.min(walked)
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

/// Walk `role` inside the ring and hold there — the role's post for the rest of
/// the day. A no-command round inside is the intended answer, not idleness:
/// standing on the band `update_wall_gate` measures "everyone is inside" against
/// is exactly what lets the gate cell be built.
///
/// Once committed the role does not leave again. Falling through to the economy
/// steps when it is already inside is the oscillation: the buyer walks to the
/// shop, the next round it is outside past its deadline, it walks back in, and
/// the gate opens and closes on alternate rounds (measured: `wall_gate_open`
/// naming the same role on every odd round of the window).
///
/// The shelter walk keeps `walk_or_remove_wall`'s demolition escape hatch, so a
/// role the crew walled out still gets in rather than pacing the outside.
fn lock_in_for_dusk(turn: &Turn, state: &BotState, role: &Unit, claimed: &mut HashSet<Pos>, plan: &mut Plan) {
    if let Some(cmd) = retreat_inside(turn, state, role, claimed) {
        plan.push(role.id, cmd);
    }
    // Inside already, or nowhere walkable to walk: both mean "no more errands
    // out there". `retreat_inside` has tried to cut a way through our own wall
    // before giving up, and the night recall keeps the same hatch.
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
/// a role the crew accidentally sealed on the wrong side of it is a hole in the
/// gate seal for the whole night, which costs more than one wall cell.
///
/// The shelter obeys the same rule as the pre-position posts
/// (`gate_clear_stands`): a role that hugs the station on the gate's own
/// approach cells jams the one door the stone carrier needs at dusk. Measured
/// on the day-2 board: the pioneer sheltered on the gate's inner shoulder, the
/// carrier with eleven stone could not reach either open cell, and the ring
/// spent the night at 18/20 with the reserve in hand.
fn retreat_inside(turn: &Turn, state: &BotState, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
    let station = turn.station()?;
    let footprint = station_footprint(station.pos);
    if footprint_distance(role.pos, &footprint) <= 1 {
        return None;
    }
    let stands = crate::brain::interior_cells(turn);
    if stands.is_empty() {
        return None;
    }
    let stands = gate_clear_stands(turn, state, stands);
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
pub(crate) fn pending_ring(turn: &Turn, state: &BotState, ignore: Pos) -> Vec<Pos> {
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let Some(gate) = wall_gate(turn, state) else {
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
pub(crate) fn outside_cells(turn: &Turn, footprint: &[Pos]) -> Vec<Pos> {
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

// ---------------------------------------------------------------------------
// Build site planning
// ---------------------------------------------------------------------------

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

    /// The closed ring around the mid-map station: all twenty cells one layer
    /// out from the footprint, which is the 19/20 the day-1 crew kept finishing
    /// on. Everyone inside is walled away from the ore.
    fn sealed_ring(extra: Vec<serde_json::Value>) -> Turn {
        let mut roles = vec![unit(10001, "station", pos(10, 24))];
        let footprint = station_footprint(pos(10, 24));
        for (index, at) in ring_cells(&footprint, 2).iter().enumerate() {
            roles.push(unit(20000 + index as i64, "wall", *at));
        }
        roles.extend(extra);
        let payload = serde_json::json!({
            // Day 2, in-day round 9: past the day-1 fortification rule, well
            // before the dusk seal.
            "roundNo": 140,
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

    fn door_of(turn: &Turn, actor: i64) -> Option<RoleCommand> {
        let state = &mut BotState::default();
        let mut claimed = HashSet::new();
        let role = turn.role_by_id(actor).expect("the actor exists");
        open_door(turn, state, role, &mut claimed)
    }

    /// 「工人可以在第二天出不去的时候选择破墙而出，但是开拓者如果夹在武器、基地
    /// 以及城墙之间，除非有人来救否则就出不去了」 — the worker has an escape
    /// hatch and the pioneer does not (任务书 4.4: `remove` is 工人 only, and
    /// `validate.rs` drops anyone else's). Here the ring is closed, every worker
    /// is out on the ore, and the pioneer is the one inside. With nobody cutting
    /// for it the pioneer spends the day behind the wall, which is the cage the
    /// owner described; the crew is the rescue, and the worker standing beside
    /// the line cuts the door from its own side.
    #[test]
    fn a_worker_cuts_the_ring_for_a_pioneer_sealed_inside() {
        let turn = sealed_ring(vec![
            unit(10002, "worker", pos(7, 24)),  // outside, beside the ring
            unit(10004, "pioneer", pos(11, 25)), // inside, and going nowhere
        ]);
        let pioneer = turn.role_by_id(10004).expect("the pioneer exists");
        assert!(
            !crate::brain::can_reach_any(&turn, pioneer, &outside_cells(&turn, &station_footprint(pos(10, 24)))),
            "test setup: the pioneer is not actually sealed in"
        );
        let cmd = door_of(&turn, 10002).expect("the crew cut the ring for the pioneer");
        assert_eq!(cmd.action, "remove", "a door is cut with `remove`: {cmd:?}");
        let target = cmd.targetPos.expect("a removal names its wall")[0];
        assert_eq!(
            footprint_distance(target, &station_footprint(pos(10, 24))),
            2,
            "{target:?} is not a cell of our ring"
        );
        // …and it is the right cell: with that one wall gone the pioneer walks
        // out. A removal anywhere else leaves it caged for the day.
        let mut blocked = turn.blocked_for(pioneer.id);
        blocked.remove(&target);
        assert!(
            crate::path::step_toward_stands(
                &turn,
                pioneer.pos,
                &outside_cells(&turn, &station_footprint(pos(10, 24))),
                &blocked
            )
            .is_some(),
            "the cell the crew cut is not the one that lets the pioneer out"
        );
    }

    /// The control for the test above: the same board with the pioneer OUTSIDE
    /// the ring. Nobody is caged, so nobody cuts — the door costs a stone and a
    /// hole the dusk has to re-seal, and it is only ever opened for a role that
    /// cannot open it for itself.
    #[test]
    fn no_door_is_cut_when_the_pioneer_is_already_out() {
        let turn = sealed_ring(vec![
            unit(10002, "worker", pos(7, 24)),
            unit(10004, "pioneer", pos(6, 24)),
        ]);
        assert!(
            door_of(&turn, 10002).is_none(),
            "a sealed-in pioneer is the only reason to cut a door from outside"
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
