//! Self-evolution task loop:
//! acceptTask → (phaseTask arrives) → prompt LLM for a sandbox script →
//! executeCmd → parse lastCmdResult → submitAnswer → iterate on error 2.
//! While a task is active the pioneer MUST stay within 1 cell of the task
//! point, so this module never emits movement.

use std::collections::HashSet;

use crate::brain::{stand_cells, walk_toward, Plan};
use crate::model::{chebyshev, Turn, Unit};
use crate::protocol::{Pos, RoleCommand};
use crate::state::{BotState, PromptPurpose, TaskSession, TaskStage};

/// Consecutive "`executeCmd` is not available" verdicts that end a session.
const MAX_WINDOW_ERRORS: i32 = 2;

/// Day-rounds that must still be available before a task point is worth
/// accepting.
///
/// 任务书 5.3: "在任务执行结束后，再次接取任务需等待 30 个回合刷新时间" — and the
/// dusk recall ends every session the pioneer is still holding when it fires.
/// So a session accepted with fewer rounds left than a session needs is not a
/// cheap attempt, it is that task point sold for 30 rounds: the recall walks
/// the pioneer off the point, the judger ends the task, and the point is gone
/// through the whole of the next morning. A working session costs four to six
/// rounds (prompt → `llmResp` → the held-back command → verdict → submit), so
/// twelve leaves room for one failed cycle and a second try.
///
/// Moved out of `day.rs` with its consumer by issue #221 phase 5c.
const TASK_MIN_ATTEMPT_ROUNDS: i64 = 12;

/// Which of the task book's three lanes a session belongs to.
///
/// 任务书 §5 splits the task line in three, and the split is not cosmetic: the
/// three are fed by three different sources and are worth three different
/// things. 推理类 arrives with the world news and is answered by reasoning about
/// it; 传闻类 is the folk legends and is answered by hunting the altar treasure;
/// 自进化类 is what the pioneer walks to a task point and accepts. Issue #206
/// §5 item 4a found the line had no such split at all — every accept went to the
/// same queue in whatever order the points happened to be in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskKind {
    /// 推理类 — from the day's official news.
    Reasoning,
    /// 传闻类 — from the folk legends, answered by the treasure hunt.
    Rumor,
    /// 自进化类 — accepted at a task point, run in the sandbox.
    SelfEvolution,
    /// Anything the book does not name. Sorted last: an unknown kind is not a
    /// reason to skip the day's known work.
    #[default]
    Other,
}

impl TaskKind {
    /// The priority the plan's ordering gives it: 推理 + 传闻 → 自进化 → 其余.
    pub fn rank(self) -> i32 {
        match self {
            TaskKind::Reasoning | TaskKind::Rumor => 0,
            TaskKind::SelfEvolution => 1,
            TaskKind::Other => 2,
        }
    }

    /// The name the log carries. Stable strings, because they are read by an
    /// analysis agent that has to group by them.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Reasoning => "reasoning",
            TaskKind::Rumor => "rumor",
            TaskKind::SelfEvolution => "self_evolution",
            TaskKind::Other => "other",
        }
    }
}

/// Which lane a task type belongs to.
///
/// The judger names the type in `taskType` (接口文档: 自进化类1 / 自进化类2), and
/// the book names the other two the same way, so the classifier is a keyword
/// scan over that string — no state, no guessing from a description, and the
/// same type always lands in the same lane.
pub fn classify(task_type: &str) -> TaskKind {
    if task_type.contains("自进化") {
        TaskKind::SelfEvolution
    } else if task_type.contains("推理") {
        TaskKind::Reasoning
    } else if task_type.contains("传闻") {
        TaskKind::Rumor
    } else {
        TaskKind::Other
    }
}

/// The last `roundNo` of the day `round_no` falls in.
pub(crate) fn turn_end_of_day(round_no: i64) -> i64 {
    let round_no = round_no.max(1);
    ((round_no - 1) / crate::model::ROUNDS_PER_DAY + 1) * crate::model::ROUNDS_PER_DAY
}

/// Feed a fresh `llmResp` into the task state machine.
///
/// The LLM responds in one of two modes:
/// - **Command mode**: returns `<<<COMMAND>>>...<<<END>>>` — we execute the command(s).
/// - **Answer mode**: returns `<<<ANSWER>>>...<<<END>>>` — after seeing sandbox
///   output (fed back as `<<<SANDBOX-OUTPUT>>>`), the LLM extracts the final
///   answer. We submit it directly.
///
/// This is the 6-step loop:
///   1. We tell the LLM the task (description + prior sandbox outputs).
///   2. LLM returns `<<<COMMAND>>>`.
///   3. We execute the script in the sandbox.
///   4. Sandbox result is fed back as `<<<SANDBOX-OUTPUT>>>`.
///   5. LLM returns `<<<COMMAND>>>` (loop to 3) or `<<<ANSWER>>>` (submit).
///      If the judger rejects, the rejection is appended to result_history
///      and the loop continues from step 4.
///   6. On success the winning script is cached as an SOP.
pub fn on_llm_resp(state: &mut BotState, resp: &str) {
    if !matches!(state.task.stage, TaskStage::Planning) {
        return;
    }
    // Priority 1: LLM returned a direct answer (<<<ANSWER>>>...<<<END>>>).
    // This happens after the LLM has seen sandbox output in result_history
    // and decided on the final answer.
    if let Some(answer) = extract_answer_marker(resp) {
        // The LLM may also include a <<<SKILL>>> summary describing the
        // approach that worked. We stash it here; it is only cached (via
        // cache_sop → extract_sop) if the judger confirms the answer is
        // correct. On rejection, sop_skill is cleared so a wrong approach
        // is never persisted as a "skill".
        let skill = extract_skill_marker(resp);
        crate::log::event(
            "task_llm_resp",
            serde_json::json!({
                "session": state.task.session_id,
                "ok": true,
                "mode": "answer",
                "answerHead": crate::log::brief(&answer, 120),
                "hasSkill": skill.is_some(),
                "fullAnswer": answer,
                "fullSkill": skill,
            }),
        );
        state.task.best_answer = answer.clone();
        state.task.no_answer_rounds = 0;
        state.task.sop_skill = skill;
        // Cache the last-executed command as the SOP: it produced the data
        // the LLM used to formulate the answer. On judger success, cache_sop
        // will save it for the next task of the same type (e.g. heritage
        // search: the curl with the correct `location=` param and dual auth
        // headers gets reused for the next city).
        state.task.sop_cmd = state.task.cmd_history.last().cloned();
        state.task.stage = TaskStage::HaveAnswer { answer };
        return;
    }
    // Priority 2: LLM returned a script to execute.
    if let Some(cmd) = extract_command(resp) {
        crate::log::event(
            "task_llm_resp",
            serde_json::json!({
                "session": state.task.session_id,
                "ok": true,
                "mode": "command",
                "cmdHead": crate::log::brief(&cmd, 300),
                "fullCommand": cmd,
            }),
        );
        state.task.stage = TaskStage::HavePlan { cmd };
    } else {
        // If extraction fails we stay in Planning and re-prompt next round.
        crate::log::event(
            "task_llm_resp",
            serde_json::json!({
                "session": state.task.session_id,
                "ok": false,
                "respHead": crate::log::brief(resp, 300),
            }),
        );
    }
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
    // Note: the full result (exit code + output head) is already logged as
    // `cmd_result` by state.rs. We do NOT emit a duplicate `task_cmd_result`
    // here — that was the source of ~30 redundant lines per match. The
    // `task_cmd_failed` event below carries the reason/streak that cmd_result
    // does not.
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
    state
        .task
        .result_history
        .push(truncate(result, RESULT_CONTEXT_LAST));
    const RESULT_HISTORY_KEEP: usize = 8;
    let excess = state
        .task
        .result_history
        .len()
        .saturating_sub(RESULT_HISTORY_KEEP);
    if excess > 0 {
        state.task.result_history.drain(..excess);
    }
    let output = strip_status_line(result);
    let code = exit_code(result);

    // Fast path: engineering-fix tasks where `./check` prints `TOKEN: xxx`.
    // The TOKEN is the answer — no need for another LLM round to format it.
    if let Some(answer) = extract_token(output) {
        crate::log::event(
            "task_answer_found",
            serde_json::json!({"exit": code, "source": "token", "answer": truncate(&answer, 120)}),
        );
        state.task.best_answer = answer.clone();
        state.task.sop_cmd = state.task.cmd_history.last().cloned();
        // Auto-generate skill for engineering-fix tasks. The LLM never enters
        // answer mode (TOKEN extracted directly from sandbox output), so no
        // <<<SKILL>>> marker exists. Without this, sop_skill stays None and
        // the next similar task has no Skill Summary — the LLM wastes a round
        // re-exploring files instead of adapting the working script.
        state.task.sop_skill = Some(
            "Engineering-fix: adapt the working script for the new app — change directory, edit config values with sed, create directories, set permissions, then run ./check. The system extracts TOKEN automatically. Do NOT re-read task files, just adapt the script."
                .to_string(),
        );
        state.task.no_answer_rounds = 0;
        state.task.stage = TaskStage::HaveAnswer { answer };
        return;
    }

    // Default path: sandbox output goes into result_history. Next round the
    // LLM sees it (wrapped in <<<SANDBOX-OUTPUT>>> by build_prompt) and
    // returns <<<ANSWER>>> with the properly formatted answer.
    //
    // The old code tried to extract `ANSWER:` from script stdout here, which
    // forced the LLM to know the exact answer format BEFORE running the
    // query. That caused format mismatches: wrong field names (`count` vs
    // `total_count`), placeholder values, zero-count submissions. Now the
    // LLM sees the data first, then formats.
    state.task.no_answer_rounds = state.task.no_answer_rounds.saturating_add(1);
    crate::log::event(
        "task_cmd_failed",
        serde_json::json!({
            "reason": if result.starts_with("[TIMEOUT]") {
                "timeout"
            } else if code.is_some_and(|code| code != 0) {
                "exit_nonzero"
            } else {
                "no_answer_marker"
            },
            "streak": state.task.no_answer_rounds,
        }),
    );
    state.task.stage = TaskStage::Planning;
}

/// The fixed setup block prepended to every command that goes to the sandbox.
///
/// The model's script is the answer to the task; this is the answer to the
/// ENVIRONMENT, and it is the same bytes every round because the three failures
/// it removes are properties of the sandbox rather than of any one task.
/// Measured across issues #126-#130 (`表 4d` counts them as `exit_nonzero` and
/// `no_answer_marker`):
///
///   * `表 4d` counts, per match, `8 exit_nonzero` + `6 no_answer_marker` (#126),
///     `5 + 4` (#128), `7 + 3` (#129), `2 + 7` (#127), `3` (#130) — 22 commands
///     across the batch that never reached their own `ANSWER:` line. A session
///     has a 24-46 round budget and a failed command costs about four of them,
///     so three failures is the session: all 27 ended `success: false`.
///   * the one named cause with a verbatim line attached is the Windows
///     checkout the task files come from. #127 r18 and r173 both died on
///     `/bin/bash: ./check: /bin/sh^M: bad interpreter: No such file or
///     directory` — session 1 lost six rounds to it and session 4 eight.
///   * `'ascii' codec can't encode characters in position 33-34: ordinal not in
///     range(128)` (#127 r39) — a Python sandbox that defaults to ASCII because
///     the locale is unset, on a task whose field names are Chinese.
///
/// The prompt has warned about the first since v14 (rule 12) and the model still
/// walks into it; doing it in the prelude is deterministic and costs the model
/// nothing.
///
/// Three rules the block obeys, because it runs in front of arbitrary model
/// output:
///
///   1. it prints NOTHING, so `ANSWER:`/`FIELDS:`/`SCHEMA:` scanning is
///      unaffected;
///   2. every statement ends in `|| true` and none of them is `set -e`, so a
///      sandbox without `find`/`sed`/`chmod` still runs the model's script;
///   3. it never changes the working directory — a relative path the model was
///      told to verify must still resolve the way the model expects.
///
/// `sed -i` without a suffix is the GNU form, which is what the sandbox has
/// (the failure text above is Linux bash with python3.11). `|| true` covers the
/// rest.
pub fn sandbox_prelude() -> &'static str {
    concat!(
        "# --- coregeek prelude: fixed sandbox setup, not model output ---\n",
        "export LC_ALL=C.UTF-8 LANG=C.UTF-8 PYTHONIOENCODING=utf-8 PYTHONUTF8=1 2>/dev/null || true\n",
        // pk-622190 session 1 (B-1 alpha): `config/alpha.conf` was owned by
        // root and the sandbox user got `[Errno 13] Permission denied` on
        // every write attempt. The LLM retried 3 times with the same error,
        // wasting the whole session. `chmod u+rw` on every file under the
        // task directory fixes this at the prelude level — the sandbox user
        // is the owner of the tree, so `u+rw` is always safe, and a file that
        // is already writable is unaffected.
        "find /tmp -type f -exec chmod u+rw {} + 2>/dev/null || true\n",
        "find /tmp -type d -exec chmod u+rwx {} + 2>/dev/null || true\n",
        "find /tmp -type f -name 'check' -exec chmod +x {} + 2>/dev/null || true\n",
        // Strip CRLF from ALL text files, not just check/*.sh. teamB(2).log
        // session 2: `sed -i 's/^port 9999$/port 8080/'` on `config/alpha.conf`
        // failed because `$` matches before `\r`, not at the actual end of
        // line. The LLM retried the same failing sed 2 times, wasting the
        // session. Extending CRLF stripping to .conf/.md/.txt/.json/.yaml/.yml
        // covers config files the LLM needs to edit with line-based tools.
        "find /tmp -type f \\( -name 'check' -o -name '*.sh' ",
        "-o -name '*.conf' -o -name '*.md' -o -name '*.txt' -o -name '*.json' ",
        "-o -name '*.yaml' -o -name '*.yml' -o -name '*.csv' -o -name '*.ini' ",
        "-o -name '*.cfg' -o -name '*.properties' \\) ",
        "-exec sed -i 's/\\r$//' {} + 2>/dev/null || true\n",
        "# --- end prelude ---\n",
    )
}

/// The exact bytes that go to the sandbox for a script the task loop produced.
/// Split out so the prelude can be asserted on the wire without a whole match.
pub fn sandbox_command(cmd: &str) -> String {
    format!("{}{}", sandbox_prelude(), cmd)
}

/// 改进3: Generate a `find` script that locates task files and workspace
/// directories in the sandbox, so the LLM can skip 2-3 rounds of path
/// exploration. Extracts any `task_*.md` filename mentioned in the
/// description and tries to `cat` it directly; also lists all task files
/// and workspace dirs as fallback.
///
/// Returns None if the description doesn't look like a self-evolution task
/// (no `task_` filename found and no `/tmp` hint).
fn auto_find_script(description: &str) -> Option<String> {
    // Extract task filename(s) from the description, e.g. "task_2_nanjing.md"
    let mut task_files: Vec<&str> = Vec::new();
    for word in description.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '.');
        if clean.starts_with("task_")
            && (clean.ends_with(".md") || clean.ends_with(".txt"))
            && !task_files.contains(&clean)
        {
            task_files.push(clean);
        }
    }
    // 改进18: Extract type tokens from task filenames for fuzzy matching.
    // From "task_2_nanjing.md" → token "nanjing"; from "task_1_api_test.md" → "api_test".
    // Used to find same-type task files so the LLM can reference their solutions.
    let mut type_tokens: Vec<String> = Vec::new();
    for tf in &task_files {
        let stem = tf.rsplit_once('.').map_or(*tf, |(stem, _)| stem);
        // task_N_<type> → take everything after the second underscore
        let parts: Vec<&str> = stem.splitn(3, '_').collect();
        if parts.len() == 3 {
            let token = parts[2].to_string();
            if !token.is_empty() && !type_tokens.contains(&token) {
                type_tokens.push(token);
            }
        }
    }
    // Also check for .py, .json, .yaml task files
    for word in description.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '.');
        if clean.starts_with("task_")
            && !task_files.contains(&clean)
            && (clean.ends_with(".py") || clean.ends_with(".json") || clean.ends_with(".yaml"))
        {
            task_files.push(clean);
        }
    }
    if task_files.is_empty() {
        return None;
    }
    let mut script = String::from("# Auto-locate task files and workspace directories\n");
    script.push_str("echo '=== TASK FILES ==='\n");
    for tf in &task_files {
        script.push_str(&format!(
            "FOUND=$(find /tmp -name '{}' -type f 2>/dev/null | head -1)\n",
            tf
        ));
        script.push_str("if [ -n \"$FOUND\" ]; then\n");
        script.push_str(&format!("    echo '--- {} ---'\n", tf));
        script.push_str("    cat \"$FOUND\"\n");
        script.push_str("else\n");
        script.push_str(&format!("    echo 'NOT FOUND: {}'\n", tf));
        script.push_str("fi\n");
    }
    // 改进18: Fuzzy-match same-type task files by type token. From each
    // extracted token (e.g. "nanjing"), find up to 3 task files whose name
    // contains the token and cat them. This lets the LLM see solutions to
    // similar tasks in its first prompt, skipping exploration rounds.
    if !type_tokens.is_empty() {
        script.push_str("echo '=== SAME-TYPE TASK FILES (fuzzy) ==='\n");
        for token in &type_tokens {
            script.push_str(&format!(
                "FUZZY=$(find /tmp -name 'task_*{}*' -type f 2>/dev/null | sort | head -3)\n",
                token
            ));
            script.push_str("for F in $FUZZY; do\n");
            script.push_str("    echo \"--- $(basename $F) ---\"\n");
            script.push_str("    cat \"$F\"\n");
            script.push_str("done\n");
        }
    }
    script.push_str("echo '=== ALL TASK FILES ==='\n");
    script.push_str("find /tmp -name 'task_*' -type f 2>/dev/null | sort\n");
    script.push_str("echo '=== WORKSPACE DIRS ==='\n");
    script.push_str("find /tmp -type d -name 'ws_*' 2>/dev/null | sort\n");
    Some(script)
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
    _pioneer: &Unit,
    plan: &mut Plan,
) -> Option<RoleCommand> {
    // Timeout guard: submit the strongest observed task result before expiry.
    // Never turn the task description or an exploratory file listing into an
    // answer; both have caused guaranteed-zero submissions in prior matches.
    //
    // SKIP when already submitted: re-submitting the same answer resets
    // `phase_missing_rounds` and `point_closed_round` (see HaveAnswer arm),
    // which prevents the closure probe from ever reaching `phase_missing >= 2`.
    // pk-621639 session 4 (beta): answer submitted at R158, guard re-fired at
    // R159/R160 (rounds_left=2/1), re-submitted each round, phase_missing never
    // reached 2, session ended `timeout` instead of `confirmed_success`.
    // The `bank_task_reward("timeout_unrejected")` path in state.rs already
    // settles unrejected submissions at timeout, so re-submitting is both
    // wasteful and harmful.
    let rounds_left = state.task.timeout_round.saturating_sub(turn.round_no);
    if rounds_left <= 2 && state.task.submitted_round.is_none() {
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
            // Rumor tasks (传闻类) are answered by the treasure hunt, not the
            // sandbox. Skip the LLM prompt loop entirely — if the treasure has
            // been summoned, submit the altar plan as the answer; if not, wait
            // for the treasure system to finish (it runs in parallel).
            if state.task.kind == TaskKind::Rumor {
                if let Some(answer) = crate::brain::treasure::rumor_task_answer(state) {
                    state.task.best_answer = answer.clone();
                    state.task.stage = TaskStage::HaveAnswer { answer };
                    return None; // HaveAnswer is processed next round
                }
                // Treasure not yet summoned — wait silently, no LLM call.
                return None;
            }
            // 改进3: Auto-locate task files before the first LLM call. When a
            // task starts with no SOP and no cmd_history, inject a `find`
            // script that locates task files and workspace dirs. The LLM sees
            // the results in its first prompt's <<<SANDBOX-OUTPUT>>> and can
            // go straight to the API query — skipping the 2-3 round path
            // exploration (cat → fail → find → cat) that plagued sessions 3/5/6.
            if state.task.sop_reuse_script.is_none()
                && state.task.cmd_history.is_empty()
                && state.task.result_history.is_empty()
            {
                if let Some(find_cmd) = auto_find_script(&state.task.description) {
                    state.task.cmd_history.push(find_cmd.clone());
                    state.task.cmd_request_round = Some(turn.round_no);
                    state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
                    crate::log::event(
                        "task_auto_find",
                        serde_json::json!({
                            "session": state.task.session_id,
                            "round": turn.round_no,
                            "cmd": crate::log::brief(&find_cmd, 200),
                        }),
                    );
                    plan.execute_cmd = Some(find_cmd);
                    return None;
                }
            }
            // SOP fast path: a cached script from a similar task is given to
            // the LLM as reference. The LLM adapts it to this task instead of
            // re-reading the task file and re-exploring from scratch.
            if state.task.sop_reuse_script.is_none() && state.task.cmd_history.is_empty() {
                if let Some((template, skill, history)) =
                    state.find_sop_with_skill(&state.task.task_type, &state.task.description)
                {
                    state.task.sop_used_template = Some(template.clone());
                    state.task.sop_reuse_script = Some(template);
                    state.task.sop_reuse_skill = skill;
                    state.task.sop_reuse_history = history;
                }
            }
            if plan.prompt.is_none() && state.request_prompt(PromptPurpose::Task, turn) {
                // LLM calls during an active task are free (do not count
                // toward the 3/day budget), per the interface doc — which is why
                // this is granted on every round the line asks for it, and why
                // the task line is the purpose that loses least by being ranked
                // last: what a refusal costs it is the round the news read took,
                // and the news read asks once a day.
                state.task.llm_request_round = Some(turn.round_no);
                let prompt = build_prompt(state, turn);
                let has_sandbox_outputs = !state.task.result_history.is_empty();
                let has_rejection = !state.task.rejection_feedback.is_empty();
                let phase = if has_rejection {
                    "fix"
                } else if has_sandbox_outputs {
                    "iterate"
                } else {
                    "explore"
                };
                crate::log::event(
                    "task_prompt_sent",
                    serde_json::json!({
                        "session": state.task.session_id,
                        "round": turn.round_no,
                        "phase": phase,
                        "hasSop": state.task.sop_reuse_script.is_some(),
                        "hasResults": has_sandbox_outputs,
                        "promptHead": crate::log::brief(&prompt, 300),
                    }),
                );
                plan.prompt = Some(prompt);
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
            // Removed same-round delay: the judger can accept executeCmd in the
            // same round as llmResp.  Saving 1 round per iteration matters for
            // query tasks (3-4 round-trips needed, sterile threshold 15).
            if plan.execute_cmd.is_none() {
                state.task.cmd_history.push(cmd.clone());
                state.task.cmd_request_round = Some(turn.round_no);
                state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
                crate::log::event(
                    "task_cmd_sent",
                    serde_json::json!({
                        "session": state.task.session_id,
                        "round": turn.round_no,
                        "cmdHead": crate::log::brief(&cmd, 300),
                    }),
                );
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
            // The red line: meta-descriptions, sentinels and exploratory output
            // are NEVER submitted — with submit-as-accumulating there is no
            // later gate to catch them.
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
            // keeps the highest pass rate, so the answer in hand is banked the
            // round it exists. The LLM is responsible for producing the correct
            // JSON shape — the bot only ensures it is valid JSON for the wire.
            state.task.best_answer = answer.clone();
            let wire = answer_wire_payload(&answer);
            let (logged, chars) = answer_for_log(&wire);
            crate::log::event(
                "task_answer_submit",
                serde_json::json!({
                    "session": state.task.session_id,
                    "task_kind": state.task.kind.as_str(),
                    "round": turn.round_no,
                    "answer": logged,
                    "chars": chars,
                    "wire": wire != answer,
                    "wrongSoFar": state.task.wrong_answers,
                    "rejections": state.task.rejections,
                    "rejectionFeedback": state.task.rejection_feedback.len(),
                }),
            );
            state.task.submitted_round = Some(turn.round_no);
            state.task.phase_missing_rounds = 0;
            state.task.point_closed_round = None;
            state.task.post_submit_error = false;
            state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };
            Some(RoleCommand::submit_answer(&wire))
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

/// How much of the most recent sandbox output is fed back to the model.
///
/// It was 600 characters — for the last TWO runs alike — while the runs
/// themselves were 2.4k-5.5k characters (`cmd_result.chars` across issues
/// #111-#115). A reconnaissance script that prints a banner, a `find` listing
/// and then the task file therefore had its task file cut off, so the model
/// could not compute an answer from what it had already read and re-ran the
/// reconnaissance instead. Issue #115 is the shape of that loop: 14 commands,
/// 13 of them with no `ANSWER:` line, six sessions, **zero submissions**, every
/// one of them ending `reason=timeout, rejections=0, wrongAnswers=0`.
///
/// The previous run keeps a smaller window than the last one because it is
/// context, not material: what the model needs to answer from is what it just
/// read.
/// Maximum characters for sandbox output fed back to the LLM. The user's
/// directive is "never truncate" — set to 100000 so full API responses,
/// file dumps, and error messages survive intact. Truncation was causing
/// the LLM to miss data it needed (pk615723 R36 had 4287 chars of heritage
/// records, was cut at 3000, key fields lost).
const RESULT_CONTEXT_LAST: usize = 100_000;
const RESULT_CONTEXT_PREVIOUS: usize = 100_000;

pub fn build_prompt(state: &BotState, turn: &Turn) -> String {
    let mut prompt = String::new();

    // ---- The 6-step loop ----
    //
    //   1. Tell the LLM the task (description + prior results).
    //   2. LLM returns <<<COMMAND>>> — we extract and execute in the sandbox.
    //   3. Sandbox result is fed back wrapped in <<<SANDBOX-OUTPUT>>>.
    //   4. LLM inspects sandbox output and decides:
    //        a) return <<<COMMAND>>> for another query → go to step 3, or
    //        b) return <<<ANSWER>>> with the formatted answer → submit.
    //   5. If the judger rejects the answer, the rejection is appended to
    //      result_history as another <<<SANDBOX-OUTPUT>>> entry and the loop
    //      continues from step 4 — the LLM sees WHY it was wrong and tries
    //      again, until timeout.
    //   6. On success, the winning script is cached as an SOP so the next
    //      task of the same type can skip exploration.
    //
    // There are NO fixed phases (read/solve/fix). The LLM directs its own
    // actions: it may read files, read API docs, query APIs, run check
    // scripts, compute answers, or fix rejected submissions — in whatever
    // order it decides. The prompt presents all available context and asks
    // "what do you want to do next?"

    let has_rejection = !state.task.rejection_feedback.is_empty();
    let left = state.task.timeout_round.saturating_sub(turn.round_no);

    prompt.push_str(
        "Write a shell or python script to execute in a sandbox (shell + python3, no internet). ",
    );
    prompt.push_str("Output all text in English to avoid encoding issues.\n\n");

    // ---- API call rules (prominent, learned from battle losses) ----
    //
    // In pk615723/pk616181 the LLM wasted 3 rounds discovering that the API
    // requires BOTH `X-API-Key` and `Authorization: Bearer` headers, then
    // timed out before submitting the answer. The API docs were unreliable
    // (only mentioned X-API-Key, wrong param name `city` vs actual `location`).
    // These rules front-load hard-won knowledge so the first API call succeeds.
    prompt.push_str("## API Call Rules (READ FIRST)\n\n");
    prompt.push_str("1. **Use `curl -v`, NOT python urllib** — `curl -v` prints response headers (e.g. `WWW-Authenticate`) that reveal missing auth, and has no SSL import pitfalls. `python3 -c 'import ssl'` fails in this sandbox.\n");
    prompt.push_str("2. **Dual auth headers (HINT)** — add BOTH headers to every API request, even if the docs only mention one:\n");
    prompt.push_str(
        "   ```
curl -v -H \"X-API-Key: <key>\" -H \"Authorization: Bearer <key>\" \"<url>\"
```\n",
    );
    prompt.push_str("   The API docs may be incomplete or outdated. Use the key found in the task files, but always send both headers.\n");
    prompt.push_str("3. **Always add `-v`** — response headers expose `WWW-Authenticate`, `Content-Type`, and error details that tell you exactly what to fix.\n");
    prompt.push_str("4. **Read error messages and FIX the command** — when an API returns 400/401/404, the response body tells you what's wrong (missing header, wrong param name, invalid value). Change the command based on the error and try again. Do NOT repeat the same request.\n");
    prompt.push_str("   - Example: docs say `?city=北京` but server says `field 'location' is required` → change to `?location=北京`.\n");
    prompt.push_str("5. **URL-encode Chinese** in query params: `$(python3 -c 'from urllib.parse import quote; print(quote(\"北京\"))')`.\n\n");

    // ---- Step 1: Task description ----
    prompt.push_str("## Task Description\n\n");
    prompt.push_str(&state.task.description);
    prompt.push_str("\n\n");

    // ---- SOP reuse: cached script + skill from a similar task ----
    //
    // 确认70 强提示: 即使有 SOP/skill 缓存，LLM 也必须重新读题。相同类型的
    // 任务逻辑一致，但题目的具体内容、要求（特别是最终数量、工作目录路径）
    // 可能发生变化。skill 只提供方法论，不能替代对当前题目内容的仔细阅读。
    prompt.push_str("⚠️ **READ THE TASK DESCRIPTION ABOVE CAREFULLY — even if a Skill/Reference is provided below.** Same-type tasks share logic, but the task content, required quantities, workspace paths, and parameters may differ. The Skill tells you HOW to solve; the Task Description tells you WHAT to solve. Always verify the current task's specific values against the Task Description before reusing any cached command.\n\n");
    let has_sop = state.task.sop_reuse_script.is_some();
    if let Some(script) = &state.task.sop_reuse_script {
        let verified = state.task.sop_used_template.is_some();
        if verified {
            prompt.push_str("## Verified Reference (from a previous SUCCESSFUL task)\n\n");
        } else {
            prompt.push_str("## Reference Script (unverified, from a previous attempt)\n\n");
        }
        // Show the skill summary first — it's the human-readable guidance
        // that tells the LLM WHAT worked and WHY, so it can produce a working
        // command directly without exploration.
        if let Some(skill) = &state.task.sop_reuse_skill {
            prompt.push_str("### Skill Summary\n\n");
            prompt.push_str(skill);
            prompt.push_str("\n\n");
        }
        // 改进1: Show the full command history (file exploration + API query)
        // so the LLM can see the file paths that were discovered, not just
        // the final command. This lets it skip the find/cat exploration
        // entirely.
        if state.task.sop_reuse_history.len() > 1 {
            prompt.push_str("### Full Command Sequence (from previous task)\n\n");
            for (i, cmd) in state.task.sop_reuse_history.iter().enumerate() {
                prompt.push_str(&format!(
                    "**Step {}**:\n```\n{}\n```\n\n",
                    i + 1,
                    crate::log::brief(cmd, 500)
                ));
            }
            prompt.push_str("The steps above show how the previous task located files and queried data. **Extract the directory paths from the exploration steps** and reuse them for the current task — the directory structure is the same across tasks of the same type, only the task filename changes (e.g. `task_1_a.md` → `task_2_b.md`).\n\n");
        }
        prompt.push_str("### Working Script (final command that produced the answer)\n\n");
        prompt.push_str("```\n");
        prompt.push_str(script);
        prompt.push_str("\n```\n");
        if verified {
            prompt.push_str("This approach was verified correct by the judger. Adapt it for the current task: change only the task-specific values (directory names, config values, query params). Produce a `<<<COMMAND>>>` with the adapted command directly.\n\n");
            prompt.push_str("⚠️ **The script above is from a PREVIOUS task** — its file paths, directory names, and config values belong to that task, NOT the current one. Before executing the adapted script, verify the current task's workspace directory from the Task Description above and replace all paths accordingly. A previous task wasted its entire session editing files in the wrong directory because it copied paths from the reference script without checking.\n\n");
        } else {
            prompt.push_str("Adapt its structure for the current task. The API docs may be incomplete — trust the working command's patterns over the docs.\n\n");
            prompt.push_str("⚠️ The script above contains paths from a previous task — verify the current task's workspace directory from the Task Description and replace all paths before executing.\n\n");
        }
    }

    // ---- Steps 3-4: Execution history (sandbox outputs fed back) ----
    //
    // Each prior result — whether from a script execution or a judger
    // rejection — appears here wrapped in <<<SANDBOX-OUTPUT>>>. The LLM's
    // job each round is to read this history and decide: run another
    // script, or extract the answer.
    if !state.task.result_history.is_empty() {
        prompt.push_str("## Sandbox Output History\n\n");
        let start = state.task.result_history.len().saturating_sub(3);
        for (idx, result) in state.task.result_history[start..].iter().enumerate() {
            let round_num = start + idx + 1;
            let is_last = round_num == state.task.result_history.len();
            let cap = if is_last {
                RESULT_CONTEXT_LAST
            } else {
                RESULT_CONTEXT_PREVIOUS
            };
            prompt.push_str(&format!("### Output #{}\n", round_num));
            prompt.push_str("<<<SANDBOX-OUTPUT>>>\n");
            prompt.push_str(&truncate(result, cap));
            prompt.push_str("\n<<<END>>>\n\n");
        }

        // ---- Compact error hint ----
        if let Some(last) = state.task.result_history.last() {
            let output = strip_status_line(last);
            let code = exit_code(last);
            let is_error = code.is_some_and(|c| c != 0)
                || output.contains("Error")
                || output.contains("error")
                || output.contains("Permission denied")
                || output.contains("No such file")
                || output.contains("401")
                || output.contains("404")
                || output.contains("Traceback")
                || last.contains("[JUDGER_REJECTION]");
            if is_error {
                prompt.push_str("⚠️ Last execution failed or was rejected — read the feedback above and change strategy. Do NOT repeat the same approach.\n\n");
            }
        }
    }

    // ---- Step 2/4: Output format (strict) ----
    prompt.push_str("## Your Role\n\n");
    prompt.push_str("You operate in a loop: you return executable shell commands, the system runs them in the sandbox and feeds the output back, you inspect the output and decide what to do next.\n\n");
    prompt.push_str("**Same-type task files**: the sandbox output may include `=== SAME-TYPE TASK FILES (fuzzy) ===` — these are tasks with similar names (e.g. same city or same API type). Read their content for reusable API endpoints, parameter names, and answer formats. If a same-type file shows the exact API call that worked, adapt it (change the parameter value) and submit immediately.\n\n");
    prompt.push_str("Each round, return EXACTLY ONE of:\n\n");
    prompt
        .push_str("**Command mode** — to query data (read files, call APIs, run check scripts):\n");
    prompt.push_str("```\n<<<COMMAND>>>\ncurl -v -H \"X-API-Key: key\" -H \"Authorization: Bearer token\" \"http://localhost:8899/api/v1/...?param=value\"\n<<<END>>>\n```\n");
    prompt.push_str("(use `<<<COMMAND:python>>>` for python; put ONLY the executable shell command(s) between markers, no explanations)\n\n");
    prompt.push_str("**改进40: You may output MULTIPLE <<<COMMAND>>> blocks** in one round for debugging — they execute sequentially in one sandbox round. Prefer `curl` commands for API queries. Each block is an **executable shell command**, NOT a script — think `curl ...`, `cat ...`, `find ...`, `python3 -c '...'`, not a multi-line bash file.\n\n");
    prompt
        .push_str("**Answer mode** — after seeing enough sandbox output, give the final answer:\n");
    prompt.push_str("```\n<<<ANSWER>>>\n{\"field1\": value1, \"field2\": value2}\n<<<END>>>\n<<<SKILL>>>\nCOMMAND: curl -v -H \"X-API-Key: ...\" -H \"Authorization: Bearer ...\" \"http://...?city=北京\"\nANSWER: {\"name\": \"记录名称(string)\", \"count\": \"符合条件的记录总数(int，需从数组聚合)\", \"earliest\": \"按某字段排序后取最早的记录名(string，不是排序键本身)\"} — each field annotated with its semantic meaning so the next task knows what to fill without re-reading the task doc.\nPATHS: task files under /tmp/1-fixed-step/1-unknown-api/; workspace under /tmp/1-fixed-step/2-engineering-fix/ws_N/\nGOTCHA: docs say city= but server needs location=; URL-encode Chinese params.\n<<<END>>>\n```\n");
    prompt.push_str("(put ONLY the best-matching answer value in <<<ANSWER>>>. The <<<SKILL>>> block is MANDATORY — the system caches it for the next task of the same type. Without it, the next task starts from scratch. Include FOUR lines:\n");
    prompt.push_str("1. **COMMAND**: the exact command that produced the correct data (the one you just ran successfully).\n");
    prompt.push_str("2. **ANSWER**: the answer shape you summarized from the sandbox output — for EACH field write its name AND a short semantic annotation describing what value goes there (not just the type). **Copy the field's definition from the task document** — the annotation must reflect the task doc's exact wording about what each field means. The next task of the same type will read this line to format its own answer WITHOUT re-reading the task document — so the annotation must be clear enough that changing a parameter (e.g. city=北京→南京) is all that's needed.\n");
    prompt.push_str("3. **PATHS**: the directory paths where task files and workspaces are located (e.g. `task files under /tmp/.../1-unknown-api/; workspace under /tmp/.../2-engineering-fix/ws_N/`). The next task will use these paths to locate files WITHOUT running `find` first.\n");
    prompt.push_str("4. **GOTCHA**: any non-obvious pitfall (wrong param name, auth header order, encoding issue, aggregation needed).\n\n");
    prompt.push_str("**Do NOT write code to submit answers** — the system handles submission. Ignore any submit endpoint mentioned in the task file.\n\n");

    // ---- Answer formatting from sandbox data ----
    //
    // The LLM frequently queries an API, gets back a raw array of records,
    // and submits that raw array directly. The judger expects a reorganized
    // object matching the task document's schema. The LLM must READ the task
    // doc's required output format, then TRANSFORM the sandbox data into that
    // shape before submitting.
    prompt.push_str("## Answer Formatting (CRITICAL — Transform Sandbox Data to Task Schema)\n\n");
    prompt.push_str("The sandbox returns RAW data (API arrays, file contents). You MUST reorganize it to match the task document's required answer format BEFORE submitting. The judger validates field names, types, and structure — raw API output is ALWAYS wrong.\n\n");
    prompt.push_str(
        "**Combining Multiple Sandbox Outputs** (CRITICAL for paginated/queried data):\n",
    );
    prompt.push_str("- You may need several `<<<COMMAND>>>` calls to gather all data. The `<<<SANDBOX-OUTPUT>>>` section shows the LAST 3 sandbox outputs.\n");
    prompt.push_str("- When the LAST output confirms you have ALL the data you need, immediately return `<<<ANSWER>>>` by combining information from ALL previous sandbox outputs.\n");
    prompt.push_str("- Do NOT run another script to 'verify' or 're-read the task' after you have the data. Every extra round wastes time and risks timeout.\n");
    prompt.push_str("- If you're tempted to run another `<<<COMMAND>>>` just to 'be sure', instead return `<<<ANSWER>>>` with the answer computed from the data you already have.\n\n");
    prompt.push_str("**General rules**:\n");
    prompt.push_str("- Read the task document's required output schema FIRST. It defines the exact field names, types, and nesting the judger expects.\n");
    prompt.push_str("- If the task asks for a COUNT/SUM/AGGREGATE, compute it from the raw data — never submit the raw array.\n");
    prompt.push_str("- If the task asks for specific FIELDS, extract only those fields from each record — drop everything else.\n");
    prompt.push_str("- If the task asks for a SINGLE value, submit a scalar or single-field object, not an array.\n");
    prompt.push_str("- If the task asks for a LIST, submit an array of the requested items (only the requested fields), not the full API response.\n");
    prompt.push_str("- When rejected for a field value mismatch, re-read the task document's exact definition of that field.\n");
    prompt.push_str("- When unsure how to aggregate, run `python3 -c` in a <<<COMMAND>>> to compute the exact fields, then submit the computed object in <<<ANSWER>>>.\n\n");

    // ---- Suggested workflow (compact) ----
    prompt.push_str("## Workflow\n");
    if has_sop {
        prompt.push_str("1. **VERIFIED COMMAND AVAILABLE** — a working command from a previous task is shown above. Adapt it for the current task and produce `<<<COMMAND>>>` directly. DO NOT re-read task files — but DO verify the current task's workspace directory from the Task Description above, and replace any paths from the reference command with the correct directory.\n");
        prompt.push_str("2. **Submit the answer AS SOON AS you have the data** — after `<<<SANDBOX-OUTPUT>>>` contains the data you need, return `<<<ANSWER>>>` IMMEDIATELY. For engineering-fix tasks: if `./check` prints `TOKEN: xxx`, the system extracts it automatically.\n");
        prompt.push_str("3. **If rejected** — the judger's rejection appears in `<<<SANDBOX-OUTPUT>>>`. Read the feedback, fix the answer, and return `<<<ANSWER>>>` again.\n\n");
    } else {
        prompt.push_str(
            "1. **Read ALL files first** — return a `<<<COMMAND>>>` to read task files:\n",
        );
        prompt.push_str("   `find /tmp -type f \\( -name '*.md' -o -name '*.txt' -o -name '*.json' -o -name '*.yaml' -o -name '*.yml' -o -name '*.conf' -o -name '*.csv' \\) -exec sh -c 'echo \"=== $1 ===\"; cat \"$1\"; echo' _ {} \\;`\n");
        prompt.push_str("   Do NOT use `head -5` (truncates). Reading everything in round 1 saves rounds. You can output multiple `<<<COMMAND>>>` blocks to read files AND query APIs in the same round.\n");
        prompt.push_str("2. **Query APIs** — follow the **API Call Rules** at the top of this prompt. Prefer `curl -v` commands (NOT python urllib). Use BOTH auth headers. Copy the EXACT URL and key from API_DOCS.md. If the task or docs mention a total record count (e.g. \"15 records\"), add `&limit=<total>` to fetch ALL records in ONE request — never paginate when you already know the count.\n");
        prompt.push_str("3. **Submit the answer AS SOON AS you have the data** — after `<<<SANDBOX-OUTPUT>>>` contains the data you need, return `<<<ANSWER>>>` IMMEDIATELY. Do NOT run another command to 'verify' or 'format' — extract the answer from the output you already have. Every extra round risks timeout.\n");
        prompt.push_str("   - Format the answer EXACTLY as the task requires (field names, types, structure from API_DOCS.md). Do NOT guess field names or submit placeholder/zero values.\n");
        prompt.push_str("   - For engineering-fix tasks: if `./check` prints `TOKEN: xxx`, the system extracts it automatically — no need for `<<<ANSWER>>>`.\n");
        prompt.push_str("4. **If rejected** — the judger's rejection appears in `<<<SANDBOX-OUTPUT>>>`. Read the feedback, fix the answer, and return `<<<ANSWER>>>` again (or `<<<COMMAND>>>` if you need to re-query).\n\n");
    }

    // ---- Sandbox pitfalls (compact) ----
    prompt.push_str("## Sandbox Rules\n");
    prompt.push_str("- 15s timeout: no sleep, no retry loops, no internet. Locale and CRLF fix already prepended.\n");
    prompt.push_str("- `bad interpreter` → `bash <script>`. Do not `set -e`, do not `cd` to unconfirmed paths.\n\n");

    // ---- What to do this round ----
    prompt.push_str("## This Round\n\n");
    prompt.push_str(&format!("Rounds remaining: {}.\n", left));
    prompt.push_str(
        "**The answer must be computed from sandbox data, not guessed from training data.**\n",
    );
    let has_sandbox_outputs = !state.task.result_history.is_empty();
    if has_sandbox_outputs {
        if has_rejection {
            prompt.push_str("Your last answer was REJECTED — read the rejection feedback in the sandbox output above and return a corrected `<<<ANSWER>>>` (or `<<<COMMAND>>>` if you need to re-query).\n");
        } else {
            prompt.push_str(
                "**Evaluate the sandbox output above against the task document's requirements:**\n",
            );
            prompt.push_str("- Does it contain ALL the data the task asks for? (Check: total records, all pages, all fields)\n");
            prompt.push_str("- **If NOT** — you still need more data. Return `<<<COMMAND>>>` with the exact command to get the missing data. You may output multiple `<<<COMMAND>>>` blocks for different queries.\n");
            prompt.push_str("- **If YES** — you have enough data. Return `<<<ANSWER>>>` with the formatted answer AND `<<<SKILL>>>` with the reusable command/answer pattern. Do NOT run another command.\n");
        }
    } else if has_sop {
        prompt.push_str("A verified working command is shown above. Adapt it for the current task and return `<<<COMMAND>>>` directly — but verify the current task's workspace directory from the Task Description and replace any paths from the reference command.\n");
    } else {
        prompt.push_str("Return `<<<COMMAND>>>` to read files and query data. Prefer `curl` commands for API calls. You may output multiple `<<<COMMAND>>>` blocks in one round for debugging.\n");
    }
    prompt.push('\n');

    // ---- Stall pressure (only after multiple answer-less rounds) ----
    if state.task.no_answer_rounds > 0 {
        prompt.push_str(&format!(
            "⚠️ {} consecutive round(s) without an answer, {} rounds remaining. Do not repeat file exploration — extract the answer from existing sandbox output.\n",
            state.task.no_answer_rounds, left,
        ));
        if state.task.no_answer_rounds >= 2 {
            prompt.push_str("You MUST return `<<<ANSWER>>>` this round, even if unsure — a wrong answer has a pass rate, no answer is 0 points.\n");
        }
        prompt.push('\n');
    }

    prompt
}

/// Extract an executable command from an LLM response.
///
/// Priority 1: `<<<COMMAND[:lang]>>>...<<<END>>>` markers — the prompt
/// instructs the LLM to put ONLY the executable script between these
/// markers, so extraction is exact and unaffected by explanatory text
/// outside the markers. This avoids the common failure mode where the
/// LLM wraps a reconnaissance script and an answer script in two
/// separate ``` blocks and we pick the wrong one.
///
/// Priority 2: fenced code blocks (```bash / ```python) — fallback for
/// LLM responses that did not use the new markers (backward compat).
pub fn extract_command(resp: &str) -> Option<String> {
    // Priority 1: <<<COMMAND[:lang]>>>...<<<END>>> markers (改进40: renamed from SCRIPT)
    // 改进40: Support multiple <<<COMMAND>>> blocks — they are independent
    // executable shell commands joined into ONE executable string with " ; "
    // so every command runs regardless of the previous one's exit code
    // (debugging context: we want all outputs even if one fails).
    let commands = extract_all_from_command_markers(resp);
    if !commands.is_empty() {
        let wrapped: Vec<String> = commands
            .iter()
            .map(|(lang, body)| wrap(lang, body))
            .collect();
        let joined = wrapped.join(" ; ");
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    } else {
        // Priority 2: fenced code blocks
        extract_from_fenced(resp)
    }
}

/// Extract ALL `<<<COMMAND[:lang]>>>...<<<END>>>` blocks from the response.
/// 改进40: Multiple commands per round — each block is an independent executable
/// shell command (prefer curl), not a script. They are executed sequentially.
fn extract_all_from_command_markers(resp: &str) -> Vec<(String, String)> {
    const START_TAG: &str = "<<<COMMAND";
    const END_TAG: &str = "<<<END>>>";
    let mut results = Vec::new();
    let mut search_from = 0;
    while let Some(start_idx) = resp[search_from..].find(START_TAG) {
        let start_idx = search_from + start_idx;
        let after_start = &resp[start_idx..];
        let tag_close = match after_start.find(">>>") {
            Some(idx) => idx,
            None => break,
        };
        let lang_spec = &after_start[START_TAG.len()..tag_close];
        let lang = lang_spec.trim_start_matches(':').trim().to_lowercase();
        let lang = if lang.is_empty() {
            "bash".to_string()
        } else {
            lang
        };

        let body_start = start_idx + tag_close + 3;
        let remaining = &resp[body_start..];
        let end_offset = match remaining.find(END_TAG) {
            Some(idx) => idx,
            None => break,
        };
        let body = remaining[..end_offset].trim_matches('\n');
        if !body.is_empty() {
            results.push((lang, body.to_string()));
        }
        search_from = body_start + end_offset + END_TAG.len();
    }
    results
}

/// Extract from ```bash / ```python fenced code blocks (fallback).
fn extract_from_fenced(resp: &str) -> Option<String> {
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

/// Parse "[exitCode:N]\n<output>" and return the output part.
pub fn strip_status_line(result: &str) -> &str {
    if let Some(rest) = result.strip_prefix("[exitCode:") {
        if let Some(nl) = rest.find('\n') {
            return rest[nl + 1..].trim_start_matches(['\r', '\n']);
        }
    }
    if let Some(rest) = result.strip_prefix("[TIMEOUT]") {
        return rest.trim_start_matches(['\r', '\n']);
    }
    if let Some(rest) = result.strip_prefix("[JUDGER_ERROR]") {
        return rest.trim_start_matches(['\r', '\n']);
    }
    result
}

/// Extract the final answer from an LLM response's `<<<ANSWER>>>...<<<END>>>`
/// marker. This is the "answer mode" of the two-mode protocol: the LLM has
/// seen sandbox output and is returning the formatted answer directly.
pub fn extract_answer_marker(resp: &str) -> Option<String> {
    const START_TAG: &str = "<<<ANSWER";
    const END_TAG: &str = "<<<END>>>";

    let start_idx = resp.find(START_TAG)?;
    let after_start = &resp[start_idx..];
    let tag_close = after_start.find(">>>")?;
    let body_start = start_idx + tag_close + 3;
    let remaining = &resp[body_start..];
    let end_offset = remaining.find(END_TAG)?;
    let body = remaining[..end_offset].trim();
    if body.is_empty() {
        return None;
    }
    Some(body.to_string())
}

/// Extract an optional `<<<SKILL>>>...<<<END>>>` summary from an LLM response.
///
/// When the LLM returns `<<<ANSWER>>>`, it may also include a `<<<SKILL>>>`
/// block describing the approach that worked — API endpoints, auth headers,
/// parameter names, gotchas discovered. This skill is cached alongside the
/// SOP on judger success, so the next task of the same type can skip
/// exploration and the LLM can produce a working command directly from the
/// skill + new question.
///
/// Returns None if no `<<<SKILL>>>` marker is present, or if the body is empty.
pub fn extract_skill_marker(resp: &str) -> Option<String> {
    const START_TAG: &str = "<<<SKILL";
    const END_TAG: &str = "<<<END>>>";

    let start_idx = resp.find(START_TAG)?;
    let after_start = &resp[start_idx..];
    let tag_close = after_start.find(">>>")?;
    let body_start = start_idx + tag_close + 3;
    let remaining = &resp[body_start..];
    let end_offset = remaining.find(END_TAG)?;
    let body = remaining[..end_offset].trim();
    if body.is_empty() {
        return None;
    }
    Some(body.to_string())
}

/// Extract a `TOKEN: <value>` from sandbox output (engineering-fix fast path).
/// The judger expects `{"token": "<value>"}`, so wrap bare values.
fn extract_token(output: &str) -> Option<String> {
    for line in output.lines().rev() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("TOKEN:") {
            let answer = rest.trim();
            if !answer.is_empty() {
                if serde_json::from_str::<serde_json::Value>(answer).is_ok() {
                    return Some(answer.to_string());
                }
                return Some(format!("{{\"token\": \"{}\"}}", answer));
            }
        }
    }
    None
}

/// Find the answer from sandbox output: only `TOKEN:` markers for
/// engineering-fix tasks. `ANSWER:`/`答案:` markers are no longer extracted
/// from sandbox output — the LLM formats the answer via `<<<ANSWER>>>` after
/// seeing the sandbox results.
pub fn extract_answer(output: &str) -> Option<String> {
    extract_token(output)
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
/// A real result that merely CONTAINS one of these words — a path, a JSON
/// field, a line of prose — is left alone; the match is on a whole value, and
/// for a JSON answer it is on every leaf of the object (issue #28's
/// `{"result":"unknown"}` / `{"status":"pending"}` are sentinels too, they just
/// arrived wrapped).
pub fn is_failure_answer(answer: &str) -> bool {
    let trimmed = answer
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == '。' || c == '.')
        .trim();
    if trimmed.is_empty() {
        return true;
    }
    if is_sentinel(trimmed) {
        return true;
    }
    // A COMPOUND placeholder is still a placeholder (issue #111). Session 1
    // printed `ANSWER: TOKEN_PENDING_MANUAL_REVIEW` and the whole-value test
    // above only ever matched a bare token, so the model's own "not ready yet"
    // marker went to the judger three rounds running — errorCode 2 each time —
    // and into `best_answer`, which is what the deadline guard submits.
    if is_placeholder_compound(trimmed) {
        return true;
    }
    // A SENTENCE is not a value (issues #112 and #114). #112's session 1
    // answered `，形式：` and #114's session 6 answered `0写成"0"，否则算错` —
    // both four to eleven characters of the task file's own prose, lifted
    // because the model's script echoed a line of the instructions through the
    // `ANSWER:` marker. Neither is a number, an identifier or a name, and both
    // scored zero three times over.
    //
    // Only a BARE scalar can be prose. An answer that arrived as a JSON
    // STRING is a value whatever it says — `"南京市，江苏省"` is a city, and the
    // quote-trim above exists so that `"N/A"` still reads as the sentinel it
    // is — so the rule is applied to the trimmed text only when the answer as
    // it arrived was not a quoted value.
    let quoted = serde_json::from_str::<serde_json::Value>(answer.trim())
        .map(|value| !value.is_object() && !value.is_array())
        .unwrap_or(false);
    if !quoted && is_prose_fragment(trimmed) {
        return true;
    }
    // A sentinel WRAPPED IN JSON is still a sentinel. Issue #28's sessions
    // submitted `{"result": "unknown"}` and `{"status": "pending"}` — the
    // model's error path dressed as an answer — and both reached the judger,
    // because the whole-answer test above only ever matched a bare token. The
    // shape is the model's own formatting, so the values are what carry the
    // meaning: an object whose every leaf is a sentinel answers nothing, and
    // submitting it can only score zero while burning the round a real attempt
    // needed. An object with no leaves at all (`{}`) is the same nothing.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let leaves = scalar_leaves(&value);
        if leaves.is_empty() && value.is_object() {
            return true;
        }
        if !leaves.is_empty() && leaves.iter().all(|leaf| is_sentinel(leaf)) {
            return true;
        }
        // A TEMPLATE IS NOT AN ANSWER, and it does not have to be all
        // placeholders to be one: the task file's own example is the shape the
        // answer must take, with one field filled in and the rest still in
        // angle brackets.
        if leaves.iter().any(|leaf| is_template_slot(leaf)) {
            return true;
        }
        // NOTE: Heritage-query zero-response checks (`total_count: 0`, etc.)
        // were removed. Under the new <<<ANSWER>>> protocol, the LLM sees
        // sandbox output before formatting the answer, so it can distinguish
        // API failures from real results. The judger's rejection feedback
        // handles incorrect answers — hardcoding field names here was not
        // generalizable across different task types.
    }
    if is_template_slot(trimmed) {
        return true;
    }
    if is_shape_mismatch(trimmed) {
        return true;
    }
    false
}

/// Detect answers that are raw API responses instead of aggregated results.
///
/// pk616181/pk615723: the LLM queried an API returning an array of records,
/// then submitted the RAW array as the answer. The judger expected a single
/// aggregated value (count, sum, or filtered subset). This function catches
/// the most common shape mismatches:
///
/// * A JSON array of objects with many fields — raw API responses carry
///   5+ fields per record; an aggregated answer is a scalar or a small
///   object with 1-3 fields.
/// * A very large JSON object with many fields — a raw API response object
///   rather than the extracted answer.
///
/// This is deliberately conservative: a JSON array of scalars (e.g. a list
/// of names) is NOT a mismatch, and a small object (e.g. `{"count": 42}`)
/// is NOT a mismatch.
fn is_shape_mismatch(answer: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(answer.trim()) else {
        return false;
    };
    match &value {
        // A raw API response is an array of objects, each with many fields.
        // An answer that is a list of scalars (strings, numbers) is fine.
        serde_json::Value::Array(arr) if !arr.is_empty() => {
            let all_objects = arr.iter().all(|v| v.is_object());
            if !all_objects {
                return false;
            }
            // Check the first element's field count: raw API records have
            // many fields; an aggregated answer object has few.
            if let Some(first) = arr.first() {
                if let Some(obj) = first.as_object() {
                    // 5+ fields in each array element = likely raw API response
                    return obj.len() >= 5;
                }
            }
            false
        }
        // A very large object with many top-level fields could be a raw API
        // response. But be conservative: only flag 10+ fields, since some
        // legitimate answers may have several fields.
        serde_json::Value::Object(obj) => obj.len() >= 10,
        _ => false,
    }
}

/// Is this value still wearing the task file's angle brackets?
///
/// The task files ship an EXAMPLE answer, and four matches in a row submitted
/// it verbatim instead of computing anything: 表 4a of #201's first submission
/// is 108 characters and `task_ended.bestAnswer` spells them out —
/// `{"city":"北京","oldest_era":"<年代最早的遗产名称>","total_count":"<总记录条数>",
/// "world_heritage_count":"<保护级别为\"世界遗产\"的数量>"}` — while #203 submitted
/// the same object twice and #205's first submission was the API document's
/// sample response. `ANSWER:` is a marker for a RESULT, and `<...>` is the
/// task's own notation for "put the value here": the prompt's pre-submit
/// self-check already says so in as many words ("ANSWER 的值里有没有 `<...>`
/// 占位符…有 = 0 分"), and 任务书 ch.6 scores `回答正确字段个数 / 全量字段个数`,
/// so a template scores the fields that were already filled in and nothing
/// else — while costing the rounds and the submissions the session needed to
/// compute the rest. Rejecting it re-plans instead, which is the whole
/// difference between the sandbox being asked again and the template being
/// banked as the best answer the deadline guard will resubmit.
fn is_template_slot(text: &str) -> bool {
    let trimmed = text
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim();
    trimmed.len() >= 3
        && trimmed.starts_with('<')
        && trimmed.ends_with('>')
        && !trimmed[1..trimmed.len() - 1].trim().is_empty()
}

/// Words that only ever appear inside a PLACEHOLDER — never in a result on
/// their own. Read by [`is_placeholder_compound`] alone: a single word from
/// this list is not a sentinel (`manual`, `status` and `value` are all
/// perfectly good answers to some task), but a whole answer assembled out of
/// them is a shape rather than a value.
const PLACEHOLDER_WORDS: [&str; 20] = [
    "token",
    "manual",
    "review",
    "value",
    "answer",
    "result",
    "status",
    "flag",
    "auto",
    "placeholder",
    "field",
    "content",
    "string",
    "text",
    "body",
    "retry",
    "waiting",
    "empty",
    "blank",
    "fill",
];

/// Is the answer a placeholder assembled out of placeholder words — the shape
/// `TOKEN_PENDING_MANUAL_REVIEW` takes?
///
/// The rule is deliberately narrow in three directions at once, because the
/// existing whole-value test is what keeps a real result alive and this must
/// not undo it: the answer must be a single unspaced token of at most 64
/// characters, it must split on a separator into at least two parts, and
/// **every** part must be a sentinel or a placeholder word. "Every", not
/// "any", is what leaves `failed_to_extract.log` (it has `to` and `log`),
/// `error_count=7` (it has `count=7`) and any real `snake_case` identifier
/// (`world_heritage_count`, `task_1_alpha`) alone.
fn is_placeholder_compound(text: &str) -> bool {
    if text.chars().count() > 64 || text.chars().any(char::is_whitespace) {
        return false;
    }
    let parts: Vec<String> = text
        .split(['_', '-', '.', '/'])
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|part| is_sentinel(part) || PLACEHOLDER_WORDS.contains(&part.as_str()))
}

/// Sentence punctuation from the task file's own prose.
///
/// A VALUE never carries it. Everything these tasks ask for — a count, an era,
/// a city name, a token — is a number, an identifier or a short name, and a
/// scalar wearing a comma or a full stop is a sentence the model copied out of
/// the instructions. The brackets that legitimately appear inside Chinese
/// titles (`《》`), and the colon that separates a field name from its value,
/// are deliberately NOT in this set.
const PROSE_PUNCTUATION: [char; 10] = ['，', '。', '；', '！', '？', '、', '"', '"', '‘', '’'];

/// Is the answer a fragment of prose rather than a value?
///
/// Gated on "not JSON at all", so a quoted Chinese string
/// (`"南京市，江苏省"`) and any JSON object or array are untouched: the only
/// thing this can reject is a bare scalar, which is exactly what `，形式：`
/// (issue #112) and `0写成"0"，否则算错` (issue #114) were.
fn is_prose_fragment(text: &str) -> bool {
    if serde_json::from_str::<serde_json::Value>(text).is_ok() {
        return false;
    }
    text.chars().any(|c| PROSE_PUNCTUATION.contains(&c))
}

/// Is this whole string one of the not-an-answer tokens? Compared case
/// insensitively and only as a whole value: a real result that merely contains
/// one of these words — a path, a JSON field, a line of prose — is not a
/// sentinel.
fn is_sentinel(text: &str) -> bool {
    const SENTINELS: [&str; 48] = [
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
        "null",
        "none",
        "nan",
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
        "no_data",
        "nodata",
        "not_found",
        "notfound",
        "unavailable",
        // A run that has not finished is not a result. `{"status":"pending"}`
        // (issue #28) is the model saying "ask me again", and the judger scores
        // it exactly like the empty answer it is.
        "pending",
        "processing",
        "running",
        "in_progress",
        "inprogress",
        "not_ready",
        "无",
        "空",
        "未知",
        "暂无",
        "待补充",
        "未完成",
        "待定",
        "提取失败",
    ];
    let lower = text.trim().to_lowercase();
    // An EMPTY value is not a value either. `{"token": ""}` is issue #115's
    // one and only answer across six sessions: the script reached the sandbox,
    // parsed the task file, and printed the right field name with nothing in
    // it. 任务书 ch.6 counts correct fields, and an empty one is not correct.
    if lower.is_empty() {
        return true;
    }
    SENTINELS.iter().any(|sentinel| lower == *sentinel)
}

/// Every scalar leaf of a JSON value, rendered as the text a sentinel test can
/// read. Objects and arrays are descended into; an empty container contributes
/// nothing.
fn scalar_leaves(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(text) => vec![text.clone()],
        serde_json::Value::Number(number) => vec![number.to_string()],
        serde_json::Value::Bool(flag) => vec![flag.to_string()],
        serde_json::Value::Array(items) => items.iter().flat_map(scalar_leaves).collect(),
        serde_json::Value::Object(map) => map.values().flat_map(scalar_leaves).collect(),
        serde_json::Value::Null => vec!["null".to_string()],
    }
}

/// Return the banked best answer for the deadline guard, or None when it is a
/// sentinel/meta-answer that must never be submitted.
pub fn partial_answer(state: &BotState) -> Option<String> {
    if state.task.best_answer.is_empty()
        || is_meta_answer(&state.task.best_answer)
        || is_failure_answer(&state.task.best_answer)
    {
        return None;
    }
    Some(state.task.best_answer.clone())
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

/// `task_answer_submit.answer` 的字符上限。
///
/// 这个字段是交给判题器的**原始字节**，也是任务 0 分唯一的现场证据——判题器回
/// `MissingNamedInput` 时，缺的是哪个字段只能从这段原文里看出来。上一版把它截到
/// 160 字符，等于把最有用的证据自己扔了。上限只用于挡住异常巨大的 LLM 回包
/// （一场所提交的次数是个位数，正常答案几十到几百字符）。
pub const ANSWER_LOG_CAP: usize = 4000;

/// 打进日志的答案：全文（上限 [`ANSWER_LOG_CAP`] 字符）+ **真实**字符数。
///
/// 长度单独给出，所以任何被上限截断的答案都能被认出来
/// （`answer.chars().count() < chars` 即截断），不会被误当成判题器收到的原文。
pub fn answer_for_log(payload: &str) -> (String, usize) {
    (truncate(payload, ANSWER_LOG_CAP), payload.chars().count())
}

/// The exact bytes handed to the judger: always a JSON document.
///
/// THE JUDGER PARSES EVERY SUBMISSION AS JSON. Its verdict for one that does
/// not parse is `答案不是合法 JSON`, and it says so before comparing a single
/// field — 表 4b carries that line in all four of issues #201/#203/#204/#205,
/// and 表 4a shows the submission it answered was `unwrapped` in every one of
/// them: the shape ladder had reduced `{"token": "fc1e78eb2a5a"}` to the bare
/// text `fc1e78eb2a5a`, which is a string in no language but the shell's, and
/// the session spent a round and a submission learning nothing. 任务书 ch.6
/// scores `回答正确字段个数 / 全量字段个数` and 接口文档 §executeCmd has the
/// judger hand-parse the payload, so a bare string is the ONE shape that can
/// never score: quoting it is the same value in the only notation the
/// comparison reads, and it leaves the ladder's other shape (the one-key
/// object the retry flips to) exactly where it was.
///
/// A payload that already parses is passed through byte for byte — including
/// the bare number `15`, which is legal JSON — so nothing that works today
/// changes.
pub fn answer_wire_payload(payload: &str) -> String {
    if serde_json::from_str::<serde_json::Value>(payload).is_ok() {
        return payload.to_string();
    }
    serde_json::to_string(&serde_json::Value::String(payload.trim().to_string()))
        .unwrap_or_else(|_| payload.to_string())
}

/// Accept or walk toward the first valid task point.
///
/// Moved out of `day.rs` by issue #221 phase 5c: the whole task lane already
/// lives here — the session, the lanes, the prompt loop, the refusals — and
/// the accept decision is the lane's front door, not the scheduler's.
impl BotState {
    pub fn next_task_point(
        &mut self,
        turn: &Turn,
        pioneer: &Unit,
        claimed: &mut HashSet<Pos>,
    ) -> Option<RoleCommand> {
        let candidate = turn
            .player_tasks
            .iter()
            .filter(|task| task.is_valid && task.cooldown_rounds == 0)
            // A point whose execution window the judger already shut stays
            // shut for this pioneer: re-accepting it only re-enters the same
            // refusal. See `BotState::task_refusals`.
            .filter(|task| {
                !self
                    .task_refusals
                    .get(&task.pos)
                    .map_or(false, |until| turn.round_no <= *until)
            })
            // P1-4: the kind outranks the distance. 推理 + 传闻 first, 自进化
            // next, everything else last; distance only decides inside a lane.
            // Until this existed the pioneer took whatever point was nearest,
            // which on a board carrying two kinds is a coin toss between the
            // day's reasoning and its sandbox. The kind is also stamped on the
            // session, so the log says which lane the rounds went to.
            .min_by_key(|task| {
                (
                    classify(&task.task_type).rank(),
                    chebyshev(pioneer.pos, task.pos),
                    task.pos.x,
                    task.pos.y,
                )
            })?;
        if chebyshev(pioneer.pos, candidate.pos) <= 1 {
            // The clock gates the ACCEPT, never the approach: a point accepted
            // with fewer rounds left than a session needs is a point sold for
            // its 30-round cooldown (see `TASK_MIN_ATTEMPT_ROUNDS`), but
            // walking toward a point is free and the morning is the only time
            // the pioneer has. Measured from where the pioneer IS, so standing
            // out at the point already costs the walk home — which is the same
            // deadline the recall will enforce.
            let recall = crate::brain::role::pioneer::recall_round(turn, pioneer);
            if turn.in_day_round + TASK_MIN_ATTEMPT_ROUNDS > recall {
                crate::log::event(
                    "task_accept_deferred",
                    serde_json::json!({
                        "round": turn.round_no,
                        "dayRound": turn.in_day_round,
                        "point": candidate.pos,
                        "recall": recall,
                    }),
                );
                return None;
            }
            let timeout = if candidate.timeout_rounds > 0 {
                candidate.timeout_rounds
            } else {
                250
            };
            self.task_session_seq = self.task_session_seq.saturating_add(1);
            let session_id = self.task_session_seq;
            let kind = classify(&candidate.task_type);
            crate::log::event(
                "task_accept",
                serde_json::json!({
                    "round": turn.round_no,
                    "session": session_id,
                    "point": candidate.pos,
                    "taskType": candidate.task_type,
                    "task_kind": kind.as_str(),
                    "kindRank": kind.rank(),
                }),
            );
            self.task = TaskSession {
                active: true,
                session_id,
                accepted_round: turn.round_no,
                timeout_round: turn.round_no + timeout,
                point: Some(candidate.pos),
                task_type: candidate.task_type.clone(),
                kind,
                ..Default::default()
            };
            return Some(RoleCommand::accept_task());
        }
        let stands = stand_cells(turn, candidate.pos);
        walk_toward(turn, pioneer, &stands, claimed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{TreasurePhase, TreasurePlan};

    /// A live self-evolution session with a comfortable timeout, in Planning.
    fn session() -> BotState {
        let mut state = BotState::default();
        state.task.active = true;
        state.task.session_id = 7;
        state.task.accepted_round = 10;
        state.task.timeout_round = 200;
        state.task.kind = TaskKind::SelfEvolution;
        state.task.task_type = "自进化类1".to_string();
        state.task.description = "Read task_7_beta.md and answer the query".to_string();
        state.task.stage = TaskStage::Planning;
        state
    }

    /// A minimal board at `round_no` with a station and pioneer id 1.
    fn turn_at(round_no: i64) -> Turn {
        let payload = serde_json::json!({
            "roundNo": round_no,
            "mapInfo": {"width": 41, "height": 32, "zones": []},
            "teamOur": {
                "type": "challenger", "goldNum": 0, "totalScore": 0, "playerTasks": [],
                "roles": [
                    {"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                     "health": 1000, "level": 1, "backPackCapability": 100, "backpack": []},
                    {"id": 1, "pos": {"x": 12, "y": 20}, "roleType": "pioneer",
                     "health": 1000, "level": 1, "backPackCapability": 100, "backpack": []}
                ]
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
        });
        let req: crate::protocol::Request =
            serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    // -- the tag protocol (issue #221 comment 2, 改进40) -------------------

    #[test]
    fn command_tag_is_the_protocol() {
        let resp = "thinking out loud\n<<<COMMAND>>>\ncurl -v http://x/api\n<<<END>>>\ndone";
        assert_eq!(
            extract_command(resp).as_deref(),
            Some("curl -v http://x/api")
        );
    }

    #[test]
    fn multiple_command_blocks_join_into_one_shell_line() {
        // 改进40: every block runs regardless of the previous one's exit code.
        let resp = "<<<COMMAND>>>\nls /tmp\n<<<END>>>\nbetween\n<<<COMMAND>>>\ncat /tmp/a.md\n<<<END>>>";
        assert_eq!(
            extract_command(resp).as_deref(),
            Some("ls /tmp ; cat /tmp/a.md")
        );
    }

    #[test]
    fn command_lang_variant_wraps_python_heredoc() {
        let cmd = extract_command("<<<COMMAND:python>>>\nprint(1)\n<<<END>>>")
            .expect("python variant extracts");
        assert!(cmd.starts_with("python3 - <<'PYEOF'"), "{cmd}");
        assert!(cmd.contains("print(1)"), "{cmd}");
        assert!(cmd.ends_with("PYEOF"), "{cmd}");
    }

    #[test]
    fn script_tag_is_no_longer_the_protocol() {
        // 改进40 renamed the tag SCRIPT → COMMAND; the in-flight phase-6 diff
        // taught <<<SCRIPT>>>, and that spelling must not be re-adopted.
        assert_eq!(extract_command("<<<SCRIPT>>>\nls /tmp\n<<<END>>>"), None);
    }

    #[test]
    fn fenced_block_remains_the_priority_two_fallback() {
        // The reference keeps fenced extraction for responses that ignore the
        // markers; removing it would waste a round on every such response.
        assert_eq!(
            extract_command("Here you go:\n```bash\necho hi\n```\n").as_deref(),
            Some("echo hi")
        );
    }

    #[test]
    fn answer_and_skill_markers_require_the_end_tag() {
        assert_eq!(
            extract_answer_marker("<<<ANSWER>>>\n{\"a\":1}\n<<<END>>>").as_deref(),
            Some("{\"a\":1}")
        );
        // No <<<END>>> → no answer: the body would run to the end of the
        // response and swallow whatever the model wrote after it.
        assert_eq!(extract_answer_marker("<<<ANSWER>>>\n{\"a\":1}"), None);
        assert_eq!(
            extract_skill_marker("<<<SKILL>>>\nCOMMAND: curl x\n<<<END>>>").as_deref(),
            Some("COMMAND: curl x")
        );
        assert_eq!(extract_skill_marker("no markers here"), None);
    }

    // -- the session state machine ------------------------------------------

    #[test]
    fn llm_resp_answer_mode_banks_answer_skill_and_last_command() {
        let mut state = session();
        state.task.cmd_history.push("curl final".to_string());
        state.task.no_answer_rounds = 3;
        let resp = "<<<ANSWER>>>\n{\"city\":\"北京\"}\n<<<END>>>\n\
                    <<<SKILL>>>\nCOMMAND: curl final\nANSWER: city=名称\nPATHS: /tmp/x\nGOTCHA: location= not city=\n<<<END>>>";
        on_llm_resp(&mut state, resp);
        assert_eq!(state.task.best_answer, "{\"city\":\"北京\"}");
        assert_eq!(state.task.no_answer_rounds, 0);
        assert_eq!(state.task.sop_cmd.as_deref(), Some("curl final"));
        let skill = state.task.sop_skill.expect("skill stashed for the SOP cache");
        assert!(skill.contains("GOTCHA"), "{skill}");
        assert!(
            matches!(state.task.stage, TaskStage::HaveAnswer { .. }),
            "{:?}",
            state.task.stage
        );
    }

    #[test]
    fn llm_resp_command_mode_stages_the_command() {
        let mut state = session();
        on_llm_resp(&mut state, "<<<COMMAND>>>\nls /tmp\n<<<END>>>");
        match &state.task.stage {
            TaskStage::HavePlan { cmd } => assert_eq!(cmd, "ls /tmp"),
            other => panic!("expected HavePlan, got {other:?}"),
        }
    }

    #[test]
    fn cmd_result_token_fast_path_answers_without_another_llm_round() {
        let mut state = session();
        state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
        state.task.cmd_history.push("./check".to_string());
        on_cmd_result(&mut state, "[exitCode:0]\nTOKEN: fc1e78\n");
        match &state.task.stage {
            TaskStage::HaveAnswer { answer } => {
                assert_eq!(answer, "{\"token\": \"fc1e78\"}")
            }
            other => panic!("expected HaveAnswer, got {other:?}"),
        }
        let skill = state
            .task
            .sop_skill
            .expect("engineering-fix skill auto-generated");
        assert!(skill.contains("Engineering-fix"), "{skill}");
        assert_eq!(state.task.sop_cmd.as_deref(), Some("./check"));
        assert_eq!(state.task.no_answer_rounds, 0);
    }

    #[test]
    fn cmd_result_no_longer_scans_answer_lines() {
        // The old `ANSWER:`/`答案:` stdout scan forced the model to know the
        // answer format BEFORE running the query; the reference deleted it —
        // the LLM formats the answer via <<<ANSWER>>> after seeing the data.
        let mut state = session();
        state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
        on_cmd_result(&mut state, "[exitCode:0]\nANSWER: 42\n");
        assert!(
            matches!(state.task.stage, TaskStage::Planning),
            "{:?}",
            state.task.stage
        );
        assert_eq!(state.task.no_answer_rounds, 1);
        assert!(state.task.best_answer.is_empty());
    }

    #[test]
    fn auto_find_script_locates_the_task_file_and_its_type() {
        let script = auto_find_script("请读取 task_7_beta.md 并回答其中的问题")
            .expect("description names a task file");
        assert!(script.contains("=== TASK FILES ==="), "{script}");
        assert!(script.contains("task_7_beta.md"), "{script}");
        // 改进18: the type token drives the fuzzy same-type search.
        assert!(script.contains("=== SAME-TYPE TASK FILES (fuzzy) ==="), "{script}");
        assert!(script.contains("task_*beta*"), "{script}");
        assert!(script.contains("=== WORKSPACE DIRS ==="), "{script}");
        assert!(auto_find_script("nothing to locate here").is_none());
    }

    #[test]
    fn result_history_is_not_truncated_at_the_old_cap() {
        // RESULT_CONTEXT_LAST went 3000 → 100_000 ("never truncate"): pk615723
        // R36 lost the key fields of a 3.4k API response to the old cap.
        let mut state = session();
        state.task.stage = TaskStage::WaitingCmdResult { attempts: 0 };
        let body = "x".repeat(4000);
        on_cmd_result(&mut state, &format!("[exitCode:0]\n{body}\n"));
        let stored = state.task.result_history.last().expect("stored");
        assert!(
            stored.chars().count() >= 4000,
            "old cap truncated to {} chars",
            stored.chars().count()
        );
    }

    // -- plan_pioneer lanes ---------------------------------------------------

    #[test]
    fn have_plan_fires_the_command_in_the_same_round() {
        // The reference removed the one-round llmResp hold: the judger accepts
        // executeCmd in the same round as llmResp, and a saved round matters.
        let mut state = session();
        state.task.stage = TaskStage::HavePlan {
            cmd: "ls /tmp".to_string(),
        };
        let turn = turn_at(20);
        let mut plan = Plan::default();
        let pioneer = turn.role_by_id(1).expect("pioneer on the board");
        let issued = plan_pioneer(&turn, &mut state, pioneer, &mut plan);
        assert!(issued.is_none());
        assert_eq!(plan.execute_cmd.as_deref(), Some("ls /tmp"));
        assert!(
            matches!(state.task.stage, TaskStage::WaitingCmdResult { attempts: 0 }),
            "{:?}",
            state.task.stage
        );
        assert_eq!(state.task.cmd_history, vec!["ls /tmp".to_string()]);
        assert_eq!(state.task.cmd_request_round, Some(20));
    }

    #[test]
    fn rumor_lane_is_answered_by_the_treasure_not_the_llm() {
        let mut state = session();
        state.task.kind = TaskKind::Rumor;
        let turn = turn_at(20);
        let mut plan = Plan::default();
        let pioneer = turn.role_by_id(1).expect("pioneer on the board");
        // Before the summon: wait silently — no prompt, no command.
        plan_pioneer(&turn, &mut state, pioneer, &mut plan);
        assert!(plan.prompt.is_none(), "rumor tasks never spend an LLM ask");
        assert!(plan.execute_cmd.is_none());
        assert!(matches!(state.task.stage, TaskStage::Planning));
        // Once the treasure is summoned, the altar plan IS the answer.
        state.treasure.plan = Some(TreasurePlan {
            pos: Pos { x: 5, y: 6 },
            items: vec!["Medicine".to_string()],
            open_day: 3,
        });
        state.treasure.phase = TreasurePhase::Summoned { round: 19 };
        plan_pioneer(&turn, &mut state, pioneer, &mut plan);
        assert!(
            matches!(state.task.stage, TaskStage::HaveAnswer { .. }),
            "{:?}",
            state.task.stage
        );
        assert!(
            state.task.best_answer.contains("\"open_day\":3"),
            "{}",
            state.task.best_answer
        );
    }

    #[test]
    fn timeout_guard_skips_an_answer_already_submitted() {
        // pk-621639: re-submitting resets the closure probes, so the deadline
        // guard must not fire once submitted_round is set.
        let mut state = session();
        state.task.timeout_round = 21;
        state.task.best_answer = "{\"count\": 8}".to_string();
        state.task.submitted_round = Some(15);
        state.task.stage = TaskStage::WaitingSubmit { attempts: 0 };
        let turn = turn_at(20); // rounds_left = 1
        let mut plan = Plan::default();
        let pioneer = turn.role_by_id(1).expect("pioneer on the board");
        let issued = plan_pioneer(&turn, &mut state, pioneer, &mut plan);
        assert!(issued.is_none(), "no re-submission after the first");
        assert_eq!(state.task.submitted_round, Some(15));
        assert!(
            matches!(state.task.stage, TaskStage::WaitingSubmit { attempts: 1 }),
            "{:?}",
            state.task.stage
        );
    }

    // -- build_prompt ---------------------------------------------------------

    #[test]
    fn prompt_teaches_the_command_protocol() {
        let state = session();
        let prompt = build_prompt(&state, &turn_at(20));
        assert!(prompt.contains("<<<COMMAND>>>"), "command mode example");
        assert!(prompt.contains("<<<ANSWER>>>"), "answer mode example");
        assert!(prompt.contains("<<<SKILL>>>"), "skill is mandatory");
        assert!(prompt.contains("X-API-Key"), "dual auth rule");
        assert!(prompt.contains("Authorization: Bearer"), "dual auth rule");
        assert!(
            !prompt.contains("<<<SCRIPT>>>"),
            "the renamed tag must not be taught anymore"
        );
    }

    #[test]
    fn prompt_wraps_prior_context_in_sandbox_output_tags() {
        let mut state = session();
        state
            .task
            .result_history
            .push("[exitCode:0]\n{\"count\": 8}".to_string());
        let prompt = build_prompt(&state, &turn_at(20));
        assert!(
            prompt.contains("<<<SANDBOX-OUTPUT>>>"),
            "history is fed back wrapped, per loop step 4"
        );
        assert!(prompt.contains("{\"count\": 8}"));
    }

    #[test]
    fn prompt_replays_the_cached_skill_and_full_command_sequence() {
        let mut state = session();
        state.task.sop_used_template = Some("curl tpl".to_string());
        state.task.sop_reuse_script = Some("curl tpl".to_string());
        state.task.sop_reuse_skill = Some("COMMAND: curl tpl\nGOTCHA: x".to_string());
        state.task.sop_reuse_history = vec!["find /tmp".to_string(), "curl tpl".to_string()];
        let prompt = build_prompt(&state, &turn_at(20));
        assert!(prompt.contains("### Skill Summary"), "{prompt}");
        assert!(prompt.contains("GOTCHA: x"));
        assert!(prompt.contains("### Full Command Sequence"), "改进1");
        assert!(prompt.contains("**Step 2**"));
        assert!(prompt.contains("verified correct by the judger"));
    }

    #[test]
    fn prompt_stall_pressure_demands_an_answer() {
        let mut state = session();
        state.task.no_answer_rounds = 2;
        let prompt = build_prompt(&state, &turn_at(20));
        assert!(
            prompt.contains("MUST return `<<<ANSWER>>>`"),
            "stalled sessions are pushed to submit"
        );
    }
}
