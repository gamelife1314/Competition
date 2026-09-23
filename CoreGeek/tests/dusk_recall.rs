//! The pioneer's dusk recall.
//!
//! Issue #15 lost a match on day 1 with the wall gate still open at nightfall:
//! `wall_gate_open` on fifteen consecutive rounds — which is the entire dusk
//! window, so the gate never sealed at all, and the record of the day did not
//! name the role holding it open. The cause was that the pioneer
//! "在 dusk（r=55）之后仍可接任务" — it
//! accepted a fresh task at r=58 and again at r=69, one round before night. The
//! gate cell is the last cell of the ring and `update_wall_gate` refuses to
//! release it while any role is still outside, so the ring kept a robot-sized
//! hole all night and the three controllers standing behind it were killed one
//! at a time.
//!
//! Two things have to hold, and they pull in opposite directions: the pioneer
//! must still run tasks for the whole morning (that is its job and its score),
//! and it must be home before the seal. Hence a rolling deadline rather than a
//! flat one — the walk back is subtracted from dusk, measured from wherever the
//! pioneer currently is.

use serde_json::{json, Value};

use coregeek::brain::day::plan as day_plan;
use coregeek::model::{footprint_distance, station_footprint, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::{BotState, TaskStage};

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

fn task_point(pos: (i32, i32)) -> Value {
    json!({
        "taskType": "自进化类1",
        "taskPosition": {"x": pos.0, "y": pos.1},
        "coldDownRounds": 0,
        "scoreReward": 10,
        "goldReward": 0,
        "isValid": true,
        "timeoutRounds": 250,
    })
}

/// A day-1 board on in-day round `in_day`. `roundNo` is the only calendar the
/// planner reads: day = (roundNo-1)/130 + 1, in-day round = (roundNo-1) % 130.
fn world(in_day: i64, roles: Vec<Value>, tasks: Vec<Value>) -> Value {
    let mut all = vec![station()];
    all.extend(roles);
    json!({
        "roundNo": in_day + 1,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 75, "totalScore": 0,
            "playerTasks": tasks,
            "roles": all,
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    })
}

/// Workers out of the way and a pioneer at `pioneer_pos` — the opening every
/// test below starts from.
fn board(in_day: i64, pioneer_pos: (i32, i32), tasks: Vec<Value>) -> Value {
    world(
        in_day,
        vec![
            role(10002, "worker", 13, 24),
            role(10003, "worker", 14, 25),
            role(10004, "pioneer", pioneer_pos.0, pioneer_pos.1),
        ],
        tasks,
    )
}

/// Both workers already inside the ring, so the gate question is about the
/// pioneer and nothing else. (9,24) and (11,25) are one cell from the station
/// footprint; (13,24) is two, and would keep the gate open on its own.
fn board_with_workers_home(in_day: i64, pioneer_pos: (i32, i32), tasks: Vec<Value>) -> Value {
    world(
        in_day,
        vec![
            role(10002, "worker", 9, 24),
            role(10003, "worker", 11, 25),
            role(10004, "pioneer", pioneer_pos.0, pioneer_pos.1),
        ],
        tasks,
    )
}

fn task_at(pos: (i32, i32), stage: TaskStage) -> BotState {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 1;
    state.task.timeout_round = 500;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();
    state.task.point = Some(Pos { x: pos.0, y: pos.1 });
    state.task.stage = stage;
    state
}

fn command_of(plan: &coregeek::brain::Plan, id: i64) -> Option<&coregeek::protocol::RoleCommand> {
    plan.commands.get(&id)
}

#[test]
fn an_active_task_is_dropped_in_time_to_walk_home() {
    // A pioneer on the far side of the map, mid-task, at r=63 — eight rounds
    // past dusk and eight rounds into the window `update_wall_gate` spends
    // asking whether it may seal. The task point is 25 cells from the base, so
    // the recall round passed long ago.
    let turn = turn_from(board(62, (35, 4), vec![task_point((36, 4))]));
    let mut state = task_at(
        (36, 4),
        TaskStage::HaveAnswer {
            answer: "42".into(),
        },
    );
    let plan = day_plan(&turn, &mut state);

    assert!(
        !plan
            .commands
            .values()
            .any(|cmd| cmd.action == "submitAnswer"),
        "issue #15: the pioneer was still working a task at r=63 instead of \
         walking home, so the wall gate could never seal"
    );
    assert!(
        !state.task.active,
        "the task has to be released, not merely ignored"
    );
    let cmd = command_of(&plan, 10004).expect("the pioneer is given something to do");
    assert_eq!(cmd.action, "move", "the recall walks the pioneer home");
    let step = cmd.targetPos.as_ref().expect("move has a target")[0];
    let footprint = station_footprint(Pos {
        x: STATION.0,
        y: STATION.1,
    });
    let before = footprint_distance(Pos { x: 35, y: 4 }, &footprint);
    assert!(
        footprint_distance(step, &footprint) < before,
        "the step {step:?} does not close on the base (was {before} cells out)"
    );
}

#[test]
fn no_task_is_accepted_once_the_recall_round_has_passed() {
    // Same board, pioneer next to the station, task point under its nose. At
    // r=60 the walk home is two rounds and dusk is long past: accepting would
    // park it outside for the seal.
    let turn = turn_from(board(60, (13, 26), vec![task_point((14, 26))]));
    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);

    assert!(
        !plan.commands.values().any(|cmd| cmd.action == "acceptTask"),
        "issue #15: 'r=69 acceptTask（又接一个新任务！）' — a task accepted at \
         dusk is a task that keeps the gate open all night"
    );
    assert!(
        !state.task.active,
        "no task session may be opened this late in the day"
    );
}

#[test]
fn the_task_point_is_still_worked_earlier_in_the_day() {
    // The other half of the contract: the recall is a deadline, not a ban. At
    // r=10 the pioneer is two cells from home and has all afternoon — it takes
    // the task, because that is what a pioneer is for.
    let turn = turn_from(board(10, (13, 26), vec![task_point((14, 26))]));
    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);

    let cmd = command_of(&plan, 10004).expect("the pioneer works the task point");
    assert_eq!(
        cmd.action, "acceptTask",
        "the dusk recall must not stop the pioneer working the morning"
    );
    assert!(state.task.active, "the session is open");
}

#[test]
fn the_recall_deadline_tightens_as_the_pioneer_drifts_out() {
    // A flat constant cannot do this job: the walk home is what has to fit
    // before dusk, so a pioneer twenty cells out has to turn around twenty
    // rounds earlier than one standing next to the base. Both are at the same
    // in-day round here, so only the distance differs.
    let near = turn_from(board(30, (13, 26), vec![task_point((14, 26))]));
    let far = turn_from(board(30, (35, 4), vec![task_point((36, 4))]));
    let mut near_state = BotState::default();
    let mut far_state = BotState::default();

    let near_plan = day_plan(&near, &mut near_state);
    let far_plan = day_plan(&far, &mut far_state);

    assert!(
        near_plan
            .commands
            .values()
            .any(|cmd| cmd.action == "acceptTask"),
        "a pioneer next to the base still has time for a task at r=30"
    );
    assert!(
        !far_plan
            .commands
            .values()
            .any(|cmd| cmd.action == "acceptTask"),
        "a pioneer twenty-five cells out does not, and must not start walking"
    );
}

#[test]
fn the_gate_seals_once_the_recalled_pioneer_is_inside() {
    // The point of the recall, end to end: `update_wall_gate` is asked at dusk
    // whether every role is inside or on its gun. With the pioneer still at its
    // task point the answer is no and the seal never happens; one round after
    // the recall walks it home the answer is yes.
    let outside = turn_from(board_with_workers_home(
        62,
        (13, 26),
        vec![task_point((14, 26))],
    ));
    let mut state = task_at(
        (14, 26),
        TaskStage::HaveAnswer {
            answer: "42".into(),
        },
    );
    let plan = day_plan(&outside, &mut state);
    assert!(
        !state.wall_gate_sealed,
        "a role at a task point is outside, so the gate stays open"
    );

    // Take the step the recall actually issued and play the next round from
    // there — so the seal below is caused by the recall, not by the test
    // quietly teleporting the pioneer home.
    let step = plan
        .commands
        .get(&10004)
        .and_then(|cmd| cmd.targetPos.as_ref())
        .map(|targets| targets[0])
        .expect("the recall walks the pioneer home");
    let inside = turn_from(board_with_workers_home(
        63,
        (step.x, step.y),
        vec![task_point((14, 26))],
    ));
    day_plan(&inside, &mut state);
    assert!(
        state.wall_gate_sealed,
        "with everyone inside the ring the dusk seal must close the gate cell"
    );
}

/// A board with one gun twenty-odd cells out and one worker sent to it — the
/// shape that decides whether the seal has to wait for the operator or not.
/// One worker and the pioneer stay home, so the only open question is the gun.
///
/// `walled_in` puts a wall on every cell around the gun, the way a ring built
/// without asking `wall_would_trap` would.
fn board_with_a_far_tower(controller: (i32, i32), walled_in: bool) -> Value {
    let mut roles = vec![
        role(10002, "worker", controller.0, controller.1),
        role(10003, "worker", 11, 25),
        role(10004, "pioneer", 9, 24),
        role(30000, "gatling", 33, 10),
    ];
    if walled_in {
        for (index, (dx, dy)) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)]
            .into_iter()
            .enumerate()
        {
            roles.push(json!({
                "id": 40000 + index as i64, "pos": {"x": 33 + dx, "y": 10 + dy},
                "roleType": "wall", "health": 3000, "level": 1,
                "backPackCapability": 0, "backpack": []
            }));
        }
    }
    world(62, roles, vec![])
}

fn gate_record(turn: &Turn, state: &mut BotState) -> Option<Value> {
    let pairs = coregeek::brain::night::stable_pairs(turn, state);
    let footprint = station_footprint(Pos {
        x: STATION.0,
        y: STATION.1,
    });
    coregeek::brain::day::gate_open_record(turn, &pairs, &footprint, &[])
}

#[test]
fn the_open_gate_record_names_who_is_out_and_where_they_are() {
    // Issue #15's shape, which cost a match and a half: both workers home and
    // the pioneer standing at a task point. The record it replaces said
    // "controllers_not_retreated" on fifteen consecutive rounds — the whole
    // dusk window, which is the same as saying the gate never sealed — and
    // named nobody, so the culprit had to be found by hand.
    let turn = turn_from(board_with_workers_home(
        62,
        (13, 26),
        vec![task_point((14, 26))],
    ));
    let mut state = task_at(
        (14, 26),
        TaskStage::HaveAnswer {
            answer: "42".into(),
        },
    );
    let record = gate_record(&turn, &mut state).expect("the pioneer is still outside");

    assert_eq!(
        record["away"],
        json!([[10004, 13, 26]]),
        "the id and the cell are the whole diagnosis: who, and how far out"
    );
    assert_eq!(record["round"], json!(63));
    assert_eq!(
        record["stuck"],
        json!([]),
        "a role on its way home is not a role walled off from its gun"
    );
}

#[test]
fn an_operator_on_its_post_is_not_someone_the_seal_is_waiting_for() {
    // The gun is twenty-odd cells from the base and the worker is out at it,
    // so by distance alone the gate would wait forever. It must not: the role
    // is on the operating cells of the tower it mans, which is where the night
    // needs it to be, and `wall_would_trap` has already guaranteed it can get
    // back out.
    let turn = turn_from(board_with_a_far_tower((32, 10), false));
    let mut state = BotState::default();
    assert_eq!(
        gate_record(&turn, &mut state),
        None,
        "everyone is either home or on post, so the gate may close"
    );
}

#[test]
fn an_operator_that_cannot_reach_its_gun_is_named_as_stuck_not_merely_late() {
    // The same post, walled in — and the operator three cells away from it.
    // This is the difference the record exists to draw: "walking" is a gate
    // that closes a round later, "stuck" is a gate that never closes, and only
    // the second one is a defect in the wall crew.
    let turn = turn_from(board_with_a_far_tower((33, 13), true));
    let mut state = BotState::default();

    let record = gate_record(&turn, &mut state).expect("the operator cannot reach its post");
    assert_eq!(record["away"], json!([[10002, 33, 13]]));
    assert_eq!(
        record["stuck"],
        json!([10002]),
        "walled off from every operating cell of its own gun"
    );
}
