//! Self-evolution task loop:
//! acceptTask → (phaseTask arrives) → prompt LLM for a sandbox script →
//! executeCmd → parse lastCmdResult → submitAnswer → iterate on error 2.
//! While a task is active the pioneer MUST stay within 1 cell of the task
//! point, so this module never emits movement.

use crate::brain::Plan;
use crate::model::{Turn, Unit};
use crate::protocol::RoleCommand;
use crate::state::{BotState, PromptPurpose, TaskStage};

/// Consecutive "`executeCmd` is not available" verdicts that end a session.
const MAX_WINDOW_ERRORS: i32 = 2;

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
pub fn on_llm_resp(state: &mut BotState, resp: &str) {
    if !matches!(state.task.stage, TaskStage::Planning) {
        return;
    }
    if let Some(cmd) = extract_command(resp) {
        crate::log::event(
            "task_llm_resp",
            serde_json::json!({
                "session": state.task.session_id,
                "ok": true,
                "cmdHead": crate::log::brief(&cmd, 300),
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
    crate::log::event(
        "task_cmd_result",
        serde_json::json!({
            "session": state.task.session_id,
            "exit": exit_code(result),
            "resultHead": crate::log::brief(strip_status_line(result), 300),
        }),
    );
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
    state.task.result_history.push(truncate(result, RESULT_CONTEXT_LAST));
    const RESULT_HISTORY_KEEP: usize = 8;
    let excess = state.task.result_history.len().saturating_sub(RESULT_HISTORY_KEEP);
    if excess > 0 {
        state.task.result_history.drain(..excess);
    }
    let output = strip_status_line(result);
    let code = exit_code(result);
    if let Some(answer) = extract_answer(output) {
        if is_meta_answer(&answer) {
            // A meta-description of the parsing step ({"status":"parsed",...})
            // is not the task's result: treat it as a failed run and re-plan.
            crate::log::event("task_answer_meta", serde_json::json!({"exit": code}));
            state.task.stage = TaskStage::Planning;
            state.task.no_answer_rounds = state.task.no_answer_rounds.saturating_add(1);
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
            state.task.no_answer_rounds = state.task.no_answer_rounds.saturating_add(1);
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
        state.task.no_answer_rounds = 0;
        state.task.stage = TaskStage::HaveAnswer { answer };
    } else {
        // No answer found: ask the LLM again with the failure context
        // (result_history carries it into the next prompt).
        //
        // THE COMMAND DID NOT FAIL. `task_cmd_failed` is the historical name
        // and it has cost this analysis two batches: the field is printed as
        // `{"exit":0,"timeout":false}` in every issue — the sandbox ran the
        // script to completion and the script simply never printed an
        // `ANSWER:` line — and that reads exactly like a broken sandbox. The
        // new `reason` says which of the three it was, and the new `head`
        // carries the first line of what the script DID print, because
        // `cmd_result` recorded only a character count and the mechanism was
        // therefore unreadable from the log (issues #111-#115, 8-13 each).
        state.task.no_answer_rounds = state.task.no_answer_rounds.saturating_add(1);
        crate::log::event(
            "task_cmd_failed",
            serde_json::json!({
                "exit": code,
                "timeout": result.starts_with("[TIMEOUT]"),
                "reason": if result.starts_with("[TIMEOUT]") {
                    "timeout"
                } else if code.map_or(false, |code| code != 0) {
                    "exit_nonzero"
                } else {
                    "no_answer_marker"
                },
                "streak": state.task.no_answer_rounds,
                "head": crate::log::headline(strip_status_line(result), 160),
            }),
        );
        state.task.stage = TaskStage::Planning;
    }
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
        "find /tmp/selfEvolutionTask -type f -name 'check' -exec chmod +x {} + 2>/dev/null || true\n",
        "find /tmp/selfEvolutionTask -type f \\( -name 'check' -o -name '*.sh' \\) ",
        "-exec sed -i 's/\\r$//' {} + 2>/dev/null || true\n",
        "# --- end prelude ---\n",
    )
}

/// The exact bytes that go to the sandbox for a script the task loop produced.
/// Split out so the prelude can be asserted on the wire without a whole match.
pub fn sandbox_command(cmd: &str) -> String {
    format!("{}{}", sandbox_prelude(), cmd)
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
            // SOP fast path: a cached script from a similar task is given to
            // the LLM as reference. The LLM adapts it to this task instead of
            // re-reading the task file and re-exploring from scratch.
            if state.task.sop_reuse_script.is_none() && state.task.cmd_history.is_empty() {
                if let Some(template) = state.find_sop(&state.task.task_type, &state.task.description) {
                    state.task.sop_used_template = Some(template.clone());
                    state.task.sop_reuse_script = Some(template);
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
                let has_results = !state.task.result_history.is_empty();
                let has_rejection = !state.task.rejection_feedback.is_empty();
                let phase = if has_rejection {
                    "fix"
                } else if has_results {
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
                        "hasResults": has_results,
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
const RESULT_CONTEXT_LAST: usize = 3000;
const RESULT_CONTEXT_PREVIOUS: usize = 1200;

pub fn build_prompt(state: &BotState, turn: &Turn) -> String {
    let mut prompt = String::new();

    // ---- LLM-driven task loop ----
    //
    // The flow is a multi-round conversation:
    //   Round N:   we send all context (task description + all prior results)
    //              → LLM returns a script → we execute it in the sandbox.
    //   Round N+1: judger returns lastCmdResult → we feed it back to LLM →
    //              LLM decides what to do next → executeCmd → ...
    //   Until an ANSWER is produced, then we submit.
    //
    // There are NO fixed phases (read/solve/fix). The LLM directs its own
    // actions: it may read files, read API docs, query APIs, run check
    // scripts, compute answers, or fix rejected submissions — in whatever
    // order it decides. The prompt presents all available context and asks
    // "what do you want to do next?"
    //
    // pk616182 post-mortem: the old 3-phase system forced the LLM to read the
    // task file in round 1, then immediately compute in round 2. But the
    // Beijing task requires reading API_DOCS.md AFTER reading the task file
    // to learn the API endpoints — the LLM had to guess endpoints and got
    // `total_count: 0`. The fixed phases cannot express this 3-step flow.

    let has_rejection = !state.task.rejection_feedback.is_empty();
    let left = state.task.timeout_round.saturating_sub(turn.round_no);

    prompt.push_str("Please write a shell or python script for me to execute in a sandbox environment.\n");
    prompt.push_str("Sandbox: shell and python3 available, no internet access.\n");
    prompt.push_str("Output all script comments, variable names, and text in English to avoid encoding issues.\n\n");

    // ---- Task description ----
    prompt.push_str("## Task Description\n\n");
    prompt.push_str(&state.task.description);
    prompt.push_str("\n\n");

    // ---- SOP reuse: cached script from a similar task ----
    if let Some(script) = &state.task.sop_reuse_script {
        prompt.push_str("## Reference Script\n\n");
        prompt.push_str("I previously solved a similar task with this script:\n\n");
        prompt.push_str("```\n");
        prompt.push_str(script);
        prompt.push_str("\n```\n");
        prompt.push_str("You may reference its structure and logic, but write a new script for this task.\n\n");
    }

    // ---- Rejection feedback (if the judger rejected a previous answer) ----
    if has_rejection {
        prompt.push_str("## Judger Feedback\n\n");
        if state.task.rejections > 0 {
            prompt.push_str(&format!(
                "The previous answer was rejected {} time(s). Last answer: {}.\n\n",
                state.task.rejections, state.task.best_answer
            ));
        }
        prompt.push_str("\nJudger's verbatim feedback on your submitted answer:\n");
        for feedback in &state.task.rejection_feedback {
            prompt.push_str("- ");
            prompt.push_str(&truncate(feedback, 300));
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    // ---- All prior execution results (the conversation history) ----
    if !state.task.result_history.is_empty() {
        prompt.push_str("## Execution History\n\n");
        let start = state.task.result_history.len().saturating_sub(3);
        for (idx, result) in state.task.result_history[start..].iter().enumerate() {
            let round_num = start + idx + 1;
            let is_last = round_num == state.task.result_history.len();
            let cap = if is_last {
                RESULT_CONTEXT_LAST
            } else {
                RESULT_CONTEXT_PREVIOUS
            };
            prompt.push_str(&format!("### Execution #{}\n", round_num));
            prompt.push_str(&truncate(result, cap));
            prompt.push_str("\n\n");
        }
    }

    // ---- Role boundary ----
    prompt.push_str("## Your Role\n");
    prompt.push_str("You write scripts to QUERY data and COMPUTE results only. **Do NOT write code to submit answers** — the system handles submission for you. If the task file mentions a submit endpoint, ignore that part; you only need to query/compute and print the result.\n\n");

    // ---- Suggested workflow ----
    prompt.push_str("## Suggested Workflow\n");
    prompt.push_str("1. **Read**: `find /tmp/selfEvolutionTask/ -type f` then `cat` the task file AND any API docs (e.g. `API_DOCS.md`). Read BOTH in the same round.\n");
    prompt.push_str("2. **Query**: Write a `curl` command (preferred, simpler) or python script to query the API. Read the API docs first to get the correct endpoint path and auth method.\n");
    prompt.push_str("3. **Verify**: Check the query result. If it failed (401/404), fix the endpoint/auth and retry — do NOT print an empty answer.\n");
    prompt.push_str("4. **Verify**: Review the query result — does it look correct? Are the fields populated with real data (not all zeros or empty)? If the data looks wrong, retry the query before proceeding.\n");
    prompt.push_str("5. **Answer**: When you are confident the result is correct, print `echo \"ANSWER: <result>\"` as the last line. The system will submit it for you. Use JSON for multi-field answers.\n");
    prompt.push_str("- For engineering-fix tasks: run `./check` after fixing. If it prints `TOKEN: xxx`, print `echo \"ANSWER: xxx\"` with that token value.\n\n");

    // ---- Sandbox pitfalls ----
    prompt.push_str("## Sandbox Pitfalls\n");
    prompt.push_str("- 15s timeout: no sleep, no retry loops, no find without maxdepth, no internet.\n");
    prompt.push_str("- URL-encode Chinese params: `from urllib.parse import quote; url = f\"...?city={quote('北京')}\"`\n");
    prompt.push_str("- If check script reports `bad interpreter`, run with `bash <script>`.\n");
    prompt.push_str("- Do not `set -e`, do not `cd` to unconfirmed paths, do not add extra status/note fields.\n");
    prompt.push_str("- Do NOT write code that POSTs to a submit endpoint — just query and print ANSWER.\n");
    prompt.push_str("- Locale and CRLF fix are already prepended to your command.\n\n");

    // ---- What to do this round ----
    prompt.push_str("## This Round\n\n");
    prompt.push_str(&format!("Rounds remaining: {}.\n", left));
    prompt.push_str("Based on the task description, execution history, and judger feedback (if any), write a script for the next step.\n");
    prompt.push_str("**The answer must be computed by the script in real-time, not guessed from training data** — sandbox data may differ from your training data.\n");
    prompt.push_str("Put your script in a fenced code block (```bash or ```python). When you have the correct result, print `echo \"ANSWER: <result>\"` as the last line of the script. Use JSON for multi-field answers.\n\n");

    // ---- Stall pressure (only after multiple answer-less rounds) ----
    if state.task.no_answer_rounds > 0 {
        prompt.push_str(&format!(
            "The last execution completed but did not print an `ANSWER:` line. {} consecutive answer-less round(s), {} rounds remaining.\n",
            state.task.no_answer_rounds, left,
        ));
        prompt.push_str("Do not repeat file exploration with find. Compute and submit based on existing results.\n");
        if state.task.no_answer_rounds >= 2 {
            prompt.push_str("You MUST print an ANSWER line this round, even if unsure — a wrong answer has a pass rate, no answer is 0 points.\n");
        }
        prompt.push('\n');
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
    }
    if is_template_slot(trimmed) {
        return true;
    }
    false
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
        .split(|c: char| c == '_' || c == '-' || c == '.' || c == '/')
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
