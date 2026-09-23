//! Tower siting: which inner-ring cells may hold a weapon, in the owner's
//! build order, and the mannability proof behind each slot (issue #221
//! phase 2c).
//!
//! Pure move out of `day.rs`, unchanged except visibility. Phase 4 replaces
//! the ring-distance siting with the comment-1 L-shape geometry (three guns
//! around one operator stand).

use std::collections::HashSet;

use crate::brain::tower_stand_cells;

use super::build::wall_gate;
use super::geometry::ring_cells;
use crate::model::{chebyshev, footprint_distance, station_footprint, Turn};
use crate::protocol::Pos;
use crate::state::BotState;

/// Desired tower cells (ring at distance 1 from the station footprint),
/// paired with the weapon kind that should stand there. Cells already
/// holding one of our towers, blacklisted cells and non-land are excluded.
///
/// One entry per EMPTY tower slot, in ABSOLUTE line order: the i-th empty slot
/// (0-based among empties) builds `config::TOWER_BUILD_ORDER[existing + i]`,
/// where `existing` is the number of towers already standing. Slot N of
/// `TOWER_CAP` therefore always reads `TOWER_BUILD_ORDER[N]`, whatever is
/// already standing. The list is at most `config::TOWER_CAP - towers standing`
/// long and may be shorter when the configured line runs out (issue #206 §5).
pub fn tower_gaps(turn: &Turn, state: &BotState) -> Vec<(Pos, String)> {
    // Positional slots, one per EMPTY tower slot (issue #206 §5). The old
    // per-kind `have[]` counting could never express the owner's line: it built
    // each kind at most once, so "two missiles" and "all missiles" were both
    // unreachable, and the gatling was forced into the third slot. Now slot i
    // of the empty-slot list builds `config::TOWER_BUILD_ORDER[existing + i]`,
    // so the line is positional: slot N reads index N, whatever is standing.
    let existing = turn.towers().len();
    if existing >= crate::config::TOWER_CAP {
        return Vec::new();
    }
    let empty_slots = crate::config::TOWER_CAP - existing;
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let occupied: HashSet<Pos> = turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    let center = Pos {
        x: turn.width / 2,
        y: turn.height / 2,
    };

    let mut cells: Vec<Pos> = ring_cells(&footprint, 1)
        .into_iter()
        .filter(|pos| turn.is_land(*pos) && !occupied.contains(pos))
        .collect();
    cells.sort_by_key(|pos| (chebyshev(*pos, center), pos.x, pos.y));

    // Cells that a standing weapon must be able to be OPERATED from. Three guns
    // packed onto adjacent ring cells steal each other's only standing room —
    // the middle one then has no adjacent free cell at all, and a weapon nobody
    // can man is a weapon that never fires (issues #12/#13/#14: "炮塔全程无人
    // 操控"). Reserve every existing tower's operating cells up front and refuse
    // a new site that would take one, or that would have fewer than two of its
    // own left once the neighbours are built out.
    let mut reserved: HashSet<Pos> = HashSet::new();
    for tower in turn.towers() {
        reserved.extend(tower_stand_cells(turn, tower.pos));
    }
    // Cell occupancy splits in two for the corridor question. A unit standing
    // somewhere is transient — it will move — but the station, a built tower
    // and a wall are there for good, and only those can strand a corridor cell.
    // Reading a teammate's position as permanent is what let the rocket land
    // on the one cell that cut the base interior in half.
    //
    // The site choice reads the SAME set, and that is what makes it stable. It
    // used to test standing room against `occupied` — every unit's footprint,
    // controllers included — and a site whose only standing cells are the cells
    // the crew happens to be standing on then flips to the next candidate the
    // moment the role walks toward it, and back the round after: measured on
    // the day-1 board, `tower_gaps` alternated between (10,22) and (10,25)
    // every single round from R35 to R48 while the worker walked (9,23) ↔
    // (9,24), which is the two-cell oscillation of §2.4 in its purest form —
    // the site list is a function of the role's position, and the role's
    // position is a function of the site list. The third gun then stood unbuilt
    // with 25 gold in the purse and one free cell on the board.
    let mut permanent: HashSet<Pos> = footprint.iter().copied().collect();
    for tower in turn.towers() {
        permanent.insert(tower.pos);
    }
    permanent.extend(turn.walls().iter().map(|wall| wall.pos));
    let standing: Vec<Pos> = turn.towers().iter().map(|tower| tower.pos).collect();
    let gate = wall_gate(turn, state);
    let mut gaps: Vec<(Pos, String)> = Vec::new();
    let mut used: HashSet<Pos> = HashSet::new();
    for slot in 0..empty_slots {
        // ABSOLUTE indexing (pk616181/pk616182, 2026-09-17): slot i of the
        // empty-slot list reads `TOWER_BUILD_ORDER[existing + i]`, not
        // `TOWER_BUILD_ORDER[i]`. Under the old relative indexing the third
        // slot (existing=2, i=0) re-read index 0 and raised a second rocket
        // instead of the gatling the line names at index 2 — so the close-
        // range swarm defense the owner configured never got built, and 70
        // robots walked through two long-cooldown guns on night 1. Absolute
        // indexing makes the line positional: slot N of TOWER_CAP always
        // reads TOWER_BUILD_ORDER[N], whatever is already standing.
        //
        // The line ran out before the slots did: build what the line names
        // and stop. Deliberately not an error — `["rocket", "railgun"]` with
        // three empty slots is two towers, which is the owner's line.
        let Some(&kind) = crate::config::TOWER_BUILD_ORDER.get(existing + slot) else {
            break;
        };
        let mut taken = permanent.clone();
        taken.extend(used.iter().copied());
        taken.extend(reserved.iter().copied());
        let mut fixed = permanent.clone();
        fixed.extend(used.iter().copied());
        // Operability is a PREFERENCE, never a veto. Two guns on a corner base
        // eat most of the five ring cells it has, and a strict "two standing
        // cells or nothing" test then rejects the third site outright — the
        // base ends the day with two guns while the third slot waits for a
        // cell that will never free up. A gun with a single standing cell still
        // fires; a gun that was never built never does, and the line in
        // `config::TOWER_BUILD_ORDER` only ever gets built by this function. So
        // the strict search runs first and
        // the fallback is exactly the pre-existing test: any free, non-
        // blacklisted ring cell. The fallback can therefore never site FEWER
        // guns than the plain search did.
        //
        // Mannability, by contrast, is a VETO in both passes: a site that
        // strands an existing gun behind the sealed shell is never a site at
        // all. So is a site whose every STAND is someone else's only way home:
        // the corridor between the station and the shell is one cell wide, and
        // at dusk every operator parks at once — a candidate whose operating
        // cells are all corridor articulation points hands one gun a stand and
        // takes the other's away (measured on the day-1 board: the rocket at
        // (9,22) had both stands on the pocket's throat, the gatling's
        // operator was sealed out of (11,22)/(12,23) whatever the arrival
        // order, and the hatch cut a wall the dusk seal then could not reach).
        // One clean stand is enough — the operator parks there, the throat
        // stays open, and every gun keeps its operator.
        let pick = |require_operable: bool| -> Option<Pos> {
            cells.iter().copied().find(|pos| {
                if used.contains(pos)
                    || state.blacklisted_builds.contains(&(*pos, kind.to_string()))
                {
                    return false;
                }
                let mut sealed = fixed.clone();
                sealed.insert(*pos);
                let others: Vec<Pos> = standing
                    .iter()
                    .copied()
                    .chain(used.iter().copied())
                    .collect();
                let guns: Vec<Pos> = others
                    .iter()
                    .copied()
                    .chain(std::iter::once(*pos))
                    .collect();
                if !guns_stay_mannable(turn, gate, &sealed, &footprint, &guns) {
                    return false;
                }
                if !others.is_empty()
                    && !operating_cells(turn, *pos, &taken)
                        .into_iter()
                        .any(|stand| {
                            let mut parked = sealed.clone();
                            parked.insert(stand);
                            guns_stay_mannable(turn, gate, &parked, &footprint, &others)
                        })
                {
                    return false;
                }
                !require_operable
                    || (!reserved.contains(pos)
                        && operating_cells(turn, *pos, &taken).len() >= 2
                        && !strands_corridor(turn, &footprint, &fixed, *pos))
            })
        };
        let site = pick(true).or_else(|| pick(false));
        if let Some(pos) = site {
            used.insert(pos);
            reserved.extend(operating_cells(turn, pos, &taken));
            gaps.push((pos, kind.to_string()));
        }
    }
    gaps
}

/// Would every gun still be MANNABLE once the shell is sealed?
///
/// `tower_gaps`' corridor test reads the board as it is now: a pocket with no
/// role in it costs nothing, so it waves the candidate through. But the crew
/// seals every ring-2 cell at dusk, and a tower whose operating cells connect
/// to the rest of the base only through a ring-2 cell is a tower nobody can
/// man from the day the shell closes over it. Worse, the dynamic safeties then
/// do their job TOO well: `wall_would_trap` vetoes the pocket's last door
/// forever, so the ring can never close, and the crew shuttles between "wall
/// the door" and "let the operator home" for the whole afternoon (measured on
/// the day-1 board with the rocket at (10,22): the gatling's stands
/// {(11,22),(12,23)} ended up enclosed by the station, three towers and the
/// east wall with (10,21) as the only door — 25 rounds of two-cell shuffle,
/// the ring finished at R69).
///
/// So a candidate is judged against the SEALED shell: every ring-2 cell is
/// blocked except the day's gate, and from that gate every gun — the ones
/// standing, the ones already chosen this call, and the candidate itself —
/// must keep at least one operating cell reachable. The gate is the right
/// anchor because it is the one cell guaranteed open until the seal; the open
/// field outside the shell stays walkable (it is never walled), so a gun that
/// can only be manned from outside is still counted as mannable.
fn guns_stay_mannable(
    turn: &Turn,
    gate: Option<Pos>,
    blocked: &HashSet<Pos>,
    footprint: &[Pos],
    guns: &[Pos],
) -> bool {
    // Only guns ON ring-1 (the corridor between station and wall ring) can
    // block each other. A tower far from the station is outside the sealed
    // shell and its controller operates from outside — it never participates
    // in the corridor blocking that stranded tower 20040 in pk613040.
    let ring1_guns: Vec<Pos> = guns
        .iter()
        .copied()
        .filter(|gun| footprint_distance(*gun, footprint) == 1)
        .collect();
    if ring1_guns.is_empty() {
        return true; // no gun in the corridor: nothing to block
    }
    // When the wall ring is not built yet (gate is None), the day's gate is
    // undecided. Test against every ring-2 cell that could become the gate and
    // accept only if at least one keeps every gun mannable — the wall crew
    // chooses the gate, and a placement that works for SOME gate is safe if
    // that gate is the one the crew leaves open.
    let gates: Vec<Pos> = match gate {
        Some(g) => vec![g],
        None => ring_cells(footprint, 2)
            .into_iter()
            .filter(|pos| turn.is_land(*pos) && !blocked.contains(pos))
            .collect(),
    };
    if gates.is_empty() {
        return true; // no ring-2 cell at all: not a verdict this test can make
    }
    // Operating cells (ring-1 neighbours) of each ring-1 gun. At dusk every
    // gun's controller parks on one of these, and a cell with a controller on
    // it is not passable for anyone else. Battle pk613040 lost tower 20040 for
    // the entire night because the station's 2×2 footprint split the ring-1
    // corridor into two halves, the controllers of the two guns in the other
    // half blocked the only two passages, and the third gun's controller was
    // sealed in a corner it could never leave.
    //
    // The check: does there exist an arrival order and stand assignment such
    // that each controller can reach its stand from the gate when only the
    // previously-arrived controllers' stands are blocked?
    let gun_stands: Vec<Vec<Pos>> = ring1_guns
        .iter()
        .map(|gun| {
            crate::model::neighbours(*gun)
                .iter()
                .copied()
                .filter(|cell| {
                    turn.is_land(*cell)
                        && !blocked.contains(cell)
                        && footprint_distance(*cell, footprint) <= 1
                })
                .collect()
        })
        .collect();
    // A gun with no stands at all is never mannable.
    if gun_stands.iter().any(|s| s.is_empty()) {
        return false;
    }
    gates.iter().any(|&gate| {
        // BFS from the gate through walkable cells (not blocked, not ring-2
        // which is sealed at dusk). Each gun needs at least one stand in the
        // reachable set — the operator can walk to it from the gate.
        //
        // This is deliberately simpler than an ordered-arrival assignment
        // search: the `pick` closure already calls `guns_stay_mannable` a
        // second time with the new gun's operator parked at a trial stand,
        // which is the check that catches one operator's stand blocking
        // another's path. Doing the full assignment here too rejects valid
        // sites where the gate connects to ring-1 through a single cell that
        // is also a gun's stand — the station footprint splits the corridor,
        // and the search exhausts itself trying to assign that chokepoint to
        // two guns at once, even though the operators arrive sequentially
        // and the second walks through the first's stand before it is
        // occupied.
        let seen = bfs_reachable(turn, gate, blocked, footprint);
        gun_stands.iter().all(|stands| {
            stands.iter().any(|stand| {
                *stand == gate || seen.contains(stand)
            })
        })
    })
}

/// BFS from `gate` through cells that are walkable inside the sealed shell:
/// land, not in `blocked`, and not at ring-2 distance (sealed at dusk, except
/// the gate itself).
fn bfs_reachable(
    turn: &Turn,
    gate: Pos,
    blocked: &HashSet<Pos>,
    footprint: &[Pos],
) -> HashSet<Pos> {
    let walkable = |cell: Pos| {
        cell == gate
            || (turn.is_land(cell)
                && !blocked.contains(&cell)
                && footprint_distance(cell, footprint) != 2)
    };
    let mut seen: HashSet<Pos> = HashSet::new();
    let mut frontier = vec![gate];
    seen.insert(gate);
    while let Some(cell) = frontier.pop() {
        for next in crate::model::neighbours(cell) {
            if seen.contains(&next) || !walkable(next) {
                continue;
            }
            seen.insert(next);
            frontier.push(next);
        }
    }
    seen
}

/// Would putting a tower on `site` strand a ROLE in the ring-1 corridor?
///
/// The band between the station and the wall is the only walkable corridor
/// inside the base, and a weapon sits on it. Two guns two cells apart leave the
/// cell between them with no ring-1 neighbour at all, and a role standing there
/// can reach nothing for the rest of the day: no gun, no gate, no way out. That
/// is issue #13's "0 角色站桩闲置", and it also stops the ring from ever
/// closing, because `wall_would_trap` sees the stranded role as the reason not
/// to seal.
///
/// The test is about occupants, not geometry: an empty pocket costs nothing
/// (nobody is in it, and anyone who walks in can walk back out the way they
/// came), while vetoing every geometric pocket rejects the third gun's only
/// site from some base positions and leaves the base with two towers.
fn strands_corridor(turn: &Turn, footprint: &[Pos], taken: &HashSet<Pos>, site: Pos) -> bool {
    let band: Vec<Pos> = ring_cells(footprint, 1);
    let free: Vec<Pos> = band
        .iter()
        .copied()
        .filter(|pos| *pos != site && !taken.contains(pos))
        .collect();
    turn.controllable().iter().any(|role| {
        band.contains(&role.pos)
            && !free
                .iter()
                .any(|other| *other != role.pos && chebyshev(*other, role.pos) == 1)
    })
}

/// Cells a weapon standing at `site` could be operated from, treating `taken`
/// as occupied. Mirrors `tower_stand_cells` for a tower that does not exist
/// yet, so a build site can be accepted or rejected before any gold is spent.
fn operating_cells(turn: &Turn, site: Pos, taken: &HashSet<Pos>) -> Vec<Pos> {
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let all: Vec<Pos> = crate::model::neighbours(site)
        .into_iter()
        .filter(|pos| turn.is_land(*pos) && !taken.contains(pos))
        .collect();
    let inner: Vec<Pos> = all
        .iter()
        .copied()
        .filter(|pos| footprint_distance(*pos, &footprint) <= 1)
        .collect();
    if inner.len() >= 2 {
        inner
    } else {
        all
    }
}
