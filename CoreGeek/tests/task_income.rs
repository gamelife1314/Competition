//! The task line's income: what a session was worth, and when it is banked.
//!
//! Issues #201/#203/#204/#205 measured `taskGoldEarned = 0` for the whole of
//! every match, and the number could not have been anything else. Both halves
//! were missing:
//!
//! * **The ceiling was never read.** `playerTasks[]` carries `scoreReward` /
//!   `goldReward` (接口文档 §1.3.2) and 任务书 ch.6 pays `任务奖励 × 通过率`, so
//!   the point's own pair is the most a session can earn — and the `task_started`
//!   record carried `{head, round, timeout}` only (WORKFLOW_REQUEST §16.1 asked
//!   for the rest, and it was never implemented). A session worth 300 points and
//!   one worth 20 logged identically, which is the one thing the analysis
//!   cannot do without: "the task line earned nothing" has to be told apart
//!   from "the task line was never worth anything".
//! * **The only reachable ending did not pay.** `bank_task_reward` hung off
//!   `confirmed_success` alone — the closure probe's three-signal confirmation
//!   (`point_closed && phase_missing_rounds >= 2 && !post_submit_error`). Every
//!   session of all four matches ended `timeout` (表 3a, 7/7), most of them one
//!   to three rounds after their last submission: #205 session 3 submitted at
//!   r42 and r43, the judger never rejected either, and the task's own
//!   `timeoutRounds` boundary ended it at r44. 接口文档 §1.3.2 settles exactly
//!   that case on "之前提交过的通过率最高的答案", so the point's reward is what
//!   that session played for — while the counter, blind by construction, said 0.
//!
//! What is asserted here is the state, not the log line: `log::event` writes to
//! stdout and is not readable in-process (see `tests/task_kind.rs` for the same
//! note), so the session holds the figures and the record copies them.

use serde_json::{json, Value};

use coregeek::brain::task::TaskKind;
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request};
use coregeek::state::{BotState, TaskStage};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// A task point as 接口文档 §1.3.2 sends it.
fn point(kind: &str, x: i32, y: i32, score: i64, gold: i64) -> Value {
    json!({
        "taskType": kind,
        "taskPosition": {"x": x, "y": y},
        "coldDownRounds": 0,
        "scoreReward": score, "goldReward": gold,
        "isValid": true, "timeoutRounds": 14,
    })
}

/// A day board. `phase_task` is the round's 任务书 text and `errors` the
/// judger's verdict for the round, both set by the test.
fn board(round_no: i64, points: Vec<Value>, phase_task: &str, errors: Vec<Value>) -> Value {
    let mut payload = json!({
        "roundNo": round_no,
        "phaseTask": phase_task,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": points,
            "roles": [
                json!({"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                       "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
                json!({"id": 10004, "pos": {"x": 12, "y": 24}, "roleType": "pioneer",
                       "health": 200, "attackPower": 0, "attackRange": 0,
                       "level": 1, "backPackCapability": 40, "backpack": []}),
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    });
    if !errors.is_empty() {
        payload["errors"] = Value::Array(errors);
    }
    payload
}

/// A session opened at round 30 on the point at (24,12), whose description has
/// not arrived yet — the state `task_started` is read against.
fn session_opened() -> BotState {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 3;
    state.task.accepted_round = 30;
    state.task.timeout_round = 44;
    state.task.point = Some(Pos { x: 24, y: 12 });
    state.task.task_type = "自进化类1".into();
    state.task.kind = TaskKind::SelfEvolution;
    state
}

// ---------------------------------------------------------------------------
// The ceiling
// ---------------------------------------------------------------------------

#[test]
fn the_session_records_what_its_own_task_point_is_worth() {
    // Two points are on the board and they are NOT worth the same, so a session
    // that grabs the first entry of `playerTasks[]` instead of its own point
    // reads 20/5 here. The session's point is (24,12) — #205's — and it is the
    // second one on the board.
    let mut state = session_opened();
    let turn = turn_from(board(
        40,
        vec![
            point("自进化类1", 20, 10, 20, 5),
            point("自进化类2", 24, 12, 300, 120),
        ],
        "请阅读task_2_nanjing.md，获取任务信息",
        vec![],
    ));
    state.observe(&turn);

    assert_eq!(
        state.task.score_reward, 300,
        "the session did not record its own point's scoreReward — the ceiling \
         every task fix is judged against"
    );
    assert_eq!(
        state.task.gold_reward, 120,
        "the session did not record its own point's goldReward"
    );
}

#[test]
fn a_session_whose_point_is_gone_records_no_ceiling() {
    // The point can be retired between the accept and the description, and a
    // session with no point has no advertised reward: it must read 0 rather
    // than borrow the neighbouring point's figures.
    let mut state = session_opened();
    state.task.point = Some(Pos { x: 1, y: 1 });
    let turn = turn_from(board(
        40,
        vec![point("自进化类2", 24, 12, 300, 120)],
        "请阅读task_2_nanjing.md，获取任务信息",
        vec![],
    ));
    state.observe(&turn);

    assert_eq!(
        (state.task.score_reward, state.task.gold_reward),
        (0, 0),
        "a session whose own point is off the board was given another point's \
         reward"
    );
}

// ---------------------------------------------------------------------------
// The ending that pays
// ---------------------------------------------------------------------------

#[test]
fn a_session_the_clock_ends_banks_its_point() {
    // #205 session 3 in miniature: the answer went in at r43, the judger never
    // rejected it, and the session's `timeoutRounds` boundary falls at r44.
    // 接口文档 §1.3.2 settles that session on the best answer it submitted, so
    // the point's reward is earned — and the counter that exists to record it
    // used to be reachable only from `confirmed_success`.
    let mut state = session_opened();
    state.task.description = "请阅读task_1_alpha.md，获取任务信息".into();
    state.task.best_answer = r#"{"token": "fc1e78eb2a5a"}"#.into();
    state.task.submitted_round = Some(43);
    state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };

    let turn = turn_from(board(
        44,
        vec![point("自进化类1", 24, 12, 300, 120)],
        "请阅读task_1_alpha.md，获取任务信息",
        vec![],
    ));
    state.observe(&turn);

    assert!(!state.task.active, "the session did not end at its deadline");
    assert_eq!(
        state.task_gold_earned, 120,
        "a session that ended at its deadline with an unrejected answer banked \
         nothing — `taskGoldEarned` stays 0 on the only ending our sessions take"
    );
}

#[test]
fn the_judgers_timeout_code_is_not_a_verdict_on_the_answer() {
    // The same ending announced by the judger instead of by our own round
    // count: 表 4b's `1 timeout`. `errorCode 1` means the SESSION timed out, and
    // §1.3.2 pays a timed-out task on the answers already submitted — so it is
    // not the "we were told the answer was bad" evidence `post_submit_error`
    // records, and it must not turn the settlement above into a failure.
    let mut state = session_opened();
    state.task.description = "请阅读task_1_alpha.md，获取任务信息".into();
    state.task.best_answer = r#"{"token": "fc1e78eb2a5a"}"#.into();
    state.task.submitted_round = Some(43);
    state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };

    let turn = turn_from(board(
        44,
        vec![point("自进化类1", 24, 12, 300, 120)],
        "请阅读task_1_alpha.md，获取任务信息",
        vec![json!({"errorCode": 1, "description": "timeout"})],
    ));
    state.observe(&turn);

    assert!(!state.task.active, "the session did not end on the timeout code");
    assert_eq!(
        state.task_gold_earned, 120,
        "the judger's own timeout verdict was read as a rejection of the answer"
    );
}

// ---------------------------------------------------------------------------
// The two controls: what must NOT pay
// ---------------------------------------------------------------------------

#[test]
fn a_session_that_never_submitted_banks_nothing() {
    // #205 session 2: `cmdRounds 0`, `rejections 0`, ended `timeout`. Nothing
    // was ever submitted, so there is no answer for §1.3.2 to settle on and
    // nothing was earned. This is the control for both tests above — without it
    // "bank on timeout" would be "bank on any timeout", which pays for a
    // session that did no work at all.
    let mut state = session_opened();
    state.task.description = "请阅读task_1_beijing.md，获取任务信息".into();
    state.task.stage = TaskStage::Planning;

    let turn = turn_from(board(
        44,
        vec![point("自进化类1", 24, 12, 300, 120)],
        "请阅读task_1_beijing.md，获取任务信息",
        vec![],
    ));
    state.observe(&turn);

    assert!(!state.task.active, "the session did not end at its deadline");
    assert_eq!(
        state.task_gold_earned, 0,
        "a session that never submitted an answer banked its point anyway"
    );
}

#[test]
fn a_session_the_judger_rejected_banks_nothing() {
    // #205 session 1: four submissions, four rejections (`键值比对不通过:
    // $/world_heritage_count: 数值不符`), then the deadline. The answer was
    // judged wrong, so the point's reward is not ours to book — the counter
    // carries the point's CEILING, and booking a ceiling for a rejected answer
    // reports income the match never earned. Driven through the real rejection
    // handler rather than by setting the flag, so what is pinned is the
    // pipeline: the rejection is what withdraws the pending submission, and the
    // deadline finds nothing to settle.
    let mut state = session_opened();
    state.task.description = "请阅读task_1_beijing.md，获取任务信息".into();
    state.task.best_answer = r#"{"city": "北京"}"#.into();
    state.task.submitted_round = Some(42);
    state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };

    let judged = turn_from(board(
        43,
        vec![point("自进化类1", 24, 12, 300, 120)],
        "请阅读task_1_beijing.md，获取任务信息",
        vec![json!({
            "errorCode": 2,
            "description": "键值比对不通过: $/world_heritage_count: 数值不符",
        })],
    ));
    state.observe(&judged);
    assert_eq!(
        state.task.submitted_round, None,
        "test setup: the rejection did not withdraw the pending submission"
    );

    let deadline = turn_from(board(
        44,
        vec![point("自进化类1", 24, 12, 300, 120)],
        "请阅读task_1_beijing.md，获取任务信息",
        vec![],
    ));
    state.observe(&deadline);

    assert!(!state.task.active, "the session did not end at its deadline");
    assert_eq!(
        state.task_gold_earned, 0,
        "an answer the judger rejected was booked as task income"
    );
}

#[test]
fn a_session_whose_rounds_were_failing_banks_nothing() {
    // The conservative half of the rule, and the one the evidence does not
    // decide: #205's r23 carries `errorCode 3` (`LLM 调用失败（3 次尝试）: Read
    // timed out`) in a round whose submission was still pending. Codes 3/4/5
    // are not verdicts on the ANSWER — 接口文档 §1.3.2's list makes only code 2
    // that — but a submission the judger was never heard back on is not
    // evidence of income either, and `taskGoldEarned` is read as "what the task
    // line earned". So the ceiling is booked only when the judger said nothing
    // at all against the answer: an error in a round after the submission keeps
    // the session out of the books, which is the reading that cannot overstate.
    let mut state = session_opened();
    state.task.description = "请阅读task_1_beijing.md，获取任务信息".into();
    state.task.best_answer = r#"{"city": "北京"}"#.into();
    state.task.submitted_round = Some(42);
    state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };

    let failed = turn_from(board(
        43,
        vec![point("自进化类1", 24, 12, 300, 120)],
        "请阅读task_1_beijing.md，获取任务信息",
        vec![json!({
            "errorCode": 3,
            "description": "LLM 调用失败（3 次尝试）: Read timed out",
        })],
    ));
    state.observe(&failed);
    assert_eq!(
        state.task.submitted_round,
        Some(42),
        "test setup: a code 3 error withdraws the submission — only code 2 does"
    );

    let deadline = turn_from(board(
        44,
        vec![point("自进化类1", 24, 12, 300, 120)],
        "请阅读task_1_beijing.md，获取任务信息",
        vec![],
    ));
    state.observe(&deadline);

    assert!(!state.task.active, "the session did not end at its deadline");
    assert_eq!(
        state.task_gold_earned, 0,
        "a session whose round errored was booked as task income"
    );
}
