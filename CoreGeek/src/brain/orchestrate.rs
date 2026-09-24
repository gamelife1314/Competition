//! The unified scheduler (issue #221).
//!
//! The whole plan is organized around the THREE PEOPLE, not around the clock:
//! every round, each controllable unit is dispatched to its role mainline
//! ([`crate::brain::role`]), which walks a single priority chain of guarded
//! actions. `turn.is_day` is an input to those guards and weights — a round
//! budget to spend, a threat parameter — and never a control-flow fork.
//!
//! # Transitional state (phase 4b of the refactor)
//!
//! BOTH workers are dispatched by their own mainlines: the economy worker (B)
//! by [`role::economy_worker`] (comment 1 §5/§6), the wall worker (A) by
//! [`role::wall_worker`] (comment 1 §4/§5) — the legacy `worker_day` dispatch
//! is deleted. `day::plan_roles` now owns only the team prompt slot, the
//! pioneer and the closing backstop. At night, A and the pioneer are still
//! planned by the legacy `night::plan_owned`; 4b-3 moves A onto the
//! single-operator L-shape model. Claim order is the legacy one — A, pioneer,
//! backstop, B — and no unit is ever commanded twice (`Plan::push` is
//! first-wins on top of that). The remaining day/night fork below is the LAST
//! one, and it dies in phase 5 when `role::pioneer` takes over and both
//! legacy files are deleted. Do not add new branches to it.

use std::collections::HashSet;

use super::role::{self, Roles};
use super::{day, night, Plan};
use crate::model::Turn;
use crate::protocol::Pos;
use crate::state::BotState;

/// Plan one round for the whole team.
pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let roles = Roles::of(turn);
    // The CONTEXT roster: units invisible to the round context — pairing,
    // buyer pick and trap vetoes. Only the economy worker: B sleeps outside
    // the ring by design (comment 1 §6), so its position must not veto the
    // last wall. A is dispatched by its own mainline but stays VISIBLE to the
    // context, exactly as the legacy dispatch counted it.
    let owned: Vec<i64> = roles.economy_worker.into_iter().collect();
    // Shared reservation set: A claims first (its mainline runs before the
    // legacy dispatch), then the pioneer, then B — the legacy claim priority,
    // which is comment 1's A→B order.
    let mut claimed: HashSet<Pos> = HashSet::new();
    let mut plan = Plan::default();
    // TRANSITIONAL (issue #221): the day/night fork. Removed in phase 5.
    if turn.is_day {
        let ctx = day::round_context(turn, state, &owned);
        if let Some(role) = roles.wall_worker.and_then(|id| turn.role_by_id(id)) {
            role::wall_worker::plan_day(turn, state, role, &ctx, &owned, &mut claimed, &mut plan);
        }
        // The legacy dispatch skips both mainline people (A: planned above;
        // B: planned below) and runs the team prompt slot, the pioneer and
        // the backstop for everybody else.
        let skip: Vec<i64> = [roles.wall_worker, roles.economy_worker]
            .into_iter()
            .flatten()
            .collect();
        day::plan_roles(turn, state, &ctx, &skip, &mut claimed, &mut plan);
    } else {
        // Night in 4b-2: A still takes its tower from the legacy pairing; the
        // L-shape single-operator model replaces it in 4b-3 (design D14-D16).
        plan = night::plan_owned(turn, state, &owned);
    }
    if let Some(id) = roles.economy_worker {
        role::economy_worker::plan(turn, state, id, &mut claimed, &mut plan);
    }
    plan
}
