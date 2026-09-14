//! The dusk gate seal (P1) and the build-site oscillation behind it.
//!
//! `docs/FAILURE-ANALYSIS-2026-09-14-b.md` §2.4: across five matches the wall
//! gate never once sealed on the first try — `wall_gate_open` for the whole
//! fifteen-round dusk window in #111 day 1, #112 both days and #115 day 1 — and
//! the base was destroyed on night 2 in three of them. `score_3` (survival) is
//! worth 550 and we scored 30.
//!
//! The record named the same role every round and its `stuck` list was always
//! empty, which is the whole diagnosis: the role could reach its post, it was
//! simply never told to go. `preposition_round` covers a controller that has a
//! tower and the pioneer has its own recall, but a role with NO gun — the odd
//! one out on a three-role board with two guns, which day 1 always is — fell
//! straight through every deadline and kept running economy errands outside the
//! ring. Measured on the board below: the unpaired worker walked to the weapon
//! shop and issued `buy Medicine` on every round of the whole dusk window while
//! the gate stayed open, and the ring kept a robot-sized hole all night.

use std::collections::HashSet;

use serde_json::{json, Value};

use coregeek::brain::day::{gate_open_record, plan as day_plan, tower_gaps};
use coregeek::brain::night;
use coregeek::model::{footprint_distance, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

const WIDTH: i32 = 41;
const HEIGHT: i32 = 32;
/// 任务书: day 1's daytime is rounds 1..=70; the dusk window is in-day 55..69.
const DAY_END: i64 = 70;
/// `DUSK_RETREAT_LEAD` in `brain::day`: the round from which a role with no gun
/// stops taking errands outside the ring.
const COMMIT_IN_DAY: i64 = coregeek::brain::economy::DUSK_ROUND - 8;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

const STATION: (i32, i32) = (10, 24);

fn station() -> Value {
    json!({
        "id": 10001, "pos": {"x": STATION.0, "y": STATION.1}, "roleType": "station",
        "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn role(id: i64, kind: &str, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": kind,
        "health": 220, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 100, "backpack": []
    })
}

fn gatling(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "gatling",
        "health": 1000, "attackPower": 10, "attackRange": 3,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn wall(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "wall",
        "health": 1000, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn zone(x: i32, y: i32, kind: &str) -> Value {
    json!({"pos": {"x": x, "y": y}, "neutralType": kind})
}

/// A day-1 board on in-day round `in_day` (roundNo 1 is day 1, in-day 0).
fn world(in_day: i64, ours: Vec<Value>, zones: Vec<Value>) -> Value {
    json!({
        "roundNo": in_day + 1,
        "mapInfo": {"width": WIDTH, "height": HEIGHT, "zones": zones},
        "teamOur": {
            "type": "challenger", "goldNum": 75, "totalScore": 0,
            "playerTasks": [], "roles": ours,
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "vendorShopList": [{"name": "stone", "price": 2}, {"name": "iron", "price": 8}],
        "weaponShopList": [{"name": "Medicine", "price": 10}],
    })
}

/// A round-by-round day driver. Only `move` commands are applied, the way the
/// judger applies them (a destination another unit already stands on is a lost
/// step); every other action is dropped, because this harness is about where
/// the crew is standing when the sun goes down, not about what it earns.
struct Day {
    template: Value,
    round: i64,
    units: Vec<(i64, Pos)>,
    /// Per round: `None` when the gate may close, `Some(record)` when it may not.
    gates: Vec<Option<Value>>,
    positions: Vec<(i64, i64, Pos)>,
    state: BotState,
}

impl Day {
    fn new(template: Value) -> Self {
        let units = template["teamOur"]["roles"]
            .as_array()
            .expect("roles")
            .iter()
            .map(|unit| {
                (
                    unit["id"].as_i64().unwrap(),
                    Pos {
                        x: unit["pos"]["x"].as_i64().unwrap() as i32,
                        y: unit["pos"]["y"].as_i64().unwrap() as i32,
                    },
                )
            })
            .collect();
        Self {
            template,
            round: 1,
            units,
            gates: Vec::new(),
            positions: Vec::new(),
            state: BotState::default(),
        }
    }

    /// The station's four cells — the ring the gate seal is measured against.
    fn footprint(&self) -> Vec<Pos> {
        let pos = Pos {
            x: STATION.0,
            y: STATION.1,
        };
        station_footprint(pos)
    }

    fn payload(&self) -> Value {
        let mut payload = self.template.clone();
        payload["roundNo"] = json!(self.round);
        for unit in payload["teamOur"]["roles"].as_array_mut().unwrap() {
            let id = unit["id"].as_i64().unwrap();
            if let Some((_, pos)) = self.units.iter().find(|(unit_id, _)| *unit_id == id) {
                unit["pos"] = json!({"x": pos.x, "y": pos.y});
            }
        }
        payload
    }

    fn step(&mut self) {
        let payload = self.payload();
        let body = serde_json::to_vec(&payload).unwrap();
        let raw = coregeek::brain::decide_with(&mut self.state, &body).expect("decision");
        let response: Value = serde_json::from_str(&raw).expect("response parses");
        let map = response["roleCommandMap"]
            .as_object()
            .cloned()
            .unwrap_or_default();

        let turn = turn_from(payload);
        let footprint = turn
            .station()
            .map(|station| station.footprint().to_vec())
            .unwrap_or_default();
        let pairs = night::stable_pairs(&turn, &mut self.state);
        self.gates.push(gate_open_record(&turn, &pairs, &footprint));
        for unit in turn.controllable() {
            self.positions.push((self.round, unit.id, unit.pos));
        }

        for unit in turn.controllable() {
            let Some(cmd) = map.get(&unit.id.to_string()) else {
                continue;
            };
            if cmd["action"].as_str() != Some("move") {
                continue;
            }
            let Some(target) = cmd["targetPos"].as_array().and_then(|list| list.first()) else {
                continue;
            };
            let target = Pos {
                x: target["x"].as_i64().unwrap() as i32,
                y: target["y"].as_i64().unwrap() as i32,
            };
            let taken = self
                .units
                .iter()
                .any(|(id, pos)| *id != unit.id && *pos == target);
            if !taken {
                if let Some(entry) = self.units.iter_mut().find(|(id, _)| *id == unit.id) {
                    entry.1 = target;
                }
            }
        }
        self.round += 1;
    }

    /// Play the whole day and hand back the trace.
    fn play_day(template: Value) -> Self {
        let mut day = Day::new(template);
        while day.round <= DAY_END {
            day.step();
        }
        day
    }

    fn pos_at(&self, id: i64, round: i64) -> Option<Pos> {
        self.positions
            .iter()
            .find(|(seen, seen_id, _)| *seen == round && *seen_id == id)
            .map(|(_, _, pos)| *pos)
    }

    /// Was the gate free to close on `round`?
    fn sealed(&self, round: i64) -> bool {
        self.gates
            .get((round - 1) as usize)
            .map(Option::is_none)
            .unwrap_or(false)
    }

    fn outside_on(&self, round: i64) -> Vec<(i64, Pos)> {
        let footprint = self.footprint();
        self.positions
            .iter()
            .filter(|(seen, _, pos)| *seen == round && footprint_distance(*pos, &footprint) > 1)
            .map(|(_, id, pos)| (*id, *pos))
            .collect()
    }
}

/// Two workers and a pioneer, one gun: the third role has no tower to man, which
/// is the board every one of issues #111/#112/#115 played on day 1. Measured on
/// our own day-1 simulation, the third gun lands at R132 — day 2 round 2 — so
/// the whole of night 1 is fought two guns to three roles.
fn two_guns_board() -> Value {
    world(
        50,
        vec![
            station(),
            gatling(10020, 11, 22),
            role(10002, "worker", 20, 20),
            role(10003, "worker", 22, 26),
            role(10004, "pioneer", 18, 30),
        ],
        vec![
            zone(18, 24, "stone"),
            zone(24, 26, "vendor"),
            zone(22, 18, "weaponShop"),
        ],
    )
}

#[test]
fn every_role_is_inside_the_ring_before_nightfall() {
    // The board the analysis was measured on: two guns and three roles, so one
    // role has no tower to man and `preposition_round` never fires for it. It
    // used to spend the whole dusk window on a shopping trip (measured: settled
    // at the weapon shop issuing `buy Medicine` every round from r52 on) while
    // `wall_gate_open` named it — the gate is the last ring cell and it does not
    // close while any role is outside.
    let day = Day::play_day(two_guns_board());
    let footprint = day.footprint();

    let settled = (1..=DAY_END)
        .find(|round| day.outside_on(*round).is_empty())
        .expect("no round of the day had the whole crew inside the ring");

    // Inside before nightfall, and inside from then on — a role that steps back
    // out re-opens the gate it just closed.
    assert!(
        settled < DAY_END,
        "the crew only came inside on R{settled}, one round before the night"
    );
    for round in settled..=DAY_END {
        let outside = day.outside_on(round);
        assert!(
            outside.is_empty(),
            "the crew was inside on R{settled} and back outside on R{round}: \
             {outside:?} — the gate re-opens and the ring keeps a robot-sized \
             hole all night (#111/#112/#115: 15/15 open rounds)"
        );
    }
    let _ = footprint;
}

#[test]
fn the_dusk_gate_seals_inside_the_window_and_stays_sealed() {
    let day = Day::play_day(two_guns_board());
    let first_sealed = (COMMIT_IN_DAY + 1..=DAY_END + 20)
        .map(|in_day| in_day + 1)
        .find(|round| day.sealed(*round))
        .expect(
            "issue #111/#112/#115: the gate never sealed — every round of the \
             dusk window recorded `wall_gate_open`",
        );
    assert!(
        first_sealed <= DAY_END,
        "the gate only sealed at R{first_sealed}, after the day had ended"
    );
    for round in first_sealed..=DAY_END {
        assert!(
            day.sealed(round),
            "the gate re-opened on R{round} after sealing on R{first_sealed}: a \
             committed role left the ring again, which is the two-cell \
             oscillation §2.4 measured"
        );
    }
}

#[test]
fn the_walk_home_never_oscillates() {
    // The failure shape in the analysis is a position SEQUENCE, not a final
    // value: #111 day 1 had one role alternating between two cells for fifteen
    // consecutive rounds, and #115 day 1 had another walking away from the base
    // (x falling, y rising) for the whole window. The property that rules both
    // out is that the walk home is a PATH and not a CYCLE: from the round a
    // role's day outside the ring is over, it never returns to a cell it has
    // already left, and it ends inside the band the gate measures against.
    //
    // (Consecutive repeats are collapsed first: a role that has arrived and is
    // holding still legitimately reports the same cell every round.)
    let day = Day::play_day(two_guns_board());
    let footprint = day.footprint();
    for id in [10002i64, 10003, 10004] {
        let mut walked: Vec<Pos> = Vec::new();
        for round in (COMMIT_IN_DAY..DAY_END).map(|in_day| in_day + 1) {
            let Some(pos) = day.pos_at(id, round) else {
                continue;
            };
            if walked.last() == Some(&pos) {
                continue;
            }
            assert!(
                !walked.contains(&pos),
                "role {id} is back on {pos:?} on R{round}, a cell it already left \
                 this dusk — the two-cell oscillation §2.4 measured (`away` \
                 alternating between two cells for fifteen rounds). Path: {walked:?}"
            );
            walked.push(pos);
        }
        let ended = walked.last().copied().expect("the role has a position");
        assert!(
            footprint_distance(ended, &footprint) <= 1,
            "role {id} never reached the inside of the ring: ended {ended:?}, \
             {} cells out",
            footprint_distance(ended, &footprint)
        );
    }
}

#[test]
fn a_role_sealed_out_by_our_own_ring_still_gets_in() {
    // HARD CONSTRAINT: never trap a role outside the ring. The lock-in walks a
    // role home, and `retreat_inside` keeps `walk_or_remove_wall`'s demolition
    // escape hatch — a ring the crew closed over a role's head costs one wall
    // cell, not the role and not the whole night's gate.
    let footprint = station_footprint(Pos {
        x: STATION.0,
        y: STATION.1,
    });
    let mut ours = vec![station(), role(10002, "worker", 20, 20)];
    let mut index = 0;
    for x in -2..WIDTH + 2 {
        for y in -2..HEIGHT + 2 {
            let pos = Pos { x, y };
            if pos.x < 0 || pos.y < 0 || pos.x >= WIDTH || pos.y >= HEIGHT {
                continue;
            }
            if footprint.contains(&pos) || footprint_distance(pos, &footprint) != 2 {
                continue;
            }
            ours.push(wall(20000 + index, pos.x, pos.y));
            index += 1;
        }
    }
    // Three cells out on the west side, beside the ring — and the crew built the
    // ring's gate cell too, so there is no way back in.
    ours.push(role(10003, "worker", 7, 24));
    let turn = turn_from(world(60, ours, Vec::new()));
    let closed = turn.role_by_id(10003).expect("the outside role exists");
    assert!(
        footprint_distance(closed.pos, &footprint) > 1,
        "test setup: the role must start outside the ring"
    );
    let stands = coregeek::brain::interior_cells(&turn);
    assert!(
        !stands.is_empty() && !coregeek::brain::can_reach_any(&turn, closed, &stands),
        "test setup: a complete ring must actually cut every route home"
    );

    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);
    let cmd = plan
        .commands
        .get(&10003)
        .expect("a role sealed out of its own base must not stand still");
    assert_eq!(
        cmd.action, "remove",
        "the escape hatch is gone: the role paced outside instead of cutting \
         back in (got {cmd:?})"
    );
}

#[test]
fn the_dusk_commitment_is_latched_for_the_rest_of_the_day() {
    // The deadline is measured from where the role IS, so re-testing it every
    // round lets a role that has walked one cell closer fall back out of the
    // commitment, take a shop errand, and walk straight back out. `BotState::
    // dusk_home` is what makes it one commitment per role per day.
    let day = Day::play_day(two_guns_board());
    let footprint = day.footprint();
    let mut committed: HashSet<i64> = HashSet::new();
    for round in COMMIT_IN_DAY..DAY_END {
        for (_, id, pos) in day.positions.iter().filter(|(seen, _, _)| *seen == round) {
            let dist = footprint_distance(*pos, &footprint);
            if committed.contains(id) {
                assert!(
                    dist <= 1,
                    "role {id} committed to sheltering on an earlier round but \
                     is {dist} cells outside the ring on R{round} ({pos:?})"
                );
            }
            if dist <= 1 {
                committed.insert(*id);
            }
        }
    }
    assert!(
        !committed.is_empty(),
        "no role ever reached the ring, so the latch was never exercised"
    );
}

#[test]
fn the_build_site_does_not_depend_on_where_the_crew_is_standing() {
    // The two-cell oscillation has a second source, and it is the one that kept
    // the third gun off the board. `tower_gaps` tested a candidate site's
    // standing room against EVERY unit's footprint, the crew included — so a
    // site whose only standing cells are the cells a role happens to occupy
    // stopped being legal the moment that role walked toward it, and was legal
    // again the round after. On the day-1 board below (two guns, the wall line
    // as it stood at R40, three roles) the second worker at (9,23) — the cell
    // the trace shows it bouncing to — moved the chosen site from (10,22) to
    // (10,25), and walking there moved it back. Measured over the whole inner
    // band, three of the worker's cells flipped the choice; with the fix only
    // the one it is literally standing on does, which is the correct answer
    // (a gun cannot be built under a unit).
    let board = |worker_at: (i32, i32)| {
        let ours = vec![
            station(),
            gatling(10020, 12, 22),
            serde_json::json!({
                "id": 10021, "pos": {"x": 12, "y": 24}, "roleType": "railgun",
                "health": 1000, "attackPower": 10, "attackRange": 6,
                "level": 1, "backPackCapability": 0, "backpack": []
            }),
            role(10002, "worker", worker_at.0, worker_at.1),
            role(10003, "worker", 16, 24),
            role(10004, "pioneer", 12, 23),
            wall(30000, 13, 21),
            wall(30000 + 1, 13, 26),
            wall(30000 + 2, 13, 23),
            wall(30000 + 3, 13, 25),
            wall(30000 + 4, 13, 24),
            wall(30000 + 5, 12, 21),
            wall(30000 + 6, 12, 26),
            wall(30000 + 7, 10, 21),
            wall(30000 + 8, 11, 21),
            wall(30000 + 9, 11, 26),
            wall(30000 + 10, 9, 21),
            wall(30000 + 11, 10, 26),
            wall(30000 + 12, 8, 21),
            wall(30000 + 13, 9, 26),
            wall(30000 + 14, 8, 22),
            wall(30000 + 15, 8, 23),
            wall(30000 + 16, 8, 26),
            wall(30000 + 17, 8, 25),
        ];
        turn_from(world(40, ours, Vec::new()))
    };
    let state = BotState::default();
    let beside = tower_gaps(&board((9, 23)), &state);
    let away = tower_gaps(&board((12, 26)), &state);
    assert!(
        !beside.is_empty() && !away.is_empty(),
        "test setup: this board must still want a third gun, or the comparison \
         is vacuous"
    );
    assert_eq!(
        beside, away,
        "the tower site moved because a worker did — standing beside the site is \
         what makes it illegal, so the crew bounces between two cells around a \
         gun that is never built"
    );
}
