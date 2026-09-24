//! The pioneer's dusk recall.
//!
//! Issue #15 lost a match on day 1 with the pioneer still out at nightfall: it
//! "在 dusk（r=55）之后仍可接任务" — it accepted a fresh task at r=58 and
//! again at r=69, one round before night, and the night's plan found a role
//! twenty-five cells from home when the robots arrived. (Back then a stuck
//! role also held the wall gate open — the gate latch itself is deleted since
//! issue #221 phase 4b: the ring's back four cells are a permanent entrance
//! and nobody seals them.)
//!
//! Two things have to hold, and they pull in opposite directions: the pioneer
//! must still run tasks for the whole morning (that is its job and its score),
//! and it must be home before nightfall. Hence a rolling deadline rather than
//! a flat one — the walk back is subtracted from dusk, measured from wherever
//! the pioneer currently is.

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
