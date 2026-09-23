//! Sell: walk to a vendor and turn the pack into gold (issue #221 phase 2b).
//!
//! Pure move out of `day.rs`, unchanged except visibility. Phase 3 replaces
//! WHEN this fires for the economy worker (the comment-1 trigger set); the
//! executor itself — stand on a vendor cell, sell, log the ledger line —
//! stays.

use std::collections::HashSet;

use crate::brain::{economy, stand_cells, walk_toward};
use crate::model::{Turn, Unit};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// Walk to a vendor and sell the most valuable ore stack.
pub(crate) fn sell_flow(
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
