//! The base's fixed layout — comment 1 §1 (the L-shape weapon nest) and §4
//! (the one-direction wall order with its permanent entrance), issue #221
//! phase 4b.
//!
//! Everything here is a pure function of the station and the map: no state, no
//! latching, no per-round choice. The legacy planner picked an entrance from
//! the day's errands (`route::entrance`), rotated the build order to it
//! (`route::build_order`) and proved each weapon site mannable against that
//! gate (`guns_stay_mannable`). Comment 1 replaces all three decisions with
//! FIXED geometry:
//!
//! * the three weapons stand in an **L around one operator cell** — the
//!   operator runs all three (design D14-D16), and every site is within
//!   Chebyshev 1 of the operator stand by construction;
//! * the wall goes up in **one direction**: the enemy-facing front column
//!   first, then the top row, then the bottom row;
//! * the back four cells are the **permanent entrance** — never built, so no
//!   gate latch, no dusk seal and no door-cutting machinery is needed to keep
//!   the base reachable (design D17).
//!
//! # Corners
//!
//! 任务书 line 26: the two player bases sit at the map's top-left and
//! bottom-right corners. Both layouts are the SAME offsets off the station
//! footprint's min corner, with the bottom-right base mirrored point-wise
//! through the footprint's centre — `(dx, dy) → (1 - dx, 1 - dy)` — which maps
//! the L and the wall order onto the mirrored corner exactly (the spec's own
//! bottom-right coordinates, verified cell by cell). Our station is the
//! top-left one in the live game ((10,24) on a 41-wide map); the mirror exists
//! so the geometry stays correct if the spawn is ever the other corner.
//!
//! Offsets are relative to `fp_min` — the footprint's `(min x, min y)` — and
//! use the game's axes (y grows "up" the map): the front column is the `x+3`
//! side, which faces the map centre and therefore the enemy, from either
//! corner.

use crate::model::{station_footprint, Turn};
use crate::protocol::Pos;

/// Which map corner our station occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseCorner {
    /// Our base is the top-left one; the enemy is to the +x/+y (map centre).
    TopLeft,
    /// Our base is the bottom-right one; every offset is point-mirrored.
    BottomRight,
}

/// The corner our station occupies: the half of the map its x sits in. The
/// two spawns are far from the centre line ((10 vs 38 on a 41-wide map), so x
/// alone decides it; a station exactly on the line counts as top-left.
pub fn base_corner(turn: &Turn) -> BaseCorner {
    match turn.station() {
        Some(station) if station.pos.x * 2 >= turn.width => BaseCorner::BottomRight,
        _ => BaseCorner::TopLeft,
    }
}

/// The station footprint's min corner `(min x, min y)` — the anchor every
/// offset below is relative to.
pub fn fp_min(turn: &Turn) -> Option<Pos> {
    let station = turn.station()?;
    let footprint = station_footprint(station.pos);
    let x = footprint.iter().map(|pos| pos.x).min()?;
    let y = footprint.iter().map(|pos| pos.y).min()?;
    Some(Pos { x, y })
}

/// Lay footprint-relative offsets out on the board, mirroring them for a
/// bottom-right base.
fn lay(turn: &Turn, offsets: &[(i32, i32)]) -> Vec<Pos> {
    let Some(min) = fp_min(turn) else {
        return Vec::new();
    };
    let mirrored = base_corner(turn) == BaseCorner::BottomRight;
    offsets
        .iter()
        .map(|&(dx, dy)| {
            let (dx, dy) = if mirrored { (1 - dx, 1 - dy) } else { (dx, dy) };
            Pos {
                x: min.x + dx,
                y: min.y + dy,
            }
        })
        .collect()
}

/// The three weapon sites of the L (comment 1 §1, option a), in build order:
/// the two cells of the L's top arm and the one below the operator. Every site
/// is a ring-1 corridor cell (footprint distance 1) and within Chebyshev 1 of
/// [`operator_cell`] — the property the whole single-operator night model
/// (design D14-D16) rests on, pinned by the unit tests below.
pub fn weapon_sites(turn: &Turn) -> Vec<Pos> {
    lay(turn, &[(-1, -1), (0, -1), (-1, 1)])
}

/// The cell the weapon operator stands on at night (comment 1 §1): the L's
/// inner corner, adjacent to all three sites and one step from the permanent
/// entrance column.
pub fn operator_cell(turn: &Turn) -> Option<Pos> {
    lay(turn, &[(-1, 0)]).pop()
}

/// The wall build order of comment 1 §4 — 16 of the ring's 20 cells, laid in
/// ONE direction: the enemy-facing front column first (「先建朝向敌人的一面」),
/// then the top row walking back toward the entrance, then the bottom row
/// walking out from it. A day that runs out of stone or rounds leaves the
/// order's TAIL open, which is the far side away from the robots. The four
/// [`entrance_cells`] are never in this list.
pub fn wall_build_order(turn: &Turn) -> Vec<Pos> {
    lay(
        turn,
        &[
            // Front column x+3, y-2 → y+3 (6 cells, enemy-facing).
            (3, -2),
            (3, -1),
            (3, 0),
            (3, 1),
            (3, 2),
            (3, 3),
            // Top row y+3, x+2 → x-2 (5 cells, walking back to the entrance).
            (2, 3),
            (1, 3),
            (0, 3),
            (-1, 3),
            (-2, 3),
            // Bottom row y-2, x-2 → x+2 (5 cells, walking out from it).
            (-2, -2),
            (-1, -2),
            (0, -2),
            (1, -2),
            (2, -2),
        ],
    )
}

/// The permanent entrance (comment 1 §4: 「后边四个格子留作永久入口」): the four
/// back-column cells that are NEVER built on. With the ring otherwise closed
/// this is the crew's only way out to the ore, the vendor and the shop — the
/// structural answer to issue #14's sealed-in economy, which the legacy
/// `open_door`/dusk-seal machinery approximated day by day.
pub fn entrance_cells(turn: &Turn) -> Vec<Pos> {
    lay(turn, &[(-2, -1), (-2, 0), (-2, 1), (-2, 2)])
}

/// The enemy-facing FRONT column of the ring — the first six cells of
/// [`wall_build_order`] (comment 1 §4: 先建朝向敌人的一面). The buy whitelist
/// of comment 1 §3 sizes 正面墙券 against exactly this face.
pub fn front_wall_cells(turn: &Turn) -> Vec<Pos> {
    lay(turn, &[(3, -2), (3, -1), (3, 0), (3, 1), (3, 2), (3, 3)])
}

/// The top and bottom rows — the ten cells [`wall_build_order`] lays after the
/// front column (the 侧面 of the whitelist's 侧面墙券1 entry).
pub fn side_wall_cells(turn: &Turn) -> Vec<Pos> {
    lay(
        turn,
        &[
            (2, 3),
            (1, 3),
            (0, 3),
            (-1, 3),
            (-2, 3),
            (-2, -2),
            (-1, -2),
            (0, -2),
            (1, -2),
            (2, -2),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::footprint_distance;

    fn pos(x: i32, y: i32) -> Pos {
        Pos { x, y }
    }

    fn unit(id: i64, kind: &str, at: Pos) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": kind,
            "health": 1000, "level": 1, "backPackCapability": 100, "backpack": []
        })
    }

    /// A board with our station at `at` on the live game's 41×32 map.
    fn board(at: Pos) -> Turn {
        let payload = serde_json::json!({
            "roundNo": 5,
            "mapInfo": {"width": 41, "height": 32, "zones": []},
            "teamOur": {
                "type": "challenger", "goldNum": 0, "totalScore": 0,
                "playerTasks": [], "roles": [unit(10001, "station", at)]
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
        });
        let req: crate::protocol::Request =
            serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    /// The live game's own spawn: our station top-left at (10,24), enemy
    /// bottom-right at (38,4).
    fn top_left() -> Turn {
        board(pos(10, 24))
    }

    fn bottom_right() -> Turn {
        board(pos(38, 4))
    }

    #[test]
    fn the_corner_follows_the_station_x() {
        assert_eq!(base_corner(&top_left()), BaseCorner::TopLeft);
        assert_eq!(base_corner(&bottom_right()), BaseCorner::BottomRight);
    }

    #[test]
    fn fp_min_is_the_footprints_low_corner() {
        // station_footprint spans {x, x+1} × {y-1, y} around the station pos.
        assert_eq!(fp_min(&top_left()), Some(pos(10, 23)));
        assert_eq!(fp_min(&bottom_right()), Some(pos(38, 3)));
    }

    /// Comment 1 §1 option a, top-left base, verified cell by cell against the
    /// spec's own coordinates: weapons (9,22),(10,22),(9,24), operator (9,23).
    #[test]
    fn the_l_sits_where_the_spec_puts_it() {
        let turn = top_left();
        assert_eq!(
            weapon_sites(&turn),
            vec![pos(9, 22), pos(10, 22), pos(9, 24)],
            "the L's three weapon cells, in build order"
        );
        assert_eq!(operator_cell(&turn), Some(pos(9, 23)));
    }

    /// The mirrored spec coordinates for a bottom-right base: fp min (38,3),
    /// weapons (40,5),(39,5),(40,3), operator (40,4).
    #[test]
    fn the_l_mirrors_onto_the_bottom_right_corner() {
        let turn = bottom_right();
        assert_eq!(
            weapon_sites(&turn),
            vec![pos(40, 5), pos(39, 5), pos(40, 3)]
        );
        assert_eq!(operator_cell(&turn), Some(pos(40, 4)));
    }

    /// The property the single-operator night model rests on: one stand within
    /// Chebyshev 1 of ALL THREE guns — and one step from the entrance column,
    /// so the operator can always reach it once the ring is closed.
    #[test]
    fn the_operator_reaches_every_gun_and_the_entrance() {
        for turn in [top_left(), bottom_right()] {
            let operator = operator_cell(&turn).expect("a station has an operator cell");
            for site in weapon_sites(&turn) {
                assert_eq!(
                    crate::model::chebyshev(operator, site),
                    1,
                    "the operator stand must touch every weapon site"
                );
            }
            assert!(
                entrance_cells(&turn)
                    .iter()
                    .any(|cell| crate::model::chebyshev(operator, *cell) <= 1),
                "the operator cell must be reachable through the permanent entrance"
            );
        }
    }

    /// Comment 1 §4, top-left base, against the spec's own coordinates: front
    /// (13,21)..(13,26), top (12,26)..(8,26), bottom (8,21)..(12,21).
    #[test]
    fn the_wall_order_is_front_then_top_then_bottom() {
        let turn = top_left();
        let order = wall_build_order(&turn);
        assert_eq!(
            order,
            vec![
                // front column x+3, y-2 → y+3
                pos(13, 21), pos(13, 22), pos(13, 23),
                pos(13, 24), pos(13, 25), pos(13, 26),
                // top row y+3, x+2 → x-2
                pos(12, 26), pos(11, 26), pos(10, 26), pos(9, 26), pos(8, 26),
                // bottom row y-2, x-2 → x+2
                pos(8, 21), pos(9, 21), pos(10, 21), pos(11, 21), pos(12, 21),
            ]
        );
        assert_eq!(
            entrance_cells(&turn),
            vec![pos(8, 22), pos(8, 23), pos(8, 24), pos(8, 25)],
            "the back four cells are the permanent entrance"
        );
    }

    /// The accounting the whole wall line is built on: order + entrance is
    /// EXACTLY the 20-cell ring, the two sets are disjoint, and the weapon L
    /// lives inside it (ring-1 corridor cells), never on a wall cell.
    #[test]
    fn the_layout_tiles_the_ring_exactly() {
        for turn in [top_left(), bottom_right()] {
            let station = turn.station().expect("the station exists");
            let footprint = station_footprint(station.pos);
            let order = wall_build_order(&turn);
            let entrance = entrance_cells(&turn);
            assert_eq!(order.len(), 16);
            assert_eq!(entrance.len(), 4);
            assert!(
                order.iter().all(|a| !entrance.contains(a)),
                "the entrance cells are never in the build order"
            );
            let ring: Vec<Pos> = crate::brain::action::geometry::ring_cells(&footprint, 2);
            let mut covered: Vec<Pos> = order.iter().chain(entrance.iter()).copied().collect();
            covered.sort_by_key(|cell| (cell.x, cell.y));
            let mut ring = ring;
            ring.sort_by_key(|cell| (cell.x, cell.y));
            assert_eq!(covered, ring, "order + entrance = the whole 20-cell ring");
            for site in weapon_sites(&turn) {
                assert_eq!(
                    footprint_distance(site, &footprint),
                    1,
                    "a weapon site is a ring-1 corridor cell"
                );
                assert!(!order.contains(&site) && !entrance.contains(&site));
            }
            assert_eq!(
                footprint_distance(operator_cell(&turn).expect("operator"), &footprint),
                1
            );
        }
    }
}
