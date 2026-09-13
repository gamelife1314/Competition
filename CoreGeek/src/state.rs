//! Cross-round bot memory: news log, mine outages, task session, treasure
//! hunt, LLM budgets and failure feedback.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::brain::news;
use crate::model::Turn;
use crate::protocol::Pos;

/// One remembered command we issued last round, used to interpret
/// `lastRoundRoleActionResults` feedback.
#[derive(Debug, Clone, Default)]
pub struct IssuedCmd {
    pub action: String,
    pub target: Option<Pos>,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Outage {
    pub ore: String,
    pub from_day: i64,
    pub to_day: i64, // inclusive
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TaskStage {
    #[default]
    WaitingDescription,
    Planning,
    HavePlan {
        cmd: String,
    },
    WaitingCmdResult {
        attempts: i32,
    },
    HaveAnswer {
        answer: String,
    },
    WaitingSubmit {
        attempts: i32,
    },
}

/// Rejected answers after which a task is abandoned rather than retried. A
/// session that has been judged wrong three times has consumed its own
/// evidence; the pioneer goes back to the wall line and the guns.
pub const MAX_WRONG_ANSWERS: i32 = 3;

#[derive(Debug, Clone, Default)]
pub struct TaskSession {
    pub active: bool,
    /// Monotonic identifier allocated after each accept. It scopes every
    /// asynchronous LLM/command response so identical output in a later task
    /// is never mistaken for a stale response from an earlier task.
    pub session_id: u64,
    pub accepted_round: i64,
    pub timeout_round: i64,
    pub point: Option<Pos>,
    pub task_type: String,
    pub description: String,
    pub description_round: i64,
    pub stage: TaskStage,
    pub best_answer: String,
    pub cmd_history: Vec<String>,
    pub result_history: Vec<String>,
    pub wrong_answers: i32,
    /// Request rounds provide the second half of the response dedupe key.
    pub llm_request_round: Option<i64>,
    pub llm_consumed_request_round: Option<i64>,
    pub cmd_request_round: Option<i64>,
    pub cmd_consumed_request_round: Option<i64>,
    /// Submission-success evidence. Success requires all three independent
    /// signals: task point closed, phaseTask absent for multiple rounds, and no
    /// error observed after submission.
    pub submitted_round: Option<i64>,
    pub phase_missing_rounds: i32,
    pub point_closed_round: Option<i64>,
    pub post_submit_error: bool,
    /// Fields the task text asked for that the produced answer did not carry.
    /// Fed back into the next prompt so the retry can close the gap.
    pub schema_gaps: Vec<String>,
    /// Fields the answer carried that the task never asked for. The judger
    /// scores the object against the schema it named, so an invented field is
    /// wrong on its own (`{"city":…,"task_id":…,"status":"completed"}` — issue
    /// #22's session 5). Fed back into the next prompt like `schema_gaps`.
    pub schema_extras: Vec<String>,
    /// Consecutive `[JUDGER_ERROR]` verdicts that name `executeCmd` as
    /// unavailable. That error is the judger telling us the task's execution
    /// window is shut — issue #17's two sessions fired four and one command
    /// into it and burned both timeouts to zero points — so two in a row end
    /// the session locally instead of re-planning into the same closed door.
    pub judger_window_errors: i32,
    /// Round in which a fresh `llmResp` was consumed. `executeCmd` sent in
    /// that same round comes back
    /// `[JUDGER_ERROR] executeCmd 仅在自进化任务执行期间可用` — see
    /// `brain::task::plan_pioneer` for the evidence — so the command is held
    /// back until the round after it.
    pub llm_resp_round: Option<i64>,
    /// The command whose sandbox run produced an answer. That is the only
    /// script worth caching for the next task of the same kind: it demonstrably
    /// reached the sandbox and ran to completion, whereas an unrun command (a
    /// `[JUDGER_ERROR]`, a timeout) or an answer-less run teaches nothing.
    pub sop_cmd: Option<String>,
}

/// A cached, parameterised script for one task fingerprint. The body keeps
/// `{{name}}` placeholders where the values that differ between two tasks of
/// the same kind go, so replaying it on a *different* task cannot silently
/// reuse the old task's inputs.
#[derive(Debug, Clone, Default)]
pub struct SopEntry {
    pub task_type: String,
    pub keywords: Vec<String>,
    pub template: String,
}

impl SopEntry {
    /// Bind this template to a new task description. A template without
    /// placeholders is reused verbatim; one WITH placeholders is only reused
    /// when every placeholder can be resolved from the description, because
    /// running it with the previous task's values would answer a different
    /// question confidently.
    pub fn bind(&self, description: &str) -> Option<String> {
        if !self.template.contains("{{") {
            return Some(self.template.clone());
        }
        let mut script = self.template.clone();
        for name in placeholders(&self.template) {
            let value = param_value(description, &name)?;
            script = script.replace(&format!("{{{{{name}}}}}"), &value);
        }
        Some(script)
    }
}

/// `{{name}}` occurrences in a script template, in order and deduped.
///
/// A name is an identifier or a short CJK noun. Anything else between doubled
/// braces is a literal (a python f-string's `{{`, a shell `${}`) and is skipped
/// rather than mistaken for a parameter — otherwise the template would look
/// unbindable and every reuse would fall back to the LLM.
pub fn placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let name = after[..end].trim();
        let is_name = !name.is_empty()
            && name.chars().count() <= 20
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
        if is_name && !out.iter().any(|old: &String| old == name) {
            out.push(name.to_string());
        }
        // Skip the opening braces even when this was a literal, so the scan
        // keeps looking for a real placeholder further along.
        rest = &rest[start + 2..];
    }
    out
}

/// Value bound to `name` in a task description, e.g. `城市名：上海` → `上海`
/// or `city = Berlin` → `Berlin`. Returns None when the description does not
/// name the parameter, which makes the SOP unusable rather than wrong.
pub fn param_value(description: &str, name: &str) -> Option<String> {
    let start = description.find(name)? + name.len();
    let rest = &description[start..];
    let trimmed = rest.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | '：' | '=' | '＝' | '是' | '为')
    });
    let value: String = trimmed
        .chars()
        .take_while(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    ',' | '，' | '。' | ';' | '；' | '、' | '\n' | '\r' | '(' | '（' | ')' | '）'
                )
        })
        .take(40)
        .collect();
    if value.is_empty() || value.chars().count() > 40 {
        None
    } else {
        Some(value)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TreasurePhase {
    #[default]
    Idle,
    AskedLlm {
        round: i64,
    },
    HavePlan,
    Done,
}

#[derive(Debug, Clone, Default)]
pub struct TreasurePlan {
    pub pos: Pos,
    pub items: Vec<String>,
    pub open_day: i64,
}

#[derive(Debug, Clone, Default)]
pub struct TreasureState {
    pub legends: Vec<(i64, String)>,
    pub plan: Option<TreasurePlan>,
    pub phase: TreasurePhase,
    pub ask_attempts: i32,
    pub summon_attempts: i32,
    pub wrong_item_rounds: i32,
    /// Set when a summon came back "too early" (code 2): the next LLM ask
    /// must re-derive the opening time instead of just pushing the day.
    pub time_feedback: bool,
}

#[derive(Debug, Default)]
pub struct BotState {
    pub current_day: i64,
    pub last_round: i64,
    pub llm_used_today: i32,
    pub summon_orders_today: i32,
    /// Walls placed this day; capped so wall-building never monopolizes the day.
    pub walls_built_today: i64,
    /// Distinct cells fortified today. The cap is measured against THIS, not
    /// against the number of placements: a wall that a role demolished to get
    /// out — or that the night's robots knocked down — has to be rebuildable
    /// without spending the day's fortification budget twice, or the ring stays
    /// open for the rest of the day with the budget "used up".
    pub walled_cells_today: HashSet<Pos>,

    /// Ring cells the economy deliberately cut open today so a role inside the
    /// wall line could reach the ore. The ring is a closed box — once the last
    /// wall is up, nothing inside can get out and the ore is all outside — so
    /// the day needs a door and the night needs it shut. Honoured only while the
    /// sun is up: from dusk these cells are ordinary gaps again and the seal
    /// crew fills them. Cleared at day rollover.
    pub door_cells: HashSet<Pos>,

    /// day -> official news text (dedup)
    pub official_seen: HashMap<i64, String>,
    /// day -> folk legend text (dedup)
    pub legend_seen: HashMap<i64, String>,
    pub outages: Vec<Outage>,
    /// lowest vendor price ever seen per ore (baseline)
    pub base_prices: HashMap<String, i64>,

    pub last_issued: HashMap<i64, IssuedCmd>,
    /// consecutive failure count per role
    pub failures: HashMap<i64, i32>,
    /// build attempts that failed, keyed by (role, target, building name)
    pub build_fails: HashMap<(i64, Pos, String), i32>,
    /// (target cell, building name) pairs blacklisted after repeated failures
    pub blacklisted_builds: HashSet<(Pos, String)>,

    pub task: TaskSession,
    /// Task point -> last round we will not accept it on.
    ///
    /// Abandoning a session is only half a fix: the pioneer is standing in the
    /// point's neighbourhood, the point still reads valid, and the next round
    /// `next_task_point` walks straight back into the window the judger just
    /// refused — accept, plan, refuse, abandon, for the rest of the day. A
    /// refusal is a property of the point, so it is remembered against the
    /// point, through the end of the day it was learned on.
    pub task_refusals: HashMap<Pos, i64>,
    /// Last allocated task session identifier. Preserved across half resets so
    /// session IDs remain monotonic for the lifetime of this bot process.
    pub task_session_seq: u64,
    pub sop_cache: Vec<SopEntry>,
    pub treasure: TreasureState,

    /// Dedup strings for non-task one-shot channels. Task LLM/command responses
    /// are deduped by (session_id, request_round) inside TaskSession.
    pub seen_llm_resp: String,
    pub seen_cmd_result: String,
    /// bombs/dizzy bought tracker can be derived from backpacks; kept simple
    pub harass_done_today: bool,

    /// Stable controller↔tower pairings, computed once per night and reused
    /// until a tower is built/destroyed or a new night starts. Without this,
    /// greedy re-pairing every round causes controllers to oscillate and
    /// weapons go unoperated.
    pub night_pairs: Vec<(i64, i64)>,
    pub night_pair_day: i64,
    pub night_pair_tower_ids: Vec<i64>,
    pub night_pair_controller_ids: Vec<i64>,
    pub night_pair_task_busy: bool,
    /// Controllers too wounded to hold a gun this round (see
    /// `brain::night::withdrawing`). Sorted. The pairing prefers a fit
    /// controller, so a change here invalidates it.
    pub night_pair_withdrawing: Vec<i64>,

    /// D1 gate stays open until every controller has reached an inside/tower
    /// stand, then remains sealed for the rest of the half.
    pub wall_gate_sealed: bool,

    /// Set the first time the radius-2 ring around the station has no holes.
    ///
    /// A ring that has been closed before and has holes NOW is not being
    /// expanded, it is BREACHED — the robots knocked it down, and every round it
    /// stays open is a round the station spends taking hits (issue #21: walls
    /// 17 → 7 and the station 1500 → 20 across D2 night, -30 residual). The
    /// later-day fortification budget exists to stop wall work from starving the
    /// economy while the ring is being BUILT; it must not ration its repair.
    /// Never cleared: the ring can only become breached again.
    pub ring_ever_complete: bool,

    /// Compact telemetry baselines used to report deltas rather than dumping
    /// full protocol payloads every round.
    pub prev_total_score: Option<i64>,
    pub prev_robot_hp: HashMap<i64, i64>,
    /// Robot id -> kill score, so a robot leaving the board can be attributed
    /// to `score2` even after its entity is gone from the payload.
    pub prev_robot_kind: HashMap<i64, i64>,
    pub prev_wall_hp: Option<i64>,
    /// Running estimate of earned kill score, used to split the total into
    /// kill / survival / residual (task) components.
    pub cum_kill_score: i64,
}

impl BotState {
    pub fn global() -> &'static Mutex<BotState> {
        static INSTANCE: OnceLock<Mutex<BotState>> = OnceLock::new();
        INSTANCE.get_or_init(|| Mutex::new(BotState::default()))
    }

    pub fn locked() -> MutexGuard<'static, BotState> {
        Self::global().lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Absorb per-round feedback. Must run before planning.
    pub fn observe(&mut self, turn: &Turn) {
        // A match is two halves (sides swap, roundNo restarts). If the round
        // number regresses, the previous match's memory (blacklisted cells,
        // task session, treasure plan…) belongs to the OTHER board — wipe it.
        if self.last_round > 0 && turn.round_no < self.last_round {
            crate::log::event(
                "state_reset",
                serde_json::json!({"fromRound": self.last_round, "toRound": turn.round_no}),
            );
            let task_session_seq = self.task_session_seq;
            *self = BotState::default();
            self.task_session_seq = task_session_seq;
        }
        // Day rollover: reset daily budgets.
        if turn.day != self.current_day {
            self.current_day = turn.day;
            self.llm_used_today = 0;
            self.summon_orders_today = 0;
            self.walls_built_today = 0;
            self.walled_cells_today.clear();
            self.harass_done_today = false;
            // Yesterday's door was sealed at dusk; today may need a new one.
            self.door_cells.clear();
            self.wall_gate_sealed = false;
        }
        self.last_round = turn.round_no;

        // News (only meaningful at day starts, but dedup by (day,text)).
        self.absorb_news(turn);

        // Baseline ore prices: track the minimum seen.
        for ore in crate::model::ORES {
            if let Some(price) = turn.vendor_prices.get(ore) {
                let entry = self.base_prices.entry(ore.to_string()).or_insert(*price);
                if *price < *entry {
                    *entry = *price;
                }
            }
        }

        // Action failure feedback.
        self.absorb_failures(turn);

        // Task / treasure feedback channels.
        self.absorb_llm_and_cmd(turn);
        self.absorb_task_events(turn);
        self.absorb_treasure_events(turn);
    }

    fn absorb_news(&mut self, turn: &Turn) {
        if !turn.official_news.is_empty()
            && turn.official_news != "今日无重大新闻"
            && self.official_seen.get(&turn.day).map(String::as_str)
                != Some(turn.official_news.as_str())
        {
            self.official_seen
                .insert(turn.day, turn.official_news.clone());
            crate::log::event(
                "news_official",
                serde_json::json!({"day": turn.day, "head": crate::log::brief(&turn.official_news, 200)}),
            );
            for outage in news::parse_official(turn.day, &turn.official_news) {
                if !self.outages.iter().any(|old| {
                    old.ore == outage.ore
                        && old.from_day == outage.from_day
                        && old.to_day == outage.to_day
                }) {
                    crate::log::event(
                        "mine_outage",
                        serde_json::json!({"ore": outage.ore, "from": outage.from_day, "to": outage.to_day}),
                    );
                    self.outages.push(outage);
                }
            }
        }
        if !turn.folk_legends.is_empty()
            && self.legend_seen.get(&turn.day).map(String::as_str)
                != Some(turn.folk_legends.as_str())
        {
            self.legend_seen.insert(turn.day, turn.folk_legends.clone());
            self.treasure
                .legends
                .push((turn.day, turn.folk_legends.clone()));
        }
    }

    pub fn ore_on_outage(&self, ore: &str, day: i64) -> bool {
        self.outages
            .iter()
            .any(|outage| outage.ore == ore && day >= outage.from_day && day <= outage.to_day)
    }

    fn absorb_failures(&mut self, turn: &Turn) {
        for (id, ok) in &turn.last_action_results {
            if *ok {
                self.failures.insert(*id, 0);
                continue;
            }
            let count = self.failures.entry(*id).or_insert(0);
            *count = count.saturating_add(1);
            if let Some(issued) = self.last_issued.get(id) {
                if issued.action == "build" {
                    if let (Some(target), Some(name)) = (issued.target, issued.name.clone()) {
                        let key = (*id, target, name.clone());
                        let fails = self.build_fails.entry(key).or_insert(0);
                        *fails = fails.saturating_add(1);
                        if *fails >= 2 && self.blacklisted_builds.insert((target, name.clone())) {
                            crate::log::event(
                                "build_blacklisted",
                                serde_json::json!({"target": target, "name": name, "role": id}),
                            );
                        }
                    }
                }
            }
        }
    }

    fn absorb_llm_and_cmd(&mut self, turn: &Turn) {
        if self.task.active {
            // Task responses are one-shot per (session, request round), not per
            // response string. Identical legitimate output in a later session
            // is therefore consumed, while an echoed payload for the same
            // request cannot advance the state machine twice.
            let llm_key = self.task.llm_request_round;
            let llm_new = !turn.llm_resp.is_empty()
                && llm_key.is_some()
                && llm_key != self.task.llm_consumed_request_round;
            let cmd_key = self.task.cmd_request_round;
            let cmd_new = !turn.last_cmd_result.is_empty()
                && cmd_key.is_some()
                && cmd_key != self.task.cmd_consumed_request_round;
            if llm_new {
                self.task.llm_consumed_request_round = llm_key;
                // The round the ANSWER arrives in is the one round the judger
                // will not run a command for (see `task::plan_pioneer`), so it
                // has to be remembered: a round number is all the planner gets.
                self.task.llm_resp_round = Some(turn.round_no);
                crate::log::event(
                    "llm_resp",
                    serde_json::json!({"session": self.task.session_id, "requestRound": llm_key, "chars": turn.llm_resp.len()}),
                );
                crate::brain::task::on_llm_resp(self, &turn.llm_resp);
            }
            if cmd_new {
                self.task.cmd_consumed_request_round = cmd_key;
                crate::log::event(
                    "cmd_result",
                    serde_json::json!({"session": self.task.session_id, "requestRound": cmd_key, "chars": turn.last_cmd_result.len()}),
                );
                crate::brain::task::on_cmd_result(self, &turn.last_cmd_result);
            }

            let just_submitted = matches!(self.task.stage, TaskStage::WaitingSubmit { .. });
            if just_submitted && turn.error_codes.iter().any(|code| *code == 2) {
                self.task.post_submit_error = true;
                self.task.wrong_answers = self.task.wrong_answers.saturating_add(1);
                self.task.stage = TaskStage::Planning;
                self.task.submitted_round = None;
                self.task.phase_missing_rounds = 0;
                self.task.point_closed_round = None;
                let failed_type = self.task.task_type.clone();
                self.drop_sop_for(&failed_type);
                // Fast abandon. The opponent's edge in issue #15 was that it
                // dropped a failing task immediately and "把开拓者投入防御",
                // while all five of our sessions burned their entire timeout.
                // Three rejected answers is enough evidence that the fourth
                // attempt is not the one; the pioneer is worth more on the wall
                // line than on a task that has already failed three times.
                if self.task.wrong_answers >= MAX_WRONG_ANSWERS {
                    self.finish_task(false, "wrong_answers");
                }
            } else if self.task.submitted_round.is_some() && !turn.error_codes.is_empty() {
                self.task.post_submit_error = true;
            }
        } else {
            let llm_new = !turn.llm_resp.is_empty() && turn.llm_resp != self.seen_llm_resp;
            if llm_new {
                self.seen_llm_resp = turn.llm_resp.clone();
                crate::log::event(
                    "llm_resp",
                    serde_json::json!({"channel": "treasure", "chars": turn.llm_resp.len()}),
                );
                if self.treasure.phase != TreasurePhase::Done {
                    crate::brain::treasure::on_llm_resp(self, &turn.llm_resp, turn.round_no);
                }
            }
        }
    }

    fn absorb_task_events(&mut self, turn: &Turn) {
        if !self.task.active {
            return;
        }
        // Task description arrived.
        if self.task.description.is_empty() && !turn.phase_task.is_empty() {
            self.task.description = turn.phase_task.clone();
            self.task.description_round = turn.round_no;
            self.task.stage = TaskStage::Planning;
            crate::log::event(
                "task_started",
                serde_json::json!({
                    "round": turn.round_no,
                    "timeout": self.task.timeout_round,
                    "head": crate::log::brief(&self.task.description, 200),
                }),
            );
        }
        let ended_by_timeout = turn.error_codes.iter().any(|code| *code == 1)
            || turn.round_no >= self.task.timeout_round;
        if ended_by_timeout {
            self.retire_dead_task_point();
            self.finish_task(false, "timeout");
            return;
        }

        // A transient empty phaseTask is not completion (#3 regression). Only
        // evaluate closure after an answer was submitted, and require three
        // independent signals over subsequent rounds.
        if let Some(submitted_round) = self.task.submitted_round {
            if turn.phase_task.is_empty() {
                self.task.phase_missing_rounds = self.task.phase_missing_rounds.saturating_add(1);
            } else {
                self.task.phase_missing_rounds = 0;
            }
            let point_closed = self
                .task
                .point
                .and_then(|point| turn.player_tasks.iter().find(|task| task.pos == point))
                .map(|task| !task.is_valid || task.cooldown_rounds > 0)
                .unwrap_or(false);
            if point_closed && self.task.point_closed_round.is_none() {
                self.task.point_closed_round = Some(turn.round_no);
            }
            let confirmed = turn.round_no > submitted_round
                && self.task.point_closed_round.is_some()
                && self.task.phase_missing_rounds >= 2
                && !self.task.post_submit_error
                && turn.error_codes.is_empty();
            crate::log::event(
                "task_closure_probe",
                serde_json::json!({
                    "session": self.task.session_id,
                    "round": turn.round_no,
                    "pointClosed": self.task.point_closed_round.is_some(),
                    "phaseMissing": self.task.phase_missing_rounds,
                    "clean": !self.task.post_submit_error && turn.error_codes.is_empty(),
                    "confirmed": confirmed,
                }),
            );
            if confirmed {
                self.finish_task(true, "confirmed_success");
            }
        }
    }

    /// A session that timed out without ever running a command never had a
    /// window to use, so the point it was accepted at is dead ground: the
    /// pioneer is standing in its neighbourhood, the point still reads
    /// `isValid`, and nothing about the judger's sandbox will have changed by
    /// the next round. Re-accepting it walks straight back into the same
    /// timeout — issue #21's four timed-out sessions, each burning its whole
    /// `timeout_rounds` budget on the identical dead point while a live one sat
    /// elsewhere on the map.
    ///
    /// This is NOT the `cmdRounds=0` premature exit (which stays fixed): the
    /// judger ended this session first — the refusal is recorded afterwards, and
    /// only for a point whose session never got a single command out. A session
    /// that DID run commands hit a working window and is free to be retried.
    fn retire_dead_task_point(&mut self) {
        if !self.task.cmd_history.is_empty() {
            return;
        }
        let Some(point) = self.task.point else {
            return;
        };
        let until = crate::brain::task::turn_end_of_day(self.task.accepted_round);
        crate::log::event(
            "task_point_retired",
            serde_json::json!({
                "session": self.task.session_id,
                "point": point,
                "round": self.task.accepted_round,
                "until": until,
            }),
        );
        self.task_refusals.insert(point, until);
    }

    pub fn finish_task(&mut self, success: bool, reason: &str) {
        if self.task.active {
            crate::log::event(
                "task_ended",
                serde_json::json!({
                    "session": self.task.session_id,
                    "success": success,
                    "reason": reason,
                    "wrongAnswers": self.task.wrong_answers,
                    "cmdRounds": self.task.cmd_history.len(),
                    "bestAnswer": crate::log::brief(&self.task.best_answer, 120),
                }),
            );
        }
        // A lack of an error is not proof that a script worked, so the
        // multi-signal success probe still owns the cache. But a script the
        // sandbox RAN and answered with is reusable whether or not the answer
        // was accepted — and that reuse is the whole point of 自进化: a session
        // that has to spend an LLM round-trip before its first command cannot
        // finish inside the 2-15 round timeouts of issues #18/#19. Only the
        // command that actually produced an answer qualifies (`sop_cmd`), never
        // one that was refused or timed out.
        if success || self.task.sop_cmd.is_some() {
            self.cache_sop();
        }
        self.task = TaskSession::default();
    }

    fn extract_sop(&self) -> Option<SopEntry> {
        let script = self
            .task
            .sop_cmd
            .clone()
            .or_else(|| self.task.cmd_history.last().cloned())?;
        if script.is_empty() {
            return None;
        }
        let keywords = keywords_of(&self.task.description);
        if keywords.is_empty() {
            return None;
        }
        Some(SopEntry {
            task_type: self.task.task_type.clone(),
            keywords,
            template: script,
        })
    }

    /// Cache the last executed task command as an SOP (deduped) once it has
    /// produced an answer. Called at task end (only for answers that were never
    /// rejected), so the NEXT task of the same type can reuse the script.
    pub fn cache_sop(&mut self) {
        if self.task.best_answer.is_empty() {
            return; // the command never produced an answer: don't cache it
        }
        let Some(entry) = self.extract_sop() else {
            return;
        };
        if !self
            .sop_cache
            .iter()
            .any(|old| old.template == entry.template)
        {
            self.sop_cache.push(entry);
        }
        if self.sop_cache.len() > 32 {
            self.sop_cache.remove(0);
        }
    }

    /// A cached script bound to `description`, ready to execute.
    ///
    /// Matching is by task FINGERPRINT — the same task type *and* a keyword
    /// overlap of at least half the smaller keyword set — never by task type
    /// alone. The same type covers tasks whose inputs differ, and replaying
    /// the wrong one produces a confidently wrong answer, which costs the
    /// whole task reward. When the fingerprint or a parameter binding is
    /// missing the caller falls back to a fresh LLM call.
    pub fn find_sop(&self, task_type: &str, description: &str) -> Option<String> {
        let keywords = keywords_of(description);
        if keywords.is_empty() {
            return None;
        }
        self.sop_cache
            .iter()
            .rev()
            .filter(|entry| entry.task_type == task_type)
            .map(|entry| {
                let overlap = entry
                    .keywords
                    .iter()
                    .filter(|kw| keywords.contains(*kw))
                    .count();
                (entry, overlap)
            })
            .filter(|(entry, overlap)| {
                *overlap >= 3 && *overlap * 2 >= entry.keywords.len().min(keywords.len())
            })
            .max_by_key(|(_, overlap)| *overlap)
            .and_then(|(entry, overlap)| {
                let script = entry.bind(description);
                crate::log::event(
                    "sop_reuse",
                    serde_json::json!({
                        "taskType": task_type,
                        "overlap": overlap,
                        "bound": script.is_some(),
                    }),
                );
                script
            })
    }

    /// Drop every cached SOP for a task type whose answer was just rejected, so
    /// the next task of that type does not instantly reuse the broken script.
    pub fn drop_sop_for(&mut self, task_type: &str) {
        if task_type.is_empty() {
            return;
        }
        let before = self.sop_cache.len();
        self.sop_cache.retain(|entry| entry.task_type != task_type);
        if self.sop_cache.len() != before {
            crate::log::event("sop_dropped", serde_json::json!({"taskType": task_type}));
        }
    }

    fn absorb_treasure_events(&mut self, turn: &Turn) {
        use TreasurePhase::*;
        if turn.last_summon_result != 0 {
            crate::log::event(
                "treasure_result",
                serde_json::json!({"code": turn.last_summon_result, "day": turn.day, "round": turn.round_no}),
            );
        }
        match turn.last_summon_result {
            1 | 4 => {
                // Treasure obtained (or emptied by the other side): stop.
                self.treasure.phase = Done;
            }
            2 => {
                // "No treasure here / not open yet". NOTE: a legal summon
                // CONSUMES the sacrifice items regardless of outcome, so
                // retries are expensive (15g per item). One in-place retry
                // with the day pushed forward, then re-ask the LLM; hard cap
                // at 4 total summons so we never loop forever.
                self.treasure.summon_attempts = self.treasure.summon_attempts.saturating_add(1);
                if self.treasure.summon_attempts >= 4 {
                    self.treasure.phase = Done;
                } else if self.treasure.summon_attempts >= 2 {
                    self.treasure.plan = None;
                    self.treasure.time_feedback = true;
                    self.treasure.phase = if self.treasure.ask_attempts >= 3 {
                        Done
                    } else {
                        Idle
                    };
                } else if let Some(plan) = &mut self.treasure.plan {
                    plan.open_day = turn.day.max(plan.open_day + 1);
                    self.treasure.phase = HavePlan;
                } else {
                    self.treasure.phase = Idle;
                }
            }
            3 => {
                // Wrong sacrifice items: ask the LLM again with feedback.
                self.treasure.wrong_item_rounds = self.treasure.wrong_item_rounds.saturating_add(1);
                self.treasure.plan = None;
                if self.treasure.wrong_item_rounds + self.treasure.ask_attempts >= 4 {
                    self.treasure.phase = Done;
                } else {
                    self.treasure.phase = Idle;
                }
            }
            _ => {}
        }
    }

    pub fn is_prompt_free(&self) -> bool {
        self.llm_used_today < 3
    }

    pub fn consume_prompt_budget(&mut self) {
        self.llm_used_today = self.llm_used_today.saturating_add(1);
    }

    pub fn consume_summon_order(&mut self) {
        self.summon_orders_today = self.summon_orders_today.saturating_add(1);
    }
}

/// Cheap keyword extraction for SOP matching over Chinese/ASCII task text.
pub fn keywords_of(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let stop = [
        "的", "了", "请", "你", "我", "在", "和", "与", "是", "个", "任务", "查询",
    ];
    for token in text.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() || token.chars().count() < 2 {
            continue;
        }
        if stop.contains(&token) {
            continue;
        }
        let lower = token.to_lowercase();
        if !out.contains(&lower) {
            out.push(lower);
        }
    }
    // Chinese text has no spaces: also add 2-gram slices of CJK runs.
    let chars: Vec<char> = text.chars().collect();
    let mut run: Vec<char> = Vec::new();
    let flush = |run: &mut Vec<char>, out: &mut Vec<String>| {
        if run.len() >= 4 {
            for window in run.windows(2) {
                let gram: String = window.iter().collect();
                if !out.contains(&gram) {
                    out.push(gram);
                }
            }
        }
        run.clear();
    };
    for ch in chars {
        if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            run.push(ch);
        } else {
            flush(&mut run, &mut out);
        }
    }
    flush(&mut run, &mut out);
    if out.len() > 40 {
        out.truncate(40);
    }
    out
}
