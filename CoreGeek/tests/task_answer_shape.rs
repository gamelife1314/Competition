//! Answer SHAPE and answer HARVESTING (issues #121-#125).
//!
//! Five matches, five `0:3` losses, and `score_1` (the task score) was zero or
//! near it in four of them. The logs say the same two things every time, and
//! neither of them is the model's arithmetic:
//!
//!   * the retry resubmitted the SAME BYTES. `submittable_answer_shaped` could
//!     only ever drop a one-key wrapper, so an answer that arrived as a bare
//!     scalar had exactly one shape to offer and offered it three times —
//!     #124 session 3 (`15`, against a declared `{"count":"integer"}`) and
//!     #122 session 6 (`2`) were both rejected three times for shape alone.
//!     The one session in the batch that succeeded, #125 session 3, is the
//!     proof of the missing direction: it was rejected as a scalar and won on
//!     the retry with `{"token": "fc1e78eb2a5a"}`.
//!
//!   * the judger NAMED the key it wanted and the loop ignored it.
//!     `键值比对不通过: $/token: 缺少键` (#123 ×2) is the judging authority
//!     stating the expected shape, and the session went back with the same
//!     35 characters.
//!
//!   * the value was in the output, unlabelled. #121 submitted NOTHING across
//!     seven sessions; #123's round-18 run printed
//!     `[ OK ] 全部通过 (6/6) TOKEN: fc1e78eb2a5a` — the exact value #125's
//!     winning session submitted — and it was discarded because it was not on
//!     an `ANSWER:` line.

use coregeek::brain::task::{
    harvest_answer, harvest_keys, merge_into, missing_key_in, missing_keys_from_feedback,
    partial_answer, submittable_answer_for_keys,
};
use coregeek::state::{BotState, DiscoveredSchema, TaskStage};

fn fields(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

// ---------------------------------------------------------------- shape flip

#[test]
fn a_rejected_scalar_is_wrapped_under_the_declared_field() {
    // Issue #124 session 3, verbatim: the sandbox echoed
    // `FIELDS: count  SCHEMA: {"count": "integer"}  ANSWER: 15`, the answer was
    // submitted bare, and the judger answered `键值比对不通过: $: 值不符` three
    // times while nothing changed shape.
    let answer = "15";
    assert_eq!(
        submittable_answer_for_keys(&fields(&["count"]), "", answer, false, &[]),
        "15",
        "the first submission is still the shape the model produced"
    );
    let retry = submittable_answer_for_keys(&fields(&["count"]), "", answer, true, &[]);
    assert_eq!(
        retry, r#"{"count":15}"#,
        "after a rejection the other shape must actually be tried"
    );
}

#[test]
fn the_first_attempt_is_never_rewritten_into_a_new_shape() {
    // No regression on issue #15: a one-key wrapper the description never named
    // is still reduced to its scalar on the first try, and the retry puts the
    // wrapper back.
    let wrapper = r#"{"token":"fc1e78eb2a5a"}"#;
    assert_eq!(
        submittable_answer_for_keys(&fields(&["token"]), "", wrapper, false, &[]),
        "fc1e78eb2a5a"
    );
    assert_eq!(
        submittable_answer_for_keys(&fields(&["token"]), "", wrapper, true, &[]),
        wrapper
    );
}

#[test]
fn a_multi_field_answer_is_never_reshaped() {
    let answer = r#"{"city":"Nanjing","count":15}"#;
    for flip in [false, true] {
        assert_eq!(
            submittable_answer_for_keys(&fields(&["city", "count"]), "", answer, flip, &[]),
            answer
        );
    }
}

#[test]
fn with_no_schema_at_all_the_scalar_stays_a_scalar() {
    // Nothing names a field, so there is no wrapper to invent.
    assert_eq!(
        submittable_answer_for_keys(&[], "", "2", true, &[]),
        "2"
    );
}

// ------------------------------------------------------ the judger's own key

#[test]
fn the_missing_key_comes_out_of_the_judgers_own_text() {
    // #123, both sessions.
    assert_eq!(
        missing_key_in("键值比对不通过: $/token: 缺少键").as_deref(),
        Some("token")
    );
    assert_eq!(
        missing_key_in("MissingNamedInput: city").as_deref(),
        Some("city")
    );
    assert_eq!(missing_key_in("键值比对不通过: $/a/b: 缺少键").as_deref(), Some("b"));
}

#[test]
fn a_value_mismatch_never_relabels_the_answer() {
    // #125 session 1 and #122 both got `$/world_heritage_count: 数值不符` — the
    // field was THERE and wrong. Re-keying on that would move a correct field
    // under a key the judger never asked for, which is how a retry destroys a
    // good answer.
    assert_eq!(missing_key_in("键值比对不通过: $/world_heritage_count: 数值不符"), None);
    assert_eq!(missing_key_in("键值比对不通过: $: 值不符"), None);
    assert_eq!(missing_key_in("答案不是合法 JSON"), None);
    assert!(missing_keys_from_feedback(&["键值比对不通过: $: 值不符".into()]).is_empty());
}

#[test]
fn a_named_key_outranks_every_shape_guess() {
    // #123: `{"port":8080,"name":"alpha-app"}` was answered with
    // `$/token: 缺少键`.
    assert_eq!(
        submittable_answer_for_keys(&[], "", r#"{"port":8080}"#, true, &fields(&["token"])),
        r#"{"token":8080}"#
    );
    // A scalar moves under the named key as it is.
    assert_eq!(
        submittable_answer_for_keys(&[], "", "2", true, &fields(&["world_heritage_count"])),
        r#"{"world_heritage_count":2}"#
    );
    // A multi-key object says nothing about which value belongs there.
    assert_eq!(
        submittable_answer_for_keys(
            &[],
            "",
            r#"{"a":1,"b":2}"#,
            true,
            &fields(&["token"])
        ),
        r#"{"a":1,"b":2}"#
    );
}

// ----------------------------------------------------------------- harvest

#[test]
fn a_value_the_script_printed_is_harvested_under_its_key() {
    // #123 round 18, verbatim.
    let output = "[ OK ] 全部通过 (6/6) TOKEN: fc1e78eb2a5a".to_string();
    assert_eq!(
        harvest_answer(&fields(&["token"]), &[output]).as_deref(),
        Some(r#"{"token":"fc1e78eb2a5a"}"#)
    );
}

#[test]
fn harvested_values_keep_their_json_type() {
    let output = "count: 15".to_string();
    assert_eq!(
        harvest_answer(&fields(&["count"]), &[output]).as_deref(),
        Some(r#"{"count":15}"#)
    );
}

#[test]
fn the_field_list_and_the_task_prose_are_not_values() {
    // `FIELDS: port, name` must not yield a `port`.
    let output = "FIELDS: port, name\nSCHEMA: {\"port\":\"integer\"}".to_string();
    assert_eq!(harvest_answer(&fields(&["port", "name"]), &[output]), None);
    // Nor must a sentence lifted out of the instructions.
    let prose = "token: 请把配置文件里的 token 原样填进来，注意大小写".to_string();
    assert_eq!(harvest_answer(&fields(&["token"]), &[prose]), None);
}

#[test]
fn a_sentinel_is_never_harvested() {
    for output in [
        "token: unknown",
        "token: N/A",
        "token:",
        "token: ",
        "token: xxx",
    ] {
        assert_eq!(
            harvest_answer(&fields(&["token"]), &[output.to_string()]),
            None,
            "{output:?} is a sentinel, not an answer"
        );
    }
}

#[test]
fn harvesting_reads_the_newest_run_first() {
    let old = "token: aaaaaaaaaaaa".to_string();
    let new = "token: fc1e78eb2a5a".to_string();
    assert_eq!(
        harvest_answer(&fields(&["token"]), &[old, new]).as_deref(),
        Some(r#"{"token":"fc1e78eb2a5a"}"#)
    );
}

#[test]
fn the_harvest_key_set_carries_the_judgers_word_and_the_echoed_schema() {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.discovered_fields = fields(&["port", "name"]);
    state.task.discovered_schema = Some(DiscoveredSchema {
        fields: fields(&["count"]),
        required: fields(&["count"]),
    });
    state.task.rejection_feedback = vec!["键值比对不通过: $/token: 缺少键".into()];
    let keys = harvest_keys(&state);
    assert!(keys.contains(&"token".to_string()), "the judger named it: {keys:?}");
    assert!(keys.contains(&"count".to_string()), "the schema declared it: {keys:?}");
    assert!(keys.contains(&"port".to_string()), "the script echoed it: {keys:?}");
}

#[test]
fn a_session_with_no_answer_marker_still_has_one_to_submit() {
    // The deadline guard's fallback: #121 submitted NOTHING across seven
    // sessions while the sandbox had printed the value.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.stage = TaskStage::Planning;
    state.task.timeout_round = 300;
    state.task.rejection_feedback = vec!["键值比对不通过: $/token: 缺少键".into()];
    state.task.result_history = vec![
        "[exitCode:0]\nFound task file: /tmp/selfEvolutionTask/1-fixed-step/2-engineering-fix/task_1_alpha.md\n[ OK ] 全部通过 (6/6) TOKEN: fc1e78eb2a5a".into(),
    ];
    assert_eq!(
        partial_answer(&state).as_deref(),
        Some(r#"{"token":"fc1e78eb2a5a"}"#)
    );
}

#[test]
fn meta_and_sentinel_output_is_still_never_submitted() {
    let mut state = BotState::default();
    state.task.active = true;
    state.task.stage = TaskStage::Planning;
    state.task.result_history =
        vec!["[exitCode:0]\n=== Task Content === # 自进化任务 B-1\ntask_1_alpha.md".into()];
    assert_eq!(partial_answer(&state), None, "exploratory output is not an answer");
}

// -------------------------------------------------------------------- merge

#[test]
fn a_harvested_key_never_overwrites_the_models_own_value() {
    assert_eq!(
        merge_into(r#"{"port":8080}"#, r#"{"token":"abc","port":9999}"#),
        r#"{"port":8080,"token":"abc"}"#
    );
    assert_eq!(
        merge_into(r#"{"port":8080}"#, r#"{"port":9999}"#),
        r#"{"port":8080}"#
    );
    // A scalar has no holes to fill: the harvested object is the only shape
    // that can carry the named key at all.
    assert_eq!(merge_into("2", r#"{"token":"abc"}"#), r#"{"token":"abc"}"#);
}

#[test]
fn a_multibyte_line_never_panics_the_harvest() {
    // The scan runs on byte offsets into the line itself rather than into a
    // lowercased copy, because `str::to_lowercase` can change a string's byte
    // length and an offset found in the copy is not an offset into the line.
    // A `line[..at]` that lands mid-character panics — in a match, in the round
    // a task is being answered.
    for line in [
        "TOKEN：  fc1e78eb2a5a  （全部通过）",
        "免责声明：本文档由系统生成，token 见下",
        "İstanbul token: abc123",
        "任务文件 task_1_alpha.md 的 TOKEN: 9f3c2a1b",
        "token:= = =",
    ] {
        let _ = harvest_answer(&fields(&["token"]), &[line.to_string()]);
        let _ = harvest_answer(&fields(&["count"]), &[line.to_string()]);
    }
    assert_eq!(
        harvest_answer(&fields(&["token"]), &["İstanbul TOKEN: abc123".to_string()]).as_deref(),
        Some(r#"{"token":"abc123"}"#)
    );
}

#[test]
fn a_bare_value_that_is_not_json_is_wrapped_on_the_first_attempt_too() {
    // Issues #131-#135, all five matches, every session: 表 4a's first row is a
    // 7-12 character payload and 表 4b answers it `答案不是合法 JSON`. The value
    // in it was a token the sandbox had already printed correctly
    // (`[OK] 全部通过 (6/6) TOKEN: fc1e78eb2a5a`); what was wrong was that a
    // bare string is not an answer in ANY schema. The old rule wrapped a scalar
    // only after a rejection AND only when it already parsed as JSON, so this
    // payload was resubmitted byte-identical until the session's 10-15 round
    // budget ran out (表 3b: every session `timeout`, `success:false`).
    let fields = fields(&["token"]);
    assert_eq!(
        submittable_answer_for_keys(&fields, "", "fc1e78eb2a5a", false, &[]),
        r#"{"token":"fc1e78eb2a5a"}"#
    );
    assert_eq!(
        submittable_answer_for_keys(&fields, "", "fc1e78eb2a5a", true, &[]),
        r#"{"token":"fc1e78eb2a5a"}"#,
        "and the retry cannot do better, because there is no legal bare form"
    );

    // The narrow exit, unchanged: a value that already IS JSON keeps the
    // model's own shape on the first attempt.
    assert_eq!(submittable_answer_for_keys(&fields, "", "15", false, &[]), "15");
    // And with no field named at all there is no wrapper to invent.
    assert_eq!(
        submittable_answer_for_keys(&[], "", "fc1e78eb2a5a", false, &[]),
        "fc1e78eb2a5a"
    );
}

#[test]
fn a_named_key_re_keys_a_value_that_is_not_json_either() {
    // 表 4b of issue #133 carries `键值比对不通过: $/token: 缺少键` and #123 the
    // same, while 表 4c shows the sandbox had already printed the token as a bare
    // `TOKEN: fc1e78eb2a5a`. The re-key that exists for exactly that verdict
    // parsed the answer as JSON first and gave up when it could not — so the one
    // submission where the key was known AND the value was known resubmitted the
    // bare string instead. The judger's spelling of the key is the strongest
    // statement about shape on the board; a non-JSON value becomes its value.
    assert_eq!(
        submittable_answer_for_keys(&[], "", "fc1e78eb2a5a", false, &fields(&["token"])),
        r#"{"token":"fc1e78eb2a5a"}"#
    );
    // Still narrow: an empty value supplies nothing, and a multi-key object
    // still says nothing about which of its values belongs under the key.
    assert_eq!(
        submittable_answer_for_keys(&[], "", "   ", false, &fields(&["token"])),
        "   "
    );
    assert_eq!(
        submittable_answer_for_keys(
            &[],
            "",
            r#"{"city":"南京","count":5}"#,
            false,
            &fields(&["token"])
        ),
        r#"{"city":"南京","count":5}"#
    );
}
