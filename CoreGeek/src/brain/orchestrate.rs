//! The unified scheduler (issue #221).
//!
//! The whole plan is organized around the THREE PEOPLE, not around the clock:
//! every round, each controllable unit is dispatched to its role mainline
//! ([`crate::brain::role`]), which walks a single priority chain of guarded
//! actions. `turn.is_day` is an input to those guards and weights — a round
//! budget to spend, a threat parameter — and never a control-flow fork.
//!
//! # Transitional state (phase 4a of the refactor)
//!
//! The economy worker (B) is dispatched by its own mainline
//! ([`role::economy_worker`], comment 1 §5/§6). The wall worker (A) now has
//! its mainline module too ([`role::wall_worker`]) — in 4a it DELEGATES to
//! the legacy `worker_day` dispatch, and the pioneer plus the team prompt slot
//! still come from `day::plan_roles`. At night, A and the pioneer are still
//! planned by the legacy `night::plan_owned`; 4b moves A onto the
//! single-operator L-shape model. Claim order is the legacy one — A, pioneer,
//! backstop, B — and no unit is ever commanded twice (`Plan::push` is
//! first-wins on top of that). The remaining day/night fork below is the LAST
//! one, and it dies in phase 5 when `role::{pioneer, wall_worker}` take over
//! and both legacy files are deleted. Do not add new branches to it.

use std::collections::HashSet;

use super::role::{self, Roles};
use super::{day, night, Plan};
use crate::model::Turn;
use crate::protocol::Pos;
use crate::state::BotState;

/// Plan one round for the whole team.
pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let roles = Roles::of(turn);
    // The CONTEXT roster: units invisible to the legacy round context —
    // pairing, gate record, buyer pick and trap vetoes. In 4a it is still
    // only the economy worker; the wall worker's day delegates to the legacy
    // dispatch, so the context must keep counting A exactly as it did.
    let owned: Vec<i64> = roles.economy_worker.into_iter().collect();
    // Shared reservation set: A claims first (its mainline runs before the
    // legacy dispatch), then the pioneer, then B — the legacy claim priority,
    // which is comment 1's A→B order.
    let mut claimed: HashSet<Pos> = HashSet::new();
    let mut plan = Plan::default();
    // TRANSITIONAL (issue #221): delegation, not dispatch. Removed in phase 5.
    if turn.is_day {
        let ctx = day::round_context(turn, state, &owned);
        if let Some(role) = roles.wall_worker.and_then(|id| turn.role_by_id(id)) {
            role::wall_worker::plan_day(turn, state, role, &ctx, &owned, &mut claimed, &mut plan);
        }
        // The legacy dispatch skips both mainline people (A: planned above;
        // B: planned below) but still runs the team prompt slot, the pioneer
        // and the backstop for everybody it owns.
        let skip: Vec<i64> = [roles.wall_worker, roles.economy_worker]
            .into_iter()
            .flatten()
            .collect();
        day::plan_roles(turn, state, &ctx, &owned, &skip, &mut claimed, &mut plan);
    } else {
        // Night in 4a: A still takes its tower from the legacy pairing; the
        // L-shape single-operator model replaces it in 4b (design D14-D16).
        plan = night::plan_owned(turn, state, &owned);
    }
    if let Some(id) = roles.economy_worker {
        role::economy_worker::plan(turn, state, id, &mut claimed, &mut plan);
    }
    plan
}
