//! Tower siting: which inner-ring cells `brain::day::tower_gaps` is willing to
//! build on, and which it refuses.
//!
//! A site is refused on three grounds — `guns_stay_mannable`, `strands_corridor`
//! and the `operating_cells` count in the strict pass — and until now no test
//! named any of them. Neutering one of them left the suite green as long as the
//! wall ring still sealed, because the day-1 boards the other tests use happen
//! to be ones where the check never fires. Every test below therefore asserts
//! the OUTCOME — the cells actually offered — on a board built so that the
//! check under test is the only thing that separates the subject board from its
//! control:
//!
//!   * `guns_stay_mannable`  → a_site_that_seals_a_gun_away_from_every_stand_is_refused
//!   * `strands_corridor`    → a_site_that_strands_a_role_in_the_corridor_is_skipped
//!   * `operating_cells`     → a_site_with_one_free_operating_cell_is_still_offered
//!   * the configured line   → the_configured_line_gets_its_sites_on_an_ordinary_board
//!
//! The siting helpers are private, so every expectation is read off
//! `tower_gaps`'s return value — the public contract `spawn`/`night` consume.

use serde_json::{json, Value};

use coregeek::brain::day::tower_gaps;
use coregeek::model::{footprint_distance, neighbours, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn station(x: i32, y: i32) -> Value {
    json!({"id": 10001, "pos": {"x": x, "y": y}, "roleType": "station",
           "health": 1500, "attackPower": 0, "attackRange": 0,
           "level": 1, "backPackCapability": 0, "backpack": []})
}

fn unit(id: i64, kind: &str, x: i32, y: i32) -> Value {
    json!({"id": id, "pos": {"x": x, "y": y}, "roleType": kind,
           "health": 220, "attackPower": 0, "attackRange": 0,
           "level": 1, "backPackCapability": 4, "backpack": []})
}

fn board(roles: Vec<Value>) -> Value {
    json!({
        "roundNo": 1,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {"type": "challenger", "goldNum": 100, "totalScore": 0,
                    "playerTasks": [], "roles": roles},
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    })
}

/// The sites the planner offers for this board. The gate is pinned
/// (`BotState::gate_cell`) because `wall_gate` otherwise reads the route
/// planner's entrance, which is a property of the day's errands and not of the
/// siting rule under test.
fn gaps(roles: Vec<Value>, gate: Pos) -> Vec<(Pos, String)> {
    let turn = turn_from(board(roles.clone()));
    let mut state = BotState::default();
    state.gate_cell = Some(gate);
    tower_gaps(&turn, &state)
}

fn cells(sites: &[(Pos, String)]) -> Vec<Pos> {
    sites.iter().map(|(pos, _)| *pos).collect()
}

/// Cells a weapon on `site` could be operated from: land neighbours outside the
/// station footprint that nothing permanent — station, wall, standing tower —
/// occupies. Mirrors the `all` branch of `brain::day::operating_cells` on a
/// board with no standing towers, and counts teammate positions as free, since
/// a role standing somewhere is transient and will move.
fn operating_cells(turn: &Turn, site: Pos) -> usize {
    let footprint = station_footprint(turn.station().expect("a station").pos);
    let permanent: Vec<Pos> = footprint
        .iter()
        .copied()
        .chain(turn.walls().iter().map(|wall| wall.pos))
        .chain(turn.towers().iter().map(|tower| tower.pos))
        .collect();
    neighbours(site)
        .iter()
        .filter(|pos| turn.is_land(**pos) && !permanent.contains(pos))
        .count()
}

/// Guards `guns_stay_mannable`: a gun whose only stands end up inside the
/// sealed shell must not be built there.
///
/// Base (0,1) sits in the corner, so the wall shell and the map edge close the
/// base off: the pocket at (0,2) has exactly two walkable neighbours, the map
/// edge being the rest. A railgun already stands on (1,2) — permanently — so a
/// tower raised on (0,2) would have no stand left that an operator could reach
/// once the ring is sealed at dusk, and no operator could ever man it. The cell
/// is refused, and (2,0) — the only other candidate — is offered instead.
///
/// The control is the same board with a role standing where the gun does: a
/// role is transient, so the pocket is still reachable and (0,2) is offered.
/// The refusal is therefore the standing gun, not the cell.
///
/// Red on revert: with `guns_stay_mannable` neutered, (0,2) is offered as the
/// second site — `[(2,0), (0,2)]` instead of `[(2,0)]`.
#[test]
fn a_site_that_seals_a_gun_away_from_every_stand_is_refused() {
    let sealed = gaps(
        vec![
            station(0, 1),
            unit(10020, "railgun", 1, 2),
            unit(10002, "worker", 2, 1),
            unit(10004, "pioneer", 2, 2),
        ],
        Pos { x: 3, y: 2 },
    );
    let offered = cells(&sealed);
    assert_eq!(
        offered,
        vec![Pos { x: 2, y: 0 }],
        "the pocket at (0,2) is unmannable behind the standing gun, so the base \
         builds only the (2,0) site; got {sealed:?}"
    );

    let control = gaps(
        vec![
            station(0, 1),
            unit(10005, "worker", 1, 2),
            unit(10002, "worker", 2, 1),
            unit(10004, "pioneer", 2, 2),
        ],
        Pos { x: 3, y: 2 },
    );
    assert!(
        cells(&control).contains(&Pos { x: 0, y: 2 }),
        "with a transient role on (1,2) the pocket stays mannable, so (0,2) is \
         a legal site — the refusal above comes from the gun, not the cell; got {control:?}"
    );
}

/// Guards `strands_corridor`: a site is skipped when the tower on it would
/// leave a role standing in the corridor with nowhere to step.
///
/// Base (10,24) is mid-map, so the ring-1 band is a closed twelve-cell loop and
/// no map edge rescues a dead end. Two guns already stand on (11,22) and
/// (12,22); a third on (12,24) would leave the role at (12,23) with every
/// neighbour taken — the guns to the north, the station footprint to the west,
/// the new tower to the south — and no ring-1 neighbour at all. That is the
/// "0 角色站桩闲置" of issue #13, and it also stops the ring from ever closing,
/// because `wall_would_trap` reads the stranded role as a reason not to seal.
/// The site is skipped and the base offers (11,25) instead.
///
/// The control moves the role off the band, and (12,24) — otherwise sitable —
/// comes back. The refusal is the occupant, not the geometry.
///
/// Red on revert: with `strands_corridor` neutered, the site flips from (11,25)
/// to (12,24).
#[test]
fn a_site_that_strands_a_role_in_the_corridor_is_skipped() {
    let stranded = gaps(
        vec![
            station(10, 24),
            unit(10020, "railgun", 11, 22),
            unit(10021, "rocket", 12, 22),
            unit(10002, "worker", 12, 23),
            unit(10004, "pioneer", 20, 15),
        ],
        Pos { x: 13, y: 22 },
    );
    let offered = cells(&stranded);
    assert_eq!(
        offered,
        vec![Pos { x: 11, y: 25 }],
        "the site at (12,24) strands the role on (12,23) in the corridor, so \
         the base falls through to (11,25); got {stranded:?}"
    );

    let control = gaps(
        vec![
            station(10, 24),
            unit(10020, "railgun", 11, 22),
            unit(10021, "rocket", 12, 22),
            unit(10002, "worker", 20, 15),
            unit(10004, "pioneer", 21, 15),
        ],
        Pos { x: 13, y: 22 },
    );
    assert_eq!(
        cells(&control),
        vec![Pos { x: 12, y: 24 }],
        "with the crew off the band the same cell is sitable, so the skip above \
         is the stranded role and not the site; got {control:?}"
    );
}

/// Guards the `operating_cells` count — and, more importantly, guards that it
/// stays a PREFERENCE and never becomes a veto.
///
/// A strict "two free standing cells or nothing" test rejects most of a corner
/// base's five ring cells outright, and the base then ends the day with two
/// guns while the third slot waits for a cell that never frees up. So the
/// strict pass runs first and the fallback accepts any free ring cell that
/// keeps the guns mannable. This board is that case: the site at (2,0) has
/// exactly ONE free operating cell — (2,1), with the walls at (3,0)/(3,1) and
/// the map edge taking the rest — and it is still offered, first.
///
/// Red on revert: deleting `.or_else(|| pick(false))` from the site choice
/// leaves (2,0) out of the day's sites. It is the regression guard against
/// "fixing" the siting rule by hardening the strict pass into a veto.
#[test]
fn a_site_with_one_free_operating_cell_is_still_offered() {
    let roles = vec![
        station(0, 1),
        unit(10050, "wall", 3, 0),
        unit(10051, "wall", 3, 1),
        unit(10052, "wall", 0, 3),
        unit(10053, "wall", 1, 3),
        unit(10002, "worker", 2, 1),
        unit(10003, "worker", 2, 2),
        unit(10004, "pioneer", 1, 2),
    ];
    let turn = turn_from(board(roles.clone()));
    let leaned_on = Pos { x: 2, y: 0 };
    assert_eq!(
        operating_cells(&turn, leaned_on),
        1,
        "the board must leave the site at (2,0) a single operating cell for \
         this test to mean anything"
    );

    let sites = gaps(roles, Pos { x: 3, y: 2 });
    assert!(
        cells(&sites).contains(&leaned_on),
        "a site with one free operating cell is a site; the strict pass may \
         rank it last but the fallback must still offer it; got {sites:?}"
    );
}

/// The control for all three above: on an ordinary day-1 board the configured
/// line is still sited, in order, on distinct inner-ring cells.
///
/// This is what proves the vetoes are not simply refusing everything — it must
/// stay green under each of the reverts named above, and it goes red if the
/// line stops being built at all (the per-kind tally the positional pass
/// replaced is the historical case).
#[test]
fn the_configured_line_gets_its_sites_on_an_ordinary_board() {
    let sites = gaps(
        vec![
            station(10, 24),
            unit(10002, "worker", 20, 15),
            unit(10003, "worker", 21, 15),
            unit(10004, "pioneer", 22, 15),
        ],
        Pos { x: 13, y: 22 },
    );

    let line = coregeek::config::TOWER_BUILD_ORDER;
    let kinds: Vec<&str> = sites.iter().map(|(_, kind)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        &line[..line.len().min(coregeek::config::TOWER_CAP)],
        "the day builds the configured line from the top; got {sites:?}"
    );

    let turn = turn_from(board(vec![station(10, 24)]));
    let footprint = station_footprint(turn.station().expect("a station").pos);
    let mut seen: Vec<Pos> = Vec::new();
    for (pos, _) in &sites {
        assert!(
            turn.is_land(*pos) && footprint_distance(*pos, &footprint) == 1,
            "{pos:?} is not a free inner-ring cell"
        );
        assert!(!seen.contains(pos), "{pos:?} is offered twice");
        seen.push(*pos);
    }
    assert_eq!(sites.len(), 2, "an empty board has room for the whole line");
}
