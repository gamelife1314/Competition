//! Cross-round bot memory: news log, mine outages, task session, treasure
//! hunt, LLM budgets and failure feedback.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::brain::coach::Coach;
use crate::brain::news;
use crate::model::{chebyshev, Turn};
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

/// Rejected answers after which a task is abandoned rather than retried.
///
/// The ceiling has not moved; what it counts has (P1-1). It used to count
/// rejections, so the four informative verdicts issue #10's opponent retried
/// against — each naming a different missing key — ended our session at the
/// third while the judger still had something new to say. It now counts
/// consecutive rejections that carried **no new text** (`TaskSession::
/// wrong_answers`), which is the only case where a fourth rewrite is provably
/// not the one: the judger has already said this exact thing. A session with no
/// rejection text at all is back to the plain three-strike ceiling, because
/// there is nothing to compare and the old rule is the only evidence available.
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
    /// Which of the task book's three lanes this session is (P1-4). Fixed at
    /// accept time from `taskType` and never revised: the log line that says
    /// why a session was worth its rounds has to name the same lane the
    /// ordering used to pick it.
    pub kind: crate::brain::task::TaskKind,
    pub description: String,
    pub description_round: i64,
    pub stage: TaskStage,
    pub best_answer: String,
    pub cmd_history: Vec<String>,
    pub result_history: Vec<String>,
    /// Consecutive rejections that carried no new information (P1-1). Reset to
    /// zero the moment the judger says something it has not said before, and
    /// incremented when it repeats itself — see [`MAX_WRONG_ANSWERS`].
    pub wrong_answers: i32,
    /// Every rejected submission this session, repeat verdicts included.
    ///
    /// `wrong_answers` is now a *give-up* counter and can go back to zero, so
    /// nothing that means "how many answers has this session burned" may read
    /// it: the shape flip in `task::plan_pioneer` alternates the submitted
    /// payload after any rejection, and the retry prompt states how many
    /// submissions have been judged wrong. Both want this count, which only
    /// ever grows.
    pub rejections: i32,
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
    /// The judger's own words about the answers it rejected, in arrival order
    /// and deduplicated.
    ///
    /// Every rejection comes back as a coarse `errorCode` (2 = "答案错误，不正确
    /// **或不完全正确**") plus one `description` that says WHICH way it was
    /// wrong — `MissingNamedInput: city`, `键值比对不通过: $/token: 缺少键`. That
    /// description was parsed into `Turn::error_descriptions` and written to the
    /// log, and then dropped; `build_prompt` retried on `schema_gaps` /
    /// `discovered_fields` instead, both of which are *guesses* at the schema
    /// derived from a placeholder description or from the LLM's own `FIELDS:`
    /// line. So the one authoritative piece of feedback the judger ever gives
    /// never reached the retry, and three rejected answers were three blind
    /// rewrites (docs/FAILURE-ANALYSIS-2026-09-14.md §3.1). This is that text,
    /// replayed verbatim into the next prompt.
    pub rejection_feedback: Vec<String>,
    /// Fields the task text asked for that the produced answer did not carry.
    /// Fed back into the next prompt so the retry can close the gap.
    pub schema_gaps: Vec<String>,
    /// Fields the answer carried that the task never asked for. The judger
    /// scores the object against the schema it named, so an invented field is
    /// wrong on its own (`{"city":…,"task_id":…,"status":"completed"}` — issue
    /// #22's session 5). Fed back into the next prompt like `schema_gaps`.
    pub schema_extras: Vec<String>,
    /// The output schema the sandbox task file actually demands, echoed back
    /// by the script itself through a `FIELDS:` line (P0-2). The placeholder
    /// `phaseTask` text ("请阅读task_X.md，获取任务信息") names no fields, so the
    /// description-derived `expected_fields` is only a fallback guess — the
    /// schema lives inside the sandbox and this is the channel that reads it.
    pub discovered_fields: Vec<String>,
    /// The output schema the sandbox task file declared, echoed back on a
    /// `SCHEMA: <json>` line (P1-2).
    ///
    /// `FIELDS:` names the fields; this says which of them are mandatory and
    /// is the only signal strong enough to reject an answer for missing a
    /// SINGLE field — `expected_fields` guesses from the task text, and a
    /// one-field guess that is wrong rejects a correct answer. Absent line,
    /// absent schema: everything downstream degrades to the `FIELDS:` and
    /// description behaviour unchanged.
    pub discovered_schema: Option<DiscoveredSchema>,
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
    /// The cached template this session is running, when it started from the
    /// SOP fast path. A rejection is charged against exactly this entry
    /// (P1-3) — never against templates that did not run.
    pub sop_used_template: Option<String>,
    /// Stage 2 of a two-stage SOP (P1-3): the cached answer script queued
    /// behind the reconnaissance command the pair ran first. Consumed on the
    /// next planning round, so a session that took the fast path runs
    /// `explore` → `answer` and never re-enters the exploration.
    pub sop_pending_answer: Option<String>,
    /// Consecutive sandbox runs that returned without an `ANSWER:` line.
    ///
    /// The population issues #113-#115 are made of: sessions that spent their
    /// whole `timeoutRounds` budget sending reconnaissance scripts and ended
    /// `reason=timeout, rejections=0, wrongAnswers=0` — no submission, no
    /// rejection, nothing to retry against, because no answer ever existed.
    /// Issue #115 alone ran 14 commands across 6 sessions with 13 of them
    /// answer-less. This counter is what lets `build_prompt` say "you have
    /// already done the reconnaissance" with a number in it instead of hoping
    /// the model notices on its own.
    pub no_answer_rounds: i32,
    /// What the task point advertised when the session opened: its
    /// `scoreReward` and `goldReward` (WORKFLOW_REQUEST §16.1).
    ///
    /// 任务书 ch.6 pays `任务积分奖励 × 通过率` (gold: `任务金币奖励 × 通过率`), so
    /// `playerTasks[]`'s own pair is the CEILING of what this session can earn
    /// and the only figure the log can honestly carry. It sat in the turn and
    /// was read by nobody: a session worth 300 points and one worth 20 produced
    /// identical records, which is exactly the pair of cases "the task line
    /// earned nothing" has to be told apart from — one is a scoring failure,
    /// the other is a scheduling one, and they call for opposite work. Held on
    /// the session rather than re-read from the turn because the point can be
    /// retired (`retire_dead_task_point`) before the record that reports it is
    /// written.
    pub score_reward: i64,
    pub gold_reward: i64,
}

/// The answer schema a sandbox script read out of the task file and echoed on
/// a `SCHEMA:` line (P1-2).
///
/// A `FIELDS:` line is a bare list; this is the schema itself. Where the two
/// disagree the schema wins, and it is the only source that may reject an
/// answer for missing a single field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveredSchema {
    /// Every field the answer may carry — the extras check reads this.
    pub fields: Vec<String>,
    /// The subset the answer must carry — the missing-field check reads this.
    /// Empty means "every field is required".
    pub required: Vec<String>,
}

/// A cached SOP bound to a live task, with both stages resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SopMatch {
    /// The cache key of the entry that matched (`SopEntry::template`). A later
    /// rejection is charged against exactly this entry.
    pub template: String,
    /// Stage 1 — reconnaissance. Runs first; `answer` is queued behind it.
    pub explore: Option<String>,
    /// Stage 2 — the script that produced an answer last time.
    pub answer: String,
}

/// A cached, parameterised script for one task fingerprint. The body keeps
/// `{{name}}` placeholders where the values that differ between two tasks of
/// the same kind go, so replaying it on a *different* task cannot silently
/// reuse the old task's inputs.
#[derive(Debug, Clone, Default)]
pub struct SopEntry {
    pub task_type: String,
    pub keywords: Vec<String>,
    /// Stage 2 of the pair: the script that produced an answer. It is also the
    /// entry's cache key — every strike, eviction and `last_rejected` comparison
    /// is made against this string.
    pub template: String,
    /// Stage 1 of the pair (P1-3): the reconnaissance command the cached
    /// session ran BEFORE the one that answered.
    ///
    /// A task of a kind we have never seen is explored in full — the sandbox
    /// has to be searched, the task file found and the output schema read
    /// before any answer exists. Once a session has done that work, the words
    /// it did it with are as reusable as the answer script: replaying the
    /// reconnaissance first (one cached command, no LLM round trip) and then
    /// the answer gives the second task of that kind a 2–3 round session
    /// instead of a repeat of the whole exploration. `None` when the session
    /// answered with its first command — there is no separate exploration to
    /// replay then, and the answer script alone is the whole pair.
    pub explore: Option<String>,
    /// Consecutive rejections charged against answers this template produced
    /// (P1-3). One strike keeps the entry — reuse with feedback, because a
    /// single rejection can be the task's input differing, not the script's
    /// logic; the second consecutive strike evicts it. A template whose
    /// answer was never judged wrong is never touched, and a rejection earned
    /// by an LLM-written script is never charged to a template that did not
    /// run — the old behaviour wiped every template of the type on any
    /// rejection, which is how good SOPs kept dying with bad tasks.
    pub rejections: i32,
    /// The bound script the judger last rejected. Reuse is only allowed when
    /// binding produces DIFFERENT bytes (new parameters, new task input):
    /// replaying the exact script that already failed is the same wrong
    /// answer with extra steps, so `find_sop` skips it and the LLM takes over.
    pub last_rejected: Option<String>,
}

impl SopEntry {
    /// Bind this template to a new task description. A template without
    /// placeholders is reused verbatim; one WITH placeholders is only reused
    /// when every placeholder can be resolved from the description, because
    /// running it with the previous task's values would answer a different
    /// question confidently.
    pub fn bind(&self, description: &str) -> Option<String> {
        bind_template(&self.template, description)
    }

    /// Bind BOTH stages of the pair to a new task (P1-3).
    ///
    /// `None` when the answer script cannot be bound — the pair is then
    /// unusable and the LLM takes the task, exactly as before. An `explore`
    /// stage that cannot be bound is dropped rather than failing the pair: it
    /// is an optimisation, and answering from the cached answer script alone is
    /// still the fast path. A template WITHOUT placeholders binds verbatim, so
    /// a pair cached from a task whose inputs never varied still replays.
    pub fn bind_pair(&self, description: &str) -> Option<SopMatch> {
        let answer = self.bind(description)?;
        Some(SopMatch {
            template: self.template.clone(),
            explore: self
                .explore
                .as_ref()
                .and_then(|template| bind_template(template, description)),
            answer,
        })
    }
}

/// Bind one template to `description`: verbatim when it has no placeholders,
/// otherwise only when every placeholder resolves.
fn bind_template(template: &str, description: &str) -> Option<String> {
    if !template.contains("{{") {
        return Some(template.to_string());
    }
    let mut script = template.to_string();
    for name in placeholders(template) {
        let value = param_value(description, &name)?;
        script = script.replace(&format!("{{{{{name}}}}}"), &value);
    }
    Some(script)
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

/// Who is asking for the round's LLM prompt. Ranked highest first; see
/// [`BotState::request_prompt`] for the ordering and the reason for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptPurpose {
    /// The day's official news → the price trend.
    News,
    /// The folk legends → the treasure.
    Treasure,
    /// The task line: self-evolution and the reasoning lanes.
    Task,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TreasurePhase {
    #[default]
    Idle,
    AskedLlm {
        round: i64,
    },
    HavePlan,
    /// A `summonTreasure` has gone out and its verdict has not come back yet
    /// (P2-2).
    ///
    /// Without this the phase stayed `HavePlan` after the summon, so the very
    /// next round the pioneer was still standing beside the altar with the
    /// items still in its pack and the day still open — and the planner fired
    /// the summon again, and again, once per round. `summon_attempts` counted
    /// those repeats rather than real summons, so the "4 attempts" cap was
    /// reached in four rounds of one gamble, and a verdict of 2 ("not open
    /// yet", which pushes the day and retries in place) never got its retry.
    /// The line looked alive in the log and could not survive its own first
    /// summon.
    Summoned {
        round: i64,
    },
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

    /// Roles that have turned for home for tonight's dusk lock-in.
    ///
    /// Latched rather than recomputed each round, because every dusk deadline in
    /// this planner is measured from where the role currently IS (`dusk_recall_round`,
    /// `pioneer_recall_round`, `preposition_round`). A deadline that shrinks as
    /// the role walks in un-fires the moment it gets closer: the role arrives,
    /// the deadline moves past the current round, the economy steps below the
    /// checkpoint take it straight back out, and it arrives again. That two-cell
    /// oscillation is what the 2026-09-14(b) analysis measured — `away`
    /// alternating between two cells for fifteen consecutive rounds while
    /// `wall_gate_open` never cleared. A role that has committed stays committed
    /// for the rest of the day; the set is cleared at day rollover.
    pub dusk_home: HashSet<i64>,

    /// day -> official news text (dedup)
    pub official_seen: HashMap<i64, String>,
    /// day -> folk legend text (dedup)
    pub legend_seen: HashMap<i64, String>,
    pub outages: Vec<Outage>,
    /// Expected price moves read off the official news (P1-1). Kept beside the
    /// outages because the two are the same text read twice: the outage is what
    /// the ore does, the outlook is what the market does about it.
    pub outlooks: Vec<news::Outlook>,
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
    /// Gold the accepted task points advertised, banked when a session was
    /// confirmed successful (P2-4). An upper bound on task income — the judger
    /// pays `奖励 × 通过率` and does not tell us the pass rate — but the only
    /// figure our own log can carry, and the one the attribution dashboard
    /// needs to separate "the task line earned something" from "it did not".
    pub task_gold_earned: i64,
    /// Threat points per compass sector around our station (P2-1). Fed by
    /// `absorb_threat` and read by `threatened_sectors`, which is what decides
    /// where — and whether — the second wall layer is built.
    pub threat_sectors: [i64; 9],
    /// Last HP seen per wall cell, so the next drop can be charged to a sector.
    pub wall_hp_seen: HashMap<Pos, i64>,
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
    /// The day's news read: the official news goes to the model once, and the
    /// keyword scan stands until — and unless — it answers (P1-1).
    pub news: news::NewsRead,

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

    /// P1-4 withdrawal hysteresis (additive; owned by the night-side change).
    /// Controllers currently in a holdout: they have been withdrawn from their
    /// gun for long enough that the pairing should stop handing it back to
    /// them until they actually recover. Membership is maintained by
    /// `brain::night` once that change lands; an empty set reproduces the
    /// pre-hysteresis behaviour exactly.
    pub withdraw_holdout: HashSet<i64>,
    /// Controller id -> last round a robot was seen inside its threat radius.
    /// The hysteresis uses this to tell "chronically threatened" (stay in
    /// holdout) from "the danger has passed" (eligible to man a gun again).
    pub withdraw_last_threat: HashMap<i64, i64>,

    /// D1 gate stays open until every controller has reached an inside/tower
    /// stand, then remains sealed for the rest of the half.
    pub wall_gate_sealed: bool,

    /// The ring cell this day leaves open — `brain::route`'s planned entrance,
    /// latched at daybreak.
    ///
    /// `None` means "the planner had no evidence" (a board with no zones on it)
    /// and every reader falls back to `route::legacy_entrance`, the fixed
    /// `(xmax + 2, ymin - 1)` cell. Latched rather than recomputed per round
    /// because the errand set moves during the day — the stone demand falls as
    /// the ring goes up, and a mine can go on outage — and an entrance that
    /// moved mid-afternoon would re-open a cell the crew had just walled.
    pub gate_cell: Option<Pos>,

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
    /// The day our station first went missing, latched once. `score3` keeps
    /// every day up to but NOT including this one (存活系数 drops to 0 on the
    /// day the base falls and after), so the rounds that follow cannot be
    /// recomputed from the current day alone — they need the day it happened.
    pub station_fell_day: Option<i64>,
    /// Last-written signatures of the change-gated blocks in the `round`
    /// record (`log::changed`). Purely a logging concern, but it has to survive
    /// from one round to the next, which makes this the only place it can live.
    pub log_sigs: crate::log::LogSigs,

    /// 内置教练（`brain::coach`）：不靠环境变量、不靠外部 workflow，从局势里读出
    /// 证据自己移动三个策略开关。它随半场一起活着（见 `observe` 的重置分支）。
    pub coach: Coach,
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
            // The coach is the one piece of memory that MUST survive the wipe:
            // what the last half taught it is exactly what the next half should
            // start from (evidence decays by half inside `end_half`).
            let mut coach = std::mem::take(&mut self.coach);
            *self = BotState::default();
            self.task_session_seq = task_session_seq;
            coach.end_half(turn.day, turn.round_no);
            self.coach = coach;
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
            // A new day, a new entrance: the errand set is re-read from the
            // board (ore prices move, veins go on outage) and the opening is
            // placed where TODAY's work is. See `brain::route::entrance`.
            self.gate_cell = None;
            // Every dusk commitment was discharged by last night's recall.
            self.dusk_home.clear();
            // A new day's news is a new question, and the day's read starts over
            // with it — including the attempt count, so one bad answer on
            // Monday does not cost the read on Tuesday.
            self.news = news::NewsRead::default();
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

        // Where the base is actually being hurt (P2-1). Runs before planning
        // so the day's wall blueprint reads this round's damage.
        self.absorb_threat(turn);

        // Task / treasure feedback channels.
        self.absorb_llm_and_cmd(turn);
        self.absorb_task_events(turn);
        self.absorb_treasure_events(turn);

        // Adaptive coach: reads the situation (both sides' station/wall HP, the
        // night's firepower gap, how many summon orders went out today) and
        // moves its own switches. Runs last so it sees the same numbers the
        // planners are about to.
        let orders_today = self.summon_orders_today;
        self.coach.observe(turn, orders_today);
    }

    /// Install the process-level coach (called once from `main.rs`; see
    /// `brain::coach::install`). Tests never call it, so `BotState::default()`
    /// stays pure in-memory and hermetic.
    pub fn install_coach(coach: Coach) {
        Self::locked().coach = coach;
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
            // The other half of the same text: which way it points each ore's
            // price. Logged per reading, because "the miner walked past the
            // copper" is only answerable next to "the news said copper was
            // about to get cheap".
            // A day the model has already read for is the model's: the scan
            // does not get to write over a reading it was the fallback for.
            // (A re-announcement of the same day's news is still "the same
            // day", so this also stops a second publication of one day's news
            // from re-pricing it from the words alone.)
            if self.news.read_day != Some(turn.day) {
                for outlook in news::price_outlook(turn.day, &turn.official_news) {
                    if self.outlooks.iter().any(|old| *old == outlook) {
                        continue;
                    }
                    crate::log::event(
                        "news_outlook",
                        serde_json::json!({
                            "ore": outlook.ore,
                            "direction": news::direction_word(outlook.direction),
                            "confidence": outlook.confidence,
                            "days": outlook.days,
                            "day": outlook.day,
                            "source": "keyword",
                        }),
                    );
                    self.outlooks.push(outlook);
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
            // The legend's own words, once a day. They were stored here and
            // never written, which made the one input the altar is inferred
            // from the one input the log did not carry — see `legend_record`.
            crate::log::event(
                "news_legend",
                crate::log::legend_record(turn.day, &turn.folk_legends),
            );
        }
    }

    pub fn ore_on_outage(&self, ore: &str, day: i64) -> bool {
        self.outages
            .iter()
            .any(|outage| outage.ore == ore && day >= outage.from_day && day <= outage.to_day)
    }

    /// The price outlook for this ore in force on `day`: the most recent
    /// reading published on or before it, and the loudest one if a single day
    /// carried several.
    ///
    /// A reading with no horizon never expires: the news that a mine is shut for
    /// two days is still the reason the ore is dear on the second of them, and
    /// the day the outage ends is the day the price is highest — waiting for the
    /// outage to pass before acting on it is the mistake the outlook exists to
    /// avoid. That is every keyword reading, and it is the reading the miner
    /// used before the model was asked. A reading that claims a horizon expires
    /// at the end of it ([`news::Outlook::days`]): the model that said "iron is
    /// dear for the next two days" has not said anything about the fifth, and
    /// pricing the fifth off it is the miner acting on a claim nobody made.
    pub fn price_outlook(&self, ore: &str, day: i64) -> Option<&news::Outlook> {
        self.outlooks
            .iter()
            .filter(|outlook| {
                outlook.ore == ore
                    && outlook.day <= day
                    && (outlook.days == 0 || day < outlook.day + outlook.days)
            })
            .max_by_key(|outlook| (outlook.day, outlook.confidence))
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
        // The news read first, and by SHAPE rather than by seniority.
        //
        // Three consumers share one response field, and the news is the only one
        // whose answer is a price reading — a task's answer is a shell script
        // and a treasure's is `{"pos":…}`, so neither can be mistaken for this.
        // Claiming it on the shape is what keeps the ranking from having to
        // serialise the channel: a script that arrives while the news read is
        // waiting falls through to the task branch below, unread and unclaimed,
        // which is exactly where it belongs.
        if self.news.awaits_response()
            && !turn.llm_resp.is_empty()
            && turn.llm_resp != self.seen_llm_resp
            && news::parse_outlook(turn.day, &turn.llm_resp).is_some()
        {
            self.seen_llm_resp = turn.llm_resp.clone();
            crate::log::event(
                "llm_resp",
                serde_json::json!({"channel": "news", "chars": turn.llm_resp.len()}),
            );
            news::on_llm_resp(self, turn.day, &turn.llm_resp);
            // Merged prompt: if treasure was also awaiting (both asked in one
            // prompt), process the treasure part of the response too.
            if matches!(self.treasure.phase, TreasurePhase::AskedLlm { .. })
                && self.treasure.phase != TreasurePhase::Done
            {
                crate::brain::treasure::on_llm_resp(self, &turn.llm_resp, turn.round_no);
            }
            return;
        }
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
                // The character count alone could not answer the only question
                // this record exists for. Issues #111-#115 each carry 11-17
                // `cmd_result` lines that say a script returned 2.4k-5.5k
                // characters and not one of them says WHAT it returned, so
                // "the model never printed an `ANSWER:` line" — the mechanism
                // behind 13 `task_cmd_failed` in #115 and every session in the
                // batch ending at zero submissions — was invisible. `answer`
                // is the verdict and `head` is the evidence for it.
                let output = crate::brain::task::strip_status_line(&turn.last_cmd_result);
                crate::log::event(
                    "cmd_result",
                    serde_json::json!({
                        "session": self.task.session_id,
                        "requestRound": cmd_key,
                        "chars": turn.last_cmd_result.len(),
                        "answer": crate::brain::task::extract_answer(output).is_some(),
                        "head": crate::log::headline(output, 160),
                    }),
                );
                crate::brain::task::on_cmd_result(self, &turn.last_cmd_result);
            }

            let just_submitted = matches!(self.task.stage, TaskStage::WaitingSubmit { .. });
            if just_submitted && turn.error_codes.iter().any(|code| *code == 2) {
                // INFORMATION-DRIVEN RETRY (P1-1). The judger's rejection text
                // is the only authority on WHY an answer was wrong, and the
                // opponent's successful path (issue #10) was four retries
                // against exactly that text. A rejection that names something
                // new is progress: the counter restarts and the session keeps
                // the rest of its timeout to act on it. A rejection that
                // repeats a complaint already in hand is the judger saying the
                // same thing twice — the one piece of evidence that a further
                // rewrite is not the one — and it is what the ceiling now
                // counts. With no text at all there is nothing to compare and
                // the plain three-strike ceiling stands, unchanged.
                let informed = self.absorb_rejection_feedback(turn);
                self.task.post_submit_error = true;
                self.task.rejections = self.task.rejections.saturating_add(1);
                if informed {
                    self.task.wrong_answers = 0;
                    crate::log::event(
                        "task_retry_informed",
                        serde_json::json!({
                            "session": self.task.session_id,
                            "round": turn.round_no,
                            "feedback": self.task.rejection_feedback.len(),
                            "rejections": self.task.rejections,
                        }),
                    );
                } else {
                    self.task.wrong_answers = self.task.wrong_answers.saturating_add(1);
                }
                self.task.stage = TaskStage::Planning;
                self.task.submitted_round = None;
                self.task.phase_missing_rounds = 0;
                self.task.point_closed_round = None;
                self.charge_sop_rejection();
                // Fast abandon, unchanged in kind. The opponent's edge in issue
                // #15 was that it dropped a failing task immediately and
                // "把开拓者投入防御", while all five of our sessions burned
                // their entire timeout. `MAX_WRONG_ANSWERS` consecutive
                // rejections that taught us nothing is that evidence; the same
                // count of *informed* rejections is not, and the timeout still
                // bounds the session either way.
                if self.task.wrong_answers >= MAX_WRONG_ANSWERS {
                    self.finish_task(false, "wrong_answers");
                }
            } else if self.task.submitted_round.is_some()
                && turn.error_codes.iter().any(|code| *code != 1)
            {
                // The timeout code is not a verdict on the ANSWER. 接口文档 §1.3.2
                // has the judger settle a timed-out task on "之前提交过的通过率最高
                // 的答案" — the answer stands, the session's clock ran out — so
                // `errorCode 1` (任务超时) is not the "we were told the answer was
                // bad" evidence this flag exists to record. Measured: #205's r26
                // carries code 1 in the round after the session's last rejection
                // and r147/r161 carry it for sessions that never submitted at
                // all; nothing in the batch turned on this, which is why it went
                // unnoticed — but a code 1 landing in the same round as an
                // unanswered submission would have marked the one settleable
                // answer this line ever sees as a failure. The other four codes
                // are unchanged: 2 is the rejection, 3/4/5 are the judger saying
                // the round did not work.
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

    /// Keep the judger's verbatim reason for every `errorCode == 2` rejection
    /// in the current round. `error_codes` and `error_descriptions` are
    /// parallel and same-order (WORKFLOW_REQUEST §7.3 表 4b), so the pair is
    /// read by index; a rejection the judger described with an empty string
    /// teaches nothing and is skipped, which is exactly today's behaviour.
    ///
    /// Returns whether this round's verdict said anything the session had not
    /// already been told — the input to the information-driven retry (P1-1).
    fn absorb_rejection_feedback(&mut self, turn: &Turn) -> bool {
        let mut learned = false;
        for (index, code) in turn.error_codes.iter().enumerate() {
            if *code != 2 {
                continue;
            }
            let Some(description) = turn.error_descriptions.get(index) else {
                continue;
            };
            let text = description.trim();
            if text.is_empty() || self.task.rejection_feedback.iter().any(|old| old == text) {
                continue;
            }
            self.task.rejection_feedback.push(text.to_string());
            learned = true;
        }
        learned
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
            // The session's CEILING (WORKFLOW_REQUEST §16.1). `playerTasks[]`
            // has carried `scoreReward` / `goldReward` all along and our log
            // never recorded them, so a session worth 300 points and one worth
            // 20 read identically — and 任务书 ch.6 pays
            // `任务积分奖励 × 通过率` (gold: `任务金币奖励 × 通过率`) at the task
            // point's own price. Without the ceiling in the log, "the task line
            // earned nothing" cannot be told from "the task line was never
            // worth anything", and the two call for opposite work.
            let reward = self
                .task
                .point
                .and_then(|point| turn.player_tasks.iter().find(|task| task.pos == point));
            self.task.score_reward = reward.map(|task| task.score_reward).unwrap_or(0);
            self.task.gold_reward = reward.map(|task| task.gold_reward).unwrap_or(0);
            crate::log::event(
                "task_started",
                serde_json::json!({
                    "round": turn.round_no,
                    "timeout": self.task.timeout_round,
                    "task_kind": self.task.kind.as_str(),
                    "scoreReward": self.task.score_reward,
                    "goldReward": self.task.gold_reward,
                    "head": crate::log::brief(&self.task.description, 200),
                }),
            );
        }
        let ended_by_timeout = turn.error_codes.iter().any(|code| *code == 1)
            || turn.round_no >= self.task.timeout_round;
        if ended_by_timeout {
            self.retire_dead_task_point();
            // AN ANSWER THAT WAS NEVER REJECTED IS AN ANSWER THE JUDGER SCORED.
            // 任务书 ch.6 pays at the end of the task, and 接口文档 §timeoutRounds
            // says a timed-out task is settled on "之前提交过的通过率最高的答案"
            // — the session does not have to be confirmed by the closure probe
            // to have earned. Measured across issues #201/#203/#204/#205: every
            // session of every match ended `timeout` (表 3a, 7/7), usually one
            // to two rounds after its last submission (#205 session 3 submitted
            // at r42 and r43 and the judger never rejected either; the task's
            // own timeout ended it at r44), while `taskGoldEarned` stayed 0 for
            // the whole match because `bank_task_reward` hung off
            // `confirmed_success` alone. The counter was therefore blind by
            // construction on the only ending these sessions ever take.
            if self.task.submitted_round.is_some() && !self.task.post_submit_error {
                self.bank_task_reward(turn, "timeout_unrejected");
            }
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
                self.bank_task_reward(turn, "confirmed_success");
                self.clear_sop_strikes();
                self.finish_task(true, "confirmed_success");
            }
        }
    }

    /// Bank what the task point advertised (P2-4).
    ///
    /// 任务书 ch.6 pays `任务奖励 × 通过率` and the judger never tells us the
    /// pass rate, so the point's own `goldReward` is the upper bound of the
    /// session's income and the only figure our log can honestly carry. Without
    /// it the round record attributes the running total to `score2`/`score3`
    /// and lumps task gold in with everything else, so "did the task line earn
    /// anything this match" — the question every task fix is judged by — had no
    /// answer in the log at all.
    fn bank_task_reward(&mut self, turn: &Turn, via: &str) {
        let Some(point) = self.task.point else {
            return;
        };
        let Some(reward) = turn
            .player_tasks
            .iter()
            .find(|task| task.pos == point)
            .map(|task| task.gold_reward)
        else {
            return;
        };
        if reward <= 0 {
            return;
        }
        self.task_gold_earned = self.task_gold_earned.saturating_add(reward);
        crate::log::event(
            "task_reward",
            serde_json::json!({
                "session": self.task.session_id,
                "round": turn.round_no,
                "point": point,
                "goldReward": reward,
                "taskGoldEarned": self.task_gold_earned,
                // Which ending banked it. `confirmed_success` is the closure
                // probe's three-signal confirmation; `timeout_unrejected` is a
                // session the judger timed out with its last submission still
                // un-answered — 接口文档 §timeoutRounds settles those on the best
                // submitted answer, so the point's reward is the ceiling they
                // were played for. Both are ceilings, not receipts.
                "via": via,
            }),
        );
    }

    /// Threat statistics per compass sector around our station (P2-1).
    ///
    /// Two inputs, deliberately weighted apart. A robot seen within
    /// [`THREAT_SECTOR_RADIUS`] of the base is one point — it might walk past.
    /// A wall that LOST HP outranks it by two orders of magnitude, because that
    /// is the only evidence that a sector is not merely approached but
    /// *breached*: the arc that actually takes damage is the arc the outer
    /// layer is worth building on, and the analysis is explicit that it is
    /// worth building on nowhere else.
    fn absorb_threat(&mut self, turn: &Turn) {
        let Some(station) = turn.station() else {
            return;
        };
        let center = station.pos;
        for robot in turn.robots.iter().filter(|robot| robot.health > 0) {
            if chebyshev(robot.pos, center) > THREAT_SECTOR_RADIUS {
                continue;
            }
            let sector = arc_sector(center, robot.pos);
            if sector == CENTRE_SECTOR {
                continue;
            }
            self.threat_sectors[sector] = self.threat_sectors[sector].saturating_add(1);
        }
        for wall in turn.walls() {
            let Some(previous) = self.wall_hp_seen.insert(wall.pos, wall.health) else {
                continue; // first sighting of this cell: no damage to attribute
            };
            if wall.health >= previous {
                continue; // repaired, or untouched
            }
            let sector = arc_sector(center, wall.pos);
            if sector == CENTRE_SECTOR {
                continue;
            }
            self.threat_sectors[sector] = self.threat_sectors[sector]
                .saturating_add(WALL_DAMAGE_WEIGHT)
                .saturating_add(previous - wall.health);
            crate::log::event(
                "wall_damage",
                serde_json::json!({
                    "round": turn.round_no,
                    "wall": wall.pos,
                    "lost": previous - wall.health,
                    "sector": sector,
                }),
            );
        }
    }

    /// Sectors of the base that are actually taking damage, most-hit first
    /// (P2-1). Empty means "no evidence yet" — and then no outer layer is
    /// built, which is the point: the second wall is only ever paid for on the
    /// arc the robots demonstrably come through.
    ///
    /// Capped at [`MAX_THREAT_SECTORS`] of the eight, so the arc can never grow
    /// into the closed second ring the analysis rules out — an open arc cannot
    /// trap a role the way a second ring would.
    pub fn threatened_sectors(&self) -> Vec<usize> {
        let peak = self.threat_sectors.iter().copied().max().unwrap_or(0);
        if peak <= 0 {
            return Vec::new();
        }
        // Top quartile of the peak: a sector the robots merely pass through
        // does not qualify beside one they have breached.
        let floor = (peak / 4).max(1);
        let mut ranked: Vec<(i64, usize)> = (0..9)
            .filter(|sector| *sector != CENTRE_SECTOR)
            .map(|sector| (self.threat_sectors[sector], sector))
            .filter(|(count, _)| *count >= floor)
            .collect();
        ranked.sort_by_key(|(count, sector)| (std::cmp::Reverse(*count), *sector));
        ranked.truncate(MAX_THREAT_SECTORS);
        ranked.into_iter().map(|(_, sector)| sector).collect()
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
                    "task_kind": self.task.kind.as_str(),
                    "success": success,
                    "reason": reason,
                    "wrongAnswers": self.task.wrong_answers,
                    // Both counters: `wrongAnswers` is the give-up count and
                    // resets on new information (P1-1), so on its own it can no
                    // longer answer "how many answers did this session burn" —
                    // which is the first thing the analysis asks of a session
                    // that ended at zero.
                    "rejections": self.task.rejections,
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
        // Stage 1 is whatever the session ran BEFORE the command that
        // answered (P1-3). `cmd_history` is in send order and holds one entry
        // per command the sandbox actually ran, so the command immediately
        // before the answering one is the reconnaissance that made it
        // possible — the `find`/`cat` round that read the task file and printed
        // the schema. A session that answered with its first command has none,
        // and the pair is then just the answer script.
        let explore = self
            .task
            .cmd_history
            .iter()
            .rposition(|command| *command == script)
            .filter(|index| *index > 0)
            .map(|index| self.task.cmd_history[index - 1].clone());
        Some(SopEntry {
            task_type: self.task.task_type.clone(),
            keywords,
            template: script,
            explore,
            ..Default::default()
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
    pub fn find_sop(&self, task_type: &str, description: &str) -> Option<SopMatch> {
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
                let pair = entry.bind_pair(description)?;
                // Reuse with feedback, not blind replay (P1-3): a template
                // carrying a strike may run again only when binding produced
                // DIFFERENT bytes — new parameters, new task input. Replaying
                // the exact script the judger already rejected is the same
                // wrong answer with extra steps, so the LLM takes this one.
                if entry.rejections > 0
                    && entry.last_rejected.as_deref() == Some(pair.answer.as_str())
                {
                    crate::log::event(
                        "sop_replay_skipped",
                        serde_json::json!({"taskType": task_type, "overlap": overlap}),
                    );
                    return None;
                }
                crate::log::event(
                    "sop_reuse",
                    serde_json::json!({
                        "taskType": task_type,
                        "overlap": overlap,
                        "bound": true,
                        // P1-3: whether this reuse is the compressed two-stage
                        // path or the single-script one is the whole question
                        // "did the pair actually save the exploration".
                        "staged": pair.explore.is_some(),
                    }),
                );
                Some(pair)
            })
    }

    /// Charge the current session's rejection against the cached template it
    /// actually ran (P1-3). One strike keeps the entry — reuse with feedback;
    /// the second consecutive strike evicts it. A session that wrote its own
    /// script (no SOP fast path) charges nothing: the old behaviour wiped
    /// every template of the type on any rejection, so a good SOP died with
    /// every bad LLM attempt.
    fn charge_sop_rejection(&mut self) {
        let Some(template) = self.task.sop_used_template.clone() else {
            return;
        };
        let rejected_script = self.task.cmd_history.first().cloned();
        let mut evicted = false;
        if let Some(entry) = self
            .sop_cache
            .iter_mut()
            .find(|entry| entry.template == template)
        {
            entry.rejections = entry.rejections.saturating_add(1);
            entry.last_rejected = rejected_script.or_else(|| Some(template.clone()));
            if entry.rejections >= 2 {
                let template = entry.template.clone();
                crate::log::event(
                    "sop_evicted",
                    serde_json::json!({"taskType": entry.task_type, "template": crate::log::brief(&template, 80)}),
                );
                evicted = true;
            }
        }
        if evicted {
            self.sop_cache.retain(|entry| entry.template != template);
        }
    }

    /// A confirmed success clears the used template's strikes: the script
    /// demonstrably works when its parameters are right.
    fn clear_sop_strikes(&mut self) {
        let Some(template) = self.task.sop_used_template.clone() else {
            return;
        };
        if let Some(entry) = self
            .sop_cache
            .iter_mut()
            .find(|entry| entry.template == template)
        {
            entry.rejections = 0;
            entry.last_rejected = None;
        }
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
                //
                // The attempt is NOT counted again here (P2-2): the summon was
                // already counted when it went out (`treasure::plan_pioneer`),
                // and counting the verdict too made the cap mean "two gambles"
                // while the log read "four".
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

    /// Whether the day's news read has not been asked for yet, and today's news
    /// is on the board.
    ///
    /// One slot of the day's budget is held for that ask until it is made — the
    /// owner's 「价格趋势……至关重要」 made mechanical. It is one slot and not a
    /// standing reservation: the moment the ask goes out (`attempts` moves off
    /// zero) the hold is released and the rest of the budget is the treasure's
    /// and the task line's as before. A day with no official news reserves
    /// nothing.
    fn news_read_reserved(&self, turn: &Turn) -> bool {
        self.news.attempts == 0
            && self.news.phase == news::ReadPhase::Idle
            && self.official_seen.contains_key(&turn.day)
    }

    /// Is the altar's window the round's business (P2-3)?
    ///
    /// The treasure's own predicate, asked with the pioneer this round actually
    /// has: `summonTreasure` is a pioneer action (`validate` admits it for no
    /// other kind), so a board with no pioneer has no race to run.
    fn treasure_race_on(&self, turn: &Turn) -> bool {
        turn.pioneer()
            .map(|pioneer| crate::brain::treasure::window_due(turn, self, pioneer))
            .unwrap_or(false)
    }

    /// Take the round's prompt on behalf of `purpose`, if it may have it.
    ///
    /// The ranking is the owner's, and each step is where it is for a reason
    /// that is about the match rather than about the code:
    ///
    /// 1. **News** — 「价格趋势直接决定了我们采集哪些矿，至关重要」. The outlook
    ///    is what prices every vein the crew may walk to this afternoon, so it
    ///    is read first, and it holds a slot of the day's budget until it has
    ///    been asked ([`Self::news_read_reserved`]).
    /// 2. **Treasure** — 「民间传闻引发的宝藏任务」. A treasure pays gold and
    ///    items, but it does not price the mining the team does every round, and
    ///    it keeps the window it has always had: it asks only while no task is
    ///    running (its planner is not reached otherwise), and a fresh news read
    ///    is allowed to go first.
    /// 3. **Task** — 「自进化任务可以晚点接」. The point pays its own reward and
    ///    its session runs ten-plus rounds, so a round spent waiting costs least
    ///    here. It is not starved: the news read is one ask per day, so what the
    ///    task line loses to it is that ask and no more.
    ///
    /// Two things make the ranking bind. The position of `news::plan_prompt` in
    /// `day::pioneer_day` — before the task line's planner and before the
    /// treasure's — decides the round the ask is made in, because `plan.prompt`
    /// is one slot and the first writer owns the call. This gate is what decides
    /// the ARGUMENT: a lower purpose is refused while a higher one still has an
    /// ask to make today.
    ///
    /// The order is not a constant (「顺序上不能固定」). The altar's window moves
    /// it: while `treasure::window_due` holds — the window is open, or the
    /// shopping for it has to happen now — the task line is stood down
    /// entirely, and the same fact moves the ACTIONS in `day::pioneer_day`.
    /// See the clause-by-clause comment in the body.
    ///
    /// The budget itself is unchanged: 接口文档's three calls per game day, and
    /// calls made while a self-evolution session is running are neither limited
    /// nor counted — so a session's own request is always granted, exactly as
    /// before, and nothing is scarce in that window.
    pub fn request_prompt(&mut self, purpose: PromptPurpose, turn: &Turn) -> bool {
        let free_window = self.task.active;
        if !free_window && !self.is_prompt_free() {
            return false;
        }
        // THE RANKING IS A FUNCTION OF THE ROUND, NOT A CHAIN.
        //
        // 「顺序上不能固定」 — the owner, re-reading 任务书. Each purpose is stood
        // down only while the ones above it actually have this round's business,
        // and each clause below is that condition rather than a fixed position:
        //
        // * **News** is never stood down. It prices every vein the crew walks to
        //   this afternoon (「价格趋势直接决定了我们采集哪些矿，至关重要」) and it
        //   costs the pioneer no movement, so it does not compete with the altar
        //   — it runs alongside it, in a channel the altar does not use.
        // * **Treasure** is stood down only by an unread day's news, which is
        //   what it has always been. It holds nothing while its own window is
        //   live: the line only asks before it has a plan (`Idle`), and a plan
        //   is what a window is made of, so the round that matters most to the
        //   treasure is one it does not spend a prompt on.
        // * **Task** waits for both — 「自进化任务可以晚点接」 — and it also waits
        //   while the altar's window is due: 「宝藏只能召唤一次要抢」. A session
        //   advanced now is the session `day::pioneer_day` abandons for the
        //   altar in this same round, and a task point comes back after its
        //   30-round refresh while the altar does not come back at all. The two
        //   cannot disagree, because they read the same predicate.
        let stood_down = match purpose {
            PromptPurpose::News => false,
            PromptPurpose::Treasure => self.news_read_reserved(turn),
            PromptPurpose::Task => self.news_read_reserved(turn) || self.treasure_race_on(turn),
        };
        if stood_down {
            return false;
        }
        if !free_window {
            self.consume_prompt_budget();
        }
        true
    }

    pub fn consume_summon_order(&mut self) {
        self.summon_orders_today = self.summon_orders_today.saturating_add(1);
    }
}

/// The dead-centre bucket of [`arc_sector`]: a unit standing on the station
/// itself has no direction, and folding it into one would credit a sector for
/// nothing.
pub const CENTRE_SECTOR: usize = 4;

/// Most sectors the second wall layer may ever cover (P2-1). Three of eight is
/// a 135° arc at the widest — open at both ends, so it can never enclose a
/// role the way a full second ring would.
pub const MAX_THREAT_SECTORS: usize = 3;

/// Radius within which a live robot counts as pressing on the base (P2-1).
const THREAT_SECTOR_RADIUS: i32 = 12;

/// Threat points charged for one wall cell losing HP (P2-1). Weighted far above
/// a mere sighting: damage taken is evidence, proximity is a guess.
const WALL_DAMAGE_WEIGHT: i64 = 25;

/// Direction bucket of `pos` around `center` on the 3×3 compass:
/// `(sign(dy)+1) * 3 + (sign(dx)+1)`, so 0 is the south-west corner, 4 is
/// [`CENTRE_SECTOR`] and 8 is the north-east corner.
///
/// Integer signs rather than an angle: the map is a grid, the question is
/// "which side of the base", and a sector boundary that lands on a float
/// rounding rule would make the two layers of P2-1 disagree about a cell.
pub fn arc_sector(center: Pos, pos: Pos) -> usize {
    let dx = (pos.x - center.x).signum() + 1;
    let dy = (pos.y - center.y).signum() + 1;
    (dy * 3 + dx) as usize
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
