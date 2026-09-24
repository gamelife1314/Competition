//! The unified scheduler (issue #221).
//!
//! The whole plan is organized around the THREE PEOPLE, not around the clock:
//! every round, each controllable unit is dispatched to its role mainline
//! ([`crate::brain::role`]), which walks a single priority chain of guarded
//! actions. `turn.is_day` is an input to those guards and weights — a round
//! budget to spend, a threat parameter — and never a control-flow fork here:
//! the dispatch below is the SAME statement list day and night, and whatever
//! day/night shaping a mainline needs lives inside the person's own file
//! (`role::wall_worker::plan` forks internally; `role::pioneer::plan_prompts`
//! and `plan_spare` carry their own guards).
//!
//! The people, in claim order (the legacy priority, which is comment 1's
//! A→B order):
//!
//! - **A, the wall worker** ([`role::wall_worker`], comment 1 §4/§5 by day;
//!   §3.4's single-operator L-shape at night — design D14-D16);
//! - **the pioneer** ([`role::pioneer`], §3.3's task/buy/repair chain, the
//!   fixed buy whitelist of §3, and Q5's night wall-repair duty);
//! - **everybody the roster does not name** — spare controllers run the
//!   pioneer's spare chain after dark and the closing backstop by day
//!   ([`role::pioneer::plan_spare`] is night-gated by construction, exactly
//!   like the legacy split: spares got only the backstop in the day planners);
//! - **the closing backstop** — the anti-idle guarantee for any unit no
//!   mainline commanded (issue #13's "0 角色站桩闲置");
//! - **B, the economy worker** ([`role::economy_worker`], §5/§6) last, as the
//!   legacy both-mode tail: B never comes home, so nothing it claims can
//!   strand the people who must be inside by dark.
//!
//! No unit is ever commanded twice: [`Plan::push`] is first-wins on top of the
//! per-mainline discipline. Both legacy dispatchers are gone — `night.rs`
//! died in phase 5a, `day.rs` in phase 5c — and their survivors live in the
//! role files, [`super::action::fight`], [`super::action::geometry`] and
//! [`super::shelter`].

use std::collections::HashSet;

use super::role::{self, Roles};
use super::{fallback_toward_station, treasure, Plan};
use crate::model::{Turn, Unit, UnitKind};
use crate::protocol::Pos;
use crate::state::BotState;

/// Plan one round for the whole team. The entry is deliberately dumb: one
/// dispatch statement per person, in claim order, with no round-shape fork —
/// anything smarter belongs inside the person's mainline.
pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let roles = Roles::of(turn);
    // The CONTEXT roster: units invisible to the trap vetoes. Only the economy
    // worker: B sleeps outside the ring by design (comment 1 §6), so its
    // position must not veto the last wall.
    let owned: Vec<i64> = roles.economy_worker.into_iter().collect();
    // Shared reservation set: A claims first, then the pioneer, then B — the
    // legacy claim priority, which is comment 1's A→B order.
    let mut claimed: HashSet<Pos> = HashSet::new();
    let mut plan = Plan::default();

    // The team prompt slot: the news/treasure ask is a property of the ROUND —
    // a dead pioneer must not silence it (issue #218) — but it is a DAY
    // property too: the legacy planners only ever asked by day, and the night
    // spends its LLM budget on nothing. The guard lives inside `plan_prompts`.
    role::pioneer::plan_prompts(turn, state, &mut plan);

    if let Some(role) = roles.wall_worker.and_then(|id| turn.role_by_id(id)) {
        role::wall_worker::plan(turn, state, role, &owned, &mut claimed, &mut plan);
    }
    if let Some(role) = roles.pioneer.and_then(|id| turn.role_by_id(id)) {
        role::pioneer::plan(turn, state, role, &mut claimed, &mut plan);
    }
    // Spare controllers — everybody the roster does not name. By night they run
    // the pioneer's spare chain (WallFixer duty, medicine runs, shelter — the
    // legacy `spare_night`); by day `plan_spare` declines immediately and they
    // fall through to the backstop below, which is exactly what the legacy day
    // planners did with them.
    for role in turn.controllable() {
        if roles.kind_of(role.id).is_none() {
            role::pioneer::plan_spare(turn, state, role, &mut claimed, &mut plan);
        }
    }
    // The closing backstop. Every roster person is skipped — each has a
    // mainline that owns its rounds, including the deliberate holds: the
    // pioneer's counter-hold and task-point wait are its plan, not idleness,
    // and a backstop walk would drag the buyer off the counter (phase 5b).
    let skip: Vec<i64> = [roles.wall_worker, roles.economy_worker, roles.pioneer]
        .into_iter()
        .flatten()
        .collect();
    plan_backstop(turn, state, &skip, &mut plan);

    if let Some(id) = roles.economy_worker {
        role::economy_worker::plan(turn, state, id, &mut claimed, &mut plan);
    }
    plan
}

/// The closing backstop (issue #221 phase 5a, moved out of the deleted
/// `day.rs` by phase 5c): the last walk for any role no mainline commanded
/// this round — the anti-idle guarantee alone.
///
/// `skip` is the DISPATCH exclusion: ids a role mainline commands, which the
/// backstop must not second-guess with a walk of its own. The pioneer is in
/// the list even while it holds the shop counter: its own mainline owns every
/// round of the buy errand, counter-hold included.
fn plan_backstop(turn: &Turn, state: &BotState, skip: &[i64], plan: &mut Plan) {
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
        if role.kind == UnitKind::Pioneer && treasure::holds_altar(turn, state, role) {
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
    state.task.active && role.kind == UnitKind::Pioneer
}
