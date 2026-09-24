//! The unified scheduler (issue #221).
//!
//! The whole plan is organized around the THREE PEOPLE, not around the clock:
//! every round, each controllable unit is dispatched to its role mainline
//! ([`crate::brain::role`]), which walks a single priority chain of guarded
//! actions. `turn.is_day` is an input to those guards and weights — a round
//! budget to spend, a threat parameter — and never a control-flow fork.
//!
//! # Transitional state (phase 5a of the refactor)
//!
//! ALL THREE people are dispatched by their own mainlines, day AND night: the
//! economy worker (B) by [`role::economy_worker`] (comment 1 §5/§6), the wall
//! worker (A) by [`role::wall_worker`] (§4/§5 by day; §3.4's single-operator
//! L-shape at night — `plan_night`, design D14-D16), and the pioneer by
//! [`role::pioneer`] (§3.3's task/buy/repair chain, the fixed buy whitelist of
//! §3, and Q5's night wall-repair duty). Both legacy dispatchers are gone:
//! `day` keeps only the round context and the closing backstop, and `night.rs`
//! is deleted — its pieces live in [`role::pioneer`], [`super::action::fight`]
//! and [`super::shelter`]. Claim order is the legacy one — A, pioneer,
//! backstop, B — and no unit is ever commanded twice (`Plan::push` is
//! first-wins on top of that). The remaining day/night fork below is
//! transitional: it exists only because the wall worker still has two entry
//! points and the backstop is a day institution. Phase 5c gives
//! `role::wall_worker` an internal day/night dispatch, moves the backstop and
//! the context into this file, and deletes `day.rs` — at which point the fork
//! and this comment die together. Do not add new branches to it.

use std::collections::HashSet;

use super::role::{self, Roles};
use super::{day, Plan};
use crate::model::Turn;
use crate::protocol::Pos;
use crate::state::BotState;

/// Plan one round for the whole team.
pub fn plan(turn: &Turn, state: &mut BotState) -> Plan {
    let roles = Roles::of(turn);
    // The CONTEXT roster: units invisible to the round context — pairing and
    // trap vetoes. Only the economy worker: B sleeps outside the ring by
    // design (comment 1 §6), so its position must not veto the last wall.
    let owned: Vec<i64> = roles.economy_worker.into_iter().collect();
    // Shared reservation set: A claims first, then the pioneer, then B — the
    // legacy claim priority, which is comment 1's A→B order.
    let mut claimed: HashSet<Pos> = HashSet::new();
    let mut plan = Plan::default();
    // TRANSITIONAL (issue #221): the day/night fork. Removed in phase 5c.
    if turn.is_day {
        let ctx = day::round_context(turn, state, &owned);
        if let Some(role) = roles.wall_worker.and_then(|id| turn.role_by_id(id)) {
            role::wall_worker::plan_day(turn, state, role, &ctx, &owned, &mut claimed, &mut plan);
        }
        // The team prompt slot: the news/treasure ask is a property of the
        // ROUND — a dead pioneer must not silence it (issue #218) — but it is
        // a DAY property too: the legacy planners only ever asked by day, and
        // the night spends its LLM budget on nothing.
        role::pioneer::plan_prompts(turn, state, &mut plan);
        if let Some(role) = roles.pioneer.and_then(|id| turn.role_by_id(id)) {
            role::pioneer::plan(turn, state, role, &mut claimed, &mut plan);
        }
        // Backstop. Every roster person is skipped — each has a mainline that
        // owns its rounds, including the deliberate holds: the pioneer's
        // counter-hold and task-point wait are its plan, not idleness, and a
        // backstop walk would drag the buyer off the counter (phase 5b).
        let skip: Vec<i64> = [roles.wall_worker, roles.economy_worker, roles.pioneer]
            .into_iter()
            .flatten()
            .collect();
        day::plan_backstop(turn, state, &skip, &mut plan);
    } else {
        // Night, dispatched by PERSON like the day (design D14-D16). A runs
        // the whole L-shape from the operator cell: guns, reload masonry, the
        // swept-board mine (comment 1 §3.4). Everybody the roster does not
        // name — the pioneer first — runs the pioneer's night chain: WallFixer
        // in hand it stands inside the ring and mends, and with the kit spent
        // it shelters (Q5's default; `pioneer_night_backup` is the switch).
        // B never comes home: its mainline below mines all night outside the
        // ring (comment 1 §6). The legacy pairing loop is deleted.
        if let Some(role) = roles.wall_worker.and_then(|id| turn.role_by_id(id)) {
            role::wall_worker::plan_night(turn, state, role, &mut claimed, &mut plan);
        }
        let spares: Vec<i64> = turn
            .controllable()
            .into_iter()
            .map(|role| role.id)
            .filter(|id| Some(*id) != roles.wall_worker && Some(*id) != roles.economy_worker)
            .collect();
        for id in spares {
            if let Some(role) = turn.role_by_id(id) {
                role::pioneer::plan(turn, state, role, &mut claimed, &mut plan);
            }
        }
    }
    if let Some(id) = roles.economy_worker {
        role::economy_worker::plan(turn, state, id, &mut claimed, &mut plan);
    }
    plan
}
