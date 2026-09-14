//! P0-1 (docs/FAILURE-ANALYSIS-2026-09-14.md §3.1): the judger's own words
//! about a rejected answer reach the retry that has to fix it.
//!
//! Every rejection comes back as a coarse `errorCode` — 2 is "答案错误，不正确
//! **或不完全正确**", which covers a wrong field NAME, a missing field and an
//! out-of-range value alike — plus one `description` that says which it was
//! (`MissingNamedInput: city`, `键值比对不通过: $/token: 缺少键`). That text was
//! parsed into the turn, written to the log, and then dropped on the floor:
//! `build_prompt` retried on `schema_gaps` and `discovered_fields`, both of
//! which are guesses at the schema, so all three retries were blind rewrites
//! and the task score stayed at zero for five straight matches. The opponent's
//! winning path in issue #10 was exactly this loop — read the rejection, add
//! the field it named, submit again.

use serde_json::{json, Value};

use coregeek::brain::task::build_prompt;
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request};
use coregeek::state::{BotState, TaskStage};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// A day board whose judger verdict for this round is `errors`/`errorDescs`.
fn board_with_verdict(errors: Vec<i64>, descriptions: Vec<&str>) -> Value {
    let mut payload = json!({
        "roundNo": 40,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 75, "totalScore": 0,
            "playerTasks": [],
            "roles": [
                {"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                 "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []},
                {"id": 10004, "pos": {"x": 12, "y": 24}, "roleType": "pioneer",
                 "health": 200, "attackPower": 0, "attackRange": 0,
                 "level": 1, "backPackCapability": 40, "backpack": []}
            ]
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    });
    if !errors.is_empty() {
        payload["errors"] = Value::Array(
            errors
                .iter()
                .zip(descriptions.iter())
                .map(|(code, description)| json!({"errorCode": code, "description": description}))
                .collect(),
        );
    }
    payload
}

/// A session that has just had an answer judged: the state machine is in
/// `WaitingSubmit`, which is the only stage a verdict is read against.
fn session_waiting_on_a_verdict() -> BotState {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 20;
    state.task.timeout_round = 300;
    state.task.point = Some(Pos { x: 24, y: 12 });
    state.task.description = "查询城市气候并输出 JSON".into();
    state.task.best_answer = r#"{"city":"Berlin"}"#.into();
    state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };
    state.task.submitted_round = Some(39);
    state
}

#[test]
fn a_rejection_is_kept_verbatim_and_reaches_the_next_prompt() {
    let mut state = session_waiting_on_a_verdict();
    let turn = turn_from(board_with_verdict(
        vec![2],
        vec!["键值比对不通过: $/token: 缺少键"],
    ));
    state.observe(&turn);

    assert_eq!(
        state.task.rejection_feedback,
        vec!["键值比对不通过: $/token: 缺少键".to_string()],
        "the judger's own reason for the rejection was dropped"
    );
    let prompt = build_prompt(&state, &turn);
    assert!(
        prompt.contains("键值比对不通过: $/token: 缺少键"),
        "the retry prompt does not carry the judger's rejection: {prompt}"
    );
    assert!(
        prompt.contains("判题器对你已提交答案的原话反馈"),
        "the rejection is not labelled as the judger's own words: {prompt}"
    );
}

#[test]
fn the_reason_is_read_against_its_own_error_code() {
    // `errors[]` and `errorDescs[]` are parallel and same-order (WORKFLOW
    // REQUEST §7.3 表 4b). A round can carry several verdicts at once and only
    // the code-2 ones are about an answer, so the pair has to be read by index.
    let mut state = session_waiting_on_a_verdict();
    let turn = turn_from(board_with_verdict(
        vec![4, 2],
        vec!["指令错误：坐标越界", "MissingNamedInput: city"],
    ));
    state.observe(&turn);

    assert_eq!(
        state.task.rejection_feedback,
        vec!["MissingNamedInput: city".to_string()],
        "a command error was read as an answer rejection, or the pairing slipped"
    );
}

#[test]
fn the_same_rejection_is_kept_once() {
    let mut state = session_waiting_on_a_verdict();
    for _ in 0..2 {
        let turn = turn_from(board_with_verdict(vec![2], vec!["MissingNamedInput: city"]));
        state.observe(&turn);
        state.task.stage = TaskStage::WaitingSubmit { attempts: 0 }; // re-submitted
    }
    assert_eq!(
        state.task.rejection_feedback,
        vec!["MissingNamedInput: city".to_string()],
        "the same verdict was stacked twice into the prompt"
    );
}

#[test]
fn a_rejection_with_no_text_degrades_to_todays_behaviour() {
    // The empty description is the case the analysis asks to survive: the code
    // still counts the wrong answer and the retry still happens, it is only the
    // verbatim line that is missing.
    let mut state = session_waiting_on_a_verdict();
    let turn = turn_from(board_with_verdict(vec![2], vec!["   "]));
    state.observe(&turn);

    assert!(
        state.task.rejection_feedback.is_empty(),
        "a blank description became prompt text: {:?}",
        state.task.rejection_feedback
    );
    assert_eq!(state.task.wrong_answers, 1, "the rejection was not counted");
    assert_eq!(state.task.stage, TaskStage::Planning, "the retry was not armed");
    let prompt = build_prompt(&state, &turn);
    assert!(
        !prompt.contains("判题器对你已提交答案的原话反馈"),
        "an empty verdict still added a feedback section to the prompt"
    );
}

#[test]
fn the_verdict_is_only_read_against_a_submitted_answer() {
    // A round can carry an error code while the session is mid-plan; only a
    // rejection of something we actually submitted is feedback about an answer.
    let mut state = session_waiting_on_a_verdict();
    state.task.stage = TaskStage::Planning;
    let turn = turn_from(board_with_verdict(vec![2], vec!["MissingNamedInput: city"]));
    state.observe(&turn);
    assert!(
        state.task.rejection_feedback.is_empty(),
        "a verdict from before the submission was fed back as a rejection"
    );
}
