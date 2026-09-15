//! Joint route / order planner.
//!
//! The owner watched the 2026-09-14 batch and named one algorithm instead of
//! four rules: *"在系统中做一个最佳路径计算算法，先建哪段城墙，先去哪里采矿，
//! 在哪里留入口，等他进来再封上。"* The four decisions are
//!
//!   1. **which ring cell the day leaves open** — the entrance,
//!   2. **which mine is worked first** — nearest-and-valuable,
//!   3. **which ring cell is built next** — the ring walked from the entrance,
//!   4. **when it is sealed** — once the working roles are back in.
//!
//! and they are one optimisation rather than four heuristics because the cost
//! they share is the same currency: **rounds spent walking**. The entrance sets
//! the walk cost of every errand; the walk cost decides which mine is worth
//! working; and the mine being worked is the side of the ring the crew is
//! standing on, which is where the next wall segment is worth building. Four
//! independent rules fought each other — a gate fixed at the station's
//! north-east corner (`xmax + 2, ymin - 1`) meant a crew mining to the south
//! walked the whole ring to get back in, and a wall order sorted by a compass
//! tiebreak meant the crew criss-crossed the shell it was building.
//!
//! # The model
//!
//! Everything below is priced in **Chebyshev rounds**, which is what a move
//! costs on this board (eight-way movement, one cell per round):
//!
//! ```text
//! cost(g, e)  = inside_leg(g) + 1 + chebyshev(g, e)      // out through gate g
//! day_cost(g) = Σ_e w_e · cost(g, e)                     // e over the errands
//! ```
//!
//! `inside_leg(g)` is the walk from the station to the cell beside `g`; on a
//! radius-2 shell of a 2×2 footprint it is 2 for every candidate, so it drops
//! out of the `argmin` but is kept in the formula because the footprint grows
//! with the station's level.
//!
//! The same model answers all four questions:
//!
//!   * the **entrance** is the `argmin_g day_cost(g)` over the shell — one
//!     cell, latched at daybreak (`BotState::gate_cell`);
//!   * the **mine** is the vein whose round-trip through today's opening is
//!     cheapest ([`trip_rounds`]), value breaking the tie — the pick and the
//!     entrance agree by construction, because they are priced by the same
//!     walk;
//!   * the **build order** is the shell walked as a single cycle from the
//!     entrance ([`build_order`]), starting on the side the errands are on, so
//!     consecutive placements are one step apart and the last cell laid is the
//!     entrance's far shoulder;
//!   * the **seal** is the day's last wall: the gate's own stone is counted in
//!     the demand all afternoon (see `brain::day`), and once the working roles
//!     are back inside the gate is built like any other cell, with
//!     `HARD_SEAL_ROUND` as the backstop.
//!
//! # Safety
//!
//! The planner chooses a *cell*; it never chooses to seal with a role outside.
//! [`open_ring_cells`] is the set of ring cells that are literally walkable
//! right now, so a route is only ever priced through a hole that exists, and
//! [`crate::brain::walk_or_remove_wall`] keeps its demolition escape hatch. The
//! dusk seal still waits for `gate_open_record` to come back empty, and
//! [`crate::brain::day::HARD_SEAL_ROUND`] is still the backstop.

use std::collections::HashSet;

use crate::model::{chebyshev, footprint_distance, station_footprint, Turn, UnitKind};
use crate::protocol::Pos;
use crate::state::BotState;

/// One point outside the ring the day intends to visit, weighted by how many
/// round trips it is expected to cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Errand {
    pub pos: Pos,
    pub trips: i64,
}

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
/// that already has stone in it would under-count every errand by the detour,
/// which is the mistake the fixed gate made in the opposite direction.
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
/// one of them, and the cheapest gap is not the one the compass picked: this is
/// the round cost an open gate used to hide, and the reason a crew mining to
/// the south spent the day walking the ring to reach a north-east door.
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

/// The outside points this day actually uses, each weighted by the trips it is
/// expected to cost.
///
/// Weighted by what the trip EARNS, because that is what decides which side of
/// the base is worth opening: an iron vein at 8 gold an ore is two stone trips,
/// and the shop and the vendor are visited several times a day on any day the
/// economy is running. When the wall line still owes stone the nearest stone
/// vein is pushed to the front — the ring is the day's product, and the stone
/// trip is the one errand that must not be a long walk. A vein on outage is not
/// an errand at all: nobody is walking there today, and the entrance must not
/// be placed for it.
pub fn errands(turn: &Turn, state: &BotState, stone_demand: i64) -> Vec<Errand> {
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let home = station.pos;
    let mut list: Vec<Errand> = Vec::new();
    let push = |pos: Pos, trips: i64, list: &mut Vec<Errand>| {
        if let Some(existing) = list.iter_mut().find(|errand| errand.pos == pos) {
            existing.trips = existing.trips.max(trips);
        } else {
            list.push(Errand { pos, trips });
        }
    };
    for (pos, ore) in turn.all_mines() {
        if state.ore_on_outage(&ore, turn.day) {
            continue;
        }
        let price = turn.vendor_prices.get(&ore).copied().unwrap_or(1).max(1);
        // The wall line's own stone outranks everything while the ring owes it:
        // the crew walks there until the shell is up, whatever it sells for.
        let trips = if stone_demand > 0 && ore == crate::model::STONE {
            12
        } else {
            price.max(2)
        };
        push(pos, trips, &mut list);
    }
    for vendor in turn.vendors() {
        push(vendor, 10, &mut list);
    }
    for shop in turn.weapon_shops() {
        push(shop, 8, &mut list);
    }
    // Gate faces the direction workers are heading: their current positions
    // also influence the entrance, so a crew clustered on one side of the base
    // doesn't have to walk through the opposite side to get out. Only matters
    // when there are actual errands — if the list is empty, the planner has
    // no evidence and falls back to the legacy corner.
    if !list.is_empty() {
        for worker in turn.workers() {
            push(worker.pos, 5, &mut list);
        }
    }
    // Deterministic, with a total order: two points of equal weight and equal
    // distance must not make the entrance flicker with hash iteration order.
    list.sort_by_key(|errand| {
        (
            std::cmp::Reverse(errand.trips),
            chebyshev(home, errand.pos),
            errand.pos.x,
            errand.pos.y,
        )
    });
    list
}

/// The ring cell the day should leave open: the one that minimises the total
/// weighted walk to everything the day intends to visit.
///
/// This is the whole of the owner's second observation. The opening belongs on
/// the side the work is on, and "the side the work is on" is not a compass
/// direction — it is the weighted mean of the errands, read off the board.
///
/// A cell that already carries one of our walls is a CANDIDATE, not a veto:
/// the entrance is chosen at daybreak, and the morning's `open_door` cuts the
/// shell there — the gate and the door are the same hole seen from two sides.
/// Counting walls as occupants (as the first draft did) meant a completed ring
/// had no legal candidate at all, so day 2 and every day after fell back to the
/// fixed corner and the planner ran only on the one day that needed it least.
///
/// `None` when the board carries no errand at all (a test fixture with no zones)
/// or no ring cell is legal; the caller then keeps the historical
/// `(xmax + 2, ymin - 1)` cell rather than inventing one, so a board the planner
/// has no evidence about behaves exactly as it did before.
pub fn entrance(turn: &Turn, state: &BotState, stone_demand: i64) -> Option<Pos> {
    let errands = errands(turn, state, stone_demand);
    if errands.is_empty() {
        return None;
    }
    let station = turn.station()?;
    // The occupants that veto a candidate are the ones that cannot be cut
    // through: a role standing on the cell (it moves, but the walk THROUGH it
    // is what we are pricing) and the station itself. A wall is cut in one
    // remove — that is the morning door, not an obstacle.
    let occupied: HashSet<Pos> = turn
        .ours
        .iter()
        .filter(|unit| unit.kind != UnitKind::Wall)
        .flat_map(|unit| unit.footprint())
        .collect();
    // The inside leg: from the base out to the cell beside `cell`. Corner cells
    // are a step further than the face centres, which is the (small) reason the
    // entrance prefers the middle of a side to a corner.
    let inside_leg = |cell: Pos| chebyshev(station.pos, cell) as i64;
    // Enemy direction tiebreak: prefer cells on the side AWAY from the enemy
    // base. Robots spawn from the enemy side, so an entrance facing them is a
    // highway into our station. This is a TIEBREAK only — errand cost still
    // dominates, so the gate stays on the economically efficient side. But
    // when two cells are close in errand cost, the one further from the enemy
    // wins. The penalty is small enough that it never overrides a meaningfully
    // shorter errand walk, but large enough to break ties consistently.
    let enemy_pos = turn.enemy_station().map(|e| e.pos);
    let score = |cell: Pos| -> i64 {
        let errand_cost: i64 = errands
            .iter()
            .map(|errand| errand.trips * (inside_leg(cell) + 1 + chebyshev(cell, errand.pos) as i64))
            .sum();
        // Tiebreak: add a small penalty for cells closer to the enemy.
        // At most 30 points — less than one errand trip, so it only breaks ties.
        let penalty = enemy_pos
            .map(|ep| {
                let dist_to_enemy = chebyshev(cell, ep) as i64;
                let station_to_enemy = chebyshev(station.pos, ep) as i64;
                // Penalize cells on the enemy-facing half of the ring.
                if dist_to_enemy < station_to_enemy {
                    30 - (dist_to_enemy - station_to_enemy / 2).max(0)
                } else {
                    0
                }
            })
            .unwrap_or(0);
        errand_cost + penalty
    };
    let mut best: Option<(i64, i32, i32, Pos)> = None;
    for cell in ring_cells(turn, 2) {
        if !turn.is_land(cell) || occupied.contains(&cell) {
            continue;
        }
        // A cell the judger has twice refused a wall on can never be sealed:
        // an entrance there is a hole the ring keeps forever.
        if state
            .blacklisted_builds
            .contains(&(cell, "wall".to_string()))
        {
            continue;
        }
        let key = (score(cell), cell.x, cell.y, cell);
        if best.as_ref().map(|current| key < *current).unwrap_or(true) {
            best = Some(key);
        }
    }
    best.map(|(_, _, _, pos)| pos)
}

/// The historical entrance: two cells past the footprint's east edge, one below
/// its south edge. Kept as the fallback for a board with no errands on it.
///
/// Clamped to the board, because the arithmetic is only in range when the base
/// sits away from the top edge. A base pressed into the top-left corner asks
/// for `y = ymin - 1 = -1`, and an entrance OFF the board is not a cell: it
/// cannot be walked to, it can never be walled, and — the reason this was
/// found — it is an anchor `tower_gaps` then measures every gun's mannability
/// against, so `guns_stay_mannable` vetoed every ring-1 site, the railgun and
/// the rocket were never sited, and
/// `tests/spawn.rs::the_three_tower_sites_sit_on_the_inner_ring_from_every_base`
/// saw a single gatling for that base.
pub fn legacy_entrance(turn: &Turn) -> Option<Pos> {
    let station = turn.station()?;
    let footprint = station_footprint(station.pos);
    let xmax = footprint.iter().map(|pos| pos.x).max()?;
    let ymin = footprint.iter().map(|pos| pos.y).min()?;
    let pos = Pos {
        x: (xmax + 2).clamp(0, turn.width - 1),
        y: (ymin - 1).clamp(0, turn.height - 1),
    };
    turn.is_land(pos).then_some(pos)
}

/// The day's entrance, latched.
///
/// Latched per day on purpose. The errand set is a live quantity — the stone
/// demand falls as the ring goes up and a mine can go on outage — and an
/// entrance that moves mid-afternoon would either re-open a cell the crew just
/// walled or wall a cell a role is walking through. One cell, chosen once at
/// daybreak, is the only version of this that is safe to build against.
pub fn gate_of(turn: &Turn, state: &BotState) -> Option<Pos> {
    state
        .gate_cell
        .or_else(|| entrance(turn, state, 1).or_else(|| legacy_entrance(turn)))
}

/// The shell as a single cycle: top edge west→east, right edge north→south,
/// bottom edge east→west, left edge south→north.
///
/// This is the one property the build order needs that a compass sweep does
/// not have: **every consecutive pair is one step apart**, the wrap-around
/// included, so the ring can be laid as a walk. The first draft walked the
/// shell greedily — "any ring neighbour except the one we came from" — on the
/// theory that a thickness-one shell is a degree-2 cycle. Under eight-way
/// movement it is not: a cell beside a corner has THREE ring neighbours
/// ((12,26) touches (11,26), (13,26) and (13,25)), the greedy walk took the
/// lexicographically first one, and the cell it skipped became reachable only
/// from an already-visited cell — so it was appended at the END of the order,
/// on the far side of the base. The crew finished the sweep beside the
/// entrance, then walked the whole ring back for the one stranded cell: the
/// criss-cross the order existed to prevent, reintroduced by the tiebreak.
fn ring_cycle(turn: &Turn, radius: i32) -> Vec<Pos> {
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
    let (west, east) = (xmin - radius, xmax + radius);
    let (north, south) = (ymin - radius, ymax + radius);
    let mut cycle = Vec::new();
    for x in west..=east {
        cycle.push(Pos { x, y: north });
    }
    for y in north + 1..=south {
        cycle.push(Pos { x: east, y });
    }
    for x in (west..=east - 1).rev() {
        cycle.push(Pos { x, y: south });
    }
    for y in (north + 1..=south - 1).rev() {
        cycle.push(Pos { x: west, y });
    }
    // Keep only cells that are genuinely on the shell. For a rectangular
    // footprint this drops nothing; the filter is what keeps a footprint the
    // formula does not fit from inventing cells rather than falling back.
    let set: HashSet<Pos> = ring_cells(turn, radius).into_iter().collect();
    cycle.retain(|pos| set.contains(pos));
    cycle
}

/// Ring cells in the order the crew should build them.
///
/// The shell is [`ring_cycle`] rotated so the entrance comes first, and the
/// order is that walk: the crew steps out of the entrance and lays stone along
/// one side, round the far end, and back to the entrance's other shoulder. Two
/// properties follow, and they are the owner's "closes fastest with least
/// walking":
///
///   * **least walking** — consecutive cells in the order are adjacent, so each
///     placement costs one step, and the whole sweep leaves and returns by the
///     entrance;
///   * **closes fastest** — the last cell placed is the entrance's other
///     shoulder, so the ring is closed by the crew standing at the door it is
///     about to seal, not at the far end of the shell.
///
/// The DIRECTION of the walk is the one joint decision left over after the
/// rotation: both directions visit every cell for the same number of steps,
/// but they are not the same day. The stone veins sit on one side of the base,
/// and the carriers come and go through the entrance all afternoon — building
/// the near side first puts the head of the order where the crew already
/// stands, building the far side first sends the first carriers across the
/// base. So among walks the enemy bearing cannot separate, the walk starts on
/// the side the day's errands weigh more: the two shoulders of the entrance are
/// scored by the same `Σ w_e · chebyshev` the entrance itself was chosen with,
/// and the cheaper shoulder is step one. The tie breaks on coordinates, which
/// is what keeps a board with no errands deterministic.
///
/// The enemy bearing comes FIRST, and it is the one thing here that is not an
/// economic question (issue #206 §6). The ring exists to stop robots, robots
/// come from the enemy base, and a day can be cut short by dusk or by stone
/// before the last cell is laid — so of the two ways round the shell the crew
/// takes the one that puts the enemy-facing edges up first and leaves the edge
/// furthest from the enemy for last. The entrance itself stays where the
/// economy put it: it is the door the crew walks in and out of all afternoon,
/// and moving it to the enemy side would be a highway into the base, not a
/// defence.
///
/// The entrance itself is index 0 and is never in the build list: it is the
/// hole the day keeps, and the dusk seal is what closes it.
pub fn build_order(turn: &Turn, state: &BotState, entrance: Pos) -> Vec<Pos> {
    let cells = ring_cells(turn, 2);
    let cycle = ring_cycle(turn, 2);
    let Some(idx) = cycle.iter().position(|pos| *pos == entrance) else {
        return cells; // entrance not on the shell: row-major fallback
    };
    let forward: Vec<Pos> = cycle[idx..]
        .iter()
        .chain(cycle[..idx].iter())
        .copied()
        .collect();
    let mut backward: Vec<Pos> = Vec::with_capacity(cycle.len());
    backward.push(entrance);
    backward.extend(cycle[..idx].iter().rev());
    backward.extend(cycle[idx + 1..].iter().rev());
    // Score each direction's FIRST BUILD CELL (index 1; index 0 is the
    // entrance in both). The whole walk is one ring either way, so the two
    // directions differ only in which side gets built while the carriers are
    // fresh — which is exactly "which shoulder is nearer to the work".
    let demand = if open_ring_cells(turn, 2).is_empty() {
        0
    } else {
        1
    };
    let errands = errands(turn, state, demand);
    let score = |cell: Pos| -> i64 {
        errands
            .iter()
            .map(|errand| errand.trips * chebyshev(cell, errand.pos) as i64)
            .sum()
    };
    // How well a walk faces the enemy, scored as Σ (index × distance to the
    // enemy): a cell far from the enemy weighs more the later it is built, so
    // the walk with the LARGER sum is the one that buries the far edge deepest
    // in the order and puts the enemy-facing edges up first. Negated so that
    // "smaller key wins" keeps holding for the whole tuple. A board with no
    // enemy base on it scores 0 either way — that tie is what leaves the
    // errand direction of every enemy-free board exactly as it was.
    let enemy = turn.enemy_station().map(|station| station.pos);
    let facing = |order: &[Pos]| -> i64 {
        let Some(enemy) = enemy else {
            return 0;
        };
        order
            .iter()
            .enumerate()
            .map(|(index, cell)| index as i64 * chebyshev(*cell, enemy) as i64)
            .sum()
    };
    let key = |order: &[Pos]| -> (i64, i64, i32, i32) {
        let head = order.get(1).copied().unwrap_or(entrance);
        (-facing(order), score(head), head.x, head.y)
    };
    let mut order = if key(&forward) <= key(&backward) {
        forward
    } else {
        backward
    };
    // Anything the cycle could not cover (a shell the rectangle formula does
    // not fit) keeps its row-major place at the end: the crew still gets to
    // it, just last.
    for cell in cells {
        if !order.contains(&cell) {
            order.push(cell);
        }
    }
    order
}

/// Where `site` sits in [`build_order`] — the sort key the wall line uses.
///
/// `usize::MAX` for a cell that is not on the shell, so an unknown site sorts
/// behind every known one instead of ahead of them.
pub fn build_rank(order: &[Pos], site: Pos) -> usize {
    order
        .iter()
        .position(|cell| *cell == site)
        .unwrap_or(usize::MAX)
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

    /// The sim's economy board: everything the day visits is EAST of the base.
    fn east_board() -> Turn {
        board(
            vec![
                (16, 24, "stone"),
                (16, 27, "stone"),
                (18, 21, "stone"),
                (20, 28, "stone"),
                (22, 19, "iron"),
                (24, 30, "iron"),
                (26, 17, "copper"),
                (30, 26, "vendor"),
                (28, 20, "weaponShop"),
            ],
            vec![],
        )
    }

    fn default_state() -> BotState {
        BotState::default()
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
    fn the_entrance_is_on_the_side_of_the_days_errands() {
        let state = default_state();
        // Everything east: the gate must land on the eastern half of the
        // shell (the west edge is x=8, the footprint's east edge x=11).
        let gate = entrance(&east_board(), &state, 1).expect("errands exist");
        assert!(
            gate.x >= 12,
            "all the day's work is east of the base, and the gate is {gate:?}"
        );
        // Everything south: four stone veins and nothing else. The gate must
        // land on the south row (y = ymax + 2 = 26).
        let south = board(
            vec![
                (10, 28, "stone"),
                (11, 29, "stone"),
                (12, 28, "stone"),
                (9, 29, "stone"),
            ],
            vec![],
        );
        let gate = entrance(&south, &state, 1).expect("errands exist");
        assert_eq!(
            gate.y, 26,
            "all the day's work is south of the base, and the gate is {gate:?}"
        );
    }

    #[test]
    fn the_entrance_is_deterministic() {
        let state = default_state();
        let board = east_board();
        let first = entrance(&board, &state, 1);
        let second = entrance(&board, &state, 1);
        assert_eq!(first, second, "the same board must produce the same plan");
        assert_eq!(
            gate_of(&board, &state),
            first,
            "an unlatched gate_of must reproduce the planner's pick"
        );
    }

    #[test]
    fn a_walled_ring_still_gets_an_entrance() {
        // Day 2's shape: the whole shell is walled, and the economy's errands
        // are all outside it. A wall is what the morning door CUTS, not an
        // occupant that vetoes the cell — the first draft counted it as one,
        // so the day with the most walking never got a planned entrance.
        let mut roles: Vec<serde_json::Value> = Vec::new();
        let probe = board(vec![], vec![]);
        for (index, cell) in ring_cells(&probe, 2).iter().enumerate() {
            roles.push(unit(20000 + index as i64, "wall", *cell));
        }
        let turn = board(
            vec![(22, 19, "iron"), (26, 17, "copper"), (28, 20, "weaponShop")],
            roles,
        );
        let state = default_state();
        let gate = entrance(&turn, &state, 0)
            .expect("a completed ring is not a reason to keep the corner gate");
        assert!(
            gate.x >= 12,
            "the errands are east and the whole shell is walled: {gate:?}"
        );
    }

    #[test]
    fn a_blacklisted_cell_is_never_the_entrance() {
        // A cell the judger has twice refused a wall on can never be sealed,
        // so it must never be chosen as the day's opening.
        let mut state = default_state();
        let turn = east_board();
        let mut eastern = 0;
        for cell in ring_cells(&turn, 2) {
            if cell.x >= 12 {
                state.blacklisted_builds.insert((cell, "wall".to_string()));
                eastern += 1;
            }
        }
        assert!(eastern > 0, "test setup: the east half was blacklisted");
        let gate = entrance(&turn, &state, 1).expect("the west half is legal");
        assert!(
            gate.x < 12,
            "every eastern cell is blacklisted, and the gate is {gate:?}"
        );
    }

    #[test]
    fn a_board_with_no_errands_keeps_the_legacy_corner() {
        let turn = board(vec![], vec![]);
        let state = default_state();
        assert_eq!(
            entrance(&turn, &state, 1),
            None,
            "no zones, no errands, no evidence: the planner says nothing"
        );
        assert_eq!(
            gate_of(&turn, &state),
            legacy_entrance(&turn),
            "and the day behaves exactly as it did before the planner"
        );
        // …but the build order is still one unbroken walk, so a board the
        // planner has no evidence about loses nothing.
        let gate = gate_of(&turn, &state).expect("legacy fallback");
        let order = build_order(&turn, &state, gate);
        assert_eq!(order.len(), 20);
    }

    #[test]
    fn a_crew_mining_south_does_not_walk_the_whole_ring() {
        // The owner's second observation, priced. The shell is complete except
        // for ONE opening; a crew inside, heading for a vein due south, must
        // pay the detour to that opening and no more.
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
        // The historical north-east corner: the same errand pays for the
        // whole southward leg twice — out the wrong side and back.
        let corner = with_gate(pos(13, 22));
        let dear = trip_rounds(&corner, from, mine);
        assert!(
            dear >= direct + 4,
            "through the legacy corner the walk is {dear}, the straight line is {direct}"
        );
        assert!(
            cheap < dear,
            "the joint model exists so that {cheap} < {dear} decides the gate"
        );
    }

    #[test]
    fn the_build_order_is_one_unbroken_walk_from_the_entrance() {
        let turn = east_board();
        let state = default_state();
        let entrance = pos(12, 26);
        let order = build_order(&turn, &state, entrance);
        // The entrance is the hole the day keeps: it heads the list and is
        // never laid as a wall during the sweep.
        assert_eq!(order.first().copied(), Some(entrance));
        assert_eq!(order.len(), 20, "the whole shell, entrance included");
        let unique: HashSet<Pos> = order.iter().copied().collect();
        assert_eq!(unique.len(), 20, "no cell is visited twice");
        // One unbroken walk: every step between consecutive cells is a single
        // move, and the walk ends on the entrance's other shoulder — the cell
        // the crew seals from, not the far end of the shell.
        for pair in order.windows(2) {
            assert_eq!(
                chebyshev(pair[0], pair[1]),
                1,
                "the walk crosses itself between {:?} and {:?}",
                pair[0],
                pair[1]
            );
        }
        let last = order.last().copied().expect("non-empty");
        assert_eq!(
            chebyshev(last, entrance),
            1,
            "the sweep ends beside the entrance it started from: {last:?}"
        );
    }

    #[test]
    fn the_build_order_starts_on_the_side_the_work_is_on() {
        // The entrance on the south row; the errands all east. The first cell
        // laid must be the entrance's EASTERN shoulder — the carriers come
        // and go through the entrance all afternoon, and the near side is
        // where they already stand.
        let turn = east_board();
        let state = default_state();
        let order = build_order(&turn, &state, pos(12, 26));
        assert_eq!(
            order.get(1).copied(),
            Some(pos(13, 26)),
            "the work is east, so the sweep goes east first: {:?}",
            &order[..4]
        );
        // The mirrored board: the same entrance, every errand west.
        let west = board(
            vec![
                (4, 24, "stone"),
                (3, 27, "stone"),
                (2, 21, "stone"),
                (5, 28, "iron"),
            ],
            vec![],
        );
        let order = build_order(&west, &state, pos(12, 26));
        assert_eq!(
            order.get(1).copied(),
            Some(pos(11, 26)),
            "the work is west, so the sweep goes west first: {:?}",
            &order[..4]
        );
    }

    #[test]
    fn the_build_order_survives_a_hole_in_the_shell() {
        // A neutral zone sitting on the shell line is a cell the ring can
        // never fill. The walk must still terminate, cover every buildable
        // cell once, and keep the entrance first.
        let mut turn = east_board();
        let hole = pos(9, 21);
        turn.zones.insert(hole, "vendor".to_string());
        let state = default_state();
        let order = build_order(&turn, &state, pos(12, 26));
        assert_eq!(order.first().copied(), Some(pos(12, 26)));
        let buildable: Vec<Pos> = order
            .iter()
            .copied()
            .filter(|cell| turn.is_land(*cell))
            .collect();
        let unique: HashSet<Pos> = buildable.iter().copied().collect();
        assert_eq!(buildable.len(), unique.len(), "no cell is visited twice");
        assert!(
            order.iter().any(|cell| *cell == hole),
            "the hole stays in the list (the caller filters it), nothing is invented"
        );
        let land_ring: HashSet<Pos> = ring_cells(&turn, 2)
            .into_iter()
            .filter(|cell| turn.is_land(*cell))
            .collect();
        let covered: HashSet<Pos> = order.into_iter().filter(|cell| turn.is_land(*cell)).collect();
        assert_eq!(covered, land_ring, "every buildable cell is covered");
    }
}
