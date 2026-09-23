//! The unified scheduler (issue #221).
//!
//! The whole plan is organized around the THREE PEOPLE, not around the clock:
//! every round, each controllable unit is dispatched to its role mainline
//! ([`crate::brain::role`]), which walks a single priority chain of guarded
//! actions. `turn.is_day` is an input to those guards and weights — a round
//! budget to spend, a threat parameter — and never a control-flow fork.
//!
//! # Transitional state (phase 3 of the refactor)
//!
//! The economy worker (B) is now dispatched by its own mainline
//! ([`role::economy_worker`], comment 1 §5/§6). Worker A and the pioneer are
//! still planned by the legacy `day::plan_owned` / `night::plan_owned`, which
//! run FIRST and skip every id owned by a mainline — so claim priority stays
//! A→B and no unit is ever commanded twice (`Plan::push` is first-wins on top
//! of that). The remaining day/night fork below is the LAST one, and it dies
//! in phase 5 when `role::{pioneer, wall_worker}` take over and both legacy
//! files are deleted. Do not add new branches to it.

use std::collections::HashSet;

use super::role::{self, Roles};
use super::{day, night, Plan};
use crate::model::Turn;
use crate::protocol::Pos;
use crate::state::BotState;

/// Plan one round for the whole team.
pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let roles = Roles::of(turn);
    // Units dispatched by a role mainline this phase — the legacy planners
    // skip them entirely (no wall duty, no shopping errand, no backstop).
    let owned: Vec<i64> = roles.economy_worker.into_iter().collect();
    // Shared reservation set: legacy first, so Worker A keeps claim priority
    // over B on veins and build sites (comment 1's A→B order).
    let mut claimed: HashSet<Pos> = HashSet::new();
    // TRANSITIONAL (issue #221): delegation, not dispatch. Removed in phase 5.
    let mut plan = if turn.is_day {
        day::plan_owned(turn, state, &owned, &mut claimed)
    } else {
        night::plan_owned(turn, state, &owned)
    };
    if let Some(id) = roles.economy_worker {
        role::economy_worker::plan(turn, state, id, &mut claimed, &mut plan);
    }
    plan
}
