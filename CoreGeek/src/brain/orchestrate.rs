//! The unified scheduler (issue #221).
//!
//! The whole plan is organized around the THREE PEOPLE, not around the clock:
//! every round, each controllable unit is dispatched to its role mainline
//! ([`crate::brain::role`]), which walks a single priority chain of guarded
//! actions. `turn.is_day` is an input to those guards and weights — a round
//! budget to spend, a threat parameter — and never a control-flow fork.
//!
//! # Transitional state (phase 1 of the refactor)
//!
//! The entry point moved here so `decide_with` has exactly one callee, but the
//! role mainlines do not exist yet: this function still delegates to the
//! legacy `day::plan` / `night::plan`. The fork below is the LAST one, and it
//! dies in phase 5 when `role::{pioneer, wall_worker, economy_worker}` take
//! over and both legacy files are deleted. Do not add new branches to it.

use super::{day, night, Plan};
use crate::model::Turn;
use crate::state::BotState;

/// Plan one round for the whole team.
pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    // TRANSITIONAL (issue #221 phase 1): delegation, not dispatch. See the
    // module docs — this fork is removed in phase 5.
    if turn.is_day {
        day::plan(turn, state)
    } else {
        night::plan(turn, state)
    }
}
