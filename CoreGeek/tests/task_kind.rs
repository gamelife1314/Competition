//! Issue #206 §5 item 4a/4d/4e/4f: the task line has three lanes.
//!
//! 任务书 §5 splits the tasks in three and feeds them from three different
//! sources — 推理类 from the day's official news, 传闻类 from the folk legends,
//! 自进化类 from the task points the pioneer walks to — and the plan's finding
//! was that the code had no such split: every candidate point was ranked by
//! distance alone and every accept went into the same queue with no record of
//! which lane it belonged to.
//!
//! What is asserted here is the two things that are observable from outside:
//! WHICH point the pioneer commits to when two kinds are on the board, and what
//! the accepted session says it is. The `task_kind` field on the log records is
//! the third half and is not assertable in-process (`log::event` writes to
//! stdout); the session field it is copied from is, and is checked here.

use serde_json::{json, Value};

use coregeek::brain::task::{classify, TaskKind};
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn point(kind: &str, x: i32, y: i32) -> Value {
    json!({
        "taskType": kind,
        "taskPosition": {"x": x, "y": y},
        "coldDownRounds": 0,
        "scoreReward": 50, "goldReward": 0,
        "isValid": true, "timeoutRounds": 250,
    })
}

/// Day 1, round 20. The pioneer stands at (10, 20); `points` are placed around
/// it by the test.
fn board(points: Vec<Value>) -> Value {
    json!({
        "roundNo": 20,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": points,
            "roles": [
                json!({"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                       "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
                json!({"id": 10004, "pos": {"x": 10, "y": 20}, "roleType": "pioneer",
                       "health": 200, "attackPower": 0, "attackRange": 0,
                       "level": 1, "backPackCapability": 40, "backpack": []}),
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "vendorShopList": [], "weaponShopList": [],
    })
}

/// Where the pioneer decided to go: (action, target).
fn decide(payload: Value) -> (String, Option<coregeek::protocol::Pos>, BotState) {
    let turn = turn_from(payload);
    let pioneer = turn.role_by_id(10004).expect("pioneer").clone();
    let mut state = BotState::default();
    let mut claimed = std::collections::HashSet::new();
    let cmd = state
        .next_task_point(&turn, &pioneer, &mut claimed)
        .expect("a board with a valid point always produces an errand");
    let target = cmd.targetPos.as_ref().and_then(|list| list.first()).copied();
    (cmd.action.clone(), target, state)
}

// ---------------------------------------------------------------------------
// The classifier
// ---------------------------------------------------------------------------

#[test]
fn the_books_three_lanes_are_told_apart_by_name() {
    // The real `taskType` values the judger sends (接口文档: `自进化类1` /
    // `自进化类2`), the two the book names for the other lanes, and one the book
    // does not name at all.
    let rows: [(&str, TaskKind); 7] = [
        ("自进化类1", TaskKind::SelfEvolution),
        ("自进化类2", TaskKind::SelfEvolution),
        ("推理类1", TaskKind::Reasoning),
        ("传闻类1", TaskKind::Rumor),
        ("", TaskKind::Other),
        ("战功类1", TaskKind::Other),
        ("reasoning", TaskKind::Other),
    ];
    for (task_type, want) in rows {
        assert_eq!(
            classify(task_type),
            want,
            "{task_type:?} was filed in the wrong lane"
        );
        assert!(!want.as_str().is_empty(), "a lane with no log name");
    }
}

#[test]
fn the_lane_order_is_reasoning_and_rumour_then_self_evolution_then_the_rest() {
    // The plan's ordering, stated as the numbers the ranking key uses.
    assert!(TaskKind::Reasoning.rank() < TaskKind::SelfEvolution.rank());
    assert!(TaskKind::Rumor.rank() < TaskKind::SelfEvolution.rank());
    assert!(TaskKind::SelfEvolution.rank() < TaskKind::Other.rank());
    assert_eq!(
        TaskKind::Reasoning.rank(),
        TaskKind::Rumor.rank(),
        "推理 and 传闻 share the top lane"
    );
}

// ---------------------------------------------------------------------------
// Which point the pioneer commits to
// ---------------------------------------------------------------------------

#[test]
fn a_reasoning_point_outranks_the_self_evolution_one_underfoot() {
    // The pioneer is standing NEXT TO a self-evolution point and four cells from
    // a reasoning point. Distance alone accepts the task under its feet — that
    // is what the old ranking did, and the control below proves it still would.
    // With the lanes in place the walk goes to the reasoning point instead.
    let reasoning = coregeek::protocol::Pos { x: 6, y: 20 };
    let self_evo = coregeek::protocol::Pos { x: 11, y: 20 };
    let (action, target, _) = decide(board(vec![
        point("自进化类1", self_evo.x, self_evo.y),
        point("推理类1", reasoning.x, reasoning.y),
    ]));
    assert_ne!(
        action, "acceptTask",
        "the pioneer accepted the 自进化 task under its feet instead of the \
         day's 推理 task"
    );
    assert_eq!(action, "move");
    let at = target.expect("a move has a target");
    assert!(
        coregeek::model::chebyshev(at, reasoning)
            < coregeek::model::chebyshev(coregeek::protocol::Pos { x: 10, y: 20 }, reasoning),
        "the pioneer stepped to {at:?}, which does not close on the reasoning \
         point at {reasoning:?}"
    );

    // The control: the same board without the reasoning point accepts the task
    // it is standing next to. Without this the assertion above could be passing
    // on something other than the lane.
    let (action, _, _) = decide(board(vec![point("自进化类1", self_evo.x, self_evo.y)]));
    assert_eq!(
        action, "acceptTask",
        "test setup: the self-evolution point underfoot is not acceptable"
    );
}

#[test]
fn a_lane_the_book_does_not_name_sorts_behind_the_ones_it_does() {
    // 其余 last: a point the book never names does not get to outrank 自进化 just
    // by being nearer. Same shape as the test above, one lane swapped.
    let self_evo = coregeek::protocol::Pos { x: 6, y: 20 };
    let unnamed = coregeek::protocol::Pos { x: 11, y: 20 };
    let (action, target, _) = decide(board(vec![
        point("战功类1", unnamed.x, unnamed.y),
        point("自进化类1", self_evo.x, self_evo.y),
    ]));
    assert_ne!(
        action, "acceptTask",
        "an unnamed kind was accepted over 自进化 because it was nearer"
    );
    let at = target.expect("a move has a target");
    assert!(
        coregeek::model::chebyshev(at, self_evo)
            < coregeek::model::chebyshev(coregeek::protocol::Pos { x: 10, y: 20 }, self_evo),
        "the pioneer stepped to {at:?}, which does not close on the \
         self-evolution point at {self_evo:?}"
    );
}

#[test]
fn the_accepted_session_says_which_lane_it_belongs_to() {
    // The field the log copies. `task_accept` now carries `task_kind` and
    // `kindRank` straight off this, so what the analysis reads is what the
    // ordering used.
    let (action, _, state) = decide(board(vec![point("自进化类2", 11, 20)]));
    assert_eq!(action, "acceptTask");
    assert!(state.task.active, "the session was not opened");
    assert_eq!(state.task.kind, TaskKind::SelfEvolution);
    assert_eq!(state.task.kind.as_str(), "self_evolution");
    assert_eq!(
        state.task.kind.rank(),
        1,
        "the accepted session does not carry the rank it was chosen by"
    );
    assert_eq!(state.task.task_type, "自进化类2");
}
