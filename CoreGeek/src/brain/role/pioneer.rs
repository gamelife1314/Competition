//! Pioneer — the third person (issue #221 phase 5, comment 1 §3.3).
//!
//! One mainline owns the pioneer around the clock; `turn.is_day` only picks
//! which half of the person's duties is live, exactly like the two worker
//! mainlines. The day chain is the comment's §3 order —
//!
//! **self_heal → (summon orders in hand burn) → altar race → do_task → shop →
//! treasure → loiter → retreat inside**
//!
//! — with the pair-based pre-position lock and the `dusk_committed` latch
//! deleted (design D1/D2): the pioneer no longer mans a gun, so the only dusk
//! arithmetic left is the person-relative recall round, re-tested every round.
//! The night chain is comment 1 §3.3's `night_repair` (Q5's default): a
//! WallFixer in the pack is a standing shift inside the wall; with the kit
//! spent (or never held) an active task keeps running, and a pioneer with
//! neither shelters, spends the base vouchers, mends and throws the items.
//!
//! Q5's switch — [`BotState::pioneer_night_backup`] — adds the backup-gunner
//! wait ON TOP of the night chain when enabled: while hostile robots live, the
//! pioneer holds inside the ring near A instead of working a task, so a dead A
//! does not leave all three weapons silent. Default off.
//!
//! The pioneer is also the team's BUYER (phase 5b): the fixed whitelist of
//! comment 1 §3 ([`crate::brain::action::shop::fixed_buy_list`]) replaces the
//! legacy intent-list shopping, and the shop step owns the cross-person
//! `buy_deadline` sync — the round the pioneer expects to stand at the
//! counter, which is B's sell deadline (comment 1 §7).
//!
//! This module also hosts the round-level team prompt slot
//! ([`plan_prompts`]): the news/treasure ask is a property of the ROUND, not
//! of the pioneer, and must survive the pioneer's death (issue #218).

use std::collections::HashSet;

use super::super::action::fight::{hostile_wave, night_medicine};
use super::super::action::shop::{
    at_shop, burn_summon_order, buy_first_affordable, fixed_buy_list, self_provision, use_medicine,
    voucher_flow, walk_to_shop,
};
use super::super::{
    combat, economy, news, shelter, stand_cells, task, treasure, walk_toward, Plan,
};
use crate::model::{chebyshev, footprint_distance, Turn, Unit, UnitKind};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// Slack between the recall round and dusk proper — the same three rounds of
/// detour allowance the legacy pre-position held, minus one because the
/// permanent entrance (design D17) makes the walk home predictable.
const PIONEER_RETREAT_SLACK: i64 = 2;

/// A session that has produced nothing for this many rounds is abandoned
/// (issue #15's lesson; issue #26 lost the base to a gate no controller had
/// retreated through).
const MAX_STERILE_ROUNDS: i64 = 15;

/// Plan the pioneer for this round: the day chain or the night chain, picked
/// inside — the scheduler never forks on the clock (issue #221 phase 5c).
pub(crate) fn plan(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    if !role.alive() {
        return;
    }
    if turn.is_day {
        plan_day(turn, state, role, claimed, plan);
    } else {
        plan_night(turn, state, role, claimed, plan);
    }
}

/// Plan a SPARE controller — any controllable the roster does not name (issue
/// #221 phase 5c). Night-only by construction: after dark the repair chain is
/// role-generic and every spare runs it (the legacy `spare_night`), while the
/// legacy DAY planners gave spares nothing but the scheduler's closing
/// backstop — so this declines immediately by day and the backstop below it in
/// [`crate::brain::orchestrate`] owns the round, exactly as before. One
/// statement list in the scheduler, the day/night split kept where the legacy
/// behavior had it.
pub(crate) fn plan_spare(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    if turn.is_day {
        return;
    }
    self::plan(turn, state, role, claimed, plan);
}

// ---------------------------------------------------------------------------
// Day
// ---------------------------------------------------------------------------

fn plan_day(
    turn: &Turn,
    state: &mut BotState,
    pioneer: &Unit,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    // 0. Dusk recall. The pioneer is the one role whose work — task points,
    //    treasure, the shop — is always outside the ring, and the night repair
    //    duty (§3.3) needs it inside. The recall round is measured from where
    //    the pioneer IS, so it tightens as the day goes on and an errand
    //    accepted at r=40 is not still being walked at r=54.
    //
    //    UNLATCHED (design D2 deletes `dusk_committed`): the old latch existed
    //    to stop a two-cell oscillation between the recall and fresh errands,
    //    but every outward step below is now gated on `!recalled` AND on the
    //    task's own `TASK_MIN_ATTEMPT_ROUNDS` fit, so the worst a re-test can
    //    do is pace one cell near the deadline. An ACTIVE session is no longer
    //    aborted at recall either: Q5's 回退任务循环 keeps a session working
    //    through the night while the pioneer carries no WallFixer, and the
    //    sterile/timeout aborts remain the session's real limits.
    let recalled = turn.in_day_round >= recall_round(turn, pioneer);
    // Is the altar's window this round's business (P2-3)? One computation, read
    // in the three places 「顺序上不能固定」 moves: the session it takes the
    // pioneer off, the altar step it puts above the task accept, and the task
    // accept it stands down.
    let altar_due = !recalled && treasure::window_due(turn, state, pioneer);
    // A session that has produced nothing at all after `MAX_STERILE_ROUNDS` is
    // not converging, and the pioneer is worth more elsewhere. Checked before
    // anything else so the log names the real reason a session ended.
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
    // 1. THE ONE-SHOT RACE (P2-3). The altar is the one thing on the board that
    //    does not come back: 任务书 5.2 — 一张地图宝藏只有一个. A session still
    //    running when the window opens is ABANDONED for the altar rather than
    //    finished — the pioneer leaves the point either way (「离开己方任务点
    //    周围一格内」 ends the task), and the only choice left is whether it
    //    leaves for the treasure or for nothing.
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
    // 2b. A carried summon order burns the moment it is held — 买完立即使用
    //     (comment 1 §3) and 东西在谁手里谁使用: the whitelist below buys the
    //     Large order with the pioneer's own gold, so the pioneer fires it.
    //     Using an item does not move the role, so this never costs a post.
    if let Some(cmd) = burn_summon_order(state, pioneer) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 3. THE ALTAR, while its window is live (P2-3). 「顺序上不能固定」 — while
    //    the window is due, the altar is the errand and the task point waits.
    if altar_due {
        if let Some(cmd) = treasure::plan_pioneer(turn, state, pioneer, claimed, plan) {
            plan.push(pioneer.id, cmd);
            return;
        }
        if treasure::holds_altar(turn, state, pioneer) {
            return;
        }
    }
    // 4. Accept a fresh task when a point is ready (walk there first). Not once
    //    the recall has fired, and not while the altar's window is due.
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
    //     pioneer below the night withdrawal threshold — the walk there too,
    //     but only before the recall.
    if let Some(cmd) = self_provision(turn, pioneer, claimed, !recalled) {
        plan.push(pioneer.id, cmd);
        return;
    }
    // 6. SHOP (comment 1 §3, phase 5b): no task to run and no altar to race —
    //    the pioneer is the buyer. The fixed whitelist decides WHAT; this step
    //    decides the errand: walk to the counter, buy the first affordable
    //    entry, and with an unaffordable list WAIT AT THE COUNTER for B's gold
    //    instead of idling at the base (issue #13's 站桩 with a purpose). The
    //    walk is an outward errand like any other, so the recall owns it; a
    //    pioneer already AT the counter still buys past the recall — the
    //    purchase costs no rounds away from home.
    let list = fixed_buy_list(turn);
    // Cross-person sync (comment 1 §7): the round the buyer expects to stand at
    //    the counter, recomputed from the pioneer's OWN walk every day round —
    //    B reads it as "my ore must be gold before this round". `None` when the
    //    whitelist is empty (nothing to buy, no deadline to race).
    state.buy_deadline =
        (!list.is_empty()).then(|| turn.round_no + economy::shop_travel(turn, pioneer.pos));
    if !list.is_empty() {
        crate::log::event(
            "shopping",
            serde_json::json!({
                "round": turn.round_no,
                "buyer": pioneer.id,
                "gold": turn.gold,
                "need": list.first().map(|need| need.name.as_str()),
                "needs": list.iter().map(|need| need.name.as_str()).collect::<Vec<_>>(),
                "atCounter": at_shop(turn, pioneer.pos),
                "buyDeadline": state.buy_deadline,
                "recalled": recalled,
            }),
        );
        if at_shop(turn, pioneer.pos) {
            if let Some(cmd) = buy_first_affordable(turn, state, pioneer, &list) {
                plan.push(pioneer.id, cmd);
                return;
            }
            if !recalled {
                return; // waiting for gold at the counter IS the errand
            }
        } else if !recalled && !pioneer.backpack_full() {
            if let Some(cmd) = walk_to_shop(turn, pioneer, claimed) {
                plan.push(pioneer.id, cmd);
                return;
            }
        }
    }
    // 7. Treasure hunt. Outdoors, so the same cutoff as the task point — and
    //    below the shop: comment 1 §3 ranks 商店 before the treasure errand.
    if !recalled {
        if let Some(cmd) = treasure::plan_pioneer(turn, state, pioneer, claimed, plan) {
            plan.push(pioneer.id, cmd);
            return;
        }
        // P2-1: waiting beside the altar for the opening day is a deliberate
        // HOLD, not idleness — the loiter/retreat steps below would each drag
        // the pioneer a cell away so the treasure step has to drag it back.
        if treasure::holds_altar(turn, state, pioneer) {
            return;
        }
    }
    // 8. Loiter next to a task point so we catch refreshes immediately — until
    //    the recall round, after which loitering IS the thing being recalled.
    if !recalled {
        loiter_at_task_point(turn, state, pioneer, claimed, plan);
        if plan.commands.contains_key(&pioneer.id) {
            return;
        }
    }
    // 9. Nothing to do at all. Retreat inside the ring instead of standing in
    //    the open: a lone role parked outside the wall line is a free kill for
    //    the robots (issue #13's "0 角色站桩闲置").
    if let Some(cmd) = retreat_inside(turn, pioneer, claimed) {
        plan.push(pioneer.id, cmd);
    }
}

/// Day-round from which the pioneer stops taking on anything outside the wall
/// line and heads home: `DUSK_ROUND` less the walk back, less a small slack.
/// The walk is measured from the pioneer's CURRENT cell, so the deadline is a
/// rolling one — the further out it has drifted, the earlier it turns around.
pub(crate) fn recall_round(turn: &Turn, pioneer: &Unit) -> i64 {
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

/// Walk the pioneer back inside the ring, onto a cell at footprint distance
/// <= 1 from the station. `None` when it is already inside or nothing inside
/// is reachable, so a genuinely boxed-in role holds rather than paces.
/// "Reachable" includes demolishing one of our own walls on the way in
/// (`walk_home_or_reroute`, P0-3); with the permanent entrance (design D17)
/// the wrong side of the ring is a pathing accident, not a daily certainty.
fn retreat_inside(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>) -> Option<RoleCommand> {
    let station = turn.station()?;
    let footprint = crate::model::station_footprint(station.pos);
    if footprint_distance(role.pos, &footprint) <= 1 {
        return None;
    }
    let stands = crate::brain::interior_cells(turn);
    if stands.is_empty() {
        return None;
    }
    crate::brain::walk_home_or_reroute(turn, role, &stands, claimed)
}

// ---------------------------------------------------------------------------
// Night — comment 1 §3.3 `night_repair`, Q5
// ---------------------------------------------------------------------------

/// The night repair-duty chain. Also the fallback for any controllable the
/// roster does not name: every step is role-generic (the task branch is
/// pioneer-gated inside).
fn plan_night(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    // Self-heal — survival outranks every other duty, including the task: a
    // potion is one round and a dead pioneer finishes no session.
    if let Some(cmd) = night_medicine(turn, role) {
        plan.push(role.id, cmd);
        return;
    }
    // Q5's switch: 先作为A的后背等敌人消灭 — while a hostile wave lives, the
    // pioneer holds inside the ring as the backup gunner instead of working a
    // task outside it, so a dead A does not leave all three weapons silent.
    // Inside the ring the chain below still runs (vouchers, mending and items
    // are all backup-compatible); only the task branch — the one step that
    // leads back OUT — stands down until the board is swept.
    let backup = state.pioneer_night_backup && hostile_wave(turn);
    // Q5: 有围墙修复包时站在墙内准备维修围墙 — the repair kit outranks the
    // task; 没有或者使用完…撤离到安全区域 is what the shelter below does, and
    // 回退任务循环 is the task branch staying alive through the night.
    let repair_duty = role.count_item("WallFixer") > 0;
    if !backup && role.kind == UnitKind::Pioneer && state.task.active && !repair_duty {
        if let Some(cmd) = task::plan_pioneer(turn, state, role, plan) {
            plan.push(role.id, cmd);
        }
        return;
    }
    // Retreat inside the wall ring next to the station BEFORE anything that
    // keeps the role out in the open. Once inside we fall through to the
    // repair duties.
    if shelter(turn, role, claimed, plan) {
        return;
    }
    // `night_repair`, first half (comment 1 §3.3: 夜间回基地使用升级券): at
    // the base, the upgrade vouchers go on before the masonry — an upgrade is
    // permanent value, and the mend below still owns every round the wall is
    // actually being chewed.
    if let Some(cmd) = voucher_flow(turn, role, claimed) {
        plan.push(role.id, cmd);
        return;
    }
    // THE MASON ROUND (Q5's 站在墙内准备维修围墙). A pioneer standing on the
    // inner band — which is where `shelter` just put it, and the band is
    // adjacent to the ring — is the second pair of hands the night never used:
    // issues #131-#135 measured the ring losing 2465-15135 HP a night with the
    // base behind it falling on night 1-3. Deliberately placed AFTER the
    // shelter walk, so the walk home still owns the round, and BEFORE the
    // items, because a wall restored to full is worth more than a Bomb that
    // may not land.
    if let Some(wall_pos) = combat::night_mend_target(turn, role) {
        crate::log::event(
            "wall_mend",
            serde_json::json!({
                "round": turn.round_no,
                "role": role.id,
                "target": [wall_pos.x, wall_pos.y],
                "duty": "spare",
            }),
        );
        plan.push(role.id, RoleCommand::use_item_at("WallFixer", wall_pos));
        return;
    }
    // Burn summon orders (harassment works at night too).
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
            // P2-3 压制窗口: the boss order is first in ORDERS, so a spare that
            // carries one already fires it ahead of the smaller waves. What
            // this records is WHY it was worth buying today — a wave aimed at
            // a base that is nearly down or at towers inside one blast radius
            // is the shot the window opened for, and the round it lands is the
            // only place that can be confirmed from the log.
            if order == "BossRobotSummonOrder" && economy::boss_suppression_window(turn) {
                crate::log::event(
                    "boss_suppression",
                    serde_json::json!({
                        "round": turn.round_no,
                        "role": role.id,
                        "enemyStationHp": turn.enemy_station().map(|station| station.health),
                        "enemyTowers": turn
                            .enemy
                            .iter()
                            .filter(|unit| unit.kind.is_tower() && unit.alive())
                            .count(),
                    }),
                );
            }
            plan.push(role.id, RoleCommand::use_item(order));
            return;
        }
    }
    // Bomb: worth it against clusters (>= 2 robots) or big targets.
    if role.count_item("Bomb") > 0 {
        if let Some(impact) = combat::bomb_impact(turn) {
            let clustered = turn
                .robots
                .iter()
                .filter(|robot| robot.health > 0 && chebyshev(impact, robot.pos) <= 1)
                .count();
            let big = turn.robots.iter().any(|robot| {
                robot.health > 0
                    && chebyshev(impact, robot.pos) <= 1
                    && combat::is_big_threat(robot.kind)
            });
            if clustered >= 2 || big {
                plan.push(role.id, RoleCommand::use_item_at("Bomb", impact));
                return;
            }
        }
    }
    // Dizzy: stall a wave pressing our base.
    if role.count_item("DizzyWeapon") > 0 {
        if let Some(impact) = combat::dizzy_impact(turn) {
            plan.push(role.id, RoleCommand::use_item_at("DizzyWeapon", impact));
            return;
        }
    }
    // (Shelter already ran above; reaching here means the role is inside the
    // ring and has nothing else to throw at the robots — holding is the plan.)
}

// ---------------------------------------------------------------------------
// Round-level team channel
// ---------------------------------------------------------------------------

/// The day's news/treasure prompt slot. A team-level channel, not a role
/// command — it costs the pioneer no movement, and it runs even when the
/// pioneer is dead (issue #218: the ask is a property of the ROUND).
///
/// DAY-only, and the guard lives here rather than in the scheduler (phase 5c:
/// the dispatch is one statement list around the clock): every legacy planner
/// asked only by day, and the night spends its LLM budget on nothing.
pub(crate) fn plan_prompts(turn: &Turn, state: &mut BotState, plan: &mut Plan) {
    if !turn.is_day {
        return;
    }
    // Merged prompt: when both news and treasure need asking, send one prompt
    // with both questions (issue #207 §3: 「用一个 prompt 向大模型发起两个
    // 提问」). Falls back to individual prompts when only one consumer needs
    // the slot.
    if !plan_merged_prompt(turn, state, plan) {
        news::plan_prompt(turn, state, plan);
        treasure::plan_ask(turn, state, plan);
    }
}

/// Returns `true` if a merged prompt was sent, `false` if either consumer
/// doesn't need asking (in which case the individual `news::plan_prompt` /
/// `treasure::plan_ask` calls handle it as before, unchanged).
///
/// BOTH is the whole rule, and each half is the consumer's OWN predicate —
/// `news::plan_prompt`'s (`wants_reading` + today's text on the board) and
/// `treasure::plan_ask`'s (`Idle` + two legends + day ≥ 2). Merge only when the
/// two asks would otherwise BOTH go out this round, and the merge can never
/// stand a consumer down.
///
/// The budget is asked for on the news's purpose — the highest ranked and the
/// one never stood down. Asking for the treasure's purpose as well would REFUSE
/// the merge on every day whose news is still unread (`news_read_reserved`
/// stands the treasure down precisely then), which is the one round the merge
/// exists for.
fn plan_merged_prompt(turn: &Turn, state: &mut BotState, plan: &mut Plan) -> bool {
    if plan.prompt.is_some() {
        return false;
    }
    // Both consumers must want the slot.
    let news_wants = state.news.wants_reading() && state.official_seen.get(&turn.day).is_some();
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

    fn wall_at(id: i64, at: Pos, hp: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": "wall",
            "health": hp, "level": 1, "backPackCapability": 0, "backpack": []
        })
    }

    /// A night board (roundNo 71+ = day 1's night) with the station at
    /// (10,24); its footprint is {(10,24),(11,24),(10,23),(11,23)}, so the
    /// interior band includes (9,22) — which is adjacent to the ring's
    /// bottom-row wall cells (9,21) and (10,21).
    fn night_board(round_no: i64, roles: Vec<serde_json::Value>) -> Turn {
        board(round_no, roles, vec![], vec![], 0)
    }

    fn board(
        round_no: i64,
        roles: Vec<serde_json::Value>,
        zones: Vec<serde_json::Value>,
        robots: Vec<serde_json::Value>,
        gold: i64,
    ) -> Turn {
        let mut all = vec![unit(10001, "station", pos(10, 24), vec![])];
        all.extend(roles);
        let payload = serde_json::json!({
            "roundNo": round_no,
            "mapInfo": {"width": 41, "height": 32, "zones": zones},
            "weaponShopList": [
                {"name": "StationUpgradeVoucher1", "price": 2250},
                {"name": "WeaponUpgradeVoucher1", "price": 1500},
                {"name": "WallUpgradeVoucher1", "price": 500},
                {"name": "LargeRobotSummonOrder", "price": 300},
                {"name": "Bomb", "price": 150},
                {"name": "WallFixer", "price": 300},
                {"name": "Medicine", "price": 100},
            ],
            "teamOur": {
                "type": "challenger", "goldNum": gold, "totalScore": 0,
                "playerTasks": [], "roles": all
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": robots},
        });
        let req: crate::protocol::Request =
            serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    fn robot(id: i64, at: Pos, kind: &str, health: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": kind,
            "health": health, "targetTeam": "challenger"
        })
    }

    fn plan_for(turn: &Turn, state: &mut BotState, id: i64) -> Plan {
        let mut claimed = HashSet::new();
        let mut plan = Plan::default();
        let role = turn.role_by_id(id).expect("the role exists");
        super::plan(turn, state, role, &mut claimed, &mut plan);
        plan
    }

    fn target(cmd: &RoleCommand) -> Pos {
        cmd.targetPos.as_ref().expect("a target")[0]
    }

    // -- night: the repair duty (moved from night.rs, phase 4b-3) -----------

    /// Q5: 有围墙修复包时站在墙内准备维修围墙 — a WallFixer in the pack is a
    /// standing shift inside the wall, and it outranks the task: the pioneer
    /// walks INTO the ring, never out to a task point, while it carries one.
    #[test]
    fn the_repair_kit_outranks_the_task() {
        let turn = night_board(
            75,
            vec![unit(20001, "pioneer", pos(20, 10), vec!["WallFixer"])],
        );
        assert!(!turn.is_day);
        let mut state = BotState::default();
        state.task.active = true;
        let plan = plan_for(&turn, &mut state, 20001);
        let cmd = plan.commands.get(&20001).expect("the repair duty walks");
        assert_eq!(cmd.action, "move");
        let station = pos(10, 24);
        assert!(
            chebyshev(target(cmd), station) < chebyshev(pos(20, 10), station),
            "the step closes on the base, not on the task point"
        );
    }

    /// Q5's other half and comment 1 §3.3 `night_repair`: inside the ring, a
    /// mender with a damaged wall beside it spends the round on the WEAKEST
    /// adjacent wall — the shelter walk is already done and no voucher, item
    /// or summon steals the mend.
    #[test]
    fn the_mender_mends_the_weakest_adjacent_wall() {
        let turn = night_board(
            75,
            vec![
                unit(20001, "pioneer", pos(9, 22), vec!["WallFixer"]),
                wall_at(20010, pos(9, 21), 700),
                wall_at(20011, pos(10, 21), 400),
            ],
        );
        let mut state = BotState::default();
        let plan = plan_for(&turn, &mut state, 20001);
        let cmd = plan.commands.get(&20001).expect("the mender mends");
        assert_eq!(cmd.action, "use");
        assert_eq!(cmd.name.as_deref(), Some("WallFixer"));
        assert_eq!(target(&cmd), pos(10, 21), "the weakest wall first");
    }

    /// Self-heal now outranks the night task branch: a wounded pioneer with a
    /// potion drinks it instead of working the session — a dead pioneer
    /// finishes no task.
    #[test]
    fn the_wounded_task_runner_drinks_first() {
        let wounded = serde_json::json!({
            "id": 20001, "pos": {"x": 9, "y": 22}, "roleType": "pioneer",
            "health": 50, "level": 1, "backPackCapability": 100,
            "backpack": ["Medicine"]
        });
        let turn = night_board(75, vec![wounded]);
        let mut state = BotState::default();
        state.task.active = true;
        let plan = plan_for(&turn, &mut state, 20001);
        let cmd = plan.commands.get(&20001).expect("the potion goes down");
        assert_eq!(cmd.action, "use");
        assert_eq!(cmd.name.as_deref(), Some("Medicine"));
    }

    /// Q5's switch, ON: while a hostile wave lives the pioneer does NOT keep
    /// working its task outside — it closes on the ring as A's backup. Off is
    /// the default and the task keeps running (the repair-kit test above pins
    /// the kit case; this pins the wave case).
    #[test]
    fn the_backup_switch_holds_the_pioneer_inside_while_robots_live() {
        let turn = board(
            75,
            vec![unit(20001, "pioneer", pos(20, 10), vec![])],
            vec![],
            vec![robot(30001, pos(12, 23), "largeRobot", 500)],
            0,
        );
        assert!(!turn.is_day);
        let mut state = BotState::default();
        state.task.active = true;
        state.pioneer_night_backup = true;
        let plan = plan_for(&turn, &mut state, 20001);
        let cmd = plan
            .commands
            .get(&20001)
            .expect("the backup walks home, it does not sit at the task point");
        assert_eq!(cmd.action, "move");
        let station = pos(10, 24);
        assert!(
            chebyshev(target(cmd), station) < chebyshev(pos(20, 10), station),
            "the backup closes on the ring while the wave lives"
        );
    }

    /// …and with the switch at its default the same board leaves the session
    /// in charge: Q5 先按照不作为A的后背处理 — the task branch owns the
    /// pioneer (holding at the point, waiting on the LLM, is most rounds), so
    /// the wave outside changes nothing.
    #[test]
    fn the_default_night_keeps_the_session_holding() {
        let turn = board(
            75,
            vec![unit(20001, "pioneer", pos(20, 10), vec![])],
            vec![],
            vec![robot(30001, pos(12, 23), "largeRobot", 500)],
            0,
        );
        let mut state = BotState::default();
        state.task.active = true;
        let plan = plan_for(&turn, &mut state, 20001);
        assert!(
            !plan.commands.contains_key(&20001),
            "the bare session holds the pioneer at its point — no backup detour"
        );
    }

    // -- day: the buyer (phase 5b) -------------------------------------------

    fn day_shop_board(round_no: i64, gold: i64, pack: Vec<&str>) -> Turn {
        board(
            round_no,
            vec![unit(20001, "pioneer", pos(20, 10), pack)],
            vec![serde_json::json!({"pos": {"x": 30, "y": 8}, "neutralType": "weaponShop"})],
            vec![],
            gold,
        )
    }

    /// Comment 1 §3's fixed order: an idle pioneer with a purchase on the list
    /// walks to the shop, and the walk writes B's sell deadline — the round
    /// the buyer expects to stand at the counter.
    #[test]
    fn the_idle_pioneer_walks_to_the_shop_and_syncs_the_deadline() {
        let turn = day_shop_board(10, 0, vec![]);
        assert!(turn.is_day);
        let mut state = BotState::default();
        let plan = plan_for(&turn, &mut state, 20001);
        let cmd = plan.commands.get(&20001).expect("the buyer walks");
        assert_eq!(cmd.action, "move");
        let shop = pos(30, 8);
        assert!(
            chebyshev(target(cmd), shop) < chebyshev(pos(20, 10), shop),
            "the step closes on the counter"
        );
        let deadline = state.buy_deadline.expect("the sync slot is written");
        assert!(
            deadline > turn.round_no,
            "the deadline is the walk away: {}",
            deadline
        );
    }

    /// At the counter with gold: the FIRST AFFORDABLE entry of the whitelist
    /// is bought — with an L1 station on the board and 2250+ gold that is the
    /// station ladder voucher ahead of everything else the list carries.
    #[test]
    fn the_counter_buys_the_whitelist_head() {
        let turn = board(
            10,
            vec![serde_json::json!({
                "id": 20001, "pos": {"x": 30, "y": 7}, "roleType": "pioneer",
                "health": 200, "level": 1, "backPackCapability": 100, "backpack": []
            })],
            vec![serde_json::json!({"pos": {"x": 30, "y": 8}, "neutralType": "weaponShop"})],
            vec![],
            5000,
        );
        let mut state = BotState::default();
        let plan = plan_for(&turn, &mut state, 20001);
        let cmd = plan.commands.get(&20001).expect("the buyer buys");
        assert_eq!(cmd.action, "buy");
        assert_eq!(
            cmd.name.as_deref(),
            Some("StationUpgradeVoucher1"),
            "the base voucher heads the whitelist (comment 1 §3)"
        );
    }

    /// WallFixer surplus conversion: capped at 5 through day 5 and 10 from
    /// day 6, counted against the TEAM's stock.
    #[test]
    fn the_fixer_cap_follows_the_day() {
        let d3 = day_shop_board(3, 0, vec!["WallFixer", "WallFixer"]);
        let fixer = crate::brain::action::shop::wall_fixer_surplus(&d3)
            .expect("below the cap there is surplus to convert");
        assert_eq!(fixer.num, 3, "day <= 5 caps at 5, two are held");
        // Day 6 starts at roundNo 5*130+1 = 651.
        let d6 = day_shop_board(651 + 9, 0, vec!["WallFixer", "WallFixer"]);
        let fixer = crate::brain::action::shop::wall_fixer_surplus(&d6)
            .expect("below the cap there is surplus to convert");
        assert_eq!(fixer.num, 8, "day 6+ caps at 10, two are held");
    }
}
