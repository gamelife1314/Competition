//! Issue #206 §6: the ring does not have to be complete.
//!
//! The owner, verbatim: 「围墙不一定得全部建造起来，后边的门，背向机器人的方向可以开着」
//! and 「城墙建造的顺序需要有个优先级保障……朝向敌人的三个方向城墙一定是完整的……0 的
//! 位置可以选择性缺口，有条件全部建造好」.
//!
//! So the ring has two halves that are not the same half:
//!
//!   * **the arc that faces the enemy** — 「一定是完整的」. A robot walks in
//!     through it, and no amount of economy justifies a hole there;
//!   * **the far shoulder** — 「可以选择性缺口」. It is where the door already
//!     is (the entrance is put on the economic side on purpose, see
//!     `tests/wall_facing_enemy.rs`), and a day that ends with it open has not
//!     failed, it has spent its stone where the robots are.
//!
//! 「有条件全部建造好」 is the other half of the rule and a real one: a crew with
//! stone to spare builds the whole ring. What the rule changes is what the day
//! OWES, never what it may build — `tests/day1_sim.rs` pins both ends of that
//! (the full ring still goes up on the standard board, and a board with no enemy
//! bearing owes every cell exactly as it did before).
//!
//! # The safety limit
//!
//! This is one place in the planner where a hole is deliberate, so every case
//! where it is NOT allowed is enumerated in `far_shoulder` and pinned here:
//! sectors the robots have already breached, a robot close enough to be at the
//! ring at all, and the last stretch before dusk — past `far_edge_cutoff` the
//! shoulder is owed again, because the ring is what holds the night.
//!
//! The build ORDER half of §6 (`route::build_order`) was already done and is
//! pinned by `tests/wall_facing_enemy.rs`; this file is about what the day
//! counts as owed. Nothing here touches `route.rs`.

use serde_json::{json, Value};

use coregeek::brain::day::{far_edge_cutoff, far_shoulder, ring_gap_split, wall_gaps};
use coregeek::brain::route::{gate_of, ring_cells};
use coregeek::model::{chebyshev, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::{arc_sector, BotState};

/// Our station, in a corner, and the enemy's, in the far one (任务书 §4.1) — the
/// board the bearing question actually gets asked on.
const BASE: (i32, i32) = (2, 3);
const ENEMY: (i32, i32) = (38, 20);
/// The stone the ring is fed from, on the far side of the base from the enemy,
/// so the errands and the bearing disagree.
const STONE: (i32, i32) = (0, 7);

fn role(id: i64, kind: &str, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": kind,
        "health": if kind == "station" { 1500 } else { 100 },
        "attackPower": 0, "attackRange": 0, "level": 1,
        "backPackCapability": if kind == "station" { 0 } else { 4 },
        "backpack": [],
    })
}

/// The crew the standard board starts with, minus the station.
fn crew() -> Vec<Value> {
    vec![
        role(10002, "worker", 6, 6),
        role(10003, "worker", 6, 7),
        role(10004, "pioneer", 7, 6),
    ]
}

/// One round's board. `robots` are the enemy's machines, which is what the
/// "never leave a hole something is standing at" rule reads; `walls` are ours.
fn board_of(round_no: i64, walls: &[Pos], robots: &[(i32, i32)], stone: bool) -> Turn {
    let mut roles = vec![role(10001, "station", BASE.0, BASE.1)];
    roles.extend(crew());
    for (n, cell) in walls.iter().enumerate() {
        roles.push(json!({
            "id": 40000 + n, "pos": {"x": cell.x, "y": cell.y},
            "roleType": "wall", "health": 1000, "attackPower": 0,
            "attackRange": 0, "level": 1, "backPackCapability": 0, "backpack": [],
        }));
    }
    if stone {
        // On a worker, not the station: `team_ores` sums `turn.controllable()`,
        // and a station is a building.
        roles[1]["backpack"] = json!(["stone"]);
    }
    let payload = json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": STONE.0, "y": STONE.1}, "neutralType": "stone"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": 75, "totalScore": 0,
            "playerTasks": [], "roles": roles,
        },
        "teamEnemy": {"roles": [role(20001, "station", ENEMY.0, ENEMY.1)]},
        "robot": {"roles": robots.iter().map(|(x, y)| json!({
            "id": 90000 + x, "pos": {"x": x, "y": y}, "type": "worker",
            "health": 100, "dizzy": false, "targetTeam": "enemy",
        })).collect::<Vec<_>>()},
    });
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn board(round_no: i64, robots: &[(i32, i32)]) -> Turn {
    board_of(round_no, &[], robots, false)
}

/// Day round `n` (round 1 is the day's round 0, so round `n + 1`).
fn day_round(n: i64) -> i64 {
    n + 1
}

fn sector_of(turn: &Turn, cell: Pos) -> usize {
    arc_sector(turn.station().expect("station").pos, cell)
}

/// The bearing opposite `sector` on the same 3×3 rose `arc_sector` lays out —
/// both signs flipped, so 8 (NE) answers 0 (SW).
fn opposite(sector: usize) -> usize {
    (2 - sector / 3) * 3 + (2 - sector % 3)
}

/// The eight bearings walked in order, so "the two either side of the enemy" can
/// be named without hard-coding which compass point the enemy happens to be on.
const ROSE: [usize; 8] = [0, 1, 2, 5, 8, 7, 6, 3];

/// The bearing `step` places along that walk.
fn ring_step(sector: usize, step: isize) -> usize {
    let at = ROSE
        .iter()
        .position(|bearing| *bearing == sector)
        .unwrap_or_else(|| panic!("{sector} is not a bearing on the rose")) as isize;
    ROSE[(at + step).rem_euclid(ROSE.len() as isize) as usize]
}

/// Every cell of the shell the day has anything to say about, owed or not:
/// `ring_gap_split` is a partition of the open shell, so this is the whole of
/// it and nothing else.
fn shell(turn: &Turn, state: &BotState) -> Vec<Pos> {
    let (owed, shoulder) = ring_gap_split(turn, state);
    owed.into_iter().chain(shoulder).collect()
}

/// The shell cell in `sector` — the finest-grained way to ask "does this
/// direction still owe stone" without restating the geometry.
fn cell_facing(turn: &Turn, state: &BotState, sector: usize) -> Pos {
    *shell(turn, state)
        .iter()
        .find(|cell| sector_of(turn, **cell) == sector)
        .unwrap_or_else(|| panic!("the shell has no open cell in sector {sector}"))
}

// ---------------------------------------------------------------------------
// What the day owes, and what it may leave
// ---------------------------------------------------------------------------

#[test]
fn the_enemy_facing_arc_is_owed_and_the_far_shoulder_is_not() {
    let turn = board(5, &[]);
    let state = BotState::default();
    let enemy = turn.enemy_station().expect("enemy station").pos;
    let near = sector_of(&turn, enemy);
    let far = opposite(near);

    // 「朝向敌人的三个方向城墙一定是完整的」: every open cell of the arc that
    // faces the enemy is owed, and none of it is written off. Asserted over the
    // cells that exist rather than over named compass points, because whether
    // the ring has a cell due north of a base in the corner is the shell's
    // business, not the rule's.
    let (owed, shoulder) = ring_gap_split(&turn, &state);
    let arc = [ring_step(near, -1), near, ring_step(near, 1)];
    let mut facing = 0;
    for cell in shell(&turn, &state) {
        if !arc.contains(&sector_of(&turn, cell)) {
            continue;
        }
        facing += 1;
        assert!(
            owed.contains(&cell),
            "{cell:?} on sector {} sits {} from the enemy at {enemy:?} and is \
             not owed: 「朝向敌人的三个方向城墙一定是完整的」",
            sector_of(&turn, cell),
            chebyshev(cell, enemy)
        );
        assert!(
            !shoulder.contains(&cell),
            "{cell:?} on sector {} was written off as an optional shoulder",
            sector_of(&turn, cell)
        );
    }
    assert!(
        facing >= 3,
        "test setup: the enemy's arc ({arc:?}) has only {facing} open cells"
    );

    // ...and the bearing away from the enemy is exactly the shoulder the owner
    // made optional.
    let away = cell_facing(&turn, &state, far);
    let (owed, shoulder) = ring_gap_split(&turn, &state);
    assert!(
        shoulder.contains(&away),
        "the cell at {away:?}, directly away from the enemy, is not optional: \
         the far shoulder is still counted as owed stone (owed={owed:?})"
    );
    assert!(
        !owed.contains(&away),
        "{away:?} faces away from the enemy and is in the owed list anyway"
    );

    // The split is a partition, not a filter: disjoint halves, and between them
    // every cell of the shell except the gate — the hole the day works through,
    // which `ring_gap_split` has always left out until the seal.
    for cell in &owed {
        assert!(!shoulder.contains(cell), "{cell:?} is in both halves");
    }
    let gate = gate_of(&turn, &state).expect("a ring with a gate");
    let footprint = station_footprint(turn.station().expect("station").pos);
    let open: Vec<Pos> = ring_cells(&turn, 2)
        .into_iter()
        .filter(|pos| turn.is_land(*pos) && *pos != gate)
        .filter(|pos| !turn.ours.iter().flat_map(|unit| unit.footprint()).any(|f| f == *pos))
        .collect();
    let mut split = owed.clone();
    split.extend(shoulder.iter().copied());
    split.sort_by_key(|pos| (pos.x, pos.y));
    let mut want = open;
    want.sort_by_key(|pos| (pos.x, pos.y));
    assert_eq!(
        split, want,
        "the two halves are not the shell: {} owed + {} optional vs {} open cells \
         (footprint {footprint:?})",
        owed.len(),
        shoulder.len(),
        want.len()
    );
}

#[test]
fn the_far_shoulder_is_not_stone_the_day_owes() {
    // The requirement as arithmetic: `stone_demand` is built out of `wall_gaps`,
    // and it is what sends a role to the mountain instead of the vendor. A
    // shoulder on that list while the ring it owes has not been started is a
    // team mining for a cell it was told it could skip.
    let turn = board(5, &[]);
    let state = BotState::default();
    let away = cell_facing(&turn, &state, opposite(sector_of(
        &turn,
        turn.enemy_station().expect("enemy station").pos,
    )));
    let gaps = wall_gaps(&turn, &state);
    assert!(
        !gaps.contains(&away),
        "the optional shoulder {away:?} is on the day's build list before the \
         ring it owes is even started (gaps={gaps:?})"
    );

    // The control that makes it a rule rather than a habit: with the enemy's
    // roles gone there is no bearing, nothing is far from anything, and the same
    // cell is owed again.
    let mut bare = board(5, &[]);
    bare.enemy.clear();
    let (owed, shoulder) = ring_gap_split(&bare, &BotState::default());
    assert!(
        shoulder.is_empty(),
        "a board with no enemy base still has an optional shoulder — with no \
         bearing to be far from, every cell faces it: {shoulder:?}"
    );
    assert!(
        owed.contains(&away),
        "the same cell, with no enemy to face, is not owed either"
    );
}

#[test]
fn a_robot_at_the_ring_makes_every_cell_owed_again() {
    // THE SAFETY LIMIT. A hole is a door while nothing is at it and a way in the
    // moment something is. The licence is withdrawn on the round a robot is
    // close enough to walk through it — read from the round every time, not
    // latched, so the cells come back the same round the robot does.
    let turn = board(5, &[]);
    let quiet = ring_gap_split(&turn, &BotState::default());
    assert!(
        !quiet.1.is_empty(),
        "test setup: the quiet board has no optional shoulder to withdraw"
    );

    let center = turn.station().expect("station").pos;
    let at_the_ring = Pos {
        x: center.x - 4,
        y: center.y,
    };
    let hot = board(5, &[(at_the_ring.x, at_the_ring.y)]);
    let (owed, shoulder) = ring_gap_split(&hot, &BotState::default());
    assert!(
        shoulder.is_empty(),
        "a robot {} cells from the base and the day is still planning to leave a \
         hole in its ring: {shoulder:?}",
        chebyshev(at_the_ring, center)
    );
    assert!(
        owed.len() > quiet.0.len(),
        "the withdrawn cells did not come back as owed"
    );
}

#[test]
fn a_sector_the_robots_have_breached_is_never_optional() {
    // Evidence beats the compass. `threatened_sectors` is the arc the walls have
    // actually been hit on, so a sector on it is enemy-facing however the
    // enemy's base happens to lie — a robot that walked round the back is at the
    // back.
    let turn = board(5, &[]);
    let state = BotState::default();
    let far = opposite(sector_of(
        &turn,
        turn.enemy_station().expect("enemy station").pos,
    ));
    let cell = cell_facing(&turn, &state, far);
    assert!(
        ring_gap_split(&turn, &state).1.contains(&cell),
        "test setup: {cell:?} is not optional to begin with"
    );
    let mut state = BotState::default();
    state.threat_sectors[far] = 25;
    assert!(
        !ring_gap_split(&turn, &state).1.contains(&cell),
        "sector {far} took wall damage and the day is still treating {cell:?} as \
         an optional shoulder"
    );
}

#[test]
fn the_last_stretch_before_dusk_owes_the_whole_ring_again() {
    // 「可以开着」 is a licence to leave the far side for last, not to leave it
    // for the night. The cutoff is measured back from dusk by the walk home, so
    // the crew that has to close it still has the rounds to.
    let state = BotState::default();
    let before = board(day_round(far_edge_cutoff() - 1), &[]);
    assert!(
        !ring_gap_split(&before, &state).1.is_empty(),
        "test setup: the shoulder is not optional before the cutoff either"
    );
    let after = board(day_round(far_edge_cutoff()), &[]);
    assert_eq!(
        after.in_day_round,
        far_edge_cutoff(),
        "test setup: this board is not on the cutoff round"
    );
    let (owed, shoulder) = ring_gap_split(&after, &state);
    assert!(
        shoulder.is_empty(),
        "at day round {} the far shoulder is still optional with dusk at {} and \
         the ring still to close: {shoulder:?}",
        far_edge_cutoff(),
        coregeek::brain::economy::DUSK_ROUND
    );
    assert!(
        !owed.is_empty(),
        "the shoulder is owed again at the cutoff, so it has to be in the owed \
         list — a cell in neither list is a cell the crew never builds"
    );
    // And the same answer from the predicate the planner routes through, so the
    // cutoff cannot be right in one place and wrong in the other.
    let far = cell_facing(&before, &state, opposite(sector_of(
        &before,
        before.enemy_station().expect("enemy station").pos,
    )));
    assert!(
        !far_shoulder(&after, &state, far),
        "{far:?} is optional at the cutoff by one reading and owed by the other"
    );
    assert!(far_shoulder(&before, &state, far), "test setup");
}

#[test]
fn the_shoulder_is_the_days_work_only_after_the_arc_is_done() {
    // 「有条件全部建造好」 and 「一定是完整的」, as one ordering. The shoulder is
    // never on the build list while a cell that faces the enemy is open — that is
    // what keeps it out of `stone_demand`, and `stone_demand` is what sends a role
    // to the mountain instead of the vendor. Once the arc is done the shoulder IS
    // the list, which is the only way 「有条件全部建造好」 can ever happen: a cell
    // that is never asked for is a cell that is never built.
    let state = BotState::default();
    let owed = ring_gap_split(&board(5, &[]), &state).0;
    assert!(!owed.is_empty(), "test setup: the arc is already closed");

    // Half the arc walled: the shoulder is still not the day's work.
    let half = board_of(5, &owed[..owed.len() / 2], &[], false);
    let gaps = wall_gaps(&half, &state);
    assert!(
        !gaps.is_empty(),
        "test setup: the arc's own gaps are not on the build list"
    );
    for cell in &ring_gap_split(&half, &state).0 {
        assert!(
            gaps.contains(cell),
            "{cell:?} faces the enemy and is not on the build list while the \
             arc is half-built (gaps={gaps:?})"
        );
    }
    for cell in ring_gap_split(&half, &state).1 {
        assert!(
            !gaps.contains(&cell),
            "the optional shoulder {cell:?} is on the build list while the arc \
             that faces the enemy is still open (gaps={gaps:?})"
        );
    }

    // ...and with the arc finished, the shoulder is what the day has left to do.
    let full = board_of(5, &owed, &[], false);
    let (left_owed, shoulder) = ring_gap_split(&full, &state);
    assert!(left_owed.is_empty(), "test setup: the arc is not walled");
    assert!(!shoulder.is_empty(), "test setup: there is no shoulder left");
    assert_eq!(
        {
            let mut gaps = wall_gaps(&full, &state);
            gaps.sort_by_key(|pos| (pos.x, pos.y));
            gaps
        },
        {
            let mut want = shoulder.clone();
            want.sort_by_key(|pos| (pos.x, pos.y));
            want
        },
        "the arc is complete and the far shoulder is still not the day's work — \
         it is then never built at all, which is 「有条件全部建造好」 failing"
    );
}
