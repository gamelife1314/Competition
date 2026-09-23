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
use coregeek::model::{chebyshev, footprint_distance, station_footprint, Turn};
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
        // The same owned list `orchestrate::plan` computes: the economy worker
        // sleeps outside the ring by design (issue #221 comment 1 §6), so the
        // production seal never waits on it — the harness must recompute the
        // record the same way or it measures a gate nobody is waiting for.
        let owned: Vec<i64> =
            coregeek::brain::role::Roles::of(&turn).economy_worker.into_iter().collect();
        self.gates.push(gate_open_record(&turn, &pairs, &footprint, &owned));
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
    //
    // Issue #221 exempts ONE role from this roll call: the economy worker
    // (10003) sleeps outside among the veins by design (comment 1 §6 —
    // distance is its only defence), and the owned-aware seal never waits on
    // it. The wall crew and the pioneer still must be home before nightfall,
    // and stay home — a role that steps back out re-opens the gate it just
    // closed.
    const ECONOMY: i64 = 10003;
    let day = Day::play_day(two_guns_board());
    let footprint = day.footprint();
    let outside = |round: i64| {
        day.outside_on(round)
            .into_iter()
            .filter(|(id, _)| *id != ECONOMY)
            .collect::<Vec<_>>()
    };

    let settled = (1..=DAY_END)
        .find(|round| outside(*round).is_empty())
        .expect("no round of the day had the homebound crew inside the ring");

    assert!(
        settled < DAY_END,
        "the crew only came inside on R{settled}, one round before the night"
    );
    for round in settled..=DAY_END {
        let outside = outside(round);
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
    // Issue #221: the economy worker (10003) has no walk home — it sleeps
    // outside by design (comment 1 §6), so the path-not-cycle property is
    // measured on the roles that DO come home. B's own anti-oscillation
    // posture is the camp (`camp_at_gap`), pinned in `economy_worker_b.rs`
    // and on the `day1_sim` spare board.
    let day = Day::play_day(two_guns_board());
    let footprint = day.footprint();
    for id in [10002i64, 10004] {
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

/// `HARD_SEAL_ROUND` in `brain::day`: the day-round past which the gate is
/// sealed whether or not the crew is home.
const HARD_SEAL_IN_DAY: i64 = coregeek::brain::economy::DUSK_ROUND + 11;

/// A complete ring around the station with one role locked outside it.
fn ring_with_a_role_locked_out() -> Value {
    let footprint = station_footprint(Pos {
        x: STATION.0,
        y: STATION.1,
    });
    let mut ours = vec![
        station(),
        gatling(10020, 11, 22),
        role(10002, "worker", 12, 24),
        role(10004, "pioneer", 10, 23),
    ];
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
    // Nine cells out on the west side, and the ring is complete: there is no
    // route home and the night recall can only cut one.
    ours.push(role(10003, "worker", 1, 24));
    world(1, ours, Vec::new())
}

#[test]
fn the_ring_is_closed_even_when_a_role_never_makes_it_home() {
    // Issues #121-#125: across five matches the gate never sealed on ANY day
    // after the first — `wall_gate_open` for 5, 7, 10, 11 and 15 of the fifteen
    // dusk rounds (#121 day 2, #122 day 2, #124 day 2, #123 day 2, #125 days 1
    // and 2), the last of those being the whole window. The seal waits for
    // every role, the day ends, and nothing revisits it: the ring then keeps a
    // robot-sized hole for the whole night, every night, while `score_3` pays
    // 10×day for a station that is still standing.
    let mut day = Day::new(ring_with_a_role_locked_out());
    let mut sealed_at: Option<i64> = None;
    while day.round <= DAY_END {
        let round = day.round;
        day.step();
        if day.state.wall_gate_sealed && sealed_at.is_none() {
            sealed_at = Some(round);
        }
    }
    // Issue #221 moved the deadline's meaning: the locked-out role is the
    // economy worker, and B sleeps outside BY DESIGN (comment 1 §6) — the
    // owned-aware seal does not wait for it at all, so the ring now closes at
    // the first dusk checkpoint instead of being forced at the deadline. What
    // must still never happen (#121-#125) is the seal not landing: a ring
    // that stays open all night is the hole the night walks through.
    assert!(
        matches!(sealed_at, Some(at) if at < HARD_SEAL_IN_DAY + 1),
        "the gate never sealed ahead of its deadline while the economy worker \
         was locked out: sealed_at = {sealed_at:?}"
    );
}

#[test]
fn a_far_role_is_home_before_the_day_ends() {
    // The measured shape of #122 and #124 day 2: a worker was still nineteen to
    // twenty cells out when the dusk window opened, walked one cell per round
    // for the whole of it, and `wall_gate_open` named it on the LAST day round
    // — the gate sealed a round too late to matter. `dusk_recall_round` was a
    // flat `DUSK_ROUND - 8`, which is the right lead only for a role one
    // ordinary walk from home.
    let far = world(
        1,
        vec![
            station(),
            gatling(10020, 11, 22),
            // The odd one out: three roles, one gun, so this worker has no
            // tower to pre-position at and `preposition_round` never fires.
            role(10002, "worker", 38, 2),
            role(10004, "pioneer", 10, 23),
        ],
        vec![zone(36, 4, "stone")],
    );
    let day = Day::play_day(far);
    let last = day
        .outside_on(DAY_END)
        .into_iter()
        .filter(|(id, _)| *id == 10002)
        .collect::<Vec<_>>();
    assert!(
        last.is_empty(),
        "the far worker was still outside the ring on the last day round: \
         {last:?} — the gate it holds open is the one the night comes through"
    );
}

/// The cell `brain::day::wall_gate` designates: `(xmax + 2, ymin - 1)` of the
/// station footprint.
fn gate_cell() -> Pos {
    let footprint = station_footprint(Pos {
        x: STATION.0,
        y: STATION.1,
    });
    Pos {
        x: footprint.iter().map(|pos| pos.x).max().unwrap() + 2,
        y: footprint.iter().map(|pos| pos.y).min().unwrap() - 1,
    }
}

#[test]
fn the_day_two_gate_is_a_door_the_stone_crew_can_close() {
    // Between the night's seal and the morning's `open_door` there is exactly
    // one hole in the ring, and on day 2+ it is the designated gate. This is
    // the day-2 half of the batch's defect: `wall_gate_open` named a straggler
    // on 5-15 rounds of the dusk window in every one of issues #121-#125, and
    // the ring kept that hole for the night.
    //
    // The property guarded here is the outcome, not one branch of it: at the
    // first dusk round of day 2, with the crew home and stone in a backpack
    // beside the gate, the gate cell is built.
    let gate = gate_cell();
    let footprint = station_footprint(Pos {
        x: STATION.0,
        y: STATION.1,
    });
    assert_eq!(
        footprint_distance(gate, &footprint),
        2,
        "test setup: the designated gate must be a ring cell"
    );
    let mut ours = vec![
        station(),
        gatling(10020, 11, 22),
        // Inside, with stone, next to the gate.
        json!({
            "id": 10002, "pos": {"x": gate.x - 1, "y": gate.y}, "roleType": "worker",
            "health": 220, "attackPower": 0, "attackRange": 0,
            "level": 1, "backPackCapability": 100,
            "backpack": ["stone", "stone", "stone", "stone", "stone", "stone"]
        }),
        role(10004, "pioneer", 10, 23),
    ];
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
            if pos == gate {
                continue; // the hole the branch is supposed to close
            }
            ours.push(wall(20000 + index, pos.x, pos.y));
            index += 1;
        }
    }
    // Day 2, in-day 55: the first round of the dusk window.
    let turn = turn_from(world(130 + 55, ours, Vec::new()));
    assert_eq!(turn.day, 2, "test setup: this is the day-2 dusk");
    assert_eq!(turn.in_day_round, 55);
    assert!(!turn.walls().iter().any(|unit| unit.pos == gate));

    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);
    let cmd = plan
        .commands
        .get(&10002)
        .expect("the stone carrier must be given a command at dusk");
    assert_eq!(
        (cmd.action.as_str(), cmd.targetPos.clone()),
        ("build", Some(vec![gate])),
        "the day-2 dusk left the gate open (got {cmd:?}): the ring keeps a \
         robot-sized hole for the whole night"
    );
}

#[test]
fn two_equidistant_doors_are_closed_in_coordinate_order() {
    // `door_cells` is a `HashSet`. `worker_day` step 3b sorts the day's doors
    // by the walk from the carrier and then takes the first one it can build —
    // so when two doors are the same distance away, the sort key alone leaves
    // the choice to hash iteration, and the same board yields a different plan
    // on a different run. The planner's tiebreak everywhere else is the
    // coordinate pair, and the door list must use it too.
    //
    // `the_day_two_gate_is_a_door_the_stone_crew_can_close` sets this board up
    // with one hole; here the gate (13,22) has a twin at (11,21), both exactly
    // one step from the carrier. The lexicographically smaller one must win.
    let gate = gate_cell();
    let twin = Pos { x: 13, y: 23 };
    let other = Pos { x: 11, y: 21 };
    let footprint = station_footprint(Pos {
        x: STATION.0,
        y: STATION.1,
    });
    for door in [gate, twin, other] {
        assert_eq!(
            footprint_distance(door, &footprint),
            2,
            "test setup: {door:?} must be a ring cell"
        );
    }
    let carrier = Pos {
        x: gate.x - 1,
        y: gate.y,
    };
    assert_eq!(
        chebyshev(carrier, twin),
        chebyshev(carrier, other),
        "test setup: the two doors must be equidistant from the carrier"
    );
    assert!(other.x < twin.x, "test setup: `other` sorts first");

    let mut ours = vec![
        station(),
        gatling(10020, 11, 22),
        json!({
            "id": 10002, "pos": {"x": carrier.x, "y": carrier.y}, "roleType": "worker",
            "health": 220, "attackPower": 0, "attackRange": 0,
            "level": 1, "backPackCapability": 100,
            "backpack": ["stone", "stone", "stone", "stone", "stone", "stone"]
        }),
        role(10004, "pioneer", 10, 23),
    ];
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
            if pos == twin || pos == other {
                continue; // both holes belong to the branch under test
            }
            ours.push(wall(20000 + index, pos.x, pos.y));
            index += 1;
        }
    }
    let turn = turn_from(world(130 + 55, ours, Vec::new()));
    assert_eq!(turn.day, 2, "test setup: this is the day-2 dusk");
    assert_eq!(turn.in_day_round, 55);

    // One fresh `BotState` per attempt: Rust seeds every `HashSet` instance
    // separately, so a 32-run sweep is what turns "usually the same" into
    // "always the same". Without the tiebreak the choice is roughly a coin
    // flip per run, and one differing run fails this test.
    let mut targets: Vec<Option<Vec<Pos>>> = Vec::new();
    for _ in 0..32 {
        let mut state = BotState::default();
        state.door_cells.insert(twin);
        state.door_cells.insert(other);
        let plan = day_plan(&turn, &mut state);
        targets.push(
            plan.commands
                .get(&10002)
                .and_then(|cmd| cmd.targetPos.clone()),
        );
    }
    assert!(
        targets.iter().all(|target| target == &targets[0]),
        "the same board closed a different door on different runs: {targets:?}"
    );
    assert_eq!(
        targets[0],
        Some(vec![other]),
        "two equidistant doors must be closed in coordinate order, smallest first"
    );
}

// ---------------------------------------------------------------------------
// The dusk hold (issues #176-#185)
// ---------------------------------------------------------------------------

/// A corridor to the station with exactly one gap in it: walls down the whole
/// height of the map at x=16 except (16,24), and a worker out at (20,24). The
/// only route from the worker to the station's inner band runs through (16,24),
/// which makes one reserved cell enough to close the day's only way home.
fn corridor_board() -> Value {
    let mut ours = vec![station(), role(10002, "worker", 20, 24)];
    let mut index = 0;
    for y in 0..HEIGHT {
        if y == 24 {
            continue;
        }
        ours.push(wall(20000 + index, 16, y));
        index += 1;
    }
    world(60, ours, Vec::new())
}

/// The dusk return must never answer "hold" (issues #176-#185).
///
/// `walk_or_remove_wall` holds — issues NO command — when `walk_toward` finds
/// nothing but a route exists once this round's claims are ignored. That is a
/// sound mid-day trade and a fatal one at a deadline, because at a deadline the
/// role holding the claim is walking home too and stops on the cell it took.
///
/// The ten reports measure the result: a role frozen on one cell for eleven or
/// twelve consecutive dusk rounds, named by `wall_gate_open` every round with an
/// EMPTY `stuck` list — and an empty `stuck` is exactly this branch's guard,
/// since `stuck` is filled by `can_reach_any`, the pathfinder with every claim
/// dropped. 179 freezes 20012 on (28,10) for eleven rounds; 178 freezes 20011 on
/// (24,16) for twelve and 20012 for eleven and then `wall_gate_forced` seals the
/// ring with both outside. Eight of the ten leave the gate open for 5-12 of the
/// fifteen dusk rounds, and the gate is the hole the night walks through.
#[test]
fn a_reserved_cell_does_not_park_a_role_at_dusk() {
    let turn = turn_from(corridor_board());
    let role = turn.role_by_id(10002).expect("the worker is on the board");
    let stands = coregeek::brain::interior_cells(&turn);
    assert!(!stands.is_empty(), "the station has an inner band");

    // A teammate has reserved the one gap for this round.
    let reserved = Pos { x: 16, y: 24 };
    let mut claimed: HashSet<Pos> = HashSet::new();
    claimed.insert(reserved);

    assert!(
        coregeek::brain::walk_or_remove_wall(&turn, role, &stands, &mut claimed).is_none(),
        "precondition: with the gap reserved, `walk_or_remove_wall` holds"
    );

    // The dusk return takes the step anyway. Reverting the second rung of
    // `walk_home_or_reroute` makes this return `None` and the role stand still
    // for the rest of the window, which is the eleven frozen rounds above.
    let mut claimed: HashSet<Pos> = HashSet::new();
    claimed.insert(reserved);
    let cmd = coregeek::brain::day::walk_home_or_reroute(&turn, role, &stands, &mut claimed)
        .expect("a dusk deadline is not a reason to stand still");
    assert_eq!(cmd.action, "move", "the answer is a step, not a demolition");
    let target = cmd
        .targetPos
        .as_ref()
        .and_then(|list| list.first())
        .expect("a move carries a destination");
    // The step must close on the gap at (16,24). The A* tie-break does NOT
    // promise WHICH of the equal-cost first steps it returns — (19,24) and
    // (19,23) both reach the gap in four rounds from (20,24) — so assert the
    // property this fix is about (a real step that closes on the gap) rather
    // than pinning one of the tied cells. The guard against a regression is the
    // `.expect(...)` above: reverting the second rung of `walk_home_or_reroute`
    // makes it return `None` and the role stand still, which panics there.
    assert_eq!(
        chebyshev(role.pos, *target),
        1,
        "the answer is a single step, not a teleport"
    );
    assert!(
        chebyshev(*target, reserved) < chebyshev(role.pos, reserved),
        "and the step genuinely closes on the gap the crew is holding"
    );
}
