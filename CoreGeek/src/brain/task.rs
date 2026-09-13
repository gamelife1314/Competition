//! Self-evolution task loop:
//! acceptTask → (phaseTask arrives) → prompt LLM for a sandbox script →
//! executeCmd → parse lastCmdResult → submitAnswer → iterate on error 2.
//! While a task is active the pioneer MUST stay within 1 cell of the task
//! point, so this module never emits movement.

use crate::brain::Plan;
use crate::model::{Turn, Unit};
use crate::protocol::RoleCommand;
use crate::state::{BotState, TaskStage};

/// Consecutive "`executeCmd` is not available" verdicts that end a session.
const MAX_WINDOW_ERRORS: i32 = 2;

/// The last `roundNo` of the day `round_no` falls in.
pub(crate) fn turn_end_of_day(round_no: i64) -> i64 {
    let round_no = round_no.max(1);
    ((round_no - 1) / crate::model::ROUNDS_PER_DAY + 1) * crate::model::ROUNDS_PER_DAY
}

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

/// Is this verdict the judger saying the task's execution window is shut?
///
/// The interface doc scopes the field — "`executeCmd` … 仅在执行任务期间才能
/// 使用" — and the judger answers a command sent outside that window with
/// `[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用`. That is not a script
/// that failed: no re-plan can make the command legal, and issue #17's two
/// sessions proved it, firing four commands and then one into the same closed
/// window until both timeouts expired at zero points.
pub fn window_closed(result: &str) -> bool {
    result.starts_with("[JUDGER_ERROR]") && result.contains("executeCmd")
}

/// Feed a fresh `lastCmdResult` into the task state machine.
pub fn on_cmd_result(state: &mut BotState, result: &str) {
    if !matches!(state.task.stage, TaskStage::WaitingCmdResult { .. }) {
        return;
    }
    if window_closed(result) {
        state.task.judger_window_errors = state.task.judger_window_errors.saturating_add(1);
        crate::log::event(
            "task_window_error",
            serde_json::json!({
                "session": state.task.session_id,
                "count": state.task.judger_window_errors,
                "reason": truncate(strip_status_line(result), 120),
            }),
        );
        // One refusal can be a transient sandbox fault; a second one to the
        // same command is the window itself. Abandoning the session is what
        // frees the pioneer for the wall line and the economy — the rounds the
        // issue lost were worth more than a task that cannot be answered.
        if state.task.judger_window_errors >= MAX_WINDOW_ERRORS {
            if let Some(point) = state.task.point {
                state
                    .task_refusals
                    .insert(point, turn_end_of_day(state.task.accepted_round));
            }
            state.finish_task(false, "judger_window_closed");
        } else {
            state.task.stage = TaskStage::Planning;
        }
        return;
    }
    state.task.judger_window_errors = 0;
    state.task.result_history.push(truncate(result, 1200));
    let output = strip_status_line(result);
    let code = exit_code(result);
    // The reconnaissance echo travels with every run: the script reports the
    // output schema it read in the sandbox task file, and the schema gate
    // trusts that over anything the placeholder `phaseTask` text implies.
    if let Some(fields) = extract_fields(output) {
        if !fields.is_empty() && fields != state.task.discovered_fields {
            crate::log::event(
                "task_fields",
                serde_json::json!({
                    "session": state.task.session_id,
                    "fields": fields,
                }),
            );
            state.task.discovered_fields = fields;
        }
    }
    if let Some(answer) = extract_answer(output) {
        if is_meta_answer(&answer) {
            // A meta-description of the parsing step ({"status":"parsed",...})
            // is not the task's result: treat it as a failed run and re-plan.
            crate::log::event("task_answer_meta", serde_json::json!({"exit": code}));
            state.task.stage = TaskStage::Planning;
            return;
        }
        if is_failure_answer(&answer) {
            // The script ran and told us it could not do the task. That is a
            // failed RUN, not an answer: re-plan (the sentinel goes into
            // `result_history` and thus into the next prompt's failure
            // context), and never record it as `best_answer` — a recorded
            // sentinel is what the deadline guard would submit.
            crate::log::event(
                "task_answer_sentinel",
                serde_json::json!({"exit": code, "answer": truncate(&answer, 60)}),
            );
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
        // The command that just answered is the one the sandbox executed, and
        // therefore the one worth reusing: it is `cmd_history`'s last entry,
        // because a session sends one command and waits for its verdict.
        state.task.sop_cmd = state.task.cmd_history.last().cloned();
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
                    // A later rejection is charged against exactly the entry
                    // that ran (P1-3), so remember which template produced
                    // these bytes.
                    let task_type = state.task.task_type.clone();
                    let description = state.task.description.clone();
                    state.task.sop_used_template = state
                        .sop_cache
                        .iter()
                        .rev()
                        .find(|entry| {
                            entry.task_type == task_type
                                && entry.bind(&description).as_deref() == Some(script.as_str())
                        })
                        .map(|entry| entry.template.clone());
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
            // THE ROUND THE LLM ANSWERS IS NOT AN EXECUTION ROUND.
            //
            // Issues #18/#19 (and #17 before them) all report the same verdict
            // on every command: `[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间
            // 可用`, from the first command of the session to the last, in match
            // after match, while `phaseTask` carried the task text and the judger
            // timed the session out on its own clock — so the task WAS running
            // and the refusal is not "no task". The one session that ever got a
            // sandbox answer back (issue #15's r=39 `task_answer_found`) is the
            // clue: it is the one that reached this arm without an `llmResp` in
            // the same round, via the SOP fast path.
            //
            // Our loop was built to fire the command in exactly the round the
            // answer arrives — prompt at r, `llmResp` at r+1, `executeCmd` at
            // r+1 — which is the one round the judger does not accept it in.
            // Every re-plan therefore walked back into the same shut door, and
            // nothing about the script could ever change that.
            //
            // So the command waits one round. It costs a single round out of a
            // 2-15 round task, and it means the very first command of a session
            // now lands in the phase of the cycle the sandbox is open in.
            if state.task.llm_resp_round == Some(turn.round_no) {
                return None;
            }
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
            // The red line, defence in depth: meta-descriptions, sentinels and
            // exploratory output are filtered upstream of every path into this
            // arm, but they are NEVER submitted no matter how they arrived —
            // with submit-as-accumulating there is no later gate to catch them.
            if is_meta_answer(&answer) || is_failure_answer(&answer) {
                crate::log::event(
                    "task_answer_blocked",
                    serde_json::json!({
                        "session": state.task.session_id,
                        "answer": truncate(&answer, 60),
                    }),
                );
                state.task.stage = TaskStage::Planning;
                return None;
            }
            // SUBMIT-AS-ACCUMULATING. The judger scores every submission and
            // keeps the highest pass rate (任务书 ch.5: 超时后"以之前提交过的
            // 通过率最高的答案计算积分与金币"), so the answer in hand is banked
            // the round it exists: a partially-correct submission raises the
            // floor and costs nothing. The old pre-submit schema gate held the
            // answer back whenever the schema looked incomplete — which is
            // every session, since the schema is only fully known after the
            // judger's verdict. Submit first, then let the rejection (or the
            // recorded gap analysis) drive a replan that overwrites the banked
            // answer with a better one. The red line stands: meta answers,
            // sentinels and exploratory output never reach this arm
            // (`on_cmd_result` and `partial_answer` filter them upstream).
            let fields = schema_fields(state);
            let gaps = schema_gaps(&fields, &answer);
            let extras = schema_extras(&fields, &answer);
            if !gaps.is_empty() || !extras.is_empty() {
                crate::log::event(
                    "task_answer_schema",
                    serde_json::json!({
                        "session": state.task.session_id,
                        "missing": gaps,
                        "extra": extras,
                        "roundsLeft": rounds_left,
                    }),
                );
                state.task.schema_gaps = gaps;
                state.task.schema_extras = extras;
            }
            // The exact bytes handed to the judger. Issue #15: the logged
            // `task_answer_found` and the submitted payload had drifted apart
            // (a corrected/merged answer replaced the recorded one between the
            // two), which made a scored-zero task impossible to diagnose from
            // the log. One event per submission, carrying what was actually
            // sent, closes that gap for good.
            let payload = submittable_answer_for(&fields, &state.task.description, &answer);
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
    prompt.push_str("2. 执行顺序固定为【侦察→作答】两段：先用 find/ls 定位并 cat 任务文件与相关文档（如 API_DOCS.md），确认接口地址、认证方式、输入数据、以及任务【要求输出的字段名】；再计算答案。侦察结论必须用一行 `FIELDS: 字段1,字段2` 打印出来（字段名以任务文件原文为准；单值答案打印 `FIELDS: value`）。即使本轮还算不出答案，也要先把已确认的字段通过 FIELDS 行打印出来——下一轮会带着它继续。\n");
    prompt.push_str("3. 脚本最后一行必须打印 `ANSWER: <最终答案>`，多字段答案用 JSON 表示，且 JSON 的字段名必须与 FIELDS 行完全一致。\n");
    prompt.push_str("4. 脚本要可复用：把可变参数（如城市名、文件名、数量）写成 `{{参数名}}` 占位符，参数名必须与任务描述里出现的字段名完全一致（例如描述里的“城市名”就用 `{{城市名}}`），脚本中不要写死具体取值；同一类任务下次会复用这段脚本并按新描述自动填参。\n");
    prompt.push_str("5. 尽量在一个脚本内完成全部步骤（find 找文件 → cat 读取 → 计算 → 打印 FIELDS 与 ANSWER），不要分多轮试探；只有带 `ANSWER:` 标记的输出才会被当作答案提交。\n");
    prompt.push_str("6. `ANSWER:` 后面必须是真实结果（数字/字符串/JSON）。找不到文件或算不出来时，**不要**打印 ANSWER 行，也不要用 `xxx`、`failed_to_extract`、`TODO`、`unknown`、`N/A` 之类的占位符占位——那会被判错并浪费一整轮；直接把报错信息打印到 stderr 即可，脚本会带着错误重试。\n");
    prompt.push_str("7. `ANSWER:` 后面**只放任务要的那个值**：是数字就只放数字（不要带单位、不要加解释），是 Markdown 标题、表格或说明文字都不算答案。多字段答案只写任务文件里点名的字段，不要自行增加 `status`、`note`、`task_id` 这类字段——判分按要求的字段逐个比对，多写一个字段会被判错。\n");
    prompt.push_str("8. 打印 ANSWER 前先自检一次：确认这个值确实由脚本从任务数据里算出来（而不是照着题面猜的或照抄示例），位数/单位/大小写与任务要求一致。\n");
    if !state.task.discovered_fields.is_empty() {
        prompt.push_str(&format!(
            "\n已从任务文件确认的输出字段：{}。ANSWER 的 JSON 必须恰好包含这些字段，不多不少。\n",
            state.task.discovered_fields.join("、")
        ));
    }
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
    if !state.task.schema_extras.is_empty() {
        prompt.push_str(&format!(
            "\n上次打印的答案多出了任务未要求的字段：{}。判分会按任务要求的字段逐个比对，多写的字段同样算错——请只保留任务描述里点名的字段，不要自行添加 status/note 之类的字段。\n",
            state.task.schema_extras.join("、")
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

/// Parse the reconnaissance echo: a `FIELDS:` line naming the exact output
/// fields the sandbox task file demands (P0-2). The placeholder `phaseTask`
/// description names no fields, so the script — which has read the real task
/// file inside the sandbox — is the only honest source of the schema. The
/// last FIELDS line wins (a later script may correct an earlier guess).
pub fn extract_fields(output: &str) -> Option<Vec<String>> {
    let trimmed = output.trim_end_matches("[TRUNCATED]").trim();
    for line in trimmed.lines().rev() {
        let line = line.trim();
        let rest = line
            .strip_prefix("FIELDS:")
            .or_else(|| line.strip_prefix("字段:"))
            .or_else(|| line.strip_prefix("字段："));
        let Some(rest) = rest else {
            continue;
        };
        let fields: Vec<String> = rest
            .split([',', '，', '、', ';', '；', ' ', '\t'])
            .map(|token| {
                token
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            })
            .filter(|token| !token.is_empty() && token.chars().count() <= 24)
            .filter(|token| {
                !matches!(token.to_lowercase().as_str(), "none" | "n/a" | "无" | "未知")
            })
            .map(|token| token.to_string())
            .fold(Vec::new(), |mut acc, token| {
                if !acc.contains(&token) {
                    acc.push(token);
                }
                acc
            });
        return Some(fields.into_iter().take(8).collect());
    }
    None
}

/// The schema the answer is checked against: the sandbox-echoed FIELDS when
/// a script has reported them, else the fields guessed from the task text.
fn schema_fields(state: &BotState) -> Vec<String> {
    if state.task.discovered_fields.is_empty() {
        expected_fields(&state.task.description)
    } else {
        state.task.discovered_fields.clone()
    }
}

/// The answer as the judger should receive it, given the schema a script has
/// echoed. Two or more named fields mean the judger compares an object field
/// by field, so the object is submitted untouched. A single named field means
/// the answer IS that one value: a one-key wrapper around it is the bot's own
/// formatting (issue #15 lost a task whose sandbox had printed the right
/// token, because `{"token":"fc1e78eb2a5a"}` was submitted where the bare
/// value was compared), so it is unwrapped. With no echo at all the legacy
/// description heuristic decides.
pub fn submittable_answer_for(fields: &[String], description: &str, answer: &str) -> String {
    if fields.len() >= 2 {
        return answer.to_string();
    }
    if fields.len() == 1 {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(answer)
        {
            if map.len() == 1 {
                let bare = map
                    .values()
                    .next()
                    .and_then(bare_scalar);
                if let Some(bare) = bare {
                    return bare;
                }
            }
        }
        return answer.to_string();
    }
    submittable_answer(description, answer)
}

/// A scalar rendered bare, or None when the value is structured/empty.
fn bare_scalar(value: &serde_json::Value) -> Option<String> {
    let bare = match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        _ => return None,
    };
    if bare.trim().is_empty() {
        None
    } else {
        Some(bare)
    }
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
///
/// Markdown headings belong here too. Issue #22's session 3 submitted one: the
/// script echoed a section title (`## 结果`) instead of a value, which is the
/// same defect as a sentinel — the `ANSWER:` marker carrying the shape of an
/// answer rather than an answer. The marker is matched on length first (see
/// `is_markdown_heading`), so a real result that merely starts with `#` — a hex
/// colour, a tag — is left alone.
pub fn is_meta_answer(answer: &str) -> bool {
    let lower = answer.to_lowercase();
    lower.contains("content_length")
        || lower.contains("contentlength")
        || lower.contains("content-length")
        || (lower.contains("\"status\"") && lower.contains("parsed"))
        || is_markdown_heading(answer)
}

/// True when the answer is markdown decoration rather than a value: a heading
/// (`# 标题`, `## 结果`) or a bolded title, i.e. a `#`/`*` run that is followed
/// by whitespace and carries the whole line.
///
/// The trailing-space requirement is what keeps this safe: `#fff` and
/// `#123456` are colour values, not headings, and are still answers.
pub fn is_markdown_heading(answer: &str) -> bool {
    let trimmed = answer.trim();
    if trimmed.contains('\n') {
        return false; // a heading is one line; several lines are output, not an answer
    }
    let decorated = trimmed.starts_with('#')
        && trimmed
            .trim_start_matches('#')
            .starts_with(|c: char| c.is_whitespace());
    let bold = trimmed.starts_with("**")
        && trimmed.ends_with("**")
        && trimmed.trim_matches('*').trim().chars().count() < trimmed.chars().count();
    decorated || bold
}

/// True when the `ANSWER:` marker carries the script's own failure notice
/// instead of a result.
///
/// Issue #20's eight sessions all died on this: the sandbox ran the script
/// (exit=0), and the answer read back was `xxx` / `failed_to_extract` — the
/// sentinel the model's error path prints when it cannot find or parse the
/// task's input. `ANSWER:` is a marker for the RESULT, so a marker holding a
/// sentinel is not an answer: submitting it scores zero, it parks the session
/// in `HaveAnswer` until the timeout expires instead of re-planning, it hides
/// every real partial field from `partial_answer`'s merge, and since a script
/// that produced an answer is now cached (`sop_cmd`), it teaches the SOP cache
/// a script whose entire purpose is to fail.
///
/// Only a whole-answer match counts. A real result that merely contains one of
/// these words — a path, a JSON field, a line of prose — is left alone.
pub fn is_failure_answer(answer: &str) -> bool {
    const SENTINELS: [&str; 34] = [
        "xxx",
        "xx",
        "x",
        "?",
        "??",
        "???",
        "todo",
        "tbd",
        "n/a",
        "na",
        "nil",
        "unknown",
        "undefined",
        "placeholder",
        "failed",
        "failure",
        "error",
        "exception",
        "failed_to_extract",
        "failed_to_parse",
        "failed-extract",
        "extract_failed",
        "extraction_failed",
        "parse_failed",
        "no_answer",
        "noanswer",
        "not_found",
        "notfound",
        "无",
        "空",
        "未知",
        "暂无",
        "待补充",
        "提取失败",
    ];
    let trimmed = answer
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == '。' || c == '.')
        .trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_lowercase();
    SENTINELS.iter().any(|sentinel| lower == *sentinel)
}

/// Return the strongest structured result seen so far. Only explicit answer
/// markers qualify; raw listings, tracebacks and task prose remain excluded.
///
/// Every evidence-backed answer the session produced — the banked
/// `best_answer` included — is merged field-by-field so the deadline
/// submission covers the most schema fields: with submit-as-accumulating the
/// judger keeps the highest pass rate ever submitted, so the fallback's job
/// is to make the final attempt the union of everything the sandbox has
/// already vouched for. No evidence, no submission (the red line).
pub fn partial_answer(state: &BotState) -> Option<String> {
    let mut answers: Vec<String> = state
        .task
        .result_history
        .iter()
        .filter_map(|result| extract_answer(strip_status_line(result)))
        .filter(|answer| !is_meta_answer(answer) && !is_failure_answer(answer))
        .collect();
    if !state.task.best_answer.is_empty()
        && !is_meta_answer(&state.task.best_answer)
        && !is_failure_answer(&state.task.best_answer)
        && !answers.iter().any(|old| *old == state.task.best_answer)
    {
        answers.push(state.task.best_answer.clone());
    }
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

/// Fields the schema asks for that `answer` does not carry. Empty means
/// "pass" — either the answer is complete or the schema is too weak to check
/// against (fewer than two fields), because a false positive here would
/// reject a correct answer.
pub fn schema_gaps(fields: &[String], answer: &str) -> Vec<String> {
    if fields.len() < 2 {
        return Vec::new();
    }
    let keys = json_keys(answer);
    let lower = answer.to_lowercase();
    fields
        .iter()
        .filter(|field| {
            let name = field.to_lowercase();
            !keys.iter().any(|key| *key == name) && !lower.contains(&name)
        })
        .cloned()
        .collect()
}

/// Fields the task text asks for that `answer` does not carry — the
/// description-derived wrapper of [`schema_gaps`].
pub fn answer_schema_gaps(description: &str, answer: &str) -> Vec<String> {
    schema_gaps(&expected_fields(description), answer)
}

/// Keys the answer carries that the schema never asked for. Empty means
/// "pass".
///
/// The other half of [`schema_gaps`], and a scoring failure in its own
/// right: the judger compares the submitted object against the schema it asked
/// for, so an extra field is a wrong answer even when every required field is
/// right. Issue #22's session 5 submitted
/// `{"city": "Nanjing", "task_id": 2, "status": "completed"}` for a task that
/// asked for two fields and got the `status` it invented on top. Same gate as
/// the missing-field check: only with a real schema (two or more fields), and
/// only when the answer is actually a JSON object — a bare scalar has no keys
/// to be extra.
pub fn schema_extras(fields: &[String], answer: &str) -> Vec<String> {
    if fields.len() < 2 {
        return Vec::new();
    }
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(answer)
    else {
        return Vec::new();
    };
    map.keys()
        .filter(|key| {
            let name = key.to_lowercase();
            !fields.iter().any(|field| field.to_lowercase() == name)
        })
        .cloned()
        .collect()
}

/// Keys the answer carries that the task text never asked for — the
/// description-derived wrapper of [`schema_extras`].
pub fn answer_schema_extras(description: &str, answer: &str) -> Vec<String> {
    if expected_fields(description).len() < 2 {
        return Vec::new();
    }
    let lower = description.to_lowercase();
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(answer)
    else {
        return Vec::new();
    };
    map.keys()
        .filter(|key| !lower.contains(&key.to_lowercase()))
        .cloned()
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
