//! P1 item 4: parameterised SOP templates, answer schema checks and the
//! partial-answer fallback. These are the task-chain behaviours that decide
//! `score1`, kept in their own file so they exercise the library directly.

use serde_json::{json, Value};

use coregeek::brain::task::{
    answer_schema_extras, answer_schema_gaps, expected_fields, is_meta_answer, merge_json_fields,
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
    state.finish_task(false, "timeout");
    assert!(
        !state.sop_cache.is_empty(),
        "a script the sandbox ran is worth reusing next task"
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

#[test]
fn a_field_the_task_never_asked_for_is_a_wrong_answer() {
    // Issue #22, session 5: `{"city": "Nanjing", "task_id": 2, "status":
    // "completed"}` for a task that asked for two fields. The judger scores the
    // submitted object against the schema it named, so the invented `status` is
    // wrong on its own — and the only place to catch it is before submission,
    // because nothing about the answer looks malformed.
    let description = "任务：输出 city 与 task_id";
    assert_eq!(
        expected_fields(description),
        vec!["city", "task_id"],
        "the task text names a two-field schema"
    );
    let invented = r#"{"city": "Nanjing", "task_id": 2, "status": "completed"}"#;
    assert_eq!(
        answer_schema_extras(description, invented),
        vec!["status"],
        "an invented field is reported"
    );
    assert!(
        answer_schema_gaps(description, invented).is_empty(),
        "every required field is present — the extra one is the whole defect"
    );

    // The judge, after submit-as-accumulating (P0-1): the misshapen answer is
    // BANKED immediately — the judger keeps the highest pass rate ever
    // submitted, so holding it back can only lower the floor. The invented
    // field is recorded for the next prompt, and the judger's rejection (or the
    // recorded extras) drives the replan that overwrites the banked attempt.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 5;
    state.task.task_type = "自进化类1".into();
    state.task.description = description.into();
    state.task.stage = TaskStage::HaveAnswer {
        answer: invented.into(),
    };
    let turn = turn_from(task_world(40));
    state.task.timeout_round = turn.round_no + 20;
    let pioneer = turn.pioneer().expect("the world has a pioneer");
    let mut plan = Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan)
        .expect("the attempt is banked even with the shape wrong");
    assert_eq!(cmd.action, "submitAnswer");
    assert!(
        matches!(state.task.stage, TaskStage::WaitingSubmit { .. }),
        "the session waits for the judger's verdict, got {:?}",
        state.task.stage
    );
    assert_eq!(state.task.schema_extras, vec!["status"]);
    assert!(coregeek::brain::task::build_prompt(&state, &turn).contains("status"));

    // The corrected answer — exactly the two fields the task asked for — is
    // submitted unchanged.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 6;
    state.task.task_type = "自进化类1".into();
    state.task.description = description.into();
    let correct = r#"{"city": "Nanjing", "task_id": 2}"#;
    state.task.stage = TaskStage::HaveAnswer {
        answer: correct.into(),
    };
    state.task.timeout_round = turn.round_no + 20;
    let mut plan = Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan)
        .expect("a conforming answer is submitted");
    assert_eq!(cmd.action, "submitAnswer");
    assert_eq!(cmd.taskAnswer.as_deref(), Some(correct));
}

fn sop(task_type: &str, keywords: &[&str], template: &str) -> SopEntry {
    SopEntry {
        task_type: task_type.into(),
        keywords: keywords.iter().map(|kw| kw.to_string()).collect(),
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
fn cached_sop_runs_without_an_llm_round_trip() {
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

    let cmd = plan.execute_cmd.expect("the cached script is executed");
    assert!(cmd.contains("上海"), "bound at execution time: {cmd}");
    assert!(
        plan.prompt.is_none(),
        "a reused SOP must not spend an LLM call"
    );
}

#[test]
fn expected_fields_only_fires_on_an_explicit_schema() {
    // Prose without a schema keyword yields nothing, so the check stays off.
    assert!(expected_fields("请统计 /tmp/selfEvolutionTask 下所有文件的行数").is_empty());
    let fields = expected_fields("输出 JSON，包含 name 和 count");
    assert_eq!(fields, vec!["name".to_string(), "count".to_string()]);
}

#[test]
fn answer_missing_required_fields_is_banked_and_feedback_recorded() {
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计结果，输出 JSON，包含 name 和 count".into();
    // A genuinely incomplete answer: one of the two required fields.
    state.task.stage = TaskStage::HaveAnswer {
        answer: "{\"name\": \"a.txt\"}".into(),
    };

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan)
        .expect("the partial answer is banked, not held back");

    assert_eq!(
        cmd.action, "submitAnswer",
        "submit-as-accumulating: the judger keeps the best pass rate, so the partial attempt is banked"
    );
    assert!(
        matches!(state.task.stage, TaskStage::WaitingSubmit { .. }),
        "then the session waits for the verdict, got {:?}",
        state.task.stage
    );
    assert_eq!(
        state.task.schema_gaps,
        vec!["count"],
        "the missing field is recorded for the replan that follows the rejection"
    );
    let prompt = coregeek::brain::task::build_prompt(&state, &turn);
    assert!(
        prompt.contains("count"),
        "the retry prompt names the missing field"
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
fn answer_with_every_required_field_is_submitted() {
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计结果，输出 JSON，包含 name 和 count".into();
    state.task.stage = TaskStage::HaveAnswer {
        answer: "{\"name\": \"a.txt\", \"count\": 3}".into(),
    };

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan)
        .expect("the complete answer is submitted");
    assert_eq!(cmd.action, "submitAnswer");
    assert!(state.task.schema_gaps.is_empty());
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

#[test]
fn partial_answer_merges_fields_observed_across_runs() {
    let mut state = BotState::default();
    state.task.result_history = vec![
        "[exitCode:0]\nANSWER: {\"name\": \"a.txt\"}".into(),
        "[exitCode:1]\nANSWER: {\"count\": 7}".into(),
    ];
    let merged = partial_answer(&state).expect("a partial answer is built");
    let value: Value = serde_json::from_str(&merged).expect("merged answer is JSON");
    assert_eq!(value["name"], json!("a.txt"));
    assert_eq!(value["count"], json!(7));
}

#[test]
fn partial_answer_includes_the_banked_best_in_the_merge() {
    // Submit-as-accumulating banks every attempt, so the deadline fallback must
    // merge the banked best WITH the run history — a field that only ever
    // appeared in best_answer is still evidence the sandbox produced.
    let mut state = BotState::default();
    state.task.result_history = vec!["[exitCode:0]\nANSWER: {\"name\": \"a.txt\"}".into()];
    state.task.best_answer = "{\"count\": 7}".into();
    let merged = partial_answer(&state).expect("a partial answer is built");
    let value: Value = serde_json::from_str(&merged).expect("merged answer is JSON");
    assert_eq!(value["name"], json!("a.txt"));
    assert_eq!(value["count"], json!(7));

    // A banked best that is a meta/sentinel value still never qualifies.
    let mut state = BotState::default();
    state.task.best_answer = "failed_to_extract".into();
    assert!(partial_answer(&state).is_none());
}

// ---------------------------------------------------------------------------
// P0-2: the FIELDS echo — the schema read out of the sandbox task file.
// ---------------------------------------------------------------------------

#[test]
fn fields_echo_is_parsed_from_command_output() {
    use coregeek::brain::task::extract_fields;
    assert_eq!(
        extract_fields("[exitCode:0]\nFIELDS: city, temperature\nANSWER: {}"),
        Some(vec!["city".to_string(), "temperature".to_string()])
    );
    // Chinese separators, a scalar answer, and no echo at all.
    assert_eq!(
        extract_fields("FIELDS: city、temperature"),
        Some(vec!["city".to_string(), "temperature".to_string()])
    );
    assert_eq!(extract_fields("FIELDS: none"), Some(vec![]));
    assert_eq!(extract_fields("just a file listing"), None);
}

#[test]
fn discovered_fields_drive_the_schema_gate() {
    // The placeholder description names no fields ("请阅读task_X.md，获取任务
    // 信息"), so without the echo the gate has nothing to check against. Once
    // the script reports the real schema from the sandbox file, an answer
    // missing one of those fields is caught — and still banked.
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "请阅读task_1.md，获取任务信息".into();
    state.task.discovered_fields = vec!["city".into(), "temperature".into()];
    state.task.stage = TaskStage::HaveAnswer {
        answer: "{\"city\": \"北京\"}".into(),
    };
    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan)
        .expect("the incomplete answer is still banked");
    assert_eq!(cmd.action, "submitAnswer");
    assert_eq!(state.task.schema_gaps, vec!["temperature"]);
    assert!(coregeek::brain::task::build_prompt(&state, &turn).contains("temperature"));
}

#[test]
fn discovered_fields_drive_the_unwrap_decision() {
    use coregeek::brain::task::submittable_answer_for;
    // Two or more echoed fields: the judger compares an object — untouched.
    let object = "{\"city\":\"Nanjing\",\"task_id\":2}";
    assert_eq!(
        submittable_answer_for(
            &["city".into(), "task_id".into()],
            "whatever",
            object
        ),
        object
    );
    // One echoed field: the answer IS that value — a one-key wrapper is the
    // bot's own formatting and comes off (issue #15's lesson).
    assert_eq!(
        submittable_answer_for(&["token".into()], "whatever", "{\"token\":\"abc123\"}"),
        "abc123"
    );
    // No echo: the legacy description heuristic decides.
    assert_eq!(
        submittable_answer_for(&[], "统计行数", "{\"count\":42}"),
        "42"
    );
}

#[test]
fn merge_json_fields_ignores_unparseable_runs() {
    assert!(merge_json_fields(&["not json".into()]).is_none());
    assert!(merge_json_fields(&["{\"a\": 1}".into()]).is_none());
    assert_eq!(
        merge_json_fields(&["{\"a\": 1}".into(), "{\"b\": 2}".into()]).as_deref(),
        Some("{\"a\":1,\"b\":2}")
    );
}

#[test]
fn schema_gaps_accept_a_plain_text_answer_that_names_the_fields() {
    // Not JSON, but it does carry both field names: nothing to complain about.
    assert!(answer_schema_gaps("输出 name 和 count", "name=a.txt, count=3").is_empty());
    // A single extracted token is too weak a signal to reject on.
    assert!(answer_schema_gaps("输出 count", "{\"total\": 3}").is_empty());
}

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
fn a_single_field_wrapper_is_unwrapped_before_submission() {
    use coregeek::brain::task::submittable_answer;

    // Issue #15 lost a task whose sandbox had already printed the right value:
    // the answer we submitted was `{"token":"fc1e78eb2a5a"}` while the judger
    // compared against the bare `fc1e78eb2a5a`. Our own prompt asks for JSON
    // only to carry *multiple* fields, so a one-key wrapper around a scalar is
    // our formatting, not the task's — unless the task text names that key, in
    // which case the wrapper is exactly what was asked for.
    assert_eq!(
        submittable_answer("从沙箱中取出访问令牌", "{\"token\":\"fc1e78eb2a5a\"}"),
        "fc1e78eb2a5a"
    );
    assert_eq!(submittable_answer("统计行数", "{\"count\":42}"), "42");
    assert_eq!(submittable_answer("是否通过", "{\"ok\":true}"), "true");
    // The task named the field: keep the shape it asked for.
    assert_eq!(
        submittable_answer("输出 token", "{\"token\":\"fc1e78eb2a5a\"}"),
        "{\"token\":\"fc1e78eb2a5a\"}"
    );
    // Two fields is a real JSON answer, never unwrapped.
    assert_eq!(
        submittable_answer("统计", "{\"name\":\"a.txt\",\"count\":3}"),
        "{\"name\":\"a.txt\",\"count\":3}"
    );
    // Non-JSON and empty scalars pass through untouched.
    assert_eq!(submittable_answer("统计", "fc1e78eb2a5a"), "fc1e78eb2a5a");
    assert_eq!(
        submittable_answer("统计", "{\"token\":\"\"}"),
        "{\"token\":\"\"}"
    );
}

#[test]
fn the_submitted_payload_is_the_bare_value_the_judger_compares() {
    // The same thing one layer up: what reaches `submitAnswer` is the unwrapped
    // value, so the logged payload and the judged payload cannot disagree.
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
        payload, "fc1e78eb2a5a",
        "the judger compares against the bare value, not our JSON wrapper"
    );
}

#[test]
fn a_repeatedly_rejected_answer_abandons_the_task() {
    use coregeek::state::MAX_WRONG_ANSWERS;

    // Issue #15: the opponent "直接放弃并把开拓者投入防御" while all five of our
    // sessions burned their whole timeout on a task that had already been
    // judged wrong. Three rejections is the evidence; the fourth attempt is not
    // the one, and the pioneer is worth more on the wall line.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 1;
    state.task.timeout_round = 500;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();

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
            "errors": [{"errorCode": 2, "description": "答案错误"}],
        }))
    };

    for round_no in 1..MAX_WRONG_ANSWERS as i64 {
        // Each submission is judged wrong: the answer goes back to planning and
        // the task survives, because one bad guess proves nothing.
        state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
        state.observe(&rejected(round_no));
        assert!(
            state.task.active,
            "the task is still alive after {round_no} rejected answer(s)"
        );
        assert_eq!(state.task.wrong_answers, round_no as i32);
    }

    state.task.stage = TaskStage::WaitingSubmit { attempts: 1 };
    state.observe(&rejected(MAX_WRONG_ANSWERS as i64 + 1));
    assert!(
        !state.task.active,
        "{MAX_WRONG_ANSWERS} rejected answers must end the task instead of \
         burning the rest of the timeout"
    );
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
