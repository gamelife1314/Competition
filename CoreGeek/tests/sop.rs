//! P1 item 4: parameterised SOP templates, answer schema checks and the
//! partial-answer fallback. These are the task-chain behaviours that decide
//! `score1`, kept in their own file so they exercise the library directly.

use serde_json::{json, Value};

use coregeek::brain::task::{
    is_meta_answer,
    partial_answer,
};
use coregeek::brain::Plan;
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request};
use coregeek::state::{BotState, SopEntry, TaskStage};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// Smallest daytime world that still produces a usable `Turn` for the task
/// state machine (the pioneer only needs to exist for `plan_pioneer`).
fn task_world(round_no: i64) -> Value {
    json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [],
            "roles": [{
                "id": 10011, "pos": {"x": 14, "y": 14}, "roleType": "pioneer",
                "health": 200, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 40, "backpack": []
            }]
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    })
}

/// `task_world` with the LLM's answer delivered in this round.
fn llm_world(round_no: i64, resp: &str) -> Value {
    let mut world = task_world(round_no);
    world["llmResp"] = json!(resp);
    world
}

#[test]
fn the_command_waits_out_the_round_the_llm_answers() {
    // Issues #18/#19: six sessions per match, every command back as
    // `[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用`, first command to
    // last, while `phaseTask` carried the task and the judger timed the session
    // out on its own clock — so the task WAS running and the refusal was never
    // "no task". The loop fired the command in exactly one phase of the round
    // cycle, the round carrying the LLM's answer, and every re-plan walked
    // straight back into it. The answer round is not an execution round: the
    // command goes out the round after, which costs one round and is the only
    // round of the cycle that was never being probed.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 1;
    state.task.timeout_round = 101;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();
    state.task.stage = TaskStage::Planning;
    state.task.llm_request_round = Some(5);

    let turn = turn_from(llm_world(
        6,
        "```bash\nls /tmp/selfEvolutionTask | wc -l\n```",
    ));
    state.observe(&turn);
    assert!(
        matches!(state.task.stage, TaskStage::HavePlan { .. }),
        "the answer is turned into a plan, got {:?}",
        state.task.stage
    );

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);
    assert!(
        plan.execute_cmd.is_none(),
        "no command is sent while the LLM's answer is the round's payload: {:?}",
        plan.execute_cmd
    );
    assert!(state.task.cmd_history.is_empty(), "nothing was executed");
    assert!(
        matches!(state.task.stage, TaskStage::HavePlan { .. }),
        "the plan is held, not discarded"
    );

    // The next round has no answer in it: this is the round the sandbox runs in.
    let turn = turn_from(task_world(7));
    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);
    assert_eq!(
        plan.execute_cmd.as_deref(),
        Some("ls /tmp/selfEvolutionTask | wc -l"),
        "the held command goes out one round later"
    );
    assert_eq!(state.task.cmd_history.len(), 1, "and it is recorded once");
    assert!(matches!(
        state.task.stage,
        TaskStage::WaitingCmdResult { .. }
    ));
}

#[test]
fn a_command_the_sandbox_ran_is_reused_even_when_its_answer_was_rejected() {
    // 自进化 is reuse, and reuse is what makes the short tasks winnable: a
    // session that must spend an LLM round-trip before its first command cannot
    // finish inside the 2-15 round timeouts of issues #18/#19 at all. What
    // earns the cache is proof that the sandbox RAN the script — an
    // `[exitCode:N]` verdict on a run that printed an answer — never the mere
    // absence of an error, which is also what a command that never ran looks
    // like.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.task_type = "自进化类1".into();
    state.task.description = "count files in directory".into();
    state.task.cmd_history = vec!["ls | wc -l".into()];
    state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
    state.task.cmd_request_round = Some(4);
    coregeek::brain::task::on_cmd_result(&mut state, "[exitCode:0]\nANSWER: 41");
    assert_eq!(
        state.task.sop_cmd.as_deref(),
        Some("ls | wc -l"),
        "the command that answered is the one the sandbox ran"
    );
    // Only a successful task caches its script: a wrong answer is not reusable,
    // and replaying it for a different task of the same kind poisons the cache.
    state.finish_task(false, "timeout");
    assert!(
        state.sop_cache.is_empty(),
        "a rejected answer is not cached: only success earns the SOP"
    );

    // A session whose only verdicts were refusals caches nothing.
    let mut refused = BotState::default();
    refused.task.active = true;
    refused.task.task_type = "自进化类1".into();
    refused.task.description = "count files in directory".into();
    refused.task.cmd_history = vec!["ls | wc -l".into()];
    refused.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
    refused.task.cmd_request_round = Some(4);
    coregeek::brain::task::on_cmd_result(
        &mut refused,
        "[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用",
    );
    refused.finish_task(false, "timeout");
    assert!(
        refused.sop_cache.is_empty(),
        "a command that never ran teaches the next task nothing"
    );
}

#[test]
fn a_script_that_says_it_failed_has_not_answered() {
    // Issue #20: eight sessions, 0 points, every one of them with a script the
    // sandbox ran to completion (exit=0) and an answer read back as `xxx` or
    // `failed_to_extract` — the sentinel the model's own error path prints when
    // it cannot find or parse the task input. `ANSWER:` marks the RESULT, so a
    // marker carrying a sentinel is a failed run: it must re-plan (the sentinel
    // rides into the next prompt's failure context), and it must not be
    // recorded, because what is recorded is what the deadline submits and what
    // the SOP cache learns.
    for sentinel in [
        "xxx",
        "failed_to_extract",
        "extract_failed",
        "TODO",
        "unknown",
        "N/A",
        "无",
        "提取失败",
        "failed",
    ] {
        let mut state = BotState::default();
        state.task.active = true;
        state.task.session_id = 1;
        state.task.task_type = "自进化类1".into();
        state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();
        state.task.cmd_history = vec!["ls /tmp/selfEvolutionTask | wc -l".into()];
        state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
        state.task.cmd_request_round = Some(4);
        coregeek::brain::task::on_cmd_result(
            &mut state,
            &format!("[exitCode:0]\nANSWER: {sentinel}"),
        );
        assert!(
            matches!(state.task.stage, TaskStage::Planning),
            "`{sentinel}` is a failed run, not an answer"
        );
        assert!(
            state.task.best_answer.is_empty(),
            "`{sentinel}` must never become the recorded answer"
        );
        assert!(
            state.task.sop_cmd.is_none(),
            "a script whose answer is a sentinel is not worth caching"
        );
        state.finish_task(false, "timeout");
        assert!(
            state.sop_cache.is_empty(),
            "`{sentinel}` must not teach the SOP cache"
        );
    }
}

#[test]
fn a_real_answer_is_still_an_answer() {
    // The other half of the sentinel rule: it is a whole-answer match, so a
    // result that merely contains one of those words survives, and a plain
    // scalar is untouched. Over-filtering here would be worse than the bug.
    use coregeek::brain::task::is_failure_answer;
    for real in [
        "41",
        "0",
        "/tmp/selfEvolutionTask/task_1.md",
        "failed_to_extract.log",
        "error_count=7",
        r#"{"files":41,"lines":812}"#,
        "提取失败的原因有三点",
    ] {
        assert!(
            !is_failure_answer(real),
            "`{real}` is a result, not a sentinel"
        );
    }
    for sentinel in ["xxx", " failed_to_extract。", "\"N/A\"", "  TODO  "] {
        assert!(is_failure_answer(sentinel), "`{sentinel}` is a sentinel");
    }

    // And the timeout guard: a session whose only "answer" was a sentinel
    // submits nothing rather than a guaranteed zero.
    let mut state = BotState::default();
    state.task.best_answer = "failed_to_extract".into();
    state.task.result_history = vec!["[exitCode:0]\nANSWER: failed_to_extract".into()];
    assert_eq!(
        partial_answer(&state),
        None,
        "a sentinel is not a partial answer either"
    );
}

#[test]
fn a_markdown_heading_is_not_an_answer() {
    // Issue #22, session 3: with the sentinels gone the model answered with a
    // section title — the shape of an answer instead of a value. Same defect,
    // same treatment: a failed run that re-plans instead of a recorded answer
    // that blocks the session until the timeout.
    for heading in ["## 结果", "# 统计结果", "### 答案：", "**结果**"] {
        let mut state = BotState::default();
        state.task.active = true;
        state.task.session_id = 3;
        state.task.task_type = "自进化类1".into();
        state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();
        state.task.cmd_history = vec!["find /tmp/selfEvolutionTask | wc -l".into()];
        state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
        state.task.cmd_request_round = Some(4);
        coregeek::brain::task::on_cmd_result(
            &mut state,
            &format!("[exitCode:0]\nANSWER: {heading}"),
        );
        assert!(
            matches!(state.task.stage, TaskStage::Planning),
            "`{heading}` is decoration, not a result"
        );
        assert!(
            state.task.best_answer.is_empty(),
            "`{heading}` must never become the recorded answer"
        );
        assert!(state.task.sop_cmd.is_none(), "nor teach the SOP cache");
    }
    // The marker is a `#` RUN followed by space, so a value that merely starts
    // with the character — a colour, a tag — is still an answer.
    for value in ["#fff", "#123456", "##41"] {
        assert!(!is_meta_answer(value), "`{value}` is a value");
    }
}

fn sop(task_type: &str, keywords: &[&str], template: &str) -> SopEntry {
    SopEntry {
        task_type: task_type.into(),
        keywords: keywords.iter().map(|kw| kw.to_string()).collect(),
        description: format!("统计{}", keywords.join("、")),
        template: template.into(),
        ..Default::default()
    }
}

#[test]
fn sop_template_binds_new_parameter_values() {
    let entry = sop(
        "自进化类1",
        &["城市名", "人口", "统计"],
        "city=\"{{城市名}}\"\npython3 report.py --city $city",
    );
    let bound = entry
        .bind("任务：统计城市名：上海 的人口")
        .expect("the parameter is present in the description");
    assert!(bound.contains("上海"), "bound script: {bound}");
    assert!(!bound.contains("{{"), "every placeholder is resolved");
}

#[test]
fn sop_template_refuses_to_run_with_stale_values() {
    // The new task does not name the parameter: running the cached script would
    // silently answer about the PREVIOUS task's city, which is worse than
    // paying for a fresh LLM round trip.
    let entry = sop(
        "自进化类1",
        &["城市名", "人口", "统计"],
        "python3 report.py --city {{城市名}}",
    );
    assert!(entry.bind("任务：统计人口总量").is_none());
}

#[test]
fn sop_reuse_requires_a_matching_fingerprint_not_just_the_type() {
    let mut state = BotState::default();
    state.sop_cache.push(sop(
        "自进化类1",
        &["城市名", "人口", "统计", "排名"],
        "python3 report.py --city {{城市名}}",
    ));

    // Same type, overlapping fingerprint, parameter bindable → reuse.
    assert!(
        state
            .find_sop("自进化类1", "任务：统计城市名：北京 的人口排名")
            .is_some(),
        "a fingerprint match with a bindable parameter is reused"
    );
    // Same type, nothing in common → fall back to the LLM.
    assert!(
        state
            .find_sop("自进化类1", "请修复编译错误并重新打包")
            .is_none(),
        "task type alone never justifies reuse"
    );
    // Same type, matching fingerprint, but the parameter cannot be bound.
    assert!(
        state
            .find_sop("自进化类1", "统计人口排名的分布情况并输出报告")
            .is_none(),
        "an unbindable template is not reused"
    );
}

#[test]
fn cached_sop_triggers_llm_with_reuse_context() {
    // The new SOP flow: a cached script is passed to the LLM as reference.
    // The LLM adapts it to the new task, rather than replaying it directly.
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "任务：统计城市名：上海 的人口排名".into();
    state.task.stage = TaskStage::Planning;
    state.sop_cache.push(sop(
        "自进化类1",
        &["城市名", "人口", "统计", "排名"],
        "echo {{城市名}}",
    ));

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);

    assert!(
        plan.prompt.is_some(),
        "SOP reuse triggers an LLM call with the cached script as reference"
    );
    assert!(
        plan.execute_cmd.is_none(),
        "no direct execution: the LLM writes the adapted script"
    );
    assert!(
        state.task.sop_reuse_script.is_some(),
        "the cached script is stored for the prompt"
    );
}

#[test]
fn a_meta_or_sentinel_answer_is_never_submitted() {
    // The red line, at the last gate it could ever cross: every production path
    // into HaveAnswer filters meta/sentinel answers upstream, but even if one
    // arrived there it is sent back to Planning, never submitted.
    let turn = turn_from(task_world(6));
    for answer in [
        "{\"status\":\"parsed\",\"content_length\":534}",
        "failed_to_extract",
    ] {
        let mut state = BotState::default();
        state.task.active = true;
        state.task.session_id = 1;
        state.task.accepted_round = 6;
        state.task.timeout_round = 260;
        state.task.task_type = "自进化类1".into();
        state.task.description = "统计结果，输出 JSON，包含 name 和 count".into();
        state.task.stage = TaskStage::HaveAnswer {
            answer: answer.into(),
        };
        let pioneer = turn.role_by_id(10011).unwrap();
        let mut plan = Plan::default();
        assert!(
            coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan).is_none(),
            "{answer:?} is never submitted"
        );
        assert!(
            matches!(state.task.stage, TaskStage::Planning),
            "{answer:?} goes back to Planning"
        );
    }
}

#[test]
fn schema_gaps_never_block_submission_near_the_timeout() {
    // Two rounds left: a partial answer still earns its pass rate, so the
    // submission must go out instead of gambling on a fresh plan.
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 8;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计结果，输出 JSON，包含 name 和 count".into();
    state.task.stage = TaskStage::HaveAnswer {
        answer: "{\"name\": \"a.txt\"}".into(),
    };

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan)
        .expect("submitted anyway at the deadline");
    assert_eq!(cmd.action, "submitAnswer");
}

// ---------------------------------------------------------------------------
// P0-2: the FIELDS echo — the schema read out of the sandbox task file.
// ---------------------------------------------------------------------------

#[test]
fn bound_sop_keeps_the_answer_marker_contract() {
    // The reused script must still be the same shape the extractor expects:
    // bound parameters change the inputs, never the ANSWER: contract.
    let entry = sop(
        "自进化类1",
        &["城市名", "人口"],
        "python3 - <<'PYEOF'\ncity = \"{{城市名}}\"\nprint(f'ANSWER: {{\"city\": \"{city}\"}}')\nPYEOF",
    );
    let bound = entry.bind("统计城市名：广州 的人口").unwrap();
    assert!(bound.contains("广州"));
    assert_eq!(
        coregeek::brain::task::extract_answer(&bound),
        None,
        "the template itself is a script, not an answer"
    );
}

#[test]
fn positional_parameter_binding_prefers_the_first_mention() {
    let entry = sop("自进化类1", &["文件名"], "wc -l {{文件名}}");
    assert_eq!(
        entry.bind("统计文件名：/tmp/a.log 的行数").unwrap(),
        "wc -l /tmp/a.log"
    );
    let _ = Pos { x: 0, y: 0 };
}

// ---------------------------------------------------------------------------
// Issue #15: what the judger is actually handed, and when a task is dropped.
// ---------------------------------------------------------------------------

/// Daytime world with a pioneer and an active task session, ready for
/// `plan_pioneer` to drive one round of the answer stage.
fn task_at(round_no: i64, description: &str, answer: &str) -> (Turn, BotState) {
    let turn = turn_from(task_world(round_no));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 1;
    state.task.timeout_round = 500;
    state.task.task_type = "自进化类1".into();
    state.task.description = description.into();
    state.task.stage = TaskStage::HaveAnswer {
        answer: answer.into(),
    };
    (turn, state)
}

#[test]
fn the_submitted_payload_is_the_bare_value_the_judger_compares() {
    // The same thing one layer up: what reaches `submitAnswer` is the unwrapped
    // value, so the logged payload and the judged payload cannot disagree.
    //
    // The unwrap is unchanged. What changed is its notation on the wire: the
    // bare text `fc1e78eb2a5a` is not a JSON document, and the judger's verdict
    // for one that is not is `答案不是合法 JSON` — a syntax rejection before any
    // field is compared, carried by 表 4b of all four of #201/#203/#204/#205
    // against exactly this payload. The bare value the judger compares and the
    // JSON string that carries it are the same value; see
    // `answer_wire_payload`.
    let (turn, mut state) = task_at(6, "从沙箱中取出访问令牌", "{\"token\":\"fc1e78eb2a5a\"}");
    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan)
        .expect("a single-field answer still submits");
    assert_eq!(cmd.action, "submitAnswer");
    let payload = cmd
        .taskAnswer
        .as_ref()
        .expect("submitAnswer carries the answer");
    assert_eq!(
        payload, "\"fc1e78eb2a5a\"",
        "the unwrapped value goes to the judger as JSON, not as bare text"
    );
}

/// A rejected submission: `round_no` with the judger's verdict text attached.
fn rejection(round_no: i64, description: &str) -> Turn {
    turn_from(json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0, "playerTasks": [],
            "roles": [{
                "id": 10011, "pos": {"x": 14, "y": 14}, "roleType": "pioneer",
                "health": 200, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 40, "backpack": []
            }]
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "errors": [{"errorCode": 2, "description": description}],
    }))
}

/// A session waiting on the verdict of an answer it just submitted.
fn session_awaiting_verdict() -> BotState {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 1;
    state.task.timeout_round = 500;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();
    state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
    state
}

#[test]
fn a_repeatedly_rejected_answer_abandons_the_task() {
    use coregeek::state::MAX_WRONG_ANSWERS;

    // Issue #15: the opponent "直接放弃并把开拓者投入防御" while all five of our
    // sessions burned their whole timeout on a task that had already been
    // judged wrong. The ceiling has not moved (P1-1) — what it counts has. It
    // now counts rejections that carried NO new text, because a repeated
    // verdict is the one piece of evidence that a further rewrite is not the
    // one: the judger has already said this exact thing.
    let mut state = session_awaiting_verdict();

    // The first rejection is informative — the session had never been told
    // anything — so it restarts the counter instead of spending it. The three
    // that follow only repeat the same complaint, and those are what end it.
    for round_no in 1..=MAX_WRONG_ANSWERS as i64 {
        state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
        state.observe(&rejection(round_no, "答案错误"));
        assert!(
            state.task.active,
            "the task is still alive after {round_no} rejected answer(s)"
        );
        assert_eq!(state.task.wrong_answers, (round_no - 1) as i32);
    }

    state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
    state.observe(&rejection(MAX_WRONG_ANSWERS as i64 + 1, "答案错误"));
    assert!(
        !state.task.active,
        "{MAX_WRONG_ANSWERS} rejections carrying no new information must end \
         the task instead of burning the rest of the timeout"
    );
    assert_eq!(
        state.task.session_id, 0,
        "the session was abandoned, not merely paused"
    );
}

#[test]
fn a_rejection_that_names_something_new_restarts_the_give_up_counter() {
    use coregeek::state::MAX_WRONG_ANSWERS;

    // Issue #10's opponent solved its task on the fourth retry by reading the
    // judger's rejection text: `MissingNamedInput: city`, then the next key,
    // then the next. Every one of those verdicts was NEW information, and under
    // the old rule the third one abandoned a session the judger was still
    // actively teaching. P1-1: a new complaint resets the counter, so the
    // session survives well past MAX_WRONG_ANSWERS rejections as long as each
    // one tells it something it did not know.
    let mut state = session_awaiting_verdict();
    for round_no in 1..=(MAX_WRONG_ANSWERS as i64 + 3) {
        state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
        let complaint = format!("键值比对不通过: $/field{round_no}: 缺少键");
        state.observe(&rejection(round_no, &complaint));
        assert!(
            state.task.active,
            "an informed rejection at round {round_no} must not end the session"
        );
        assert_eq!(
            state.task.wrong_answers, 0,
            "new information restarts the give-up counter"
        );
        assert_eq!(
            state.task.rejections, round_no as i32,
            "the monotonic rejection count still records every attempt"
        );
    }
    assert_eq!(
        state.task.rejection_feedback.len(),
        (MAX_WRONG_ANSWERS + 3) as usize,
        "every distinct complaint reached the retry prompt"
    );
}

#[test]
fn a_verdict_with_no_text_keeps_the_plain_three_strike_ceiling() {
    use coregeek::state::MAX_WRONG_ANSWERS;

    // The fallback the analysis asks to keep: with nothing to compare there is
    // no such thing as "new information", so the old three-strike ceiling is
    // the only evidence available and it stands unchanged.
    let mut state = session_awaiting_verdict();
    for round_no in 1..MAX_WRONG_ANSWERS as i64 {
        state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
        state.observe(&rejection(round_no, ""));
        assert!(state.task.active, "still alive after {round_no} blank verdicts");
        assert_eq!(state.task.wrong_answers, round_no as i32);
    }
    state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
    state.observe(&rejection(MAX_WRONG_ANSWERS as i64, ""));
    assert!(!state.task.active, "the ceiling still ends a blind session");
}

/// A world whose `lastCmdResult` is the judger refusing `executeCmd`.
fn refused_command(round_no: i64, result: &str) -> Turn {
    turn_from(json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [{
                "taskType": "自进化类1", "taskPosition": {"x": 14, "y": 14},
                "isValid": true, "coldDownRounds": 0, "timeoutRounds": 100,
                "scoreReward": 10, "goldReward": 10,
            }],
            "roles": [{
                "id": 10011, "pos": {"x": 14, "y": 14}, "roleType": "pioneer",
                "health": 200, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 40, "backpack": []
            }]
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "lastCmdResult": result,
    }))
}

/// Put the session in the state a sent `executeCmd` leaves behind: waiting for
/// the verdict of round `request_round`.
fn awaiting_verdict(state: &mut BotState, request_round: i64) {
    state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
    state.task.cmd_request_round = Some(request_round);
    state.task.cmd_consumed_request_round = None;
}

#[test]
fn a_closed_execution_window_ends_the_session_instead_of_looping() {
    use coregeek::brain::task::window_closed;

    // Issue #17: the judger answered our `executeCmd` with
    // "[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用" four times in one
    // session and once in the next, and both sessions ran out their timeouts at
    // zero task points. Every one of those rounds went back to Planning, asked
    // the LLM for another script and fired it into the same shut window — the
    // script was never the problem, so no amount of re-planning could help.
    assert!(window_closed(
        "[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用"
    ));
    // A script that genuinely failed in the sandbox is NOT this: those are the
    // rounds where a new script is exactly the right answer.
    assert!(!window_closed(
        "[exitCode:1] Traceback (most recent call last)"
    ));
    assert!(!window_closed("[TIMEOUT] 执行超时"));
    assert!(!window_closed("[JUDGER_ERROR] 未知指令 submitAnswer"));

    let (_, mut state) = task_at(1, "统计 /tmp/selfEvolutionTask 下的文件数量", "");
    state.task.point = Some(Pos { x: 14, y: 14 });
    state.task.accepted_round = 1;

    awaiting_verdict(&mut state, 5);
    state.observe(&refused_command(
        6,
        "[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用",
    ));
    assert!(
        state.task.active,
        "a single refusal is retried — it can be a transient sandbox fault"
    );
    assert_eq!(state.task.stage, TaskStage::Planning);

    awaiting_verdict(&mut state, 7);
    state.observe(&refused_command(
        8,
        "[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用",
    ));
    assert!(
        !state.task.active,
        "a second refusal to the same command is the window itself: the \
         session has to end rather than re-plan into it"
    );
    assert_eq!(
        state.task_refusals.get(&Pos { x: 14, y: 14 }),
        Some(&130),
        "the point is remembered as refused through the end of the day, or the \
         pioneer walks straight back into the same window"
    );

    // …and the pioneer is free again: the next round it takes an errand instead
    // of accepting the refused point a third time.
    let turn = refused_command(9, "");
    let mut claimed = std::collections::HashSet::new();
    let pioneer = turn.role_by_id(10011).unwrap();
    assert!(
        state
            .next_task_point(&turn, pioneer, &mut claimed)
            .is_none(),
        "the refused point is not accepted again on the same day"
    );

    // A later day is a fresh offer: the point is no longer held against it.
    let turn = refused_command(131, "");
    let pioneer = turn.role_by_id(10011).unwrap();
    let cmd = state
        .next_task_point(&turn, pioneer, &mut claimed)
        .expect("the refusal does not last past the day it was learned on");
    assert_eq!(cmd.action, "acceptTask");
}

#[test]
fn a_verdict_that_is_not_about_the_window_clears_the_streak() {
    // Two refusals in a row end a session; a real sandbox run in between proves
    // the window is open, so the count restarts.
    let (_, mut state) = task_at(1, "统计 /tmp/selfEvolutionTask 下的文件数量", "");
    state.task.point = Some(Pos { x: 14, y: 14 });
    state.task.accepted_round = 1;

    awaiting_verdict(&mut state, 5);
    state.observe(&refused_command(
        6,
        "[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用",
    ));
    assert_eq!(state.task.judger_window_errors, 1);

    awaiting_verdict(&mut state, 7);
    state.observe(&refused_command(8, "[exitCode:1] NameError: name 'x'"));
    assert_eq!(state.task.judger_window_errors, 0);
    assert!(
        state.task.active,
        "a script error is re-planned, not abandoned"
    );

    awaiting_verdict(&mut state, 9);
    state.observe(&refused_command(
        10,
        "[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用",
    ));
    assert!(
        state.task.active,
        "one refusal after a working window is not enough to abandon"
    );
}

#[test]
fn a_sentinel_wrapped_in_json_is_still_a_sentinel() {
    // Issue #28: the model's error path dresses its give-up in the answer's
    // JSON shape — `{"result": "unknown"}`, `{"status": "pending"}` — and the
    // whole-answer test let both through to the judger. 任务书 ch.6 scores
    // `回答正确字段个数 / 全量字段个数`, so a wrapper full of nothing is a
    // guaranteed zero, and it costs the round a real attempt needed.
    use coregeek::brain::task::is_failure_answer;
    for sentinel in [
        r#"{"result": "unknown"}"#,
        r#"{"status": "pending"}"#,
        r#"{"status":"processing"}"#,
        r#"{"answer": "N/A"}"#,
        r#"{"value": null}"#,
        r#"["unknown", "none"]"#,
        "{}",
    ] {
        assert!(
            is_failure_answer(sentinel),
            "`{sentinel}` answers nothing and must never be submitted"
        );
    }
    // The rule is "every leaf is a sentinel", never "contains a sentinel word":
    // a real object that happens to carry one sentinel-valued field still has
    // fields 任务书 ch.6 will score.
    for real in [
        r#"{"unknown": 42}"#,
        r#"{"status": "completed", "count": 7}"#,
        r#"{"total_count": 0}"#,
        r#"{"city": "南京", "count": 5}"#,
        r#"{"note": "unknown", "answer": 12}"#,
    ] {
        assert!(
            !is_failure_answer(real),
            "`{real}` carries real fields and must survive the filter"
        );
    }
}

// ---------------------------------------------------------------------------
// P1-2: the SCHEMA echo — the output schema read out of the sandbox task file.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// SOP reuse: a cached script is passed to the LLM as reference, not replayed
// directly. The LLM adapts it to the new task description.
// ---------------------------------------------------------------------------

use coregeek::state::keywords_of;

#[test]
fn a_cached_sop_triggers_an_llm_call_with_reuse_context() {
    // The new SOP flow: find a cached script → set sop_reuse_script → build a
    // prompt that includes the cached script as reference. The LLM adapts it
    // instead of re-reading the task file from scratch.
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "任务：统计城市名：北京 的人口排名".into();
    state.task.stage = TaskStage::Planning;
    state.sop_cache.push(sop(
        "自进化类1",
        &["城市名", "人口", "统计", "排名"],
        "python3 report.py --city {{城市名}}",
    ));

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);
    assert!(
        plan.prompt.is_some(),
        "SOP reuse triggers an LLM call, not direct replay"
    );
    assert!(
        plan.execute_cmd.is_none(),
        "no direct execution: the LLM writes the adapted script"
    );
    assert_eq!(
        state.task.sop_reuse_script.as_deref(),
        Some("python3 report.py --city {{城市名}}"),
        "the cached script is stored for the prompt to include"
    );
    assert_eq!(
        state.task.sop_used_template.as_deref(),
        Some("python3 report.py --city {{城市名}}"),
        "a later rejection is charged to the entry that matched"
    );
}

#[test]
fn a_session_that_answered_caches_the_script() {
    // cache_sop stores the script that produced the answer. No separate
    // explore stage is cached — the LLM adapts the answer script directly.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计城市名：北京 的人口".into();
    state.task.cmd_history = vec![
        "find /tmp/selfEvolutionTask -maxdepth 4".into(),
        "python3 report.py --city 北京".into(),
    ];
    state.task.best_answer = "2200".into();
    state.task.sop_cmd = Some("python3 report.py --city 北京".into());
    state.cache_sop();

    assert_eq!(state.sop_cache.len(), 1);
    assert_eq!(state.sop_cache[0].template, "python3 report.py --city 北京");

    // A session that answered with its FIRST command: same behavior, the
    // answering script is the template.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 2;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计城市名：北京 的人口".into();
    state.task.cmd_history = vec!["python3 report.py --city 北京".into()];
    state.task.best_answer = "2200".into();
    state.task.sop_cmd = Some("python3 report.py --city 北京".into());
    state.cache_sop();
    assert_eq!(state.sop_cache[0].template, "python3 report.py --city 北京");
}

#[test]
fn a_rejected_sop_is_still_evicted_entry_by_entry() {
    // Per-entry strike eviction: a rejection charges the used template, and
    // two consecutive strikes evict exactly that entry.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 300;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计城市名：北京 的人口".into();
    state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };
    state.task.sop_used_template = Some("python3 report.py --city {{城市名}}".into());
    state.task.cmd_history = vec!["python3 report.py --city 北京".into()];
    state.sop_cache.push(sop(
        "自进化类1",
        &["城市名", "人口", "统计"],
        "python3 report.py --city {{城市名}}",
    ));

    let rejected = |round_no: i64| {
        turn_from(json!({
            "roundNo": round_no,
            "mapInfo": {"width": 41, "height": 32, "zones": []},
            "teamOur": {
                "type": "challenger", "goldNum": 0, "totalScore": 0, "playerTasks": [],
                "roles": [{
                    "id": 10011, "pos": {"x": 14, "y": 14}, "roleType": "pioneer",
                    "health": 200, "attackPower": 0, "attackRange": 0,
                    "backPackCapability": 40, "backpack": []
                }]
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
            "errors": [{"errorCode": 2, "description": "键值比对不通过: $/token: 缺少键"}],
        }))
    };

    state.observe(&rejected(7));
    assert_eq!(state.sop_cache.len(), 1, "one strike keeps the entry");
    assert_eq!(state.sop_cache[0].rejections, 1);

    state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };
    state.task.sop_used_template = Some("python3 report.py --city {{城市名}}".into());
    state.observe(&rejected(8));
    assert!(
        state.sop_cache.is_empty(),
        "a second consecutive strike evicts the entry"
    );
}

// ---------------------------------------------------------------------------
// P2-4: the task line's income, banked where the attribution can read it.
// ---------------------------------------------------------------------------

/// `task_world` whose task point has been closed by the judger (`isValid`
/// false), which is how a confirmed session's closure is signalled.
fn closed_point_world(round_no: i64, gold_reward: i64) -> Value {
    let mut world = task_world(round_no);
    world["teamOur"]["playerTasks"] = json!([{
        "taskType": "自进化类1",
        "taskPosition": {"x": 14, "y": 14},
        "coldDownRounds": 0,
        "scoreReward": 50,
        "goldReward": gold_reward,
        "isValid": false,
        "timeoutRounds": 100,
    }]);
    world
}

/// A session that has submitted an answer and is waiting on the closure probe.
fn session_awaiting_closure() -> BotState {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 1;
    state.task.submitted_round = Some(1);
    state.task.timeout_round = 500;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();
    state.task.point = Some(Pos { x: 14, y: 14 });
    state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
    state
}

#[test]
fn a_confirmed_task_banks_the_points_advertised_gold() {
    // 任务书 ch.6 pays 奖励 × 通过率 and the judger never tells us the pass
    // rate, so the point's own `goldReward` is the upper bound of the session's
    // income and the only figure the log can honestly carry. Without it the
    // round record lumps task gold in with everything else, and "did the task
    // line earn anything this match" has no answer in the log at all.
    let mut state = session_awaiting_closure();
    assert_eq!(state.task_gold_earned, 0);

    // Closure takes three independent signals over subsequent rounds: the point
    // closed, `phaseTask` empty twice, no errors. The first round only starts
    // the count.
    state.observe(&turn_from(closed_point_world(2, 120)));
    assert!(
        state.task.active,
        "one quiet round is not a confirmed session"
    );
    assert_eq!(state.task_gold_earned, 0, "nothing is banked before it is confirmed");

    state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
    state.observe(&turn_from(closed_point_world(3, 120)));
    assert!(!state.task.active, "the session is confirmed and retired");
    assert_eq!(
        state.task_gold_earned, 120,
        "the point's advertised reward is banked"
    );
}

#[test]
fn only_a_confirmation_banks_gold_never_a_rejection() {
    // A rejected answer ends the session earlier and must leave the attribution
    // untouched: the task line earned nothing, and a report that read a
    // rejection as income would credit every fix twice.
    let mut state = session_awaiting_closure();
    let mut payload = closed_point_world(2, 120);
    payload["errors"] = json!([{"errorCode": 2, "description": "键值比对不通过: $/token: 缺少键"}]);
    state.observe(&turn_from(payload));
    assert_eq!(state.task_gold_earned, 0);
}

#[test]
fn a_point_with_no_advertised_reward_banks_nothing() {
    // Older captures and stale points carry `goldReward` 0 — banking it would
    // write a `task_reward` record that claims an income of nothing.
    let mut state = session_awaiting_closure();
    state.observe(&turn_from(closed_point_world(2, 0)));
    state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
    state.observe(&turn_from(closed_point_world(3, 0)));
    assert_eq!(state.task_gold_earned, 0);
}
