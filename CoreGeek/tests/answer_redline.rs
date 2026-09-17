//! The answer red line, widened (issues #111-#115).
//!
//! Every one of the five matches in the 2026-09-14 (b) batch submitted an
//! answer the red line was supposed to stop, three rounds running, and every
//! one of them came back `errorCode 2`:
//!
//! * #111 session 1 printed `ANSWER: TOKEN_PENDING_MANUAL_REVIEW` — the
//!   model's own "ask me again later" marker, in SCREAMING_SNAKE. Rounds 16,
//!   22 and 23, all rejected `答案不是合法 JSON`, and it was still sitting in
//!   `best_answer`, which is the value the deadline guard submits.
//! * #112 session 1 printed `ANSWER: ，形式：` — four characters of the task
//!   file's prose, lifted through the marker.
//! * #114 session 6 printed `ANSWER: 0写成"0"，否则算错` — eleven characters of
//!   the task's own *instructions* to the model.
//! * #115 printed `ANSWER: {"token": ""}` once in six sessions: right field
//!   name, nothing in it.
//!
//! The old rule was a whole-value match against a fixed list, so a placeholder
//! wearing a separator, a sentence wearing Chinese punctuation and an empty
//! value all walked straight through. These tests pin both halves: the new
//! cases are caught, and the real answers the old rule protected still are
//! not.

use coregeek::brain::task::{is_failure_answer, partial_answer};
use coregeek::state::BotState;

/// The three shapes the batch actually submitted, plus the ones a model
/// reaches for next. None of these answers anything 任务书 ch.6 can score.
#[test]
fn a_placeholders_wearing_separators_is_still_a_placeholder() {
    for sentinel in [
        "TOKEN_PENDING_MANUAL_REVIEW",
        "PENDING_MANUAL_REVIEW",
        "token_pending_manual_review",
        "STATUS_PENDING",
        "PLACEHOLDER_VALUE",
        "ANSWER_PENDING",
        "result.status.pending",
        "unknown_value",
        "TODO_FILL",
    ] {
        assert!(
            is_failure_answer(sentinel),
            "`{sentinel}` is a placeholder shape, not a value"
        );
    }
}

/// A sentence is not a value. Both of these were submitted verbatim.
#[test]
fn a_sentence_from_the_task_file_is_not_an_answer() {
    for prose in [
        "，形式：",
        "0写成\"0\"，否则算错",
        "答案应该是数字，不要带单位",
        "如果找不到文件，打印错误信息。",
    ] {
        assert!(
            is_failure_answer(prose),
            "`{prose}` is prose the model copied, not a result"
        );
    }

    // The BOUNDARY, pinned deliberately. A top-level quoted string is a value
    // the model chose to quote, and a Chinese answer may legitimately carry a
    // comma inside it — a poem line, a place name with a province. Blocking
    // that costs a whole task reward, which is the more expensive mistake, so
    // the prose rule stops at the quotation mark: `，形式：` is caught, and
    // `"，形式："` is not. No match in this batch ever produced the quoted
    // form; both real cases (#112, #114) were bare.
    for quoted in ["\"，形式：\"", "\"床前明月光，疑是地上霜\""] {
        assert!(
            !is_failure_answer(quoted),
            "`{quoted}` arrived as a quoted value and is left alone"
        );
    }
}

/// An empty value answers nothing, wrapped or not. #115's `{"token": ""}` is
/// the whole task line for that match — one answer in six sessions.
#[test]
fn an_empty_value_is_not_an_answer() {
    for empty in [
        r#"{"token": ""}"#,
        r#"{"token": "   "}"#,
        r#"["", ""]"#,
        r#"{"token": "", "value": "none"}"#,
    ] {
        assert!(
            is_failure_answer(empty),
            "`{empty}` answers nothing and must never be submitted"
        );
    }
    // But the rule stays "EVERY leaf is nothing". 任务书 ch.6 scores the
    // submitted object field by field, so an object carrying one real field
    // still earns that field's share of 通过率 — rejecting it here would throw
    // away the partial credit the whole submit-as-accumulating design exists
    // to bank.
    assert!(
        !is_failure_answer(r#"{"city": "", "count": 3}"#),
        "one empty field among real ones is partial credit, not a sentinel"
    );
}

/// The other half, and the one that costs more when it is wrong: every answer
/// the old rule let through and was right to. A false positive here rejects a
/// correct answer and the whole task reward with it.
#[test]
fn a_real_answer_survives_the_wider_rule() {
    for real in [
        // The scalars the batch actually produced.
        "62",
        "2",
        "9599.0",
        "Beijing",
        "Nanjing",
        r#"{"city": "Nanjing"}"#,
        r#"{"city": "Beijing", "temperature": 22, "humidity": 45}"#,
        // Compound tokens: the rule is "every part is a placeholder", so a
        // real identifier split on its separators is untouched even when one
        // of its parts is a word the placeholder list happens to hold.
        "failed_to_extract.log",
        "error_count=7",
        "world_heritage_count",
        "total_count",
        "task_1_alpha",
        "2024_09_14",
        "value_2023",
        // Prose in Chinese without sentence punctuation, and a quoted string
        // that DOES carry it: both are values, and the JS N/JSON gate keeps
        // the second one out of the prose rule's reach.
        "提取失败的原因有三点",
        "李白",
        "《静夜思》",
        "\"南京市，江苏省\"",
        r#"{"city": "南京，中国"}"#,
        r#"{"note": "unknown", "answer": 12}"#,
        r#"{"total_count": 0}"#,
        r#"{"unknown": 42}"#,
    ] {
        assert!(
            !is_failure_answer(real),
            "`{real}` is a result and must survive the filter"
        );
    }
}

/// Red line, defence in depth: a blocked answer must not become the thing the
/// deadline guard submits. This is what made #111's three rejections cost more
/// than three rounds — `best_answer` held the placeholder, so the last-round
/// fallback would have re-sent it.
#[test]
fn a_blocked_answer_never_becomes_the_deadline_submission() {
    let mut state = BotState::default();
    state.task.best_answer = "TOKEN_PENDING_MANUAL_REVIEW".into();
    state.task.result_history = vec![
        "[exitCode:0]\n=== Reading Task File ===\nANSWER: TOKEN_PENDING_MANUAL_REVIEW".into(),
    ];
    assert_eq!(
        partial_answer(&state),
        None,
        "a compound placeholder is not a partial answer either"
    );

    state.task.best_answer = "，形式：".into();
    state.task.result_history =
        vec!["[exitCode:0]\ncat: task_1.md\nANSWER: ，形式：".into()];
    assert_eq!(
        partial_answer(&state),
        None,
        "prose lifted out of the task file is not a partial answer"
    );

    // A real answer in the SAME session still counts: the filter is per
    // answer, not per session.
    state.task.best_answer = r#"{"city": "Nanjing"}"#.into();
    state.task.result_history =
        vec!["[exitCode:0]\nANSWER: {\"city\": \"Nanjing\"}".into()];
    assert_eq!(
        partial_answer(&state).as_deref(),
        Some(r#"{"city": "Nanjing"}"#),
        "the widened rule must not swallow a real answer"
    );
}

/// A template is not an answer — and it does not have to be all placeholders
/// to be one (#201/#203/#204/#205).
///
/// All four matches submitted the task file's own EXAMPLE, verbatim, as one of
/// their seven submissions, and #201's `task_ended.bestAnswer` spells it out
/// character for character. Two things made it look like an answer: it is valid
/// JSON, and half of it is filled in — `"city":"北京"` and a real `types` array
/// sit beside the fields still in the task's angle brackets, so the
/// all-leaves-are-sentinels rule above never saw it. 任务书 ch.6 scores
/// `回答正确字段个数 / 全量字段个数`, so a template banks the fields that were
/// already filled in and nothing else, while spending the round and the
/// submission the session needed for the rest — and the task prompt's own
/// pre-submit self-check already calls it zero ("ANSWER 的值里有没有 `<...>`
/// 占位符…有 = 0 分").
#[test]
fn a_template_wearing_real_values_is_still_a_template() {
    // #201's, from the issue: the task file's example answer for the Beijing
    // heritage query, with `city` and `types` filled in and three fields left
    // in brackets.
    let template = concat!(
        r#"{"city":"北京","oldest_era":"<年代最早的遗产名称>","#,
        r#""total_count":"<总记录条数>","#,
        r#""types":["古建筑","古墓葬","近现代重要史迹","石窟寺及石刻","其他"],"#,
        r#""world_heritage_count":"<保护级别为\"世界遗产\"的数量>"}"#,
    );
    assert!(
        is_failure_answer(template),
        "the task file's own template was submitted as an answer: {template}"
    );

    for slot in [
        // One bracketed field is enough: a single unfilled field is a field the
        // judger counts as wrong, and 通过率 is scored per field.
        r#"{"city":"北京","total_count":"<总记录条数>"}"#,
        // The bare slot, as the sandbox prints it when a field was never read.
        "<年代最早的遗产名称>",
        "\"<总记录条数>\"",
        "`<token>`",
        // #205's r14-r43 submission was this: a field whose value the script
        // echoed straight out of the task file's SCHEMA line.
        r#"{"world_heritage_count":"<保护级别为\"世界遗产\"的数量>"}"#,
    ] {
        assert!(
            is_failure_answer(slot),
            "`{slot}` is a slot waiting to be filled, not a value"
        );
    }

    // The other half, and the one that costs more when it is wrong: the SAME
    // object with the slots filled is the answer the task wanted, and a `<>`
    // that is not a slot — a comparison, an HTML fragment, a half-written one —
    // is not a template.
    for real in [
        concat!(
            r#"{"city":"北京","oldest_era":"明清","total_count":"15","#,
            r#""types":["古建筑","古墓葬"],"world_heritage_count":"3"}"#,
        ),
        "a < b",
        "<unclosed",
        "closed>",
        "<>",
        r#"{"note": "count < 10"}"#,
    ] {
        assert!(
            !is_failure_answer(real),
            "`{real}` is a value and must survive the filter"
        );
    }
}

/// The template must not become the thing the deadline guard submits either:
/// it was sitting in `best_answer` as the session's strongest result, which is
/// exactly the value `partial_answer` falls back on.
#[test]
fn a_template_never_becomes_the_deadline_submission() {
    let mut state = BotState::default();
    state.task.best_answer =
        r#"{"city":"北京","total_count":"<总记录条数>"}"#.into();
    state.task.result_history = vec![
        "[exitCode:0]\n=== Reading Task File ===\nANSWER: {\"city\":\"北京\",\"total_count\":\"<总记录条数>\"}"
            .into(),
    ];
    assert_eq!(
        partial_answer(&state),
        None,
        "the task file's template is not a partial answer"
    );
}
