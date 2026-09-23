//! Build: the command-producing wall/tower executors — place-or-walk, the
//! gate machinery, door cutting and wall repair (issue #221 phase 2c).
//!
//! Pure move out of `day.rs`, unchanged except visibility. Phase 4 deletes
//! the gate latch (`wall_gate`/`update_wall_gate`/`open_door`, D17) in favor
//! of the fixed back-4 entrance.

use std::collections::HashSet;

use crate::brain::day::{night_goal, outside_cells, HARD_SEAL_ROUND};
use crate::brain::route;
use crate::brain::{economy, stand_cells, walk_toward};
use crate::model::{chebyshev, footprint_distance, station_footprint, Turn, Unit};
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

/// The post a role locks to at dusk must not be the gate's own throat.
///
/// The entrance's ring-1 neighbours are the only way back in once the shell is
/// up, and a role parked on the wrong one turns the gate into a cul-de-sac:
/// the towers face the work side by design, so the gate's approach cells are
/// exactly the cells the gun operators pre-position onto. The naive version of
/// this rule avoided every cell adjacent to the gate — and made it worse: the
/// one approach that is a dead-end STUB is the best parking on the board
/// (a role there blocks nothing), and with it filtered out the pioneer was
/// pushed onto the through-cell, jamming the stone carrier's last walk of the
/// day (the day-2 board: eleven stone in hand, two open cells, no route).
///
/// So the rule is precise: a stand is refused only when parking on it leaves
/// the gate with NO usable approach — no other ring-1 neighbour of the gate
/// that the crew can still reach from inside without passing through this
/// stand. Stub approaches pass the test (parking there costs nothing), as does
/// every stand that is not the gate's neighbour at all. When every stand a
/// tower has is a throat — a corner base is like that — the post stands: a
/// manned gun still outranks a clear lane, and `gate_open_record` counts
/// either as home.
pub(crate) fn gate_clear_stands(turn: &Turn, state: &BotState, stands: Vec<Pos>) -> Vec<Pos> {
    let clear: Vec<Pos> = stands
        .iter()
        .copied()
        .filter(|stand| !parks_on_gate_throat(turn, state, *stand))
        .collect();
    if clear.is_empty() {
        stands
    } else {
        clear
    }
}

/// Would parking on `stand` close the gate's last usable approach?
///
/// Simulates the stand as occupied and asks whether any OTHER interior
/// neighbour of the gate is still reachable from the inside band. The gate's
/// through-cells are the ones the stone carrier walks at dusk; a stand that
/// swallows the last of them is not a post, it is a cork.
fn parks_on_gate_throat(turn: &Turn, state: &BotState, stand: Pos) -> bool {
    let Some(gate) = wall_gate(turn, state) else {
        return false;
    };
    if chebyshev(stand, gate) != 1 {
        return false; // not on the gate's approach at all: cannot cork it
    }
    let Some(station) = turn.station() else {
        return false;
    };
    let footprint = station_footprint(station.pos);
    // The gate's remaining approach: its interior neighbours minus this stand,
    // minus anything permanently occupied.
    let approach: Vec<Pos> = crate::model::neighbours(gate)
        .into_iter()
        .filter(|pos| *pos != stand)
        .filter(|pos| turn.is_land(*pos) && footprint_distance(*pos, &footprint) == 1)
        .filter(|pos| {
            !turn
                .ours
                .iter()
                .any(|unit| unit.kind != crate::model::UnitKind::Wall
                    && unit.footprint().contains(pos))
        })
        .collect();
    if approach.is_empty() {
        return true; // this stand is the gate's only approach: a cork
    }
    // BFS from the remaining approach cells themselves, through inside cells
    // and the gate, with `stand` blocked. If the walk reaches any interior
    // cell BEYOND the seeds, the gate still has a through-path and parking
    // here costs nothing; if it does not, every remaining approach is a dead
    // stub cut off by this stand, and the stand is the cork. Seeding from the
    // whole band instead would mark every interior cell reachable by
    // definition — the test then never fires, which is exactly what the first
    // version did (measured: the pioneer parked on the gate's through-cell
    // anyway, and the carrier's seal walk found no route).
    let mut blocked = turn.blocked_for(-1);
    blocked.insert(stand);
    let interior: HashSet<Pos> = crate::brain::interior_cells(turn).into_iter().collect();
    let mut seen: HashSet<Pos> = approach.iter().copied().collect();
    let mut frontier: Vec<Pos> = approach.clone();
    while let Some(cell) = frontier.pop() {
        for next in crate::model::neighbours(cell) {
            if seen.contains(&next) || blocked.contains(&next) || !turn.is_land(next) {
                continue;
            }
            if next != gate && footprint_distance(next, &footprint) > 1 {
                continue; // stay inside the shell (the gate itself is allowed)
            }
            seen.insert(next);
            frontier.push(next);
        }
    }
    !seen
        .iter()
        .any(|cell| interior.contains(cell) && !approach.contains(cell))
}

/// Cut a door in our own wall line.
///
/// The ring is a closed box, and everything the economy runs on is outside it.
/// While the wall crew is still building, the ring has gaps and the question
/// never comes up; the moment the last cell is filled, every role inside is
/// walled away from the ore with an empty pack and the whole day produces no
/// command at all. That is issue #14 — "R18 gold 105, then 216 rounds
/// untouched" — in its final form, and it repeats every day after the ring
/// closes.
///
/// So a role that is inside, has an errand outside and can reach neither cuts
/// its own way out. The opening is recorded in `door_cells` and `wall_gaps`
/// stops offering it for the rest of the day, or the wall crew would re-seal it
/// under the digger's feet every round (demolish, rebuild, demolish) and the
/// day would be spent oscillating over one cell. At dusk `door_cells` stops
/// being honoured, the cell becomes an ordinary gap again and the seal crew
/// fills it: the door is a daytime fixture, the ring must be closed at night.
pub(crate) fn open_door(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    // Past dusk the wall line is closing, not opening.
    if turn.in_day_round >= economy::DUSK_ROUND || turn.walls().is_empty() {
        return None;
    }
    // Day 1 is the fortification day. The ring goes up from the outside in and
    // the crew walks in and out of the gaps it leaves; a door cut into that
    // line is a hole the day does not get back (measured: five cells lost, the
    // ring finished at R32 and burrowed through at R48-R57). From day 2 the
    // ring stands and the economy has to live behind it.
    if turn.day <= 1 {
        return None;
    }
    let station = turn.station()?;
    let footprint = station.footprint();
    let outside = outside_cells(turn, &footprint);
    // Sealed in: inside the ring, with no route to the open map. Asked of a
    // role by its own id, because the crew's reservations are `blocked_for`'s
    // business and someone else's claim is not this role's wall.
    let sealed_in = |who: &Unit| {
        let blocked = turn.blocked_for(who.id);
        crate::path::step_toward_stands(turn, who.pos, &outside, &blocked).is_none()
    };
    // The rescue. A way out already exists for this worker — the ring is
    // incomplete, the door is open, or it was never trapped — so it has nothing
    // to cut for itself. But the PIONEER cannot cut for itself at all: 任务书
    // 4.4 gives `remove` to 工人 only and `validate.rs` drops anyone else's, so
    // a ring that closes over the pioneer is a cage it leaves only if someone
    // comes for it — 「除非有人来救否则就出不去了」. The crew IS the rescue, and
    // the worker standing outside the line cuts the same ring from its own
    // side. So the worker with the errand outside is the one who acts, and the
    // wall it opens is the pioneer's door.
    let rescue = turn.pioneer().is_some_and(|pioneer| {
        footprint_distance(pioneer.pos, &footprint) <= 1 && sealed_in(pioneer)
    });
    // Already outside, or standing on the wall line itself: nothing to cut —
    // unless this worker is the one going for the pioneer.
    if footprint_distance(role.pos, &footprint) > 1 && !rescue {
        return None;
    }
    // Never demolish a wall we do not have to: this worker cuts for itself when
    // it is the trapped one, and for the pioneer when the pioneer is.
    if !sealed_in(role) && !rescue {
        return None;
    }
    // One door per day, and one is enough. A teammate's cut from THIS round is
    // invisible in the turn's occupancy (the wall only comes down when the
    // judger applies the command), so without this check every trapped role
    // cuts its own hole in the same round — two stones spent, two cells to
    // re-seal at dusk (measured in the day-2 simulation: two `door_open`
    // events on the same round).
    if !state.door_cells.is_empty() {
        return None;
    }
    let gate = wall_gate(turn, state);
    // THE DOOR IS THE GATE. When the gate still has its wall, cutting it is
    // worth a walk: anything else puts the day's hole on the side the role
    // happened to park on, and every errand of the day pays the ring for it —
    // measured on the day-2 board: nobody stood beside the gate at daybreak,
    // the crew cut the west wall (8,22) instead, the vendor sat fourteen
    // cells east of the hole, and the seller reached it the round its gun
    // deadline fired — yanked home without selling, day 2 frozen. So when the
    // gate is walled the role walks to its inner side first and cuts it next
    // round; the adjacent fallback below is for a gate already open or one no
    // inside role can stand beside.
    if let Some(gate) = gate {
        let gate_walled = turn.walls().iter().any(|wall| wall.pos == gate);
        if gate_walled {
            if chebyshev(role.pos, gate) == 1 {
                if !claimed.insert(gate) {
                    return None; // a teammate is already cutting this round
                }
                state.door_cells.insert(gate);
                crate::log::event(
                    "door_open",
                    serde_json::json!({
                        "round": turn.round_no,
                        "role": role.id,
                        "target": gate,
                        "gate": true,
                    }),
                );
                return Some(RoleCommand::remove(gate));
            }
            let inner: Vec<Pos> = stand_cells(turn, gate)
                .into_iter()
                .filter(|pos| footprint_distance(*pos, &footprint) <= 1)
                .collect();
            let inner = if inner.is_empty() {
                stand_cells(turn, gate)
            } else {
                inner
            };
            if let Some(cmd) = walk_toward(turn, role, &inner, claimed) {
                return Some(cmd);
            }
            // No walkable way to the gate's inner side: fall through to the
            // nearest-adjacent cut rather than stay sealed in.
        }
    }
    let order = match gate {
        Some(gate) => route::build_order(turn, state, gate),
        None => Vec::new(),
    };
    let target = turn
        .walls()
        .into_iter()
        .map(|wall| wall.pos)
        .filter(|pos| chebyshev(role.pos, *pos) == 1)
        .filter(|pos| footprint_distance(*pos, &footprint) == 2)
        .min_by_key(|pos| {
            (
                route::build_rank(&order, *pos),
                gate == Some(*pos),
                pos.x,
                pos.y,
            )
        })?;
    if !claimed.insert(target) {
        return None; // a teammate is already cutting this round
    }
    state.door_cells.insert(target);
    crate::log::event(
        "door_open",
        serde_json::json!({
            "round": turn.round_no,
            "role": role.id,
            "target": target,
            "gate": gate == Some(target),
        }),
    );
    Some(RoleCommand::remove(target))
}

/// The wall this role should mend first, if any.
///
/// The rank puts the walls holding an opening first — a gap the ring has not
/// filled yet, or the daytime door. That is where the line is already broken
/// and where the next robot comes through; a wall at 300 HP beside a gap is a
/// breach waiting for the night, while the same wall on a closed stretch can
/// wait a round. Then the weakest wall, then the nearest.
pub fn repair_target(turn: &Turn, state: &BotState, role: &Unit, wall_gaps: &[Pos]) -> Option<Pos> {
    let beside_opening = |pos: Pos| {
        state
            .door_cells
            .iter()
            .any(|door| chebyshev(*door, pos) <= 1)
            || wall_gaps.iter().any(|gap| chebyshev(*gap, pos) <= 1)
    };
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

/// The ring cell this day leaves open.
///
/// Since the joint planner landed this is `brain::route`'s entrance — the cell
/// that minimises the day's weighted walk to its errands, latched at daybreak
/// in `BotState::gate_cell` — with the historical fixed cell
/// (`xmax + 2, ymin - 1`) as the fallback for a board the planner has no
/// evidence about.
pub(crate) fn wall_gate(turn: &Turn, state: &BotState) -> Option<Pos> {
    crate::brain::route::gate_of(turn, state)
}

pub(crate) fn update_wall_gate(turn: &Turn, state: &mut BotState, pairs: &[(i64, i64)]) {
    // The seal checkpoint is dusk on EVERY day, not just the first. The ring is
    // re-opened each morning by `open_door` (the ore is outside it), so without
    // a daily seal the wall line the crew spends the day rebuilding is a wall
    // line with a hole in it.
    if state.wall_gate_sealed || turn.in_day_round < economy::DUSK_ROUND {
        return;
    }
    let Some(station) = turn.station() else {
        return;
    };
    let footprint = station.footprint();
    let open = gate_open_record(turn, pairs, &footprint);
    if let Some(record) = &open {
        // THE HARD DEADLINE. Up to [`HARD_SEAL_ROUND`] a straggler still holds
        // the ring open, which is the whole point of waiting — the crew walks
        // in and the seal costs nobody. Past it the trade inverts: the day has
        // three rounds left, the night planner never revisits this flag, and a
        // ring that is still open at nightfall stays open for the night. See
        // [`HARD_SEAL_ROUND`] for the measurement across issues #121-#125.
        if turn.in_day_round < HARD_SEAL_ROUND {
            crate::log::event("wall_gate_open", record.clone());
            return;
        }
        // Who the deadline overrode, in the same two lists `wall_gate_open`
        // carries, so the next batch can tell "the seal was late" from "the
        // seal was forced" without re-deriving it from the positions.
        crate::log::event(
            "wall_gate_forced",
            serde_json::json!({
                "round": turn.round_no,
                "dayRound": turn.in_day_round,
                "away": record["away"].clone(),
                "stuck": record["stuck"].clone(),
            }),
        );
    }
    state.wall_gate_sealed = true;
    crate::log::event(
        "wall_gate_seal",
        serde_json::json!({
            "round": turn.round_no,
            "dayRound": turn.in_day_round,
            "reason": if open.is_some() { "deadline" } else { "all_home" },
        }),
    );
}

/// The `wall_gate_open` record — who the dusk seal is still waiting on, or
/// `None` when nobody is and the gate may close.
///
/// The decision and the record are the same computation on purpose. The record
/// used to name the class ("controllers_not_retreated") and never the culprit,
/// so the fifteen open rounds of the dusk window — which is the whole window,
/// i.e. a gate that never sealed at all — could not be attributed from the log
/// to anyone. That question cost issue #15 a match and a half, and its answer
/// was "the pioneer is standing at a task point", which is only visible if the
/// record carries positions.
///
/// `away` is `[id, x, y]` for every controllable role that is neither home nor
/// on the operating cells of the tower it will man tonight; `stuck` names the
/// subset that cannot walk to its post at all. The two are different failures:
/// a role on its way in is a gate that seals a round or two later, while a role
/// walled off from its gun is a gate that never seals, and it is the case the
/// wall crew is supposed to make impossible (`wall_would_trap`). Both lists are
/// empty when there is nothing to say, and `log::event` prunes the empty one
/// out, so a round with a single role walking home costs one short line.
///
/// `footprint` is passed in rather than looked up because a missing station is
/// not "everyone is home": the caller has already decided what to do about a
/// base that is gone, and this function must not answer it by accident.
pub fn gate_open_record(
    turn: &Turn,
    pairs: &[(i64, i64)],
    footprint: &[Pos],
) -> Option<serde_json::Value> {
    let mut away: Vec<serde_json::Value> = Vec::new();
    let mut stuck: Vec<i64> = Vec::new();
    for role in turn.controllable() {
        if footprint_distance(role.pos, footprint) <= 1 {
            continue; // home: this one is not what the seal is waiting on
        }
        let stands = night_goal(turn, pairs, role.id);
        if stands
            .as_ref()
            .map_or(false, |stands| stands.contains(&role.pos))
        {
            continue; // already on the operating cells of the tower it mans
        }
        away.push(serde_json::json!([role.id, role.pos.x, role.pos.y]));
        if stands.map_or(false, |stands| !crate::brain::can_reach_any(turn, role, &stands)) {
            stuck.push(role.id);
        }
    }
    if away.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "round": turn.round_no,
        "away": away,
        "stuck": stuck,
    }))
}
