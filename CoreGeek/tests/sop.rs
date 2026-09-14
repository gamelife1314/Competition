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

#[test]
fn a_rejected_answer_is_retried_in_the_other_shape() {
    // 任务书 ch.6 scores the answer field by field, so the wrapper the judger
    // expects is worth as much as the value inside it — and errorCode 2 does
    // not say which of the two was wrong. The retry therefore flips the shape
    // instead of resubmitting the identical bytes.
    use coregeek::brain::task::submittable_answer_shaped;
    let fields = ["token".to_string()];
    let answer = r#"{"token":"fc1e78eb2a5a"}"#;

    // Normal: a single echoed field means the answer IS that value (issue #15).
    assert_eq!(submittable_answer_shaped(&fields, "取 token", answer, false), "fc1e78eb2a5a");
    // Rejected once: submit the shape we did not try — the keyed object.
    assert_eq!(submittable_answer_shaped(&fields, "取 token", answer, true), answer);

    // With no echoed schema the legacy description rule applies, and it flips
    // the same way: the description never names the key, so the wrapper goes —
    // unless the previous attempt already went bare.
    assert_eq!(submittable_answer_shaped(&[], "统计文件数量", answer, false), "fc1e78eb2a5a");
    assert_eq!(submittable_answer_shaped(&[], "统计文件数量", answer, true), answer);

    // Two or more fields mean the object is the answer's real shape; there is
    // nothing to flip and the flip must not invent one.
    let two = ["city".to_string(), "count".to_string()];
    let object = r#"{"city":"南京","count":5}"#;
    assert_eq!(submittable_answer_shaped(&two, "城市与数量", object, false), object);
    assert_eq!(submittable_answer_shaped(&two, "城市与数量", object, true), object);

    // A bare scalar that is NOT JSON at all is the one case where the retry
    // must change something, and the earlier reading of this line ("only the
    // value can be wrong there") is what issues #131-#135 disproved. Five
    // matches, every session, first submission verbatim from the sandbox:
    // `fc1e78eb2a5a`, submitted bare, answered `答案不是合法 JSON` (表 4b,
    // round 23 in all five). The value was never wrong — a token with no JSON
    // wrapper is not an answer in any schema — and resubmitting it identical
    // burned the session's whole 10-15 round budget (表 3b: every session
    // `timeout`, `success:false`). With one known field the wrapper is the only
    // legal shape, so it goes out on the retry AND on the first attempt.
    assert_eq!(
        submittable_answer_shaped(&fields, "取 token", "fc1e78eb2a5a", true),
        r#"{"token":"fc1e78eb2a5a"}"#
    );
    assert_eq!(
        submittable_answer_shaped(&fields, "取 token", "fc1e78eb2a5a", false),
        r#"{"token":"fc1e78eb2a5a"}"#,
        "a non-JSON scalar has no legal bare form, so the first attempt wraps too"
    );
    // The narrow exit still holds: a scalar that ALREADY parses as JSON keeps
    // the model's own shape on the first attempt.
    assert_eq!(submittable_answer_shaped(&fields, "取 token", "15", false), "15");
}

// ---------------------------------------------------------------------------
// P1-2: the SCHEMA echo — the output schema read out of the sandbox task file.
// ---------------------------------------------------------------------------

#[test]
fn a_schema_echo_is_parsed_from_command_output() {
    use coregeek::brain::task::extract_schema;
    use coregeek::state::DiscoveredSchema;

    // A JSON-Schema object: properties are the fields, `required` the subset
    // the answer must carry.
    assert_eq!(
        extract_schema(
            "[exitCode:0]\nSCHEMA: {\"type\":\"object\",\"properties\":{\"token\":{\"type\":\"string\"},\"count\":{\"type\":\"integer\"}},\"required\":[\"token\"]}"
        ),
        Some(DiscoveredSchema {
            fields: vec!["count".into(), "token".into()],
            required: vec!["token".into()],
        })
    );

    // A flat field→type map, which is the shape a task file's own "输出格式"
    // block takes when it is not written as JSON Schema. With no `required`
    // key every named field is required — the only reading that cannot reject
    // a correct answer.
    assert_eq!(
        extract_schema("SCHEMA: {\"token\":\"string\",\"count\":\"integer\"}"),
        Some(DiscoveredSchema {
            fields: vec!["count".into(), "token".into()],
            required: vec!["count".into(), "token".into()],
        })
    );

    // A bare array of names, the fullwidth colon, and the last line winning.
    assert_eq!(
        extract_schema("SCHEMA: [\"city\", \"temperature\"]\nSCHEMA：[\"only\"]"),
        Some(DiscoveredSchema {
            fields: vec!["only".into()],
            required: vec!["only".into()],
        })
    );

    // Junk teaches nothing and must not blank a schema already in hand.
    assert_eq!(extract_schema("SCHEMA: not json"), None);
    assert_eq!(extract_schema("SCHEMA: {}"), None);
    assert_eq!(extract_schema("SCHEMA: []"), None);
    // No line at all: the no-regression case.
    assert_eq!(extract_schema("FIELDS: city, count\nANSWER: {}"), None);
}

#[test]
fn a_declared_schema_outranks_the_fields_echo() {
    use coregeek::brain::task::on_cmd_result;
    use coregeek::state::{DiscoveredSchema, TaskStage};

    // A script that prints both. `FIELDS` names four; the schema it read out of
    // the task file names two and marks one required — the schema is the task
    // speaking for itself and wins.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
    state.task.cmd_history.push("python3 solve.py".into());
    on_cmd_result(
        &mut state,
        "[exitCode:0]\nFIELDS: a, b, c, d\nSCHEMA: {\"properties\":{\"token\":{},\"count\":{}},\"required\":[\"token\"]}\nANSWER: {\"token\":\"x\"}",
    );

    assert_eq!(
        state.task.discovered_schema,
        Some(DiscoveredSchema {
            fields: vec!["count".into(), "token".into()],
            required: vec!["token".into()],
        })
    );
    // The FIELDS echo is still recorded — it is a different channel and the
    // schema may be replaced by a later run's better one.
    assert_eq!(state.task.discovered_fields, vec!["a", "b", "c", "d"]);
}

#[test]
fn a_declared_schema_rejects_a_single_missing_field() {
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "请阅读task_1.md，获取任务信息".into();
    // ONE required field, and the answer does not carry it. The description
    // names no fields at all, so nothing but the declared schema can catch
    // this — and a one-field *guess* never may (it would reject a correct
    // answer on a hunch), which is exactly why the schema has to be declared.
    state.task.discovered_schema = Some(coregeek::state::DiscoveredSchema {
        fields: vec!["count".into()],
        required: vec!["count".into()],
    });
    state.task.stage = TaskStage::HaveAnswer {
        answer: "{\"total\": 3}".into(),
    };

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    let cmd = coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan)
        .expect("submit-as-accumulating still banks the attempt");
    assert_eq!(cmd.action, "submitAnswer");
    assert_eq!(
        state.task.schema_gaps,
        vec!["count"],
        "a declared schema may reject on one field"
    );
    assert!(
        coregeek::brain::task::build_prompt(&state, &turn).contains("count"),
        "and the retry prompt names it"
    );

    // The same answer with no declared schema: the single-field guess is too
    // weak to act on, and nothing is recorded. This is the no-regression half.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "请阅读task_1.md，获取任务信息".into();
    state.task.stage = TaskStage::HaveAnswer {
        answer: "{\"total\": 3}".into(),
    };
    coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);
    assert!(
        state.task.schema_gaps.is_empty(),
        "an undeclared single field is never checked"
    );
}

#[test]
fn a_declared_single_field_schema_drives_the_unwrap() {
    use coregeek::brain::task::plan_pioneer;

    // The schema says the answer IS one value, so the bot's own one-key wrapper
    // comes off before submission (the issue #15 lesson, now driven by the
    // schema rather than by the description heuristic).
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "请阅读task_1.md，获取任务信息".into();
    state.task.discovered_schema = Some(coregeek::state::DiscoveredSchema {
        fields: vec!["token".into()],
        required: vec!["token".into()],
    });
    state.task.stage = TaskStage::HaveAnswer {
        answer: "{\"token\":\"fc1e78eb2a5a\"}".into(),
    };
    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    let cmd = plan_pioneer(&turn, &mut state, pioneer, &mut plan).expect("submitted");
    assert_eq!(cmd.taskAnswer.as_deref(), Some("fc1e78eb2a5a"));

    // Two declared fields: the object is the answer's real shape. Untouched.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "请阅读task_1.md，获取任务信息".into();
    state.task.discovered_schema = Some(coregeek::state::DiscoveredSchema {
        fields: vec!["city".into(), "count".into()],
        required: vec!["city".into(), "count".into()],
    });
    let object = "{\"city\":\"南京\",\"count\":5}";
    state.task.stage = TaskStage::HaveAnswer {
        answer: object.into(),
    };
    let mut plan = Plan::default();
    let cmd = plan_pioneer(&turn, &mut state, pioneer, &mut plan).expect("submitted");
    assert_eq!(cmd.taskAnswer.as_deref(), Some(object));
}

// ---------------------------------------------------------------------------
// P1-3: the two-stage SOP — an explore template and an answer template.
// ---------------------------------------------------------------------------

use coregeek::state::keywords_of;

/// A cached pair: `explore` reconnaissance in front of the answering script.
fn sop_pair(task_type: &str, description: &str, explore: &str, answer: &str) -> SopEntry {
    SopEntry {
        task_type: task_type.into(),
        keywords: keywords_of(description),
        template: answer.into(),
        explore: Some(explore.into()),
        ..Default::default()
    }
}

#[test]
fn a_cached_pair_runs_the_exploration_before_the_answer() {
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "任务：统计城市名：北京 的人口".into();
    state.task.stage = TaskStage::Planning;
    state.sop_cache.push(sop_pair(
        "自进化类1",
        "统计城市名：北京 的人口",
        "find /tmp/selfEvolutionTask -maxdepth 4",
        "python3 report.py --city {{城市名}}",
    ));

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);
    assert_eq!(
        plan.execute_cmd.as_deref(),
        Some("find /tmp/selfEvolutionTask -maxdepth 4"),
        "stage 1 is the reconnaissance, not the answer"
    );
    assert!(plan.prompt.is_none(), "no LLM call: the pair is cached");
    assert_eq!(
        state.task.sop_pending_answer.as_deref(),
        Some("python3 report.py --city 北京"),
        "stage 2 is bound and queued behind it"
    );
    assert_eq!(
        state.task.sop_used_template.as_deref(),
        Some("python3 report.py --city {{城市名}}"),
        "a later rejection is charged to the entry that ran"
    );

    // The reconnaissance came back without an answer — which is what
    // reconnaissance does. The queued answer goes out next, still with no LLM
    // round trip: 2 rounds, not a replay of the whole exploration.
    coregeek::brain::task::on_cmd_result(
        &mut state,
        "[exitCode:0]\nFIELDS: city, population\n(no answer yet)",
    );
    assert!(matches!(state.task.stage, TaskStage::Planning));
    assert_eq!(state.task.discovered_fields, vec!["city", "population"]);

    let mut plan = Plan::default();
    coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);
    assert_eq!(
        plan.execute_cmd.as_deref(),
        Some("python3 report.py --city 北京"),
        "stage 2 answers"
    );
    assert!(plan.prompt.is_none(), "still no LLM round trip");
    assert!(
        state.task.sop_pending_answer.is_none(),
        "the queued answer is consumed exactly once"
    );
}

#[test]
fn a_pair_without_an_exploration_still_runs_in_one_command() {
    // The single-script SOP is unchanged: a pair whose session answered with
    // its first command has no stage 1, and the answer goes out immediately.
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
    assert_eq!(plan.execute_cmd.as_deref(), Some("echo 上海"));
    assert!(state.task.sop_pending_answer.is_none());
}

#[test]
fn a_session_that_explored_before_answering_caches_the_pair() {
    // The cache is built from evidence, not from a guess about which command
    // "looks like" reconnaissance: `cmd_history` is in send order, and the
    // command immediately before the one that answered is the exploration that
    // made it possible.
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
    assert_eq!(
        state.sop_cache[0].explore.as_deref(),
        Some("find /tmp/selfEvolutionTask -maxdepth 4")
    );
    assert_eq!(state.sop_cache[0].template, "python3 report.py --city 北京");

    // A session that answered with its FIRST command explored nothing separate:
    // there is no stage 1 to replay, and inventing one would be a guess.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 2;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计城市名：北京 的人口".into();
    state.task.cmd_history = vec!["python3 report.py --city 北京".into()];
    state.task.best_answer = "2200".into();
    state.task.sop_cmd = Some("python3 report.py --city 北京".into());
    state.cache_sop();
    assert_eq!(state.sop_cache[0].explore, None);
}

#[test]
fn an_unbindable_exploration_still_lets_the_answer_run() {
    // Stage 1 is an optimisation. If its parameters cannot be resolved from the
    // new description, the answer script alone is still the fast path — failing
    // the whole pair would throw away a perfectly good answer script.
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "任务：统计城市名：北京 的人口排名".into();
    state.task.stage = TaskStage::Planning;
    state.sop_cache.push(sop_pair(
        "自进化类1",
        "统计城市名：北京 的人口排名",
        "cat /tmp/{{不存在的参数}}/task.md",
        "echo {{城市名}}",
    ));

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);
    assert_eq!(
        plan.execute_cmd.as_deref(),
        Some("echo 北京"),
        "the answer script alone is used when stage 1 cannot be bound"
    );
}

#[test]
fn a_rejected_pair_is_still_evicted_entry_by_entry() {
    // P1-3 keeps the existing per-entry strike eviction: the pair is charged
    // like any other SOP, and two consecutive strikes evict exactly that entry.
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
    state.sop_cache.push(sop_pair(
        "自进化类1",
        "统计城市名：北京 的人口",
        "find /tmp/selfEvolutionTask -maxdepth 4",
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
    assert_eq!(state.sop_cache[0].explore.is_some(), true);

    state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };
    state.task.sop_used_template = Some("python3 report.py --city {{城市名}}".into());
    state.observe(&rejected(8));
    assert!(
        state.sop_cache.is_empty(),
        "a second consecutive strike evicts the pair, exploration included"
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
