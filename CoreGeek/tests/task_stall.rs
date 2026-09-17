//! The reconnaissance loop (issues #111-#115).
//!
//! Every session in the batch ended `reason=timeout` with `rejections=0` and
//! `wrongAnswers=0` — **no answer was ever submitted**, so the P0-1 rejection
//! feedback loop (which can only act on a rejected answer) had nothing to act
//! on. #115 is the shape in full: 14 commands across 6 sessions, 13 of them
//! logged `task_cmd_failed`, one single `task_answer_found` in the whole match,
//! and zero submissions.
//!
//! The `task_cmd_failed` records say what went wrong and, for two batches, were
//! read as the opposite: `{"exit": 0, "timeout": false}` is not a sandbox that
//! failed, it is a sandbox that ran the script to completion and got no
//! `ANSWER:` line out of it. The `cmd_sent.head` values say why — rounds 12, 15
//! and 18 of #115 are all "Step 1: Locate … the task file", the same
//! reconnaissance three times over.
//!
//! The mechanism is the context window. `build_prompt` fed back the last two
//! runs truncated to 600 characters each, while those runs were 2390, 3081 and
//! 5467 characters (`cmd_result.chars`). The task file the script had just
//! `cat`ed was cut off before its requirements, so the model's only honest
//! reading was that the read had failed — and it read again.

use serde_json::{json, Value};

use coregeek::brain::task::{build_prompt, on_cmd_result};
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::{BotState, TaskStage};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// A round whose request carries nothing but the round number — `build_prompt`
/// reads the turn only for that, and the state machine reads the task fields
/// off `BotState`.
fn board(round_no: i64) -> Value {
    json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {"type": "challenger", "goldNum": 75, "totalScore": 0, "playerTasks": [], "roles": []},
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    })
}

/// A session mid-reconnaissance: one command out, no answer back.
fn session_planning() -> BotState {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 20;
    state.task.description_round = 22;
    state.task.timeout_round = 60;
    state.task.description = "请阅读task_1_alpha.md，获取任务信息".into();
    state.task.stage = TaskStage::Planning;
    state
}

/// A `lastCmdResult` of the shape #115 produced: a banner, a `find` listing,
/// then the task file — with the specifications the answer needs somewhere
/// past the old 600-character cut.
fn reconnaissance_result(exit: i64) -> String {
    let body = format!(
        "[exitCode:{exit}]\n=== RECONNAISSANCE PHASE ===\n\
         /tmp/selfEvolutionTask/1-fixed-step/2-engineering-fix/task_1_alpha.md\n\
         /tmp/selfEvolutionTask/1-fixed-step/2-engineering-fix/ws_1/spec.md\n\
         === Reading Task File ===\n{}\n\
         FIELDS: total_count, world_heritage_count, types, oldest_era\n",
        "任务要求：读取 ws_1/data.csv，统计世界遗产总数。\n".repeat(30)
    );
    body
}

/// Feed one answer-less command result through the state machine.
fn absorb(state: &mut BotState, result: &str) {
    state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
    state.task.cmd_history.push("find /tmp/selfEvolutionTask/".into());
    on_cmd_result(state, result);
}

#[test]
fn a_run_without_an_answer_is_counted_as_a_stall_not_a_failure() {
    // The record is the finding: `exit: 0` and `timeout: false` are the
    // sandbox saying the script worked, so what the batch logged as "13 failed
    // commands" was 13 scripts that never printed an answer.
    let mut state = session_planning();
    absorb(&mut state, &reconnaissance_result(0));
    assert_eq!(state.task.no_answer_rounds, 1);
    assert!(
        matches!(state.task.stage, TaskStage::Planning),
        "an answer-less run re-plans; it does not end the session"
    );

    absorb(&mut state, &reconnaissance_result(0));
    assert_eq!(
        state.task.no_answer_rounds, 2,
        "the streak is what the prompt counts, not a per-run flag"
    );

    // A run that DOES answer clears it: the next re-plan is not a stall.
    state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
    on_cmd_result(
        &mut state,
        "[exitCode:0]\n=== Reading Task File ===\nANSWER: {\"total_count\": 42}",
    );
    assert_eq!(state.task.no_answer_rounds, 0);
    assert!(matches!(state.task.stage, TaskStage::HaveAnswer { .. }));
}

#[test]
fn a_blocked_answer_still_counts_as_a_stall() {
    // #111's session 1 printed `ANSWER: TOKEN_PENDING_MANUAL_REVIEW` on three
    // separate runs. Each one is a run that produced no usable answer, so each
    // one has to push the prompt the same way an answer-less run does —
    // otherwise the session with the loudest failure signal looks the calmest.
    let mut state = session_planning();
    absorb(
        &mut state,
        "[exitCode:0]\nANSWER: TOKEN_PENDING_MANUAL_REVIEW",
    );
    assert_eq!(state.task.no_answer_rounds, 1);
    assert!(
        state.task.best_answer.is_empty(),
        "a placeholder never becomes the value the deadline guard submits"
    );

    absorb(&mut state, "[exitCode:0]\nANSWER: ，形式：");
    assert_eq!(state.task.no_answer_rounds, 2);
    assert!(state.task.best_answer.is_empty());
}

#[test]
fn the_task_file_the_script_already_read_reaches_the_retry() {
    // The mechanism, tested end to end: the answer the model needs sits well
    // past the old 600-character cut, and it must appear in the next prompt.
    let mut state = session_planning();
    let result = reconnaissance_result(0);
    assert!(
        result.chars().count() > 1200,
        "the fixture has to be longer than the old window to test it"
    );
    absorb(&mut state, &result);

    let turn = turn_from(board(40));
    let prompt = build_prompt(&state, &turn);
    assert!(
        prompt.contains("统计世界遗产总数"),
        "the content the script read must survive into the retry prompt"
    );
    assert!(
        prompt.contains("FIELDS: total_count"),
        "the reconnaissance echo travels with it"
    );
    assert!(
        prompt.contains("did not print an `ANSWER:` line"),
        "and the retry is told what was actually missing: the answer marker"
    );
    assert!(
        prompt.contains("Do not repeat file exploration with find"),
        "the exploration the last three rounds repeated is called off by name"
    );
}

#[test]
fn the_second_answerless_round_stops_asking_and_starts_insisting() {
    // One answer-less run can be a script that needed two passes. Two is a
    // loop, and the round it costs is a round the task does not have: the
    // timeout is 2-15 rounds and a command round-trip is 2-3 of them.
    let mut state = session_planning();
    absorb(&mut state, &reconnaissance_result(0));
    let first = build_prompt(&state, &turn_from(board(40)));
    assert!(
        !first.contains("You MUST print an ANSWER line"),
        "the first answer-less round is a warning, not an ultimatum"
    );

    absorb(&mut state, &reconnaissance_result(0));
    let second = build_prompt(&state, &turn_from(board(41)));
    assert!(
        second.contains("You MUST print an ANSWER line"),
        "the second one says the round is the task's last chance to score"
    );
    assert!(
        second.contains("2 consecutive answer-less round(s)"),
        "and it says how long the loop has been running"
    );
    assert!(
        second.contains("Rounds remaining: 19"),
        "with the budget left, so the trade is visible: {second}"
    );
}

#[test]
fn a_session_that_never_stalls_gets_no_stall_text() {
    // No-regression half: the ordinary path — reconnaissance, answer, submit —
    // must not carry a single word about stalling, or the prompt grows for
    // every session while only the broken ones need it.
    let mut state = session_planning();
    state.task.result_history = vec!["[exitCode:0]\nANSWER: 42".into()];
    state.task.stage = TaskStage::Planning;
    let prompt = build_prompt(&state, &turn_from(board(40)));
    assert!(
        !prompt.contains("did not print an `ANSWER:` line"),
        "the stall paragraph belongs to sessions that stalled"
    );
    assert!(
        !prompt.contains("You MUST print an ANSWER line"),
        "and so does the ultimatum"
    );
}

#[test]
fn the_result_window_is_wider_than_the_old_six_hundred_characters() {
    // Pinned as a number because it is the whole fix: the runs are 2.4k-5.5k
    // characters, so a 600-character window can only ever show the banner.
    let mut state = session_planning();
    // A marker 1500 characters in: inside the new window, far outside the old.
    let filler = "x".repeat(1500);
    absorb(
        &mut state,
        &format!("[exitCode:0]\n{filler}\nREQUIREMENT_MARKER ANSWER_IS_42\n"),
    );
    let prompt = build_prompt(&state, &turn_from(board(40)));
    assert!(
        prompt.contains("REQUIREMENT_MARKER"),
        "1500 characters into the run is inside the window now"
    );
}
