//! The pioneer must HOLD its task point.
//!
//! Issue #17 lost both of its self-evolution sessions to
//! `[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用`, with the sandbox
//! commanding four rounds in the first session and one in the second before the
//! window shut. The window is not a timer: 任务书 5.3 defines the task's life as
//! "接取任务到任务结束期间" and lists exactly four ways it ends —
//!
//!   任务被完成 / 计数回合超过超时回合数 / **离开己方任务点周围一格内** / 开拓者死亡
//!
//! — so stepping one cell away from the task point ENDS the task. The planner
//! knows this (`task.rs`: "While a task is active the pioneer MUST stay within 1
//! cell of the task point, so this module never emits movement"), and
//! `plan_pioneer` duly returns no command while it waits for the LLM, waits for
//! the sandbox verdict, or waits out the submission. What it did not know is
//! that `plan` has a backstop underneath it: every controllable role with no
//! command is walked toward the station, and a pioneer standing at a task point
//! is a role with no command on every round of Planning, HavePlan,
//! WaitingCmdResult and WaitingSubmit. So the backstop walked the pioneer home,
//! the judger ended the task the moment it stepped off the point, and every
//! `executeCmd` after that was refused — which the bot then read as a broken
//! script and re-planned into, round after round, until the timeout took the
//! session with it. Both sessions scored zero, against the opponent's 345.
//!
//! These tests pin the hold for every stage that emits no command, and pin the
//! walk home that the dusk recall is still allowed to make (issue #15).

use serde_json::{json, Value};

use coregeek::brain::day::plan as day_plan;
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request};
use coregeek::state::{BotState, TaskStage};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

const STATION: (i32, i32) = (10, 24);
/// The task point, twenty-five cells from the base — a walk the backstop is
/// happy to start, and the one cell of slack 任务书 5.3 allows.
const POINT: (i32, i32) = (36, 4);

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

/// An early-day board: `in_day` is the 0-based round within day 1, so nothing
/// the dusk recall owns has fired yet.
fn board(in_day: i64, pioneer: (i32, i32)) -> Value {
    json!({
        "roundNo": in_day + 1,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 75, "totalScore": 0,
            "playerTasks": [task_point(POINT)],
            "roles": [
                station(),
                role(10002, "worker", 14, 24),
                role(10003, "worker", 15, 25),
                role(10004, "pioneer", pioneer.0, pioneer.1),
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    })
}

/// A live session sitting in `stage`, accepted a moment ago, nowhere near its
/// timeout. The description is already in hand, which is what a session looks
/// like on every round after the first.
fn session_at(stage: TaskStage) -> BotState {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 5;
    state.task.timeout_round = 300;
    state.task.point = Some(Pos {
        x: POINT.0,
        y: POINT.1,
    });
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();
    state.task.stage = stage;
    state
}

/// The pioneer's step this round, if it was given one.
fn step_of(plan: &coregeek::brain::Plan, id: i64) -> Option<Pos> {
    plan.commands
        .get(&id)
        .and_then(|cmd| cmd.targetPos.as_ref())
        .map(|targets| targets[0])
}

/// Every stage that deliberately emits no command, so the backstop is the only
/// thing that could move the pioneer. All five are ordinary waiting states: the
/// session spends most of its life in them.
fn waiting_stages() -> Vec<(&'static str, TaskStage)> {
    vec![
        ("waiting for phaseTask", TaskStage::WaitingDescription),
        ("waiting for the LLM", TaskStage::Planning),
        (
            "plan in hand, command queued",
            TaskStage::HavePlan {
                cmd: "ls /tmp/selfEvolutionTask/".into(),
            },
        ),
        (
            "waiting for the sandbox verdict",
            TaskStage::WaitingCmdResult { attempts: 0 },
        ),
        (
            "waiting out the submission",
            TaskStage::WaitingSubmit { attempts: 0 },
        ),
    ]
}

#[test]
fn a_pioneer_on_its_task_point_is_never_walked_away_from_it() {
    // The task ends the moment the pioneer leaves the point's neighbourhood, so
    // a single step home is not a detour — it is the task.
    for (label, stage) in waiting_stages() {
        let turn = turn_from(board(10, (35, 4)));
        let mut state = session_at(stage);
        let plan = day_plan(&turn, &mut state);

        if let Some(step) = step_of(&plan, 10004) {
            let before = (35 - POINT.0).abs().max((4 - POINT.1).abs());
            let after = (step.x - POINT.0).abs().max((step.y - POINT.1).abs());
            assert!(
                after <= 1,
                "issue #17: while {label} the pioneer was moved from its task \
                 point {POINT:?} to {step:?} ({before} → {after} cells away) — \
                 任务书 5.3 ends the task on \"离开己方任务点周围一格内\", which is \
                 what turned every later executeCmd into \
                 \"[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用\""
            );
        }
        assert!(state.task.active, "while {label} the session is still open");
    }
}

#[test]
fn a_pioneer_standing_off_its_point_walks_back_onto_it() {
    // The mirror image, and the one case where the pioneer is allowed to move
    // while a task is live: if something already nudged it off the point, the
    // only move worth making is the one that puts it back inside the single
    // cell 任务书 5.3 allows.
    let turn = turn_from(board(10, (33, 4)));
    let mut state = session_at(TaskStage::Planning);
    let plan = day_plan(&turn, &mut state);

    let step = step_of(&plan, 10004).expect("the pioneer walks back to its point");
    let before = (33 - POINT.0).abs().max((4 - POINT.1).abs());
    let after = (step.x - POINT.0).abs().max((step.y - POINT.1).abs());
    assert!(
        after < before,
        "the step {step:?} does not close on the task point {POINT:?} \
         ({before} → {after})"
    );
}

#[test]
fn the_dusk_recall_may_still_take_the_pioneer_home() {
    // Issue #15 is the constraint on the fix above: a task held past dusk is a
    // pioneer outside the ring when the wave lands, and the gate seal waits for
    // it. The recall is allowed to end the session and walk home — that trade
    // was made deliberately, and the hold must not quietly veto it.
    let turn = turn_from(board(64, (35, 4)));
    let mut state = session_at(TaskStage::Planning);
    let plan = day_plan(&turn, &mut state);

    assert!(
        !state.task.active,
        "the dusk recall ends the session rather than leaving the pioneer out"
    );
    let step = step_of(&plan, 10004).expect("the recall walks the pioneer home");
    let before = (35 - STATION.0).abs().max((4 - STATION.1).abs());
    let after = (step.x - STATION.0).abs().max((step.y - STATION.1).abs());
    assert!(
        after < before,
        "the step {step:?} does not close on the base (was {before} cells out)"
    );
}

#[test]
fn a_task_point_is_not_accepted_without_the_rounds_to_use_it() {
    // 任务书 5.3: "在任务执行结束后，再次接取任务需等待 30 个回合刷新时间", and the
    // dusk recall ends any session still open when it fires. So a late accept
    // is not a cheap attempt — it is that point sold for 30 rounds, because
    // the recall ends the task and the judger starts the cooldown. Issue #26
    // lost its base to the nine `wall_gate_open` rounds a pioneer standing on
    // a point it could not finish had spent outside the ring.
    let accept_of = |plan: &coregeek::brain::Plan| {
        plan.commands
            .get(&10004)
            .map(|cmd| cmd.action.clone())
            .unwrap_or_default()
    };

    // Early day, twelve-plus rounds before the recall: accept.
    let turn = turn_from(board(5, (35, 4)));
    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);
    assert_eq!(
        accept_of(&plan),
        "acceptTask",
        "an early accept has the whole day to work with"
    );

    // Late day at the same point, same pioneer: no accept. The recall is seven
    // rounds away, which is not one full cycle of prompt → llmResp → command →
    // verdict → submit.
    let turn = turn_from(board(20, (35, 4)));
    let mut state = BotState::default();
    let plan = day_plan(&turn, &mut state);
    assert_ne!(
        accept_of(&plan),
        "acceptTask",
        "accepting now burns the point's 30-round cooldown for an attempt that \
         cannot even complete one cycle"
    );
    assert!(
        !state.task.active,
        "a deferred accept opens no session"
    );
}

#[test]
fn a_sterile_session_hands_the_pioneer_back_to_the_wall_line() {
    // Issue #15: the opponent dropped failing tasks and "把开拓者投入防御"; a
    // session that has spent three full LLM cycles (prompt → answer → command
    // → verdict) and submitted nothing has produced no reason to believe the
    // fourth is the one. Issue #26 lost the base while its controllers — the
    // pioneer among them — were still outside the ring.
    let mut state = session_at(TaskStage::Planning);
    state.task.accepted_round = 5;
    let turn = turn_from(board(18, (35, 4))); // round 19: 14 rounds in
    day_plan(&turn, &mut state);
    assert!(state.task.active, "fourteen rounds is not yet sterile");

    let turn = turn_from(board(20, (35, 4))); // round 21: 16 rounds in
    day_plan(&turn, &mut state);
    assert!(
        !state.task.active,
        "a session with nothing submitted after 15 rounds gives the pioneer back"
    );
}

#[test]
fn a_session_with_a_submission_is_never_sterile() {
    // The other half: one answer in hand is evidence the loop is working, and
    // `MAX_WRONG_ANSWERS` already governs how many rejections it survives.
    // Cutting here would throw away a banked answer 任务书 ch.6 still scores by
    // 通过率.
    let mut state = session_at(TaskStage::WaitingSubmit { attempts: 3 });
    state.task.accepted_round = 5;
    state.task.best_answer = r#"{"count": 41}"#.into();
    state.task.submitted_round = Some(9);
    let turn = turn_from(board(20, (35, 4)));
    day_plan(&turn, &mut state);
    assert!(
        state.task.active,
        "a session that has submitted keeps its point"
    );
}
