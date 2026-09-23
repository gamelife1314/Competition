//! The wall worker (Worker A) mainline — issue #221 phase 4.
//!
//! The person comment 1 describes: builds and mans the three weapons, mines
//! the day's stone quota, builds and repairs the wall in one direction with
//! the back four cells left as the permanent entrance, spends only the rounds
//! those duties leave over on ROI mining and selling, and at night operates
//! the weapons — mining after the elimination, outside the robot zones.
//!
//! # Transitional state (phase 4a)
//!
//! This file is a DELEGATION, not yet the spec: [`plan_day`] hands the round
//! to the legacy `worker_day` dispatch (through [`day::plan_worker_legacy`])
//! and reproduces the one backstop branch the legacy `plan_owned` closing loop
//! gave this role. The scheduler calls it BEFORE [`day::plan_roles`], so A's
//! claim priority over the pioneer and B is exactly the legacy order, and the
//! `ctx_owned` roster it forwards is still `[B]`, so every trap veto inside
//! the legacy dispatch counts A the way it always did. Phase 4b replaces the
//! delegation with the comment-1 §5 mainline (L-shape weapon geometry,
//! one-direction wall order, fixed entrance, personal round budgets).
//!
//! The NIGHT stays with the legacy `night::plan_owned` in 4a: A still takes
//! its tower from the `stable_pairs` pairing, and the single-operator L-shape
//! model (design D14-D16) lands with 4b. Owning A at night before that would
//! leave its gun silent — the transitional cost `night::plan_owned`'s own
//! comment names.

use std::collections::HashSet;

use crate::brain::day::{self, RoundCtx};
use crate::brain::Plan;
use crate::model::{Turn, Unit};
use crate::protocol::Pos;
use crate::state::BotState;

/// Plan Worker A's day round.
///
/// `ctx_owned` is the context roster [`day::round_context`] was computed
/// with — the pairing, the gate record and the trap vetoes all read the same
/// list, and in this phase it is still `[B]` alone.
pub(crate) fn plan_day(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    ctx: &RoundCtx,
    ctx_owned: &[i64],
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    day::plan_worker_legacy(turn, state, role, ctx, ctx_owned, claimed, plan);
    // The per-person backstop, identical to the branch the legacy `plan_owned`
    // closing loop ran for this role when every step above declined (its two
    // hold-skip exemptions — task point, altar — are pioneer-only). Declining
    // is fine, standing still is not: issue #13's "0 角色站桩闲置". The
    // fallback ignores `claimed` by design, so running it before the pioneer's
    // dispatch instead of after changes nothing observable.
    if !plan.commands.contains_key(&role.id) {
        if let Some(cmd) = day::fallback_toward_station(turn, role) {
            plan.push(role.id, cmd);
        }
    }
}
