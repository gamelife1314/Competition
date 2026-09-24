//! Tower siting: which cells `brain::day::tower_gaps` offers, and in which
//! order.
//!
//! Since issue #221 phase 4b the sites are FIXED — comment 1 §1's L around the
//! operator stand ([`coregeek::brain::action::base_layout`]) — so there is
//! nothing left to "sit": no mannability veto, no corridor veto, no operating-
//! cell preference. What the offer still has to get right is the bookkeeping
//! around the fixed list:
//!
//!   * a unit standing on a site hides THAT site for the round — and only
//!     that one;
//!   * the kinds come off `TOWER_BUILD_ORDER` by ABSOLUTE index (existing
//!     towers included), so a base that already has a rocket is offered a
//!     railgun next and never a second rocket — the pk616181/pk616182 bug;
//!   * the offer stops at `TOWER_CAP`.
//!
//! The siting helpers are private, so every expectation is read off
//! `tower_gaps`'s return value — the public contract `spawn`/`night` consume.

use serde_json::{json, Value};

use coregeek::brain::day::tower_gaps;
use coregeek::model::{footprint_distance, station_footprint, Turn};
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

/// The sites the planner offers for this board.
fn gaps(roles: Vec<Value>) -> Vec<(Pos, String)> {
    let turn = turn_from(board(roles));
    let state = BotState::default();
    tower_gaps(&turn, &state)
}

fn cells(sites: &[(Pos, String)]) -> Vec<Pos> {
    sites.iter().map(|(pos, _)| *pos).collect()
}

fn pos(x: i32, y: i32) -> Pos {
    Pos { x, y }
}

/// The configured line is sited, in order, on the fixed L cells: for the
/// mid-map station (10,24) those are (9,22), (10,22) and (9,24) — every one a
/// free inner-ring cell, distinct, in layout order, with the kinds read off
/// `TOWER_BUILD_ORDER` from the top.
#[test]
fn the_configured_line_gets_its_sites_on_an_ordinary_board() {
    let sites = gaps(vec![
        station(10, 24),
        unit(10002, "worker", 20, 15),
        unit(10003, "worker", 21, 15),
        unit(10004, "pioneer", 22, 15),
    ]);

    assert_eq!(
        cells(&sites),
        vec![pos(9, 22), pos(10, 22), pos(9, 24)],
        "the offer must be the fixed L in layout order; got {sites:?}"
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
    for (site, _) in &sites {
        assert!(
            turn.is_land(*site) && footprint_distance(*site, &footprint) == 1,
            "{site:?} is not a free inner-ring cell"
        );
    }
    assert_eq!(sites.len(), 3, "an empty board has room for the whole line");
}

/// A role standing on a site hides THAT site for the round — the judger
/// refuses a build under a footprint — and the kinds still come off the top of
/// the line: the hidden first site does not push the rocket onto the second.
#[test]
fn a_role_standing_on_a_site_hides_only_that_site() {
    let sites = gaps(vec![
        station(10, 24),
        unit(10002, "worker", 9, 22), // on the first L site
        unit(10003, "worker", 21, 15),
        unit(10004, "pioneer", 22, 15),
    ]);
    assert_eq!(
        cells(&sites),
        vec![pos(10, 22), pos(9, 24)],
        "the occupied site drops out and the rest of the L stands; got {sites:?}"
    );
    let kinds: Vec<&str> = sites.iter().map(|(_, kind)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        &coregeek::config::TOWER_BUILD_ORDER[..2],
        "the line is read from the top of the config, not from the hidden \
         site's slot; got {sites:?}"
    );
}

/// The absolute-index rule (pk616181/pk616182): with a rocket already standing
/// on the first site, the NEXT offer is the config's second kind — a base is
/// never sold a second rocket while a railgun slot is empty. And with the cap
/// reached, the offer is empty.
#[test]
fn an_existing_tower_shifts_the_line_by_absolute_index() {
    let sites = gaps(vec![
        station(10, 24),
        unit(10020, "rocket", 9, 22), // already built on the first site
        unit(10002, "worker", 20, 15),
        unit(10003, "worker", 21, 15),
        unit(10004, "pioneer", 22, 15),
    ]);
    let line = coregeek::config::TOWER_BUILD_ORDER;
    assert_eq!(
        sites,
        vec![(pos(10, 22), line[1].to_string()), (pos(9, 24), line[2].to_string())],
        "existing towers shift the line by absolute index; got {sites:?}"
    );

    let full = gaps(vec![
        station(10, 24),
        unit(10020, "rocket", 9, 22),
        unit(10021, "railgun", 10, 22),
        unit(10022, "gatling", 9, 24),
        unit(10002, "worker", 20, 15),
    ]);
    assert!(
        full.is_empty(),
        "at TOWER_CAP the line is finished and nothing is offered; got {full:?}"
    );
}
