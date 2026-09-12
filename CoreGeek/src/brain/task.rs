//! Self-evolution task loop:
//! acceptTask → (phaseTask arrives) → prompt LLM for a sandbox script →
//! executeCmd → parse lastCmdResult → submitAnswer → iterate on error 2.
//! While a task is active the pioneer MUST stay within 1 cell of the task
//! point, so this module never emits movement.

use crate::brain::Plan;
use crate::model::{Turn, Unit};
use crate::protocol::RoleCommand;
use crate::state::{BotState, TaskStage};

/// Feed a fresh `llmResp` into the task state machine.
pub fn on_llm_resp(state: &mut BotState, resp: &str) {
    if !matches!(state.task.stage, TaskStage::Planning) {
        return;
    }
    if let Some(cmd) = extract_command(resp) {
        state.task.stage = TaskStage::HavePlan { cmd };
    }
    // If extraction fails we stay in Planning and re-prompt next round.
}

/// Feed a fresh `lastCmdResult` into the task state machine.
pub fn on_cmd_result(state: &mut BotState, result: &str) {
    if !matches!(state.task.stage, TaskStage::WaitingCmdResult { .. }) {
        return;
    }
    state.task.result_history.push(truncate(result, 1200));
    let output = strip_status_line(result);
    let code = exit_code(result);
    if let Some(answer) = extract_answer(output) {
        if is_meta_answer(&answer) {
            // A meta-description of the parsing step ({"status":"parsed",...})
            // is not the task's result: treat it as a failed run and re-plan.
            crate::log::event("task_answer_meta", serde_json::json!({"exit": code}));
            state.task.stage = TaskStage::Planning;
            return;
        }
        // An explicit ANSWER marker is trusted even when the exit code is
        // non-zero (trailing cleanup may fail after the answer was printed);
        // a wrong verdict comes back as errorCode 2 and re-triggers Planning.
        crate::log::event(
            "task_answer_found",
            serde_json::json!({"exit": code, "answer": truncate(&answer, 120)}),
        );
        state.task.best_answer = answer.clone();
        state.task.stage = TaskStage::HaveAnswer { answer };
    } else {
        // No answer found: ask the LLM again with the failure context
        // (result_history carries it into the next prompt).
        crate::log::event(
            "task_cmd_failed",
            serde_json::json!({"exit": code, "timeout": result.starts_with("[TIMEOUT]")}),
        );
        state.task.stage = TaskStage::Planning;
    }
}

/// Parse the "[exitCode:N]" header; None for TIMEOUT/JUDGER_ERROR/raw output.
pub fn exit_code(result: &str) -> Option<i64> {
    let rest = result.strip_prefix("[exitCode:")?;
    let end = rest.find(']')?;
    rest[..end].parse().ok()
}

/// Per-round pioneer behaviour while a task is active.
pub fn plan_pioneer(
    turn: &Turn,
    state: &mut BotState,
    pioneer: &Unit,
    plan: &mut Plan,
) -> Option<RoleCommand> {
    // Timeout guard: submit the strongest observed task result before expiry.
    // Never turn the task description or an exploratory file listing into an
    // answer; both have caused guaranteed-zero submissions in prior matches.
    let rounds_left = state.task.timeout_round.saturating_sub(turn.round_no);
    if rounds_left <= 2 {
        if let Some(answer) = partial_answer(state) {
            state.task.best_answer = answer.clone();
            state.task.stage = TaskStage::HaveAnswer { answer };
        }
    }

    match state.task.stage.clone() {
        TaskStage::WaitingDescription => {
            // Accepted but phaseTask not delivered yet. Do NOT abandon here —
            // ending on an empty command history produced the cmdRounds=0
            // spin. The task only ends on timeout (timeout_round) or an
            // explicit error code, enforced in absorb_task_events.
            None
        }
        TaskStage::Planning => {
            // SOP fast path: a cached script whose fingerprint matches this
            // task AND whose parameters bind to this description runs first.
            if state.task.cmd_history.is_empty() {
                if let Some(script) = state.find_sop(&state.task.task_type, &state.task.description)
                {
                    state.task.stage = TaskStage::HavePlan { cmd: script };
                    return plan_pioneer(turn, state, pioneer, plan);
                }
            }
            if plan.prompt.is_none() {
                // LLM calls during an active task are free (do not count
                // toward the 3/day budget), per the interface doc.
                state.task.llm_request_round = Some(turn.round_no);
                plan.prompt = Some(build_prompt(state, turn));
            }
            None
        }
        TaskStage::HavePlan { cmd } => {
            if plan.execute_cmd.is_none() {
                state.task.cmd_history.push(truncate(&cmd, 800));
                state.task.cmd_request_round = Some(turn.round_no);
                state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
                plan.execute_cmd = Some(cmd);
            }
            None
        }
        TaskStage::WaitingCmdResult { attempts } => {
            // The judger executes in the same round; result arrives next
            // round. If nothing came back for several rounds, re-plan.
            if attempts >= 3 {
                state.task.stage = TaskStage::Planning;
            } else {
                state.task.stage = TaskStage::WaitingCmdResult {
                    attempts: attempts + 1,
                };
            }
            None
        }
        TaskStage::HaveAnswer { answer } => {
            // Schema check before submitting: an answer whose shape does not
            // match what the task asked for scores nothing, and the round is
            // better spent re-planning with the missing fields named. Inside
            // the last few rounds the partial pass rate is worth more than the
            // chance of a complete answer, so the check yields.
            let gaps = answer_schema_gaps(&state.task.description, &answer);
            if !gaps.is_empty() && rounds_left > SCHEMA_GRACE {
                crate::log::event(
                    "task_answer_schema",
                    serde_json::json!({
                        "session": state.task.session_id,
                        "missing": gaps,
                        "roundsLeft": rounds_left,
                    }),
                );
                state.task.schema_gaps = gaps;
                state.task.stage = TaskStage::Planning;
                return None;
            }
            // The exact bytes handed to the judger. Issue #15: the logged
            // `task_answer_found` and the submitted payload had drifted apart
            // (a corrected/merged answer replaced the recorded one between the
            // two), which made a scored-zero task impossible to diagnose from
            // the log. One event per submission, carrying what was actually
            // sent, closes that gap for good.
            let payload = submittable_answer(&state.task.description, &answer);
            crate::log::event(
                "task_answer_submit",
                serde_json::json!({
                    "session": state.task.session_id,
                    "round": turn.round_no,
                    "answer": truncate(&payload, 160),
                    "rewritten": payload != answer,
                }),
            );
            state.task.submitted_round = Some(turn.round_no);
            state.task.phase_missing_rounds = 0;
            state.task.point_closed_round = None;
            state.task.post_submit_error = false;
            state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };
            Some(RoleCommand::submit_answer(&payload))
        }
        TaskStage::WaitingSubmit { attempts } => {
            // Do not re-plan merely because the success verdict is implicit.
            // observe() owns closure and waits for task-point, phase and clean
            // error-window evidence. Retain the stage until success or timeout.
            state.task.stage = TaskStage::WaitingSubmit {
                attempts: attempts.saturating_add(1),
            };
            None
        }
    }
}

pub fn build_prompt(state: &BotState, _turn: &Turn) -> String {
    let mut prompt = String::new();
    prompt.push_str("你在一个隔离沙盒中执行任务，沙盒可运行基础 shell 与 python3（无外网）。\n");
    prompt.push_str(
        "环境说明：任务相关文件（如 task_X.md、输入数据）都放在 /tmp/selfEvolutionTask/ 目录下。\n",
    );
    prompt.push_str("请先用 `find /tmp/selfEvolutionTask/ -maxdepth 4` 或 `ls -R /tmp/selfEvolutionTask/` 查看有哪些文件；任务文件可能在多层子目录里（如 1-fixed-step/2-engineering-fix/task_X.md）。必须用 `find`/`ls` 输出的【真实完整路径】去 `cat`，不要假设文件在根目录、不要直接 `cat task_X.md`。\n");
    prompt.push_str("任务描述：\n");
    prompt.push_str(&state.task.description);
    prompt.push_str("\n\n要求：\n");
    prompt.push_str(
        "1. 给出可直接执行的命令或脚本（放在 ```bash 或 ```python 代码块中），完成全部子任务。\n",
    );
    prompt.push_str("2. 脚本最后一行必须打印 `ANSWER: <最终答案>`，多字段答案用 JSON 表示。\n");
    prompt.push_str("3. 脚本要可复用：把可变参数（如城市名、文件名、数量）写成 `{{参数名}}` 占位符，参数名必须与任务描述里出现的字段名完全一致（例如描述里的“城市名”就用 `{{城市名}}`），脚本中不要写死具体取值；同一类任务下次会复用这段脚本并按新描述自动填参。\n");
    prompt.push_str("4. 尽量在一个脚本内完成全部步骤（find 找文件 → cat 读取 → 计算 → 打印 ANSWER），不要分多轮试探；只有带 `ANSWER:` 标记的输出才会被当作答案提交。\n");
    if !state.task.result_history.is_empty() {
        prompt.push_str("\n上次执行输出（请修正错误）：\n");
        let start = state.task.result_history.len().saturating_sub(2);
        for result in &state.task.result_history[start..] {
            prompt.push_str(&truncate(result, 600));
            prompt.push('\n');
        }
        if state.task.wrong_answers > 0 {
            prompt.push_str(&format!(
                "\n注意：之前提交的答案被判错 {} 次，上次答案：{}。请重新分析题目要求的字段与格式。\n",
                state.task.wrong_answers, state.task.best_answer
            ));
        }
    }
    if !state.task.schema_gaps.is_empty() {
        prompt.push_str(&format!(
            "\n上次打印的答案缺少任务要求的字段：{}。请在 ANSWER 的 JSON 中补全这些字段（字段名与任务描述一致）。\n",
            state.task.schema_gaps.join("、")
        ));
    }
    prompt
}

/// Extract an executable command from an LLM response: prefer fenced code
/// blocks; python blocks get wrapped in a heredoc.
pub fn extract_command(resp: &str) -> Option<String> {
    let mut blocks: Vec<(String, String)> = Vec::new(); // (lang, body)
    let mut current_lang: Option<String> = None;
    let mut body: Vec<String> = Vec::new();
    for line in resp.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if let Some(lang) = current_lang.take() {
                blocks.push((lang, body.join("\n")));
                body.clear();
            } else {
                let lang = trimmed.trim_start_matches('`').trim().to_lowercase();
                current_lang = Some(if lang.is_empty() { "bash".into() } else { lang });
            }
        } else if current_lang.is_some() {
            body.push(line.to_string());
        }
    }
    if blocks.is_empty() {
        // No fences: a short single-line command is still usable.
        let candidate = resp.trim();
        if !candidate.is_empty()
            && candidate.chars().count() <= 160
            && !candidate.contains('\n')
            && looks_like_command(candidate)
        {
            return Some(candidate.to_string());
        }
        return None;
    }
    // Prefer python blocks (richer), then bash/sh, then anything.
    let order = ["python", "python3", "bash", "sh", "shell"];
    for lang in order {
        if let Some((_found, code)) = blocks.iter().find(|(l, _)| l == lang) {
            return Some(wrap(lang, code));
        }
    }
    let (lang, code) = &blocks[0];
    Some(wrap(lang, code))
}

fn wrap(lang: &str, code: &str) -> String {
    if lang.starts_with("python") {
        format!("python3 - <<'PYEOF'\n{code}\nPYEOF")
    } else {
        code.to_string()
    }
}

fn looks_like_command(text: &str) -> bool {
    const HEADS: [&str; 12] = [
        "ls", "cat", "curl", "python", "sh", "bash", "echo", "grep", "cd", "./", "pip", "wc",
    ];
    HEADS.iter().any(|head| text.starts_with(head))
}

/// Parse "[exitCode:N]\n<output>" and return the output part.
pub fn strip_status_line(result: &str) -> &str {
    if let Some(rest) = result.strip_prefix("[exitCode:") {
        if let Some(nl) = rest.find('\n') {
            return rest[nl + 1..].trim_start_matches(|c| c == '\r' || c == '\n');
        }
    }
    if let Some(rest) = result.strip_prefix("[TIMEOUT]") {
        return rest.trim_start_matches(|c| c == '\r' || c == '\n');
    }
    if let Some(rest) = result.strip_prefix("[JUDGER_ERROR]") {
        return rest.trim_start_matches(|c| c == '\r' || c == '\n');
    }
    result
}

/// Find the answer: only an explicit "ANSWER:" (or "答案:") marker line is
/// ever an answer. Raw stdout — a `find`/`ls` file listing, a traceback, any
/// unmarked line — is exploratory output and must NOT be submitted.
pub fn extract_answer(output: &str) -> Option<String> {
    let trimmed = output.trim_end_matches("[TRUNCATED]").trim();
    if trimmed.is_empty() {
        return None;
    }
    for line in trimmed.lines().rev() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ANSWER:") {
            let answer = rest.trim();
            if !answer.is_empty() {
                return Some(answer.to_string());
            }
        }
        if let Some(rest) = line.strip_prefix("答案:") {
            let answer = rest.trim();
            if !answer.is_empty() {
                return Some(answer.to_string());
            }
        }
    }
    None
}

/// The answer as the judger should receive it.
///
/// Our own prompt tells the model "多字段答案用 JSON 表示" — JSON is for the
/// multi-field case. A one-key wrapper around a scalar is therefore the BOT's
/// formatting, not the task's: issue #15 lost a task whose sandbox had already
/// printed the right token, because the payload submitted was
/// `{"token":"fc1e78eb2a5a"}` while the judger was comparing against the bare
/// value. So a single-field object is unwrapped — but only when the task text
/// never names that field, because when it does, the wrapper is exactly what
/// was asked for and unwrapping it would break a legitimate multi-field answer.
pub fn submittable_answer(description: &str, answer: &str) -> String {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(answer)
    else {
        return answer.to_string();
    };
    if map.len() != 1 {
        return answer.to_string();
    }
    let Some((key, value)) = map.iter().next() else {
        return answer.to_string();
    };
    if description.to_lowercase().contains(&key.to_lowercase()) {
        return answer.to_string(); // the task named this field: keep the shape
    }
    let bare = match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        _ => return answer.to_string(),
    };
    if bare.trim().is_empty() {
        answer.to_string()
    } else {
        bare
    }
}

/// True when the answer describes the parsing step instead of the result —
/// e.g. `{"status":"parsed","content_length":534}`. Submitting these scores
/// nothing, so they are filtered out before submission.
pub fn is_meta_answer(answer: &str) -> bool {
    let lower = answer.to_lowercase();
    lower.contains("content_length")
        || lower.contains("contentlength")
        || lower.contains("content-length")
        || (lower.contains("\"status\"") && lower.contains("parsed"))
}

/// Return the strongest structured result seen so far. Only explicit answer
/// markers qualify; raw listings, tracebacks and task prose remain excluded.
///
/// With no confirmed answer, every `ANSWER:` object in the command history is
/// merged field-by-field so a run that only printed part of the result still
/// earns its pass rate instead of submitting nothing.
pub fn partial_answer(state: &BotState) -> Option<String> {
    if !state.task.best_answer.is_empty() && !is_meta_answer(&state.task.best_answer) {
        return Some(state.task.best_answer.clone());
    }
    let answers: Vec<String> = state
        .task
        .result_history
        .iter()
        .filter_map(|result| extract_answer(strip_status_line(result)))
        .filter(|answer| !is_meta_answer(answer))
        .collect();
    let best = answers.last()?.clone();
    Some(merge_json_fields(&answers).unwrap_or(best))
}

/// Union of the fields of every JSON object among `answers` (later runs win on
/// a conflicting key). None when fewer than two of them parse as objects —
/// there is nothing to merge then.
pub fn merge_json_fields(answers: &[String]) -> Option<String> {
    let mut merged = serde_json::Map::new();
    let mut objects = 0usize;
    for answer in answers {
        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(answer)
        {
            objects += 1;
            for (key, value) in map {
                merged.insert(key, value);
            }
        }
    }
    if objects < 2 || merged.is_empty() {
        return None;
    }
    serde_json::to_string(&serde_json::Value::Object(merged)).ok()
}

/// Rounds of grace before a schema mismatch is submitted anyway: re-planning
/// costs a round-trip, so past this point the partial pass rate wins.
const SCHEMA_GRACE: i64 = 4;

/// Field names the task text asks the answer to carry.
///
/// Deliberately conservative: an empty list means "no schema found" and the
/// answer is accepted as-is, because a false positive here would reject a
/// correct answer. Fields are only collected after an explicit trigger word
/// (输出/返回/字段/包含/…), and a single collected token is too weak a signal to
/// act on — the caller ignores anything shorter than two fields.
pub fn expected_fields(description: &str) -> Vec<String> {
    const TRIGGERS: [&str; 8] = [
        "输出", "返回", "字段", "包含", "需要", "答案", "格式", "结果",
    ];
    const NOISE: [&str; 18] = [
        "JSON",
        "json",
        "Json",
        "格式",
        "要求",
        "结果",
        "内容",
        "字符串",
        "数组",
        "对象",
        "如下",
        "答案",
        "输出",
        "返回",
        "包含",
        "字段",
        "需要",
        "一个",
    ];
    let mut fields: Vec<String> = Vec::new();
    for line in description.lines() {
        let Some(index) = TRIGGERS
            .iter()
            .filter_map(|trigger| line.find(trigger))
            .min()
        else {
            continue;
        };
        for token in line[index..].split(|c: char| {
            matches!(
                c,
                '：' | ':'
                    | '、'
                    | '，'
                    | ','
                    | '和'
                    | '与'
                    | '及'
                    | ' '
                    | '\t'
                    | '='
                    | '＝'
                    | '"'
                    | '\''
                    | '“'
                    | '”'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '('
                    | ')'
                    | '（'
                    | '）'
                    | '。'
                    | '；'
                    | ';'
                    | '等'
            )
        }) {
            let token = token.trim();
            if !(2..=12).contains(&token.chars().count()) || NOISE.contains(&token) {
                continue;
            }
            if token.chars().any(char::is_whitespace) {
                continue;
            }
            if !fields.iter().any(|old| old == token) {
                fields.push(token.to_string());
            }
        }
        if !fields.is_empty() {
            break;
        }
    }
    fields.truncate(8);
    fields
}

/// Fields the task asked for that `answer` does not carry. Empty means "pass"
/// — either the answer is complete or no schema could be derived.
pub fn answer_schema_gaps(description: &str, answer: &str) -> Vec<String> {
    let fields = expected_fields(description);
    if fields.len() < 2 {
        return Vec::new();
    }
    let keys = json_keys(answer);
    let lower = answer.to_lowercase();
    fields
        .into_iter()
        .filter(|field| {
            let name = field.to_lowercase();
            !keys.iter().any(|key| *key == name) && !lower.contains(&name)
        })
        .collect()
}

/// Keys of `answer` when it is a JSON object (or an array of objects),
/// lowercased; empty otherwise.
fn json_keys(answer: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(answer) else {
        return Vec::new();
    };
    let object = match value {
        serde_json::Value::Object(map) => Some(map),
        serde_json::Value::Array(items) => items.into_iter().find_map(|item| match item {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        }),
        _ => None,
    };
    object
        .map(|map| map.keys().map(|key| key.to_lowercase()).collect())
        .unwrap_or_default()
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}
