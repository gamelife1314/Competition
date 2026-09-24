//! Walk pricing around the ring.
//!
//! The legacy planner lived here: it chose the day's entrance from the errand
//! set, rotated the wall order around it and sealed the gate at dusk (issue
//! #221 design D17 deletes all of it). Comment 1 §4 replaces the four decisions
//! with fixed geometry — the build order is [`super::action::base_layout::wall_build_order`]
//! and the entrance is the permanent back column, neither of which is priced
//! per day. What remains is the one question the geometry cannot answer: **how
//! many rounds does a walk cost** when a role starts inside a shell that is
//! mostly stone. Everything that paces a day against nightfall — the mine
//! pick, the sell-vs-finish margin, the dusk recall — reads [`trip_rounds`].

use std::collections::HashSet;

use crate::model::{chebyshev, footprint_distance, station_footprint, Turn};
use crate::protocol::Pos;

/// The ring shell the day fortifies: every cell at footprint distance exactly
/// `radius`, in a deterministic (row-major) order.
pub fn ring_cells(turn: &Turn, radius: i32) -> Vec<Pos> {
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let xs: Vec<i32> = footprint.iter().map(|pos| pos.x).collect();
    let ys: Vec<i32> = footprint.iter().map(|pos| pos.y).collect();
    let (xmin, xmax) = (
        *xs.iter().min().unwrap_or(&0),
        *xs.iter().max().unwrap_or(&0),
    );
    let (ymin, ymax) = (
        *ys.iter().min().unwrap_or(&0),
        *ys.iter().max().unwrap_or(&0),
    );
    let mut cells = Vec::new();
    for x in xmin - radius..=xmax + radius {
        for y in ymin - radius..=ymax + radius {
            let pos = Pos { x, y };
            if footprint.contains(&pos) {
                continue;
            }
            if footprint_distance(pos, &footprint) == radius {
                cells.push(pos);
            }
        }
    }
    cells
}

/// The shell cells a role can walk through RIGHT NOW: ring cells that are land
/// and carry no wall of ours.
///
/// This is the honest input to a walk-cost model. A ring priced through a cell
/// that already has stone in it would under-count every errand by the detour.
/// On a closed ring this is exactly the four permanent entrance cells — the
/// only holes that ever exist by design.
pub fn open_ring_cells(turn: &Turn, radius: i32) -> Vec<Pos> {
    let walls: HashSet<Pos> = turn.walls().iter().map(|wall| wall.pos).collect();
    let mut cells: Vec<Pos> = ring_cells(turn, radius)
        .into_iter()
        .filter(|pos| turn.is_land(*pos) && !walls.contains(pos))
        .collect();
    cells.sort_by_key(|pos| (pos.x, pos.y));
    cells
}

/// Rounds to walk from `from` to `to`, priced through the ring.
///
/// A role already outside walks there directly — that is what "outside" means.
/// A role inside a ring whose only openings are its gaps has to leave through
/// one of them, and the cheapest gap is the one on the errand's side: this is
/// the round cost a fixed corner entrance used to hide, and the reason a crew
/// mining to the south once spent the day walking the ring to reach a
/// north-east door. The permanent entrance makes the detour a property of the
/// layout rather than of the day, but a role inside a half-built ring still
/// pays per hole, and this is the number that says how much.
pub fn trip_rounds(turn: &Turn, from: Pos, to: Pos) -> i64 {
    let Some(station) = turn.station() else {
        return chebyshev(from, to) as i64;
    };
    let footprint = station_footprint(station.pos);
    if footprint_distance(from, &footprint) > 1 {
        return chebyshev(from, to) as i64; // already at the wall line or beyond it
    }
    let openings = open_ring_cells(turn, 2);
    if openings.is_empty() {
        // Nothing to price through — a sealed box with a role inside it. Fall
        // back to the straight line rather than inventing a route.
        return chebyshev(from, to) as i64;
    }
    openings
        .iter()
        .map(|gap| (chebyshev(from, *gap) + 1 + chebyshev(*gap, to)) as i64)
        .min()
        .unwrap_or_else(|| chebyshev(from, to) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32, y: i32) -> Pos {
        Pos { x, y }
    }

    fn unit(id: i64, kind: &str, at: Pos) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": kind,
            "health": 1000, "level": 1, "backPackCapability": 100, "backpack": []
        })
    }

    /// The day-1 opening from the day-1 simulation: station at (10,24), two
    /// workers and a pioneer inside, and whatever zones the test asks for.
    fn board(zones: Vec<(i32, i32, &str)>, mut extra_roles: Vec<serde_json::Value>) -> Turn {
        let mut roles = vec![
            unit(10001, "station", pos(10, 24)),
            unit(10002, "worker", pos(13, 24)),
            unit(10003, "worker", pos(14, 25)),
            unit(10004, "pioneer", pos(13, 26)),
        ];
        roles.append(&mut extra_roles);
        let zones: Vec<serde_json::Value> = zones
            .into_iter()
            .map(|(x, y, kind)| serde_json::json!({"pos": {"x": x, "y": y}, "neutralType": kind}))
            .collect();
        let payload = serde_json::json!({
            "roundNo": 5,
            "mapInfo": {"width": 41, "height": 32, "zones": zones},
            "teamOur": {
                "type": "challenger", "goldNum": 75, "totalScore": 0,
                "playerTasks": [], "roles": roles
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
            "vendorShopList": [
                {"name": "stone", "price": 2},
                {"name": "iron", "price": 8},
                {"name": "copper", "price": 12},
            ],
        });
        let req: crate::protocol::Request =
            serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    #[test]
    fn the_radius_two_shell_is_twenty_cells() {
        let turn = board(vec![], vec![]);
        let ring = ring_cells(&turn, 2);
        assert_eq!(ring.len(), 20);
        let footprint = station_footprint(pos(10, 24));
        assert!(ring
            .iter()
            .all(|cell| footprint_distance(*cell, &footprint) == 2));
    }

    #[test]
    fn a_crew_mining_south_does_not_walk_the_whole_ring() {
        // The walk cost, priced. The shell is complete except for ONE opening;
        // a crew inside, heading for a vein due south, must pay the detour to
        // that opening and no more — through an opening on the far side it
        // pays for the whole southward leg twice.
        let mine = pos(11, 28);
        let from = pos(12, 24); // the ring-1 band, inside the shell
        let direct = chebyshev(from, mine) as i64;
        let with_gate = |gate: Pos| {
            let mut roles: Vec<serde_json::Value> = Vec::new();
            let probe = board(vec![], vec![]);
            for (index, cell) in ring_cells(&probe, 2).iter().enumerate() {
                if *cell == gate {
                    continue; // the day's opening
                }
                roles.push(unit(20000 + index as i64, "wall", *cell));
            }
            board(vec![(11, 28, "stone")], roles)
        };
        // The opening on the mine's side: the walk is the straight line plus
        // at most the one-cell sidestep to the gate and back.
        let south = with_gate(pos(12, 26));
        let cheap = trip_rounds(&south, from, mine);
        assert!(
            cheap <= direct + 2,
            "through the south opening the walk is {cheap}, the straight line is {direct}"
        );
        // A north-east corner hole: the same errand pays for the whole
        // southward leg twice — out the wrong side and back.
        let corner = with_gate(pos(13, 22));
        let dear = trip_rounds(&corner, from, mine);
        assert!(
            dear >= direct + 4,
            "through the corner opening the walk is {dear}, the straight line is {direct}"
        );
        assert!(
            cheap < dear,
            "the pricing exists so that {cheap} < {dear} decides the margin"
        );
    }
}
