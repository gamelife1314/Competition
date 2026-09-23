//! Mine: pick a vein, walk to it, dig — the shared executor both worker
//! mainlines call (issue #221 phase 2b).
//!
//! Pure move out of `day.rs`, unchanged except visibility. Phase 3/4 will
//! replace the stone-demand plumbing with the economy worker's single-vein
//! latch and the wall worker's quota arithmetic; until then `day::plan`
//! keeps calling in with the same arguments.

use std::collections::HashSet;

use crate::brain::day::{gun_deadline, MIN_WALL_LOAD};
use crate::brain::{economy, stand_cells, walk_toward};
use crate::model::{chebyshev, Turn, Unit, STONE};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

/// Can a stone trip to `vein` still land a load before this role has to be at
/// its gun? Walk out, dig `MIN_WALL_LOAD`, walk back to the wall line: a vein
/// further than that is not a wall this day, it is a walk into the dusk.
/// Issue #17: "我方仅 14 次 collect，金币峰值仅 75（初始值）… 城墙体系彻底缺失
/// （0 墙）" — the crew crossed the map for stone it could not bring home, and
/// the day produced neither a wall nor a coin.
fn stone_trip_fits(
    turn: &Turn,
    role: &Unit,
    pairs: &[(i64, i64)],
    vein: Pos,
    gaps: &[Pos],
) -> bool {
    let back = gaps
        .iter()
        .map(|gap| chebyshev(vein, *gap) as i64)
        .min()
        .unwrap_or(0);
    let rounds = chebyshev(role.pos, vein) as i64 + MIN_WALL_LOAD + back + 1;
    rounds <= gun_deadline(turn, role, pairs) - turn.in_day_round
}

/// Does the team already carry the stone the day owes the ring?
///
/// THE one bound on the stone-first rule. `stone_demand` is what the day still
/// owes — the gaps, the doors the economy cut, and the gate's own stone — and
/// stone is dug while the TEAM is short of that number, not one fetch longer.
/// The bound is what keeps "dig stone first" from becoming "dig stone all day":
/// it is asked by `mine_flow`'s `want_stone` (who digs) and by `plan`'s
/// `shared_wall_duty` (who is still tied to the wall line), and a team with the
/// ring's stone in its packs answers yes to both at once. Two workers splitting
/// a 20-cell ring hold 10 each and neither pack ever reaches a batch, so the
/// test is the TEAM's stone and never one pack's.
pub(crate) fn stone_covered(turn: &Turn, stone_demand: i64) -> bool {
    stone_demand <= 0 || economy::team_ores(turn, STONE) >= stone_demand
}

/// Walk to the nearest mine and collect. Stone is preferred while the wall
/// line still needs the load this role is carrying; distance always beats ore
/// value.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mine_flow(
    turn: &Turn,
    state: &BotState,
    role: &Unit,
    stone_demand: i64,
    keep_gold_loop: bool,
    pairs: &[(i64, i64)],
    wall_gaps: &[Pos],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    // Fetch stone until the TEAM holds everything the dusk still owes (the
    // gaps, the doors, the gate — `stone_demand`). The build step's floor keeps
    // the pool at exactly that level, so the sweep can end with the gate's
    // stone in hand and no extra fetch scheduled after it — the one fetch
    // that never fits the pre-position lock. `stone_demand` is the gap count,
    // not this role's shortfall: two workers splitting a 20-cell ring hold 10
    // each, so a per-pack test keeps both of them digging stone long after the
    // ring has all the stone it can use, and the sellable ore that funds the
    // rest of the day never gets mined.
    //
    // The dedicated economy worker never joins that queue outside day 1: it is
    // the role that has to keep carrying ore the vendor will buy, and stone is
    // the one ore the vendor is refused. See
    // `economy::choose_sellable_mine` for the freeze that cost issue #18 the
    // whole match.
    let want_stone = !keep_gold_loop && !stone_covered(turn, stone_demand);
    let pick = if keep_gold_loop {
        economy::choose_sellable_mine(turn, state, role, claimed)?
    } else {
        economy::choose_mine(
            turn,
            state,
            role,
            if want_stone { stone_demand } else { 0 },
            claimed,
        )?
    };
    // Stone is what the ring wants, but only a trip that can carry a load home
    // before this role must be at its gun is a wall trip. When no vein is that
    // close, the demand is not "stone" any more — it is an unwinnable walk, and
    // the day is worth more spent on ore that sells (issue #17's frozen purse).
    let stone_errand = want_stone && pick.1 == STONE;
    let (mine, ore) = if stone_errand && !stone_trip_fits(turn, role, pairs, pick.0, wall_gaps) {
        economy::choose_mine(turn, state, role, 0, claimed)?
    } else {
        pick
    };
    // On a stone errand the trip is the point: an ore picked up on the way
    // there is a detour that fills the pack with the wrong load, and the stone
    // is never reached. Everywhere else the pass-by mine is free money — and
    // that includes the day when the ring's stone turned out to be out of
    // reach, because the errand is ore that sells by then, not stone.
    let strict = stone_errand && ore == STONE;
    // Collect an adjacent mine of the preferred ore directly. A mine a
    // teammate has merely RESERVED as a walking target is still collectable by
    // a role already standing next to it: the reservation decides who walks
    // there, not who may dig. Without the second look the role stands on the
    // ore with an empty pack, refuses to collect because someone else claimed
    // the cell, cannot walk to it either (it is already on a stand, so the
    // path search returns "no step"), and produces no command at all for the
    // rest of the day — issue #13's idle role, caused by a claim.
    //
    // While the ring is still short of stone that courtesy stops at stone.
    // `nearest_adjacent_mine` also accepts a vein of ANY ore, which is right
    // for the general miner — a windfall is a windfall — but on the stone
    // errand it is how the wall line loses its day: with the stone 22 cells out
    // (issue #17) every carrier that brushed the iron on the way stopped there
    // and mined four ore instead, twice over, and the day ended 8 collects and
    // 0 walls with gold frozen at the 75 it started with. We came for stone;
    // the ore under our feet can wait for the walk home.
    let adjacent = if strict {
        adjacent_mine_of(turn, role, STONE)
    } else {
        nearest_adjacent_mine(turn, role, &ore, claimed)
            .or_else(|| nearest_adjacent_mine(turn, role, &ore, &HashSet::new()))
    };
    if let Some(mine_pos) = adjacent {
        claimed.insert(mine_pos);
        return Some(RoleCommand::collect(mine_pos));
    }
    // Which vein the day is being spent on, and why. The wall line is the one
    // errand whose failure is invisible until the night (issues #12-#17 all
    // opened with "0 墙"), so the pick, the ring's demand and whether the trip
    // can still be paid for in walls are worth a line in the log.
    crate::log::event(
        "mine_pick",
        serde_json::json!({
            "round": turn.round_no,
            "role": role.id,
            "wantStone": want_stone,
            "stoneErrand": stone_errand,
            "ore": ore,
            "mine": mine,
        }),
    );
    let stands = stand_cells(turn, mine);
    let cmd = walk_toward(turn, role, &stands, claimed)?;
    claimed.insert(mine);
    Some(cmd)
}

/// A mine of the preferred ore (or any valuable ore) we already stand next to.
/// A mine of exactly `ore` this role is already standing next to.
///
/// The narrow sibling of `nearest_adjacent_mine`, for the errands where the
/// wrong ore is not a windfall but a lost day.
fn adjacent_mine_of(turn: &Turn, role: &Unit, ore: &str) -> Option<Pos> {
    turn.all_mines()
        .into_iter()
        .filter(|(pos, kind)| kind == ore && chebyshev(role.pos, *pos) == 1)
        .map(|(pos, _kind)| pos)
        .min_by_key(|pos| (pos.x, pos.y))
}

fn nearest_adjacent_mine(
    turn: &Turn,
    role: &Unit,
    preferred_ore: &str,
    claimed: &HashSet<Pos>,
) -> Option<Pos> {
    let mut best: Option<(i64, Pos)> = None;
    for (pos, ore) in turn.all_mines() {
        if claimed.contains(&pos) || chebyshev(role.pos, pos) != 1 {
            continue;
        }
        let value = if ore == preferred_ore { 0 } else { 1 };
        if best.map(|(v, _)| value < v).unwrap_or(true) {
            best = Some((value, pos));
        }
    }
    best.map(|(_, pos)| pos)
}
