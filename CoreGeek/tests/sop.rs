//! P1 item 4: parameterised SOP templates, answer schema checks and the
//! partial-answer fallback. These are the task-chain behaviours that decide
//! `score1`, kept in their own file so they exercise the library directly.

use serde_json::{json, Value};

use coregeek::brain::task::{
    answer_schema_gaps, expected_fields, merge_json_fields, partial_answer,
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

fn sop(task_type: &str, keywords: &[&str], template: &str) -> SopEntry {
    SopEntry {
        task_type: task_type.into(),
        keywords: keywords.iter().map(|kw| kw.to_string()).collect(),
        template: template.into(),
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
fn answer_missing_required_fields_is_sent_back_to_planning() {
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 260;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计结果，输出 JSON，包含 name 和 count".into();
    // Exactly the class of answer that scored zero in issue #11: a description
    // of the parsing step instead of the result.
    state.task.stage = TaskStage::HaveAnswer {
        answer: "{\"status\":\"parsed\",\"content_length\":534}".into(),
    };

    let pioneer = turn.role_by_id(10011).unwrap();
    let mut plan = Plan::default();
    coregeek::brain::task::plan_pioneer(&turn, &mut state, pioneer, &mut plan);

    assert!(
        plan.commands.is_empty()
            || plan
                .commands
                .values()
                .all(|cmd| cmd.action != "submitAnswer"),
        "a schema-mismatched answer is not submitted"
    );
    assert!(
        matches!(state.task.stage, TaskStage::Planning),
        "and the round is spent re-planning, got {:?}",
        state.task.stage
    );
    assert_eq!(state.task.schema_gaps, vec!["name", "count"]);
    let prompt = coregeek::brain::task::build_prompt(&state, &turn);
    assert!(
        prompt.contains("name") && prompt.contains("count"),
        "the retry prompt names the missing fields"
    );
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
    // schema check must yield instead of gambling on a fresh plan.
    let turn = turn_from(task_world(6));
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 6;
    state.task.timeout_round = 8;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计结果，输出 JSON，包含 name 和 count".into();
    state.task.stage = TaskStage::HaveAnswer {
        answer: "{\"status\":\"parsed\"}".into(),
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
