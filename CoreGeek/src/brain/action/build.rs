//! Build: the command-producing wall/tower executors — place-or-walk and wall
//! repair (issue #221 phase 2c).
//!
//! Pure move out of `day.rs`, unchanged except visibility. Phase 4b deleted
//! the gate machinery that used to live here (`wall_gate`/`update_wall_gate`/
//! `open_door`/`gate_clear_stands`, design D17): comment 1 §4's permanent
//! back-four entrance is never built, so there is no gate to latch, no dusk
//! seal to time and no door to re-cut every morning — and with no door cell,
//! repair ranks purely by the wall line's own gaps.

use std::collections::HashSet;

use crate::brain::{stand_cells, walk_toward};
use crate::model::{chebyshev, Turn, Unit};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

pub(crate) fn build_or_walk(
    turn: &Turn,
    role: &Unit,
    target: Pos,
    name: &str,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if chebyshev(role.pos, target) == 1 && turn.is_land(target) {
        return Some(RoleCommand::build(target, name));
    }
    let stands = stand_cells(turn, target);
    walk_toward(turn, role, &stands, claimed)
}

/// The wall this role should mend first, if any.
///
/// The rank puts the walls holding an opening first — a gap the ring has not
/// filled yet. That is where the line is already broken and where the next
/// robot comes through; a wall at 300 HP beside a gap is a breach waiting for
/// the night, while the same wall on a closed stretch can wait a round. Then
/// the weakest wall, then the nearest.
///
/// `_state` is vestigial: the rank used to also put the daytime door's
/// neighbours first, and the door died with the gate (D17). The parameter
/// stays so the public shape `wall_repair`'s callers use is untouched.
pub fn repair_target(turn: &Turn, _state: &BotState, role: &Unit, wall_gaps: &[Pos]) -> Option<Pos> {
    let beside_opening =
        |pos: Pos| wall_gaps.iter().any(|gap| chebyshev(*gap, pos) <= 1);
    turn.walls()
        .into_iter()
        .filter(|wall| wall.health < crate::brain::combat::wall_max_hp(wall.level))
        .min_by_key(|wall| {
            (
                !beside_opening(wall.pos),
                wall.health,
                chebyshev(role.pos, wall.pos),
                wall.pos.x,
                wall.pos.y,
            )
        })
        .map(|wall| wall.pos)
}

/// Walk to the wall that most needs mending and patch it.
///
/// Issue #16: the ring was rebuilt but never repaired. `WallFixer` costs 10
/// gold and restores its target to FULL HP (任务书: "目标坐标所在围墙回满血"),
/// which is by far the cheapest HP on the board — cheaper than a wall upgrade
/// voucher (20 gold for +500) and cheaper than a whole new wall. The kit was in
/// the shopping list, but the repair itself only ever fired for a role that
/// happened to be standing next to a damaged wall at the end of the day, and
/// mining returns several steps earlier, so a worker with stone to dig never
/// got there. Repair is therefore an ERRAND: pick the worst wall, walk to it,
/// mend it.
pub(crate) fn repair_flow(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    wall_gaps: &[Pos],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if role.count_item("WallFixer") == 0 {
        return None;
    }
    let target = repair_target(turn, state, role, wall_gaps)?;
    if chebyshev(role.pos, target) == 1 {
        return Some(RoleCommand::use_item_at("WallFixer", target));
    }
    // Walk to the wall's inner side: the ring's outside is where the robots
    // are, and a worker mending from out there is a worker the night can pick
    // off. `stand_cells` may offer cell that is itself a wall (the neighbouring
    // ring cells); `walk_toward` drops those, since walls block movement.
    let stands = stand_cells(turn, target);
    walk_toward(turn, role, &stands, claimed)
}
