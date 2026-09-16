//! Self-evolution task loop:
//! acceptTask → (phaseTask arrives) → prompt LLM for a sandbox script →
//! executeCmd → parse lastCmdResult → submitAnswer → iterate on error 2.
//! While a task is active the pioneer MUST stay within 1 cell of the task
//! point, so this module never emits movement.

use crate::brain::Plan;
use crate::model::{Turn, Unit};
use crate::protocol::RoleCommand;
use crate::state::{BotState, DiscoveredSchema, PromptPurpose, TaskStage};

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
    // The entry has to survive at least as far as the prompt's window, or the
    // model is shown a cut-down copy of a cut-down copy. `RESULT_CONTEXT_LAST`
    // is the widest window `build_prompt` uses, so that is the floor here.
    state.task.result_history.push(truncate(result, RESULT_CONTEXT_LAST));
    // `build_prompt` reads the last two; the rest is only there so a session
    // that re-plans many times does not accumulate the whole match in memory.
    const RESULT_HISTORY_KEEP: usize = 8;
    let excess = state.task.result_history.len().saturating_sub(RESULT_HISTORY_KEEP);
    if excess > 0 {
        state.task.result_history.drain(..excess);
    }
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
    // The schema itself, when the script read one out of the task file and
    // echoed it (P1-2). Preferred over the FIELDS list: it says which fields
    // are MANDATORY, which is the difference between a retry that adds a
    // missing key and a retry that guesses.
    if let Some(schema) = extract_schema(output) {
        if state.task.discovered_schema.as_ref() != Some(&schema) {
            crate::log::event(
                "task_schema",
                serde_json::json!({
                    "session": state.task.session_id,
                    "fields": schema.fields,
                    "required": schema.required,
                }),
            );
            state.task.discovered_schema = Some(schema);
        }
    }
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
            let (fields, required, authoritative) = schema_view(state);
            let gaps = schema_gaps_with(authoritative, &required, &answer);
            let extras = schema_extras_with(authoritative, &fields, &answer);
            if !gaps.is_empty() || !extras.is_empty() {
                crate::log::event(
                    "task_answer_schema",
                    serde_json::json!({
                        "session": state.task.session_id,
                        "missing": gaps,
                        "extra": extras,
                        "roundsLeft": rounds_left,
                        // Which source the verdict came from. Only a declared
                        // schema may reject a single-field answer (P1-2), so a
                        // `task_answer_schema` record without this cannot be
                        // told from a description guess.
                        "source": if authoritative { "schema" } else { "guess" },
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
            // 任务书 ch.6 scores `回答正确字段个数 / 全量字段个数`: a shape the
            // judger did not expect scores zero however right the value is, and
            // an errorCode 2 does not say which of the two shapes was wrong. So
            // a retry after any rejection submits the OTHER one — same value,
            // other wrapper — instead of the identical bytes with a new number
            // in them, which is what the three-strike abandon used to be.
            // Any rejection flips the shape, so this reads the monotonic count:
            // `wrong_answers` restarts on new information (P1-1), and a flip
            // that un-happens because the judger explained itself would submit
            // the shape it already rejected.
            let flip = state.task.rejections > 0;
            // The judger's named keys first, then the keys the session has
            // evidence for — a run that printed `TOKEN: …` answers a `$/token`
            // rejection without another sandbox round (#123).
            let named = missing_keys_from_feedback(&state.task.rejection_feedback);
            let mut wanted = named.clone();
            if let Some(schema) = &state.task.discovered_schema {
                for field in &schema.required {
                    if !wanted.iter().any(|old| old.eq_ignore_ascii_case(field)) {
                        wanted.push(field.clone());
                    }
                }
            }
            let merged = harvest_answer(&wanted, &state.task.result_history)
                .map(|harvested| merge_into(&answer, &harvested))
                .unwrap_or_else(|| answer.clone());
            let payload = submittable_answer_for_keys(
                &fields,
                &state.task.description,
                &merged,
                flip,
                &named,
            );
            // The shape ladder decides WHICH shape; this decides that the bytes
            // are a shape the judger can read at all (see
            // `answer_wire_payload`). `shape` below is still read off the
            // ladder's output, so 表 4a keeps saying which direction it went.
            let wire = answer_wire_payload(&payload);
            let (logged, chars) = answer_for_log(&wire);
            crate::log::event(
                "task_answer_submit",
                serde_json::json!({
                    "session": state.task.session_id,
                    "task_kind": state.task.kind.as_str(),
                    "round": turn.round_no,
                    "answer": logged,
                    "chars": chars,
                    "wire": wire != payload,
                    "rewritten": payload != answer,
                    "flipped": flip,
                    // Which direction the rewrite took, so 表 4a can say whether
                    // the first submission of a session went out wrapped
                    // (issues #131-#135: every session's first payload was a
                    // bare non-JSON scalar and every one of them came back
                    // `答案不是合法 JSON`). `rewritten` alone cannot: the
                    // first-attempt wrap and the post-rejection unwrap both set
                    // it.
                    "shape": if payload == answer {
                        "as-is"
                    } else if payload.starts_with('{') && !answer.starts_with('{') {
                        "wrapped"
                    } else if !payload.starts_with('{') && answer.starts_with('{') {
                        "unwrapped"
                    } else {
                        "rekeyed"
                    },
                    "wrongSoFar": state.task.wrong_answers,
                    "rejections": state.task.rejections,
                    // WORKFLOW_REQUEST §10 请求六之三: how many of the judger's
                    // own rejection lines this retry's prompt carried. The
                    // feedback is appended at the END of the prompt and
                    // `prompt_sent.head` only keeps the first 300 characters,
                    // so without this field the log can prove the judger
                    // rejected an answer but never that the reason reached the
                    // retry — which is the whole premise of P0-1 and P1-1.
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

    // ---- Round-by-round task execution ----
    //
    // The flow is multi-round, each round a full LLM round-trip:
    //   Round N:   we send prompt → LLM returns a script → we put it in
    //              executeCmd → judger runs it in the sandbox.
    //   Round N+1: judger returns lastCmdResult → we feed it back to LLM →
    //              LLM returns next script → executeCmd → ...
    //   Until an ANSWER is produced, then we submit.
    //
    // pk606150 post-mortem: the old 14-rule prompt was a wall of format
    // mechanics dumped on every round. The LLM saw `task_1_beijing`, guessed
    // `{"world_heritage_count":7}` from training data, and submitted at R16
    // — 4 rounds before it actually read the task file at R20. The prompt
    // never said "read the file first"; it said "print FIELDS/SCHEMA/ANSWER
    // in this exact format" and the LLM obliged with a fabricated answer.
    //
    // The fix: each round's prompt says what THAT ROUND should do, in plain
    // language. No rule dumps. The state machine's `discovered_fields` tracks
    // whether the task file has been read (a FIELDS line was echoed), and the
    // prompt branches accordingly:
    //   - No result history → "write a script to find and read the task file"
    //   - Has results but no FIELDS → "extract fields from what you just read"
    //   - Has FIELDS but no ANSWER → "query the sandbox and compute the answer"
    //   - Has ANSWER but rejected → "fix per the judger's feedback"

    let has_results = !state.task.result_history.is_empty();
    let has_fields = !state.task.discovered_fields.is_empty();
    let has_rejection = !state.task.rejection_feedback.is_empty();
    let left = state.task.timeout_round.saturating_sub(turn.round_no);

    prompt.push_str("请帮我写一段 shell 或 python 脚本，我会在沙盒环境中执行它。\n");
    prompt.push_str("沙盒环境：可运行 shell 和 python3，不能联网。\n\n");
    prompt.push_str("任务描述：\n");
    prompt.push_str(&state.task.description);
    prompt.push_str("\n\n");

    // ---- Determine what this round should do ----
    //
    // Three phases:
    //   read  — first command: LLM writes a script to read the task file
    //   solve — has execution results: LLM writes a script to compute the answer
    //   fix   — answer rejected or schema mismatch: LLM writes a script to fix
    //
    // SOP reuse: cached script shown as context in whichever phase we're in.

    let has_schema_issues = !state.task.schema_gaps.is_empty()
        || !state.task.schema_extras.is_empty();
    let phase = if has_rejection || has_schema_issues {
        "fix"
    } else if !has_results {
        "read"
    } else {
        "solve"
    };

    // SOP reuse context: if we have a cached script from a similar task,
    // show it to the LLM as reference regardless of phase.
    if let Some(script) = &state.task.sop_reuse_script {
        prompt.push_str("我之前遇到过类似的任务，当时用的脚本是：\n\n");
        prompt.push_str("```\n");
        prompt.push_str(script);
        prompt.push_str("\n```\n\n");
        prompt.push_str("可以参考这个脚本的结构和逻辑，但请根据本次任务的实际需求写新的脚本。\n\n");
    }

    match phase {
        // ---- First command: LLM writes a script to read the task file ----
        "read" => {
            prompt.push_str("根据上面的任务描述，写一段 shell 或 python 脚本读取任务内容。\n");
            prompt.push_str("脚本需要找到任务文件并完整读取它的内容。不要凭文件名猜答案，先读题，拿到内容之后下一轮再写脚本查数据。\n");
            prompt.push_str(&format!("\n任务剩余 {} 回合。\n", left));
        }

        // ---- Has execution results: compute the answer ----
        "solve" => {
            if has_fields {
                prompt.push_str(&format!(
                    "任务文件已读完，输出字段：{}。\n\n",
                    state.task.discovered_fields.join("、")
                ));

                if let Some(schema) = &state.task.discovered_schema {
                    if !schema.fields.is_empty() {
                        let required = if schema.required.is_empty() {
                            schema.fields.clone()
                        } else {
                            schema.required.clone()
                        };
                        prompt.push_str(&format!(
                            "输出结构：字段 {}，必填 {}。\n\n",
                            schema.fields.join("、"),
                            required.join("、")
                        ));
                    }
                }
            }

            // Feed back the last command's output.
            if !state.task.result_history.is_empty() {
                prompt.push_str("上次执行结果：\n");
                let start = state.task.result_history.len().saturating_sub(2);
                for (offset, result) in state.task.result_history[start..].iter().enumerate() {
                    let is_last = start + offset + 1 == state.task.result_history.len();
                    let cap = if is_last {
                        RESULT_CONTEXT_LAST
                    } else {
                        RESULT_CONTEXT_PREVIOUS
                    };
                    prompt.push_str(&truncate(result, cap));
                    prompt.push('\n');
                }
                prompt.push('\n');
            }

            prompt.push_str("请根据任务文件的内容和上面的执行结果，写一段脚本：\n");
            prompt.push_str("- 调用沙盒内的 API / 运行 check 脚本 / 修改配置文件，拿到真实结果。\n");
            prompt.push_str("- **答案必须由脚本实时计算，不能从训练数据猜测**——沙盒里的数据和你的训练数据可能不同。\n");
            prompt.push_str("- 最后一行打印 `echo \"ANSWER: <结果>\"`，多字段用 JSON，字段名与 FIELDS 一致。\n");
            prompt.push_str("- 脚本可复用：可变参数用 `{{参数名}}` 占位符，同类任务下次只换参数。\n\n");

            // Pressure: only if the model has failed to produce an answer
            // for multiple rounds AFTER reading the task file.
            if state.task.no_answer_rounds > 0 {
                prompt.push_str(&format!(
                    "⚠ 已连续 {} 轮没有 ANSWER，剩余 {} 回合。不要再重读文件，直接根据已有结果计算并提交。\n",
                    state.task.no_answer_rounds, left,
                ));
                if state.task.no_answer_rounds >= 2 {
                    prompt.push_str("即使没把握也要提交最可能的值——写错有通过率，空手是 0 分。\n");
                }
                prompt.push('\n');
            }

            // Compact sandbox pitfalls — only the ones that actually kill scripts.
            prompt.push_str("注意事项：\n");
            prompt.push_str("- 15 秒超时：不要 sleep / 重试循环 / find / 不带 maxdepth / 访问外网。\n");
            prompt.push_str("- 中文参数 URL 编码：`from urllib.parse import quote; url = f\"...?city={quote('北京')}\"`\n");
            prompt.push_str("- check 脚本报 `bad interpreter` 时用 `bash <脚本>` 运行。\n");
            prompt.push_str("- 不要 `set -e`，不要 `cd` 到未确认路径，不要多写 status/note 字段。\n");
            prompt.push_str("- 命令开头已自动加好 locale 和 CRLF 修复。\n");
        }

        // ---- Answer was rejected or has schema issues: fix it ----
        "fix" => {
            if state.task.rejections > 0 {
                prompt.push_str(&format!(
                    "之前提交的答案被判错 {} 次，上次答案：{}。\n\n",
                    state.task.rejections, state.task.best_answer
                ));
            } else if !state.task.best_answer.is_empty() {
                prompt.push_str(&format!("上次答案：{}。\n\n", state.task.best_answer));
            }

            if !state.task.schema_gaps.is_empty() {
                prompt.push_str(&format!(
                    "缺少字段：{}\n",
                    state.task.schema_gaps.join("、")
                ));
            }
            if !state.task.schema_extras.is_empty() {
                prompt.push_str(&format!(
                    "多余字段：{}\n",
                    state.task.schema_extras.join("、")
                ));
            }

            if !state.task.rejection_feedback.is_empty() {
                prompt.push_str("\n判题器原话反馈：\n");
                for feedback in &state.task.rejection_feedback {
                    prompt.push_str("- ");
                    prompt.push_str(&truncate(feedback, 300));
                    prompt.push('\n');
                }
            }

            // Feed back the last command output for context.
            if !state.task.result_history.is_empty() {
                prompt.push_str("\n上次执行结果：\n");
                let start = state.task.result_history.len().saturating_sub(2);
                for (offset, result) in state.task.result_history[start..].iter().enumerate() {
                    let is_last = start + offset + 1 == state.task.result_history.len();
                    let cap = if is_last {
                        RESULT_CONTEXT_LAST
                    } else {
                        RESULT_CONTEXT_PREVIOUS
                    };
                    prompt.push_str(&truncate(result, cap));
                    prompt.push('\n');
                }
            }

            prompt.push_str("\n请写一段脚本修正答案：\n");
            prompt.push_str("- 按判题器点名的键逐字修正：缺哪个补哪个，值不符的重算那一个。\n");
            prompt.push_str("- 判题器没提到的字段保持原样，不要增删。\n");
            prompt.push_str("- 最后一行打印 `echo \"ANSWER: <修正后的结果>\"`。\n");
            prompt.push_str(&format!("\n任务剩余 {} 回合。\n", left));
        }

        _ => {}
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

/// Parse the schema echo: a `SCHEMA:` line carrying the output schema a script
/// read out of the sandbox task file (P1-2). Three shapes are accepted, widest
/// first — a JSON-Schema object (`{"properties":{…},"required":[…]}`), a flat
/// field→type map (`{"token":"string"}`), and a bare array of names. The last
/// SCHEMA line wins, as with `FIELDS:`.
///
/// No line, no schema: every caller then degrades to the `FIELDS:` list and the
/// description heuristic exactly as before, which is the no-regression case the
/// analysis asks for.
pub fn extract_schema(output: &str) -> Option<DiscoveredSchema> {
    let trimmed = output.trim_end_matches("[TRUNCATED]").trim();
    for line in trimmed.lines().rev() {
        let line = line.trim();
        let rest = line
            .strip_prefix("SCHEMA:")
            .or_else(|| line.strip_prefix("SCHEMA："))
            .or_else(|| line.strip_prefix("schema:"))
            .or_else(|| line.strip_prefix("schema："))
            .or_else(|| line.strip_prefix("输出schema:"))
            .or_else(|| line.strip_prefix("输出schema："));
        let Some(rest) = rest else {
            continue;
        };
        let json = rest.trim();
        if json.is_empty() {
            continue;
        }
        return parse_schema(json);
    }
    None
}

/// Keys that describe a schema rather than name a field, so a flat map carrying
/// one of them is not read as an output field.
const SCHEMA_KEYWORDS: [&str; 7] = [
    "type",
    "properties",
    "required",
    "title",
    "description",
    "items",
    "additionalProperties",
];

/// Turn the JSON on a `SCHEMA:` line into the field list the gate checks
/// against. `None` when it parses to nothing usable — an unparseable or empty
/// echo teaches nothing and must not silently blank a schema already known.
pub fn parse_schema(json: &str) -> Option<DiscoveredSchema> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let (mut fields, mut required): (Vec<String>, Vec<String>) = match &value {
        serde_json::Value::Array(items) => (
            items.iter().filter_map(|item| item.as_str()).map(name_of).collect(),
            Vec::new(),
        ),
        serde_json::Value::Object(map) => {
            match map.get("properties").and_then(|value| value.as_object()) {
                Some(properties) => {
                    let fields: Vec<String> = properties.keys().map(name_of).collect();
                    let required = map
                        .get("required")
                        .and_then(|value| value.as_array())
                        .map(|list| {
                            list.iter()
                                .filter_map(|item| item.as_str())
                                .map(name_of)
                                .filter(|name| fields.contains(name))
                                .collect()
                        })
                        .unwrap_or_default();
                    (fields, required)
                }
                // A flat `{field: type}` map — the shape a task file's own
                // "输出格式" block takes when it is not written as JSON Schema.
                None => (
                    map.keys()
                        .filter(|key| !SCHEMA_KEYWORDS.contains(&key.as_str()))
                        .map(name_of)
                        .collect(),
                    Vec::new(),
                ),
            }
        }
        _ => return None,
    };
    // A JSON object has no order to preserve (serde_json backs it with a
    // `BTreeMap`, and the sort makes the fact explicit rather than incidental),
    // so the object forms are put in name order. Only the array form carries an
    // order the task file chose, and it keeps it.
    if value.is_object() {
        fields.sort();
    }
    let mut seen: Vec<String> = Vec::new();
    fields.retain(|field| {
        if field.is_empty() || seen.contains(field) {
            return false;
        }
        seen.push(field.clone());
        true
    });
    if fields.is_empty() || fields.len() > 16 {
        return None;
    }
    // An empty `required` means "every named field is required", which is the
    // only reading that cannot reject a correct answer: the schema named the
    // field, and the answer omitted it.
    if required.is_empty() {
        required = fields.clone();
    }
    Some(DiscoveredSchema { fields, required })
}

fn name_of(name: impl AsRef<str>) -> String {
    name.as_ref().trim().to_string()
}

/// The schema the answer is checked against (P1-2), strongest source first:
/// the `SCHEMA:` echo, then the `FIELDS:` echo, then the fields guessed from
/// the task text — as three parts: every field the answer may carry, the subset it
/// must carry, and whether the source is authoritative enough to check a
/// SINGLE field against.
///
/// Only a `SCHEMA:` line earns that last flag. `FIELDS:` is a list the script
/// printed and `expected_fields` is a guess from prose; rejecting a correct
/// one-field answer because a guess was wrong costs the whole task reward, so
/// neither may. A declared schema is the task speaking for itself.
fn schema_view(state: &BotState) -> (Vec<String>, Vec<String>, bool) {
    if let Some(schema) = &state.task.discovered_schema {
        if !schema.fields.is_empty() {
            let required = if schema.required.is_empty() {
                schema.fields.clone()
            } else {
                schema.required.clone()
            };
            return (schema.fields.clone(), required, true);
        }
    }
    let fields = if state.task.discovered_fields.is_empty() {
        expected_fields(&state.task.description)
    } else {
        state.task.discovered_fields.clone()
    };
    (fields.clone(), fields, false)
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
    submittable_answer_shaped(fields, description, answer, false)
}

/// [`submittable_answer_for`] with the single-key-object decision inverted.
///
/// The judger scores 任务书 ch.6 as `回答正确字段个数 / 全量字段个数`, so a wrapper
/// it did not expect zeroes the answer however right the value is — and the
/// value is the LLM's job, not ours. Shape is the one thing we still control:
/// once a submission has come back errorCode 2, the retry submits the OTHER
/// shape rather than the identical bytes with a new number in them. Two shapes
/// are all there is (bare scalar, one-key object), so one flip exhausts the
/// space; `wrong_answers` counts the rejections and `plan_pioneer` flips on any.
pub fn submittable_answer_shaped(
    fields: &[String],
    description: &str,
    answer: &str,
    flip: bool,
) -> String {
    submittable_answer_for_keys(fields, description, answer, flip, &[])
}

/// [`submittable_answer_shaped`] plus the keys the JUDGER itself named as
/// missing.
///
/// Issues #121-#125 carry the verdict verbatim: `键值比对不通过: $/token: 缺少键`
/// (#123 session 1, twice) and `键值比对不通过: $: 值不符` (#122 session 6,
/// #124 session 3). Both sessions then re-submitted the same bytes — the
/// retry loop only ever swapped a one-key OBJECT for its scalar, and a scalar
/// answer took the early return above, so the other shape was never tried.
/// Measured cost: three submissions each and a whole task scored zero with the
/// sandbox having already printed the right value.
///
/// Two additions:
///   * a key the judger named outranks every guess about shape — the value is
///     moved under exactly that key, spelled exactly as the judger spelled it;
///   * once a rejection has landed, a bare scalar with one known field is
///     wrapped instead of resubmitted. That is the direction that was missing,
///     and issue #125's session 3 is the proof it wins: `SCHEMA:
///     {"token":"string"}` was echoed, the bare value was rejected once, and
///     the 25-character `{"token": "…"}` submitted by the retry was
///     `confirmed_success`.
pub fn submittable_answer_for_keys(
    fields: &[String],
    description: &str,
    answer: &str,
    flip: bool,
    missing_keys: &[String],
) -> String {
    // A key the judger named comes first: it is the only statement about the
    // expected shape that comes from the judging authority rather than from our
    // own reading of the task text.
    for key in missing_keys {
        if let Some(value) = value_for_named_key(answer, key) {
            // RE-KEY, do not duplicate. The value moves under the key the
            // judger named and the label we invented is dropped: a value kept
            // under both names is wrong under one of them by construction, and
            // an invented key is the `schema_extras` defect the prompt already
            // warns against.
            let mut map = serde_json::Map::new();
            map.insert(key.clone(), value);
            if let Ok(text) = serde_json::to_string(&serde_json::Value::Object(map)) {
                return text;
            }
        }
    }
    if fields.len() >= 2 {
        return answer.to_string();
    }
    // A value that is not JSON AT ALL cannot satisfy any schema, and its verdict
    // is always a syntax rejection rather than a shape preference: 表 4b of
    // issues #131-#135 carries `答案不是合法 JSON` against the first submission
    // of **every** session in five straight matches (round 23 in all five), and
    // each of those sessions then had one submission left before its deadline.
    // The old rule wrapped a scalar only after a rejection, and only when it
    // already parsed as JSON — so a bare `fc1e78eb2a5a` was resubmitted
    // byte-identical forever (the `from_str` in the flip arm below fails, the
    // object arm below fails, and both fall through to `answer.to_string()`).
    //
    // With exactly one known field the wrapper is the ONLY legal shape, so there
    // is no working shape to preserve and the wrap happens on the first attempt
    // too. The exit is deliberately narrow: a value that already parses as JSON
    // keeps the old behaviour byte for byte (the model's own shape goes out
    // first, the retry tries the other one), which is what
    // `the_first_attempt_is_never_rewritten_into_a_new_shape` and
    // `a_rejected_scalar_is_wrapped_under_the_declared_field` pin.
    if fields.len() == 1 && serde_json::from_str::<serde_json::Value>(answer).is_err() {
        let mut map = serde_json::Map::new();
        map.insert(
            fields[0].clone(),
            serde_json::Value::String(answer.trim().to_string()),
        );
        if let Ok(text) = serde_json::to_string(&serde_json::Value::Object(map)) {
            return text;
        }
    }
    // The inverse direction (issues #121-#125). The object arm below can only
    // ever DROP a wrapper; a scalar therefore had exactly one shape to offer and
    // the retry spent its rounds resubmitting it. A declared or echoed
    // single-field schema says the answer is that field's value, so once the
    // scalar has been rejected the wrapper is the other shape to try.
    if flip && fields.len() == 1 {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(answer) {
            if !value.is_object() && !value.is_array() {
                let mut map = serde_json::Map::new();
                map.insert(fields[0].clone(), value);
                if let Ok(text) = serde_json::to_string(&serde_json::Value::Object(map)) {
                    return text;
                }
            }
        }
    }
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
    // An echoed single field means the answer IS that value, so the wrapper is
    // ours to drop. With no echo at all the legacy description heuristic
    // decides: a wrapper around a field the task never named is ours too.
    let unwrap = if fields.len() == 1 {
        true
    } else {
        !description.to_lowercase().contains(&key.to_lowercase())
    };
    if unwrap == flip {
        return answer.to_string(); // `flip` reached the other shape: keep this one
    }
    bare_scalar(value).unwrap_or_else(|| answer.to_string())
}

/// The value to file under a key the judger named, taken from the answer we
/// already have. `None` when the answer cannot supply one: a multi-key object
/// says nothing about which of its values belongs under the judger's key, and
/// guessing there would replace a wrong answer with a different wrong answer.
///
/// An answer that is not JSON at all supplies the whole of itself. That arm used
/// to `?` out at the parse, so on the very boards the judger had just explained
/// itself on — `键值比对不通过: $/token: 缺少键` (表 4b, issues #123 and #133)
/// beside a sandbox that printed the token as a bare `TOKEN: fc1e78eb2a5a` — the
/// named key was known, the value was known, and the re-key still did not happen:
/// the retry resubmitted the bare string and the session ran out of rounds. The
/// judger's own spelling of the key outranks every guess about shape, so a bare
/// scalar becomes that key's value.
fn value_for_named_key(answer: &str, key: &str) -> Option<serde_json::Value> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(answer) else {
        let trimmed = answer.trim();
        return (!trimmed.is_empty()).then(|| serde_json::Value::String(trimmed.to_string()));
    };
    match value {
        serde_json::Value::Object(map) => {
            if map.len() != 1 || map.contains_key(key) {
                return None;
            }
            map.into_iter().next().map(|(_, value)| value)
        }
        other if !other.is_array() && !other.is_null() => Some(other),
        _ => None,
    }
}

/// Every key the judger has named as missing, oldest verdict first.
///
/// The rejection text is the judging authority's own statement about the shape
/// it wanted — `键值比对不通过: $/token: 缺少键` names `token` — and it is the one
/// input that no amount of re-reading the task text can substitute for.
pub fn missing_keys_from_feedback(feedback: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for text in feedback {
        if let Some(key) = missing_key_in(text) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys.truncate(4);
    keys
}

/// The JSON pointer segment named by a "missing key" verdict, e.g.
/// `键值比对不通过: $/token: 缺少键` -> `token`.
///
/// Deliberately narrow: the text must say something was MISSING (`缺少键`,
/// `MissingNamedInput`) and must carry an explicit `$/…` pointer, so a value
/// mismatch (`$/world_heritage_count: 数值不符` — the field was there and
/// wrong) never relabels the answer. Re-keying on a value mismatch is how a
/// correct field gets destroyed by a retry.
pub fn missing_key_in(text: &str) -> Option<String> {
    let missing = text.contains("缺少键")
        || text.contains("MissingNamedInput")
        || text.contains("missing key");
    if !missing {
        return None;
    }
    // The two shapes the judger uses: a JSON pointer before the verdict
    // (`键值比对不通过: $/token: 缺少键`) and the key named after it
    // (`MissingNamedInput: city`).
    if let Some(start) = text.find("$/") {
        let rest = &text[start + 2..];
        let end = rest
            .find(|c: char| c == ':' || c == '：' || c.is_whitespace() || c == '，' || c == ',')
            .unwrap_or(rest.len());
        return clean_key(rest[..end].rsplit('/').find(|part| !part.is_empty())?);
    }
    let after = ["MissingNamedInput", "missing key", "缺少键"]
        .iter()
        .filter_map(|marker| text.find(marker).map(|at| at + marker.len()))
        .min()?;
    let rest = text[after..].trim_start_matches(|c: char| {
        c == ':' || c == '：' || c == ' ' || c == '\t' || c == '$' || c == '/' || c == '"'
    });
    let end = rest
        .find(|c: char| c == ':' || c == '：' || c.is_whitespace() || c == ',' || c == '，' || c == '"')
        .unwrap_or(rest.len());
    clean_key(&rest[..end])
}

fn clean_key(key: &str) -> Option<String> {
    let key = key.trim().trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
    if key.is_empty() || key.chars().count() > 64 {
        return None;
    }
    Some(key.to_string())
}

/// `answer` with every key of `harvested` it does not already carry.
///
/// Keys already present are left exactly as the model wrote them — a harvested
/// value never overwrites the session's own answer, it only fills a hole the
/// judger has already complained about. A scalar `answer` has no holes to fill
/// and is replaced outright, because the harvested object is the only shape
/// that can carry the named key at all.
pub fn merge_into(answer: &str, harvested: &str) -> String {
    let Ok(serde_json::Value::Object(extra)) = serde_json::from_str::<serde_json::Value>(harvested)
    else {
        return answer.to_string();
    };
    match serde_json::from_str::<serde_json::Value>(answer) {
        Ok(serde_json::Value::Object(mut map)) => {
            let mut changed = false;
            for (key, value) in extra {
                if !map.keys().any(|old| old.eq_ignore_ascii_case(&key)) {
                    map.insert(key, value);
                    changed = true;
                }
            }
            if !changed {
                return answer.to_string();
            }
            serde_json::to_string(&serde_json::Value::Object(map))
                .unwrap_or_else(|_| answer.to_string())
        }
        Ok(_) => harvested.to_string(),
        Err(_) => harvested.to_string(),
    }
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
    // NO `ANSWER:` LINE, BUT THE VALUE IS IN THE OUTPUT (issues #121-#125).
    //
    // Every one of these five matches ran 5-7 sessions and submitted almost
    // nothing: #121 submitted zero answers across seven sessions, #122 found
    // one, #124 three. The sandbox output they discarded `ANSWER:`-less is not
    // a file listing — #123's round-18 run printed
    // `[ OK ] 全部通过 (6/6) TOKEN: fc1e78eb2a5a`, which is the exact value
    // issue #125's one successful session submitted as `{"token": "…"}`. The
    // model's script answered; it simply labelled the line.
    //
    // Only keys the session has EVIDENCE for are read: a field the task file
    // declared, a field a script echoed, or a key the judger itself named. That
    // is what keeps this clear of the red line — this is the task's own value
    // under the task's own field name, never a stray line of exploratory
    // output dressed as an answer.
    let harvested = harvest_answer(&harvest_keys(state), &state.task.result_history);
    match answers.last() {
        Some(best) => {
            let merged = merge_json_fields(&answers);
            Some(merged.or(harvested).unwrap_or_else(|| best.clone()))
        }
        None => harvested,
    }
}

/// Keys a run's raw output may carry an answer under, strongest evidence first:
/// the declared schema, the echoed field list, fields named in the description,
/// and — because #123's winning key came from nowhere else — every key the
/// judger named as missing.
pub fn harvest_keys(state: &BotState) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let push = |key: &str, keys: &mut Vec<String>| {
        let key = key.trim();
        if key.is_empty() || key.chars().count() > 32 {
            return;
        }
        if !keys.iter().any(|old| old.eq_ignore_ascii_case(key)) {
            keys.push(key.to_string());
        }
    };
    if let Some(schema) = &state.task.discovered_schema {
        for field in schema.required.iter().chain(schema.fields.iter()) {
            push(field, &mut keys);
        }
    }
    for field in &state.task.discovered_fields {
        push(field, &mut keys);
    }
    for field in &expected_fields(&state.task.description) {
        push(field, &mut keys);
    }
    for key in missing_keys_from_feedback(&state.task.rejection_feedback) {
        push(&key, &mut keys);
    }
    keys.truncate(8);
    keys
}

/// Pull a `<key>: <value>` pair for each of `keys` out of raw sandbox output.
///
/// Returns a JSON object, or `None` when the output carries none of the keys.
/// The value is read as JSON when it parses as a JSON scalar (`15` stays a
/// number) and as a string otherwise (`fc1e78eb2a5a`).
pub fn harvest_answer(keys: &[String], outputs: &[String]) -> Option<String> {
    let mut map = serde_json::Map::new();
    for key in keys {
        for output in outputs.iter().rev() {
            if let Some(value) = value_for_key(strip_status_line(output), key) {
                map.insert(key.clone(), value);
                break;
            }
        }
    }
    if map.is_empty() {
        return None;
    }
    serde_json::to_string(&serde_json::Value::Object(map)).ok()
}

/// The value on a `key: value` (or `"key": value`) line, when the line is one.
///
/// The key must sit on a word boundary and be followed by a separator, so
/// `FIELDS: port, name` yields nothing for `port` and a JSON line
/// (`{"token": "x"}`) is left to `extract_answer`, which already understands
/// it. Sentinels and container fragments are refused: a harvested value has to
/// be a real scalar or it is the exploratory output the red line forbids.
fn value_for_key(output: &str, key: &str) -> Option<serde_json::Value> {
    let key_bytes = key.as_bytes();
    if key_bytes.is_empty() {
        return None;
    }
    for line in output.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Scanned on BYTE offsets into `line` itself, not into a lowercased
        // copy: `str::to_lowercase` can change a string's byte length
        // (`İ` → `i̇`), so an offset found in the copy is not an offset into the
        // line, and `line[..at]` would then be a slice that panics on a
        // char boundary — inside a match, in the round a task is being
        // answered. `eq_ignore_ascii_case` gives the case-insensitivity the
        // field names need without ever building the copy.
        let bytes = line.as_bytes();
        let mut at = 0usize;
        while at + key_bytes.len() <= bytes.len() {
            let hit = bytes[at..at + key_bytes.len()].eq_ignore_ascii_case(key_bytes)
                && line.is_char_boundary(at)
                && line.is_char_boundary(at + key_bytes.len());
            if !hit {
                at += 1;
                continue;
            }
            let after = at + key_bytes.len();
            at = after;
            let before_ok = at == key_bytes.len()
                || !line[..at - key_bytes.len()]
                    .chars()
                    .next_back()
                    .map_or(false, |c| c.is_alphanumeric() || c == '_');
            if !before_ok {
                continue;
            }
            let rest = line[after..].trim_start();
            let rest = rest
                .strip_prefix('"')
                .or_else(|| rest.strip_prefix('\''))
                .unwrap_or(rest)
                .trim_start();
            let Some(rest) = rest
                .strip_prefix(':')
                .or_else(|| rest.strip_prefix('：'))
                .or_else(|| rest.strip_prefix('='))
            else {
                continue;
            };
            let value = rest
                .trim()
                .trim_end_matches(|c: char| c == ',' || c == ';' || c == '，' || c == '；')
                .trim()
                .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
                .trim();
            if value.is_empty()
                || value.chars().count() > 200
                || value.contains('{')
                || value.contains('}')
                || value.contains('[')
                || value.contains(']')
                || value.contains('\n')
                || is_failure_answer(value)
                || is_marker_word(value)
            {
                continue;
            }
            // The value must be a value, not the rest of a sentence: `key:`
            // followed by a whole clause is the task file's own prose, which is
            // the same defect `is_failure_answer` guards the `ANSWER:` marker
            // against.
            if value.split_whitespace().count() > 4 {
                continue;
            }
            return Some(match serde_json::from_str::<serde_json::Value>(value) {
                Ok(parsed) if !parsed.is_object() && !parsed.is_array() => parsed,
                _ => serde_json::Value::String(value.to_string()),
            });
        }
    }
    None
}

/// Words that are a marker, never a value.
fn is_marker_word(text: &str) -> bool {
    matches!(
        text.trim().to_lowercase().as_str(),
        "fields" | "schema" | "answer" | "token_pending"
    )
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
                    | '"'
                    | '"'
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
    schema_gaps_with(false, fields, answer)
}

/// [`schema_gaps`] with the two-field floor lifted for a schema the task file
/// itself declared (P1-2). A `SCHEMA:` line naming one required field is the
/// task speaking, so an answer missing it IS incomplete; a one-field guess from
/// prose is not, and stays ignored exactly as before.
pub fn schema_gaps_with(authoritative: bool, fields: &[String], answer: &str) -> Vec<String> {
    if fields.is_empty() || (!authoritative && fields.len() < 2) {
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
    schema_extras_with(false, fields, answer)
}

/// [`schema_extras`] with the two-field floor lifted for a declared schema
/// (P1-2) — see [`schema_gaps_with`].
pub fn schema_extras_with(
    authoritative: bool,
    fields: &[String],
    answer: &str,
) -> Vec<String> {
    if fields.is_empty() || (!authoritative && fields.len() < 2) {
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
