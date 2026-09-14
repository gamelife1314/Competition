//! Night repair that can fire while the wall is under attack, and the dawn
//! that is not a kill (issues #131-#135).
//!
//! **1. The night mend was unreachable.** `cooldown_repair` mends from a paired
//! controller on a reload round; every other mend call at night went through
//! `combat::repair_target(turn, role, 3)`, whose safety test is measured from
//! the WALL — `!robots_near(wall.pos)` with `chebyshev(robot, wall) < 3`. A wall
//! the wave is chewing on has robots beside it by construction, so that
//! predicate is false for exactly the walls the night is losing. Measured over
//! the batch: `ourWallLost` 2465-15135 a night, the base behind the ring falling
//! on night 1-3 in four of five matches, and 表 2a showing the first `WallFixer`
//! bought on day 2 at round 147-154 — after the night that mattered. Net: zero
//! wall HP restored on night 1 in all five.
//!
//! `combat::night_mend_target` is the night's rule: the danger is measured
//! against the OPERATOR (not being meleed), which is the thing that can be
//! killed, and which leaves a wall being shelled from two or three cells out
//! repairable. A `WallFixer` restores its target to FULL for 10 gold — the
//! cheapest HP on the board by a wide margin.
//!
//! **2. Dawn is not a kill.** 任务书 4.7.3 clears the surviving wave on the first
//! day round. `log_round` counted that removal as a kill, so `cum_kill_score`
//! grew by one night's survivors every morning and `residual` —
//! `total - kill - survival`, the one column `abreport` reads as "score_1 plus
//! estimate error" — went to -106 … -409. 表 8 of the five matches carries those
//! numbers, and they were read for five batches as "the task line is losing us
//! 400 points": impossible, because 任务书 ch.6 pays `score_1 = 奖励 × 通过率`
//! and neither factor is ever negative.

use serde_json::{json, Value};

use coregeek::brain::combat::{night_mend_target, repair_target};
use coregeek::brain::night;
use coregeek::model::{station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

const STATION: (i32, i32) = (10, 20);
/// Inside the ring: footprint distance 1 from the station's own four cells, so
/// `interior_cells` holds it and the spare duty does not spend the round
/// walking home.
const INSIDE: Pos = Pos { x: 12, y: 20 };
/// A cell on the radius-2 wall line, adjacent to [`INSIDE`].
const RING: Pos = Pos { x: 13, y: 20 };

fn station() -> Value {
    json!({
        "id": 10001, "pos": {"x": STATION.0, "y": STATION.1}, "roleType": "station",
        "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn worker(id: i64, pos: Pos, items: Vec<&str>) -> Value {
    json!({
        "id": id, "pos": {"x": pos.x, "y": pos.y}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 100, "backpack": items
    })
}

fn wall(id: i64, pos: Pos, health: i64) -> Value {
    json!({
        "id": id, "pos": {"x": pos.x, "y": pos.y}, "roleType": "wall",
        "health": health, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn robot(id: i64, pos: Pos) -> Value {
    json!({
        "id": id, "pos": {"x": pos.x, "y": pos.y}, "roleType": "smallRobot",
        "health": 40, "abnormalState": "", "targetTeam": "challenger"
    })
}

fn board(round: i64, roles: Vec<Value>, robots: Vec<Value>) -> Value {
    json!({
        "roundNo": round,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {"type": "challenger", "goldNum": 0, "totalScore": 0,
                    "playerTasks": [], "roles": roles},
        "teamEnemy": {"roles": []},
        "robot": {"roles": robots},
    })
}

/// A night board: station, one damaged ring wall, the operator inside.
fn night_board(robots: Vec<Value>) -> Value {
    board(
        71,
        vec![
            station(),
            wall(40001, RING, 300),
            worker(10010, INSIDE, vec!["WallFixer"]),
        ],
        robots,
    )
}

#[test]
fn a_wall_being_shelled_is_repairable_at_night_but_not_by_the_day_rule() {
    // The robot sits two cells from the wall and four from the operator: it is
    // chewing the ring, and it is not on top of the mender. That is the whole
    // night, every night, and it is exactly the board the old rule refused.
    let turn = turn_from(night_board(vec![robot(90001, Pos { x: 15, y: 20 })]));
    let role = turn.role_by_id(10010).expect("the operator is on the board");

    assert_eq!(
        repair_target(&turn, role, 3),
        None,
        "the day rule measures danger from the WALL, so a wall under fire is \
         never repairable — this is the defect the batch measured"
    );
    assert_eq!(
        night_mend_target(&turn, role),
        Some(RING),
        "the night rule measures danger from the OPERATOR, which is four cells \
         from that robot and in no danger"
    );
}

#[test]
fn a_mender_being_meleed_does_not_stop_to_patch_the_wall() {
    // One cell from the operator: it is being hit, and that round belongs to
    // the heal/withdraw rules, not to the mason's.
    let turn = turn_from(night_board(vec![robot(90001, Pos { x: 13, y: 21 })]));
    let role = turn.role_by_id(10010).expect("the operator is on the board");
    assert_eq!(night_mend_target(&turn, role), None);
}

#[test]
fn a_full_health_wall_is_not_worth_a_kit() {
    // 10 gold buys a full restore, so a wall already at `wall_max_hp(1)` = 1000
    // is 10 gold for nothing — the check the errand would otherwise spend the
    // team's scarcest resource on.
    let mut roles = vec![station(), wall(40001, RING, 1000), worker(10010, INSIDE, vec!["WallFixer"])];
    roles.push(wall(40002, Pos { x: 13, y: 21 }, 1000));
    let turn = turn_from(board(71, roles, vec![]));
    let role = turn.role_by_id(10010).expect("the operator is on the board");
    assert_eq!(night_mend_target(&turn, role), None);
}

#[test]
fn a_mender_without_a_kit_is_not_a_mender() {
    let mut roles = vec![station(), wall(40001, RING, 300), worker(10010, INSIDE, vec![])];
    roles.push(wall(40002, Pos { x: 13, y: 21 }, 1000));
    let turn = turn_from(board(71, roles, vec![]));
    let role = turn.role_by_id(10010).expect("the operator is on the board");
    assert_eq!(night_mend_target(&turn, role), None);
}

#[test]
fn the_spare_role_spends_the_night_round_mending_the_ring() {
    // The end-to-end shape: no towers on the board, so the operator is a SPARE,
    // it is already standing on an `interior_cells` stand (so shelter issues no
    // walk) — and the round it used to spend on nothing is now a mend. This is
    // the round that was silently dropped in all five matches: the paired
    // controller only mends on a reload round, and 表 2a shows the kits did not
    // exist before day 2 anyway.
    let turn = turn_from(night_board(vec![robot(90001, Pos { x: 15, y: 20 })]));
    let mut state = BotState::default();
    let plan = night::plan(&turn, &mut state);

    let cmd = plan
        .commands
        .get(&10010)
        .expect("the spare has something to do with a damaged ring and a kit");
    assert_eq!(cmd.action, "use", "the mend is an item use, not a move");
    assert_eq!(cmd.name.as_deref(), Some("WallFixer"));
    assert_eq!(
        cmd.targetPos.as_ref().and_then(|list| list.first()).copied(),
        Some(RING),
        "and it is aimed at the damaged cell of the ring"
    );
}

#[test]
fn a_spare_with_nothing_to_mend_still_shelters_first() {
    // No regression on the no-night-mining rule: the walk home still outranks
    // the errand, so a spare that is OUTSIDE the ring moves in this round and
    // only becomes a mason once it is standing inside.
    let far = Pos { x: 4, y: 20 };
    let mut roles = vec![station(), wall(40001, RING, 300), worker(10010, far, vec!["WallFixer"])];
    roles.push(wall(40002, Pos { x: 13, y: 21 }, 1000));
    let turn = turn_from(board(71, roles, vec![robot(90001, Pos { x: 15, y: 20 })]));
    let mut state = BotState::default();
    let plan = night::plan(&turn, &mut state);

    let cmd = plan.commands.get(&10010).expect("the spare moves");
    assert_eq!(cmd.action, "move", "shelter owns the round until it is home");
    let step = cmd.targetPos.as_ref().expect("a move is aimed")[0];
    assert!(
        step.x > far.x,
        "and the step is toward the station, not toward the wall: {step:?}"
    );
}

#[test]
fn the_dawn_clear_is_not_scored_as_a_kill() {
    // 任务书 4.7.3: "黑夜结束后，在第二天早上的第一个回合，残余机器人自动清除".
    // Two rounds: the last night round leaves two robots alive, the first day
    // round finds the board cleared. The judger's total does not move, so a
    // `kill` that jumps here is phantom — and it lands in `residual`, the
    // column the analysis workflow reads as the task score.
    use coregeek::brain::decide_with;

    let mut roles = vec![station(), worker(10010, INSIDE, vec![])];
    roles.push(worker(10011, Pos { x: 12, y: 19 }, vec![]));
    let night = board(130, roles.clone(), vec![robot(90001, Pos { x: 30, y: 5 }), robot(90002, Pos { x: 31, y: 6 })]);
    let dawn = board(131, roles, vec![]);

    let mut state = BotState::default();
    decide_with(&mut state, &night.to_string().into_bytes()).expect("night decides");
    let before = state.cum_kill_score;
    decide_with(&mut state, &dawn.to_string().into_bytes()).expect("dawn decides");

    assert_eq!(
        state.cum_kill_score, before,
        "the wave the dawn cleared was not shot, and 任务书 4.7.2 scores 击杀数"
    );
}

#[test]
fn a_robot_that_really_died_before_dawn_is_still_a_kill() {
    // The narrow exit: the same two robots, but they leave the board on a NIGHT
    // round. That is a kill and must keep scoring, or the fix would trade one
    // wrong attribution for another.
    use coregeek::brain::decide_with;

    let roles = vec![
        station(),
        worker(10010, INSIDE, vec![]),
        worker(10011, Pos { x: 12, y: 19 }, vec![]),
    ];
    let with = board(129, roles.clone(), vec![robot(90001, Pos { x: 30, y: 5 }), robot(90002, Pos { x: 31, y: 6 })]);
    let without = board(130, roles, vec![]);

    let mut state = BotState::default();
    decide_with(&mut state, &with.to_string().into_bytes()).expect("round 129 decides");
    let before = state.cum_kill_score;
    decide_with(&mut state, &without.to_string().into_bytes()).expect("round 130 decides");

    assert_eq!(
        state.cum_kill_score,
        before + 2,
        "two small robots at 1 point each, on a night round"
    );
}

#[test]
fn the_ring_cell_used_here_really_is_on_the_ring() {
    // Guards the fixture, not the code: `RING` must be a radius-2 cell adjacent
    // to an interior cell, or every assertion above is about a different board.
    let turn = turn_from(night_board(vec![]));
    let footprint = station_footprint(turn.station().expect("station").pos);
    assert_eq!(
        coregeek::model::footprint_distance(RING, &footprint),
        2,
        "{RING:?} has to be on the wall line"
    );
    assert_eq!(
        coregeek::model::footprint_distance(INSIDE, &footprint),
        1,
        "{INSIDE:?} has to be an interior stand"
    );
    assert_eq!(coregeek::model::chebyshev(INSIDE, RING), 1);
}

/// A tower that can fire from `(12,21)`, its operator one cell inside the ring,
/// a damaged ring cell beside the operator, and a robot far away hunting us.
fn manned_ring_board(robot_pos: Option<Pos>) -> Value {
    let mut roles = vec![
        station(),
        json!({"id": 20020, "pos": {"x": 12, "y": 21}, "roleType": "gatling",
               "health": 1000, "attackPower": 10, "attackRange": 3, "level": 1,
               "cooldown": 0, "backPackCapability": 0, "backpack": []}),
        wall(40001, Pos { x: 13, y: 20 }, 300),
        worker(10010, Pos { x: 12, y: 20 }, vec!["WallFixer"]),
    ];
    roles.push(wall(40002, Pos { x: 10, y: 21 }, 1000));
    let robots = robot_pos.into_iter().map(|pos| robot(90001, pos)).collect();
    board(71, roles, robots)
}

#[test]
fn a_ready_gun_with_no_shot_worth_taking_mends_the_ring_instead() {
    // `表 6a` of issues #131-#135 makes `no_target_reserved_for_robots` the
    // second-largest silence bucket in the batch after `fired` — 42, 41, 30, 27
    // and 18 tower-rounds a match. In every one of those rounds the trigger had
    // already come up empty for that gun (the robots hunting us were outside
    // every ready tower's reach), so nothing is traded away by mending: the
    // tower fires the instant a target exists, and until then the operator is
    // standing beside a `WallFixer` and a wall the night is losing.
    let turn = turn_from(manned_ring_board(Some(Pos { x: 30, y: 5 })));
    let mut state = BotState::default();
    let plan = night::plan(&turn, &mut state);

    let cmd = plan
        .commands
        .get(&10010)
        .expect("the operator has a kit and a damaged ring cell beside it");
    assert_eq!(cmd.action, "use");
    assert_eq!(cmd.name.as_deref(), Some("WallFixer"));
    assert_eq!(
        cmd.targetPos.as_ref().and_then(|list| list.first()).copied(),
        Some(Pos { x: 13, y: 20 })
    );
    assert!(
        !plan.commands.contains_key(&20020),
        "the gun still does not fire: there is no target and the opportunistic \
         shot is still held back"
    );
}

#[test]
fn a_gun_with_a_target_still_shoots_instead_of_mending() {
    // The gate, unchanged: a round the gun CAN fire is a round it fires. The
    // same board with the robot inside the gatling's three-cell reach must
    // produce an attack and no mend at all.
    let turn = turn_from(manned_ring_board(Some(Pos { x: 14, y: 21 })));
    let mut state = BotState::default();
    let plan = night::plan(&turn, &mut state);

    let shot = plan.commands.get(&20020).expect("a target in range is a shot");
    assert_eq!(shot.action, "attack");
    assert!(
        plan.commands.get(&10010).map(|cmd| cmd.action.as_str()) != Some("use"),
        "the kit waits for a round the gun cannot use"
    );
}
