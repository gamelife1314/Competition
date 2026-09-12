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
        // An explicit ANSWER marker is trusted even when the exit code is
        // non-zero (trailing cleanup may fail after the answer was printed);
        // a wrong verdict comes back as errorCode 2 and re-triggers Planning.
        crate::log::event(
            "task_answer_found",
            serde_json::json!({"exit": code, "answer": truncate(&answer, 120)}),
        );
        state.task.best_answer = answer.clone();
        state.task.stage = TaskStage::HaveAnswer { answer };
        // Cache the working command immediately so the next task reuses it.
        state.cache_sop();
    } else {
        // No answer found: ask the LLM again with the failure context
        // (result_history carries it into the next prompt).
        crate::log::event("task_cmd_failed", serde_json::json!({"exit": code, "timeout": result.starts_with("[TIMEOUT]")}));
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
pub fn plan_pioneer(turn: &Turn, state: &mut BotState, pioneer: &Unit, plan: &mut Plan) -> Option<RoleCommand> {
    // Timeout guard: submit whatever we have before the task expires.
    let rounds_left = state.task.timeout_round.saturating_sub(turn.round_no);
    if rounds_left <= 2 {
        if !state.task.best_answer.is_empty() {
            state.task.stage = TaskStage::HaveAnswer { answer: state.task.best_answer.clone() };
        } else if matches!(state.task.stage, TaskStage::WaitingCmdResult { .. } | TaskStage::Planning) {
            // Last resort: submit a placeholder derived from the description.
            let guess = guess_from_description(&state.task.description);
            if !guess.is_empty() {
                state.task.stage = TaskStage::HaveAnswer { answer: guess };
            }
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
            // SOP fast path: a cached script for a similar task runs first.
            if state.task.cmd_history.is_empty() {
                if let Some(sop) = state.find_sop(&state.task.description.clone()) {
                    let script = sop.script.clone();
                    state.task.stage = TaskStage::HavePlan { cmd: script };
                    return plan_pioneer(turn, state, pioneer, plan);
                }
            }
            if plan.prompt.is_none() {
                // LLM calls during an active task are free (do not count
                // toward the 3/day budget), per the interface doc.
                plan.prompt = Some(build_prompt(state, turn));
            }
            None
        }
        TaskStage::HavePlan { cmd } => {
            if plan.execute_cmd.is_none() {
                state.task.cmd_history.push(truncate(&cmd, 800));
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
                state.task.stage = TaskStage::WaitingCmdResult { attempts: attempts + 1 };
            }
            None
        }
        TaskStage::HaveAnswer { answer } => {
            state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };
            Some(RoleCommand::submit_answer(&answer))
        }
        TaskStage::WaitingSubmit { attempts } => {
            if attempts >= 6 {
                // Verdict never arrived; force a re-plan.
                state.task.stage = TaskStage::Planning;
            } else {
                state.task.stage = TaskStage::WaitingSubmit { attempts: attempts + 1 };
            }
            None
        }
    }
}

pub fn build_prompt(state: &BotState, _turn: &Turn) -> String {
    let mut prompt = String::new();
    prompt.push_str("你在一个隔离沙盒中执行任务，沙盒可运行基础 shell 与 python3（无外网）。\n");
    prompt.push_str("环境说明：任务相关文件（如 task_X.md、输入数据）都放在 /tmp/selfEvolutionTask/ 目录下。\n");
    prompt.push_str("请先用 `find /tmp/selfEvolutionTask/ -maxdepth 3` 或 `ls -R /tmp/selfEvolutionTask/` 查看有哪些文件，再 `cat /tmp/selfEvolutionTask/<对应文件名>` 读取内容；不要直接 `cat task_X.md`（根目录没有该文件）。\n");
    prompt.push_str("任务描述：\n");
    prompt.push_str(&state.task.description);
    prompt.push_str("\n\n要求：\n");
    prompt.push_str("1. 给出可直接执行的命令或脚本（放在 ```bash 或 ```python 代码块中），完成全部子任务。\n");
    prompt.push_str("2. 脚本最后一行必须打印 `ANSWER: <最终答案>`，多字段答案用 JSON 表示。\n");
    prompt.push_str("3. 脚本要可复用：把可变参数（如城市名）写在开头变量里。\n");
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

/// Find the answer: prefer an explicit "ANSWER:" marker line, else the last
/// non-empty output line.
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
    for line in trimmed.lines().rev() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with("Traceback") {
            return Some(truncate(line, 400));
        }
    }
    None
}

fn guess_from_description(description: &str) -> String {
    // Extremely crude fallback so a timeout still scores partial credit.
    truncate(description.trim(), 100)
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}
