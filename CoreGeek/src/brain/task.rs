//! Self-evolution task loop:
//! acceptTask → (phaseTask arrives) → prompt LLM for a sandbox script →
//! executeCmd → parse lastCmdResult → submitAnswer → iterate on error 2.
//! While a task is active the pioneer MUST stay within 1 cell of the task
//! point, so this module never emits movement.

use crate::brain::Plan;
use crate::model::{Turn, Unit};
use crate::protocol::RoleCommand;
use crate::state::{BotState, DiscoveredSchema, TaskStage};

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
            // STAGE 2 OF A TWO-STAGE SOP (P1-3). A pair that carried an
            // exploration script when it was cached queued its answer behind
            // it; this is that answer, taken the round the exploration has
            // finished. Checked before the fast path so a fresh `find_sop`
            // cannot restart the exploration the pair just completed.
            if let Some(answer) = state.task.sop_pending_answer.take() {
                state.task.stage = TaskStage::HavePlan { cmd: answer };
                return plan_pioneer(turn, state, pioneer, plan);
            }
            // SOP fast path: a cached pair whose fingerprint matches this task
            // AND whose parameters bind to this description runs first.
            if state.task.cmd_history.is_empty() {
                if let Some(pair) = state.find_sop(&state.task.task_type, &state.task.description) {
                    // A later rejection is charged against exactly the entry
                    // that ran (P1-3), so remember which template produced
                    // these bytes.
                    state.task.sop_used_template = Some(pair.template);
                    state.task.stage = match pair.explore {
                        // Stage 1 first, answer queued behind it: the second
                        // task of a kind costs 2–3 rounds instead of a replay
                        // of the whole exploration.
                        Some(explore) => {
                            state.task.sop_pending_answer = Some(pair.answer);
                            TaskStage::HavePlan { cmd: explore }
                        }
                        None => TaskStage::HavePlan { cmd: pair.answer },
                    };
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
            let payload =
                submittable_answer_shaped(&fields, &state.task.description, &answer, flip);
            let (logged, chars) = answer_for_log(&payload);
            crate::log::event(
                "task_answer_submit",
                serde_json::json!({
                    "session": state.task.session_id,
                    "round": turn.round_no,
                    "answer": logged,
                    "chars": chars,
                    "rewritten": payload != answer,
                    "flipped": flip,
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
    prompt.push_str("你在一个隔离沙盒中执行任务，沙盒可运行基础 shell 与 python3（无外网）。\n");
    prompt.push_str(
        "环境说明：任务相关文件（如 task_X.md、输入数据）通常放在 /tmp/selfEvolutionTask/ 目录下。\n",
    );
    prompt.push_str("请先用 `find /tmp/selfEvolutionTask/ -maxdepth 4` 或 `ls -R /tmp/selfEvolutionTask/` 查看有哪些文件；任务文件可能在多层子目录里（如 1-fixed-step/2-engineering-fix/task_X.md）。必须用 `find`/`ls` 输出的【真实完整路径】去 `cat`，不要假设文件在根目录、不要直接 `cat task_X.md`。\n");
    prompt.push_str("如果 /tmp/selfEvolutionTask/ 不存在或为空，说明目录在别处：用 `ls -la /` 和 `find / -maxdepth 4 -name 'task*' 2>/dev/null | head -50` 定位真实目录，并把找到的路径打印出来（下一轮会带着它继续）。不要因为一个路径不存在就放弃。\n");
    prompt.push_str("任务描述：\n");
    prompt.push_str(&state.task.description);
    prompt.push_str("\n\n要求：\n");
    prompt.push_str(
        "1. 给出可直接执行的命令或脚本（放在 ```bash 或 ```python 代码块中），完成全部子任务。\n",
    );
    prompt.push_str("2. 执行顺序固定为【侦察→作答】两段：先用 find/ls 定位并 cat 任务文件与相关文档（如 API_DOCS.md），确认接口地址、认证方式、输入数据、以及任务【要求输出的字段名】；再计算答案。侦察结论必须用一行 `FIELDS: 字段1,字段2` 打印出来（字段名以任务文件原文为准；单值答案打印 `FIELDS: value`）。即使本轮还算不出答案，也要先把已确认的字段通过 FIELDS 行打印出来——下一轮会带着它继续。若任务文件里写明了输出结构（JSON Schema，或「输出字段：名字+类型」这类格式），再打印一行 `SCHEMA: <原文 JSON>`——把文件里那段结构原样贴成一行 JSON（例如 `SCHEMA: {\"token\":\"string\",\"count\":\"integer\"}`，或 `SCHEMA: {\"properties\":{...},\"required\":[...]}`）。判分只按这个结构逐字段比对，所以这一行比任何推断都权威。\n");
    prompt.push_str("3. 脚本最后一行必须打印 `ANSWER: <最终答案>`，多字段答案用 JSON 表示，且 JSON 的字段名必须与 FIELDS 行完全一致。\n");
    prompt.push_str("4. 脚本要可复用：把可变参数（如城市名、文件名、数量）写成 `{{参数名}}` 占位符，参数名必须与任务描述里出现的字段名完全一致（例如描述里的“城市名”就用 `{{城市名}}`），脚本中不要写死具体取值；同一类任务下次会复用这段脚本并按新描述自动填参。\n");
    prompt.push_str("5. 尽量在一个脚本内完成全部步骤（find 找文件 → cat 读取 → 计算 → 打印 FIELDS 与 ANSWER），不要分多轮试探；只有带 `ANSWER:` 标记的输出才会被当作答案提交。\n");
    prompt.push_str("6. `ANSWER:` 后面必须是真实结果（数字/字符串/JSON）。找不到文件或算不出来时，**不要**打印 ANSWER 行，也不要用 `xxx`、`failed_to_extract`、`TODO`、`unknown`、`N/A` 之类的占位符占位——那会被判错并浪费一整轮；直接把报错信息打印到 stderr 即可，脚本会带着错误重试。\n");
    prompt.push_str("7. `ANSWER:` 后面**只放任务要的那个值**：是数字就只放数字（不要带单位、不要加解释），是 Markdown 标题、表格或说明文字都不算答案。多字段答案只写任务文件里点名的字段，不要自行增加 `status`、`note`、`task_id` 这类字段——判分按要求的字段逐个比对，多写一个字段会被判错。\n");
    prompt.push_str("8. 打印 ANSWER 前先自检一次：确认这个值确实由脚本从任务数据里算出来（而不是照着题面猜的或照抄示例），位数/单位/大小写与任务要求一致。\n");
    // 接口文档 §executeCmd: "判题器会在本回合执行，执行时长不得超过15秒，否则
    // 视为执行指令超时". A timed-out command returns `[TIMEOUT]` with no answer,
    // so the round is spent twice — once on the script that never finished and
    // once on the replan. Nothing in the old prompt mentioned the ceiling.
    prompt.push_str("9. 判题器对每条命令有 **15 秒硬超时**，超时整条命令作废（拿不到任何输出）：脚本必须在 15 秒内跑完并打印结果。因此不要 `sleep`、不要写重试/轮询循环、不要不带 `-maxdepth` 去 `find /`（扫全盘很慢）、不要访问外网地址（沙盒无外网，连接会挂到超时）。\n");
    if !state.task.discovered_fields.is_empty() {
        prompt.push_str(&format!(
            "\n已从任务文件确认的输出字段：{}。ANSWER 的 JSON 必须恰好包含这些字段，不多不少。\n",
            state.task.discovered_fields.join("、")
        ));
    }
    // The declared schema, when a script has read one out of the task file
    // (P1-2). It outranks the FIELDS list above: it names which fields are
    // MANDATORY, which is the difference between a retry that adds the missing
    // key and one that guesses at it.
    if let Some(schema) = &state.task.discovered_schema {
        if !schema.fields.is_empty() {
            let required = if schema.required.is_empty() {
                schema.fields.clone()
            } else {
                schema.required.clone()
            };
            prompt.push_str(&format!(
                "\n任务文件声明的输出结构：字段 {}；其中必填 {}。ANSWER 的 JSON 必须恰好包含这些字段（必填的一个都不能少），不要增删。\n",
                schema.fields.join("、"),
                required.join("、")
            ));
        }
    }
    // THE RECONNAISSANCE IS ALREADY DONE. Saying so is the difference between
    // a retry that answers and a retry that runs `find` again: with the output
    // above cut short, the model's only honest reading of "上次执行输出" is
    // that the read failed, so it reads again, forever. The streak makes the
    // cost of another exploration round explicit, and past the first round the
    // instruction stops being a preference and becomes the round's whole job.
    if state.task.no_answer_rounds > 0 {
        let left = state.task.timeout_round.saturating_sub(turn.round_no);
        prompt.push_str(&format!(
            "\n⚠ 你上一次（以及之前连续 {} 次）的脚本**没有打印 `ANSWER:` 行**，因此本轮没有任何答案可以提交。任务剩余 {} 回合，再花一轮侦察就会超时得 0 分。\n\
             侦察阶段已经结束：任务文件的真实路径和内容就在下面「上次执行输出」里（脚本自己 cat 出来的）。\n\
             **本轮不要再用 find / ls / cat / grep 去找文件或重读任务文件**，直接用你已经读到的内容计算，并在脚本最后一行打印 `ANSWER: <值>`。\n",
            state.task.no_answer_rounds, left,
        ));
        if state.task.no_answer_rounds >= 2 {
            prompt.push_str(
                "你已经连续两轮以上没有给出答案，再侦察一次这个任务必然超时。**本轮必须打印 ANSWER 行**：即使对某个字段没有十足把握，也要按任务文件里读到的数据给出最可能的取值——空手而归和写错都同样得 0 分，但写错还有通过率。\n",
            );
        }
    }
    if !state.task.result_history.is_empty() {
        prompt.push_str("\n上次执行输出（请修正错误）：\n");
        let start = state.task.result_history.len().saturating_sub(2);
        for (offset, result) in state.task.result_history[start..].iter().enumerate() {
            // Only the final entry is the material the answer has to come out
            // of; anything before it is context for the retry.
            let is_last = start + offset + 1 == state.task.result_history.len();
            let cap = if is_last {
                RESULT_CONTEXT_LAST
            } else {
                RESULT_CONTEXT_PREVIOUS
            };
            prompt.push_str(&truncate(result, cap));
            prompt.push('\n');
        }
        // The monotonic count: `wrong_answers` restarts whenever the judger
        // explained itself (P1-1), and "judged wrong 0 times" in front of a
        // session that has submitted four times reads as a first attempt.
        if state.task.rejections > 0 {
            prompt.push_str(&format!(
                "\n注意：之前提交的答案被判错 {} 次，上次答案：{}。请重新分析题目要求的字段与格式。\n",
                state.task.rejections, state.task.best_answer
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
    // The judger's own rejection text, verbatim and last (P0-1). Everything
    // above this point is inference — `discovered_fields` is what the sandbox
    // echoed, `schema_gaps`/`schema_extras` are a diff against a field list
    // guessed from the task text. This is the only line the judging authority
    // wrote itself ("缺少键 $/token"), and it is the one the successful opponent
    // path in issue #10 retried against. Ordered oldest-first so the newest
    // verdict is the last thing read before the answer is rewritten.
    if !state.task.rejection_feedback.is_empty() {
        prompt.push_str("\n判题器对你已提交答案的原话反馈（按时间先后）：\n");
        for feedback in &state.task.rejection_feedback {
            prompt.push_str("- ");
            prompt.push_str(&truncate(feedback, 300));
            prompt.push('\n');
        }
        prompt.push_str(
            "请严格按判题器原话修正：它点名缺哪个键就补哪个键（键名逐字照抄），说哪个键的值不符就只重算那一个值。判题器没有提到的字段一律保持原样，不要顺手增删。\n",
        );
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
    if fields.len() >= 2 {
        return answer.to_string();
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
    }
    false
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
const PROSE_PUNCTUATION: [char; 10] = ['，', '。', '；', '！', '？', '、', '“', '”', '‘', '’'];

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
