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

#[derive(Debug, Clone, Default)]
pub struct TaskSession {
    pub active: bool,
    pub accepted_round: i64,
    pub timeout_round: i64,
    pub point: Option<Pos>,
    pub description: String,
    pub description_round: i64,
    pub stage: TaskStage,
    pub best_answer: String,
    pub cmd_history: Vec<String>,
    pub result_history: Vec<String>,
    pub wrong_answers: i32,
}

#[derive(Debug, Clone, Default)]
pub struct SopEntry {
    pub keywords: Vec<String>,
    pub script: String,
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
    pub items_bought: bool,
}

#[derive(Debug, Default)]
pub struct BotState {
    pub current_day: i64,
    pub last_round: i64,
    pub llm_used_today: i32,
    pub summon_orders_today: i32,

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
    pub sop_cache: Vec<SopEntry>,
    pub treasure: TreasureState,

    /// dedup strings for one-shot fields delivered via request
    pub seen_llm_resp: String,
    pub seen_cmd_result: String,
    /// bombs/dizzy bought tracker can be derived from backpacks; kept simple
    pub harass_done_today: bool,
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
        // Day rollover: reset daily budgets.
        if turn.day != self.current_day {
            self.current_day = turn.day;
            self.llm_used_today = 0;
            self.summon_orders_today = 0;
            self.harass_done_today = false;
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
            && self.official_seen.get(&turn.day).map(String::as_str) != Some(turn.official_news.as_str())
        {
            self.official_seen.insert(turn.day, turn.official_news.clone());
            crate::log::event(
                "news_official",
                serde_json::json!({"day": turn.day, "head": crate::log::brief(&turn.official_news, 200)}),
            );
            for outage in news::parse_official(turn.day, &turn.official_news) {
                if !self.outages.iter().any(|old| {
                    old.ore == outage.ore && old.from_day == outage.from_day && old.to_day == outage.to_day
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
            && self.legend_seen.get(&turn.day).map(String::as_str) != Some(turn.folk_legends.as_str())
        {
            self.legend_seen.insert(turn.day, turn.folk_legends.clone());
            self.treasure.legends.push((turn.day, turn.folk_legends.clone()));
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
        let llm_new =
            !turn.llm_resp.is_empty() && turn.llm_resp != self.seen_llm_resp;
        let cmd_new =
            !turn.last_cmd_result.is_empty() && turn.last_cmd_result != self.seen_cmd_result;
        if llm_new {
            self.seen_llm_resp = turn.llm_resp.clone();
        }
        if cmd_new {
            self.seen_cmd_result = turn.last_cmd_result.clone();
        }
        // Task channel: only consumed while a task is active.
        if llm_new {
            crate::log::event(
                "llm_resp",
                serde_json::json!({
                    "channel": if self.task.active { "task" } else { "treasure" },
                    "head": crate::log::brief(&turn.llm_resp, 300),
                }),
            );
        }
        if cmd_new {
            crate::log::event("cmd_result", serde_json::json!({"head": crate::log::brief(&turn.last_cmd_result, 300)}));
        }
        if self.task.active {
            if llm_new {
                crate::brain::task::on_llm_resp(self, &turn.llm_resp);
            }
            if cmd_new {
                crate::brain::task::on_cmd_result(self, &turn.last_cmd_result);
            }
        } else if llm_new && self.treasure.phase != TreasurePhase::Done {
            crate::brain::treasure::on_llm_resp(self, &turn.llm_resp, turn.round_no);
        }
        // Wrong-answer feedback: only meaningful right after we submitted an
        // answer (stale errorCode=2 in later payloads must not re-trigger).
        let just_submitted = matches!(self.task.stage, TaskStage::WaitingSubmit { .. });
        if just_submitted && turn.error_codes.iter().any(|code| *code == 2) {
            self.task.wrong_answers = self.task.wrong_answers.saturating_add(1);
            self.task.stage = TaskStage::Planning;
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
        // Task ended: description was known but phaseTask cleared, or timeout
        // error reported.
        let ended_by_field = !self.task.description.is_empty()
            && turn.phase_task.is_empty()
            && turn.round_no > self.task.description_round;
        let ended_by_timeout = turn.error_codes.iter().any(|code| *code == 1);
        if ended_by_field || ended_by_timeout || turn.round_no >= self.task.timeout_round {
            self.finish_task();
        }
    }

    pub fn finish_task(&mut self) {
        if self.task.active {
            crate::log::event(
                "task_ended",
                serde_json::json!({
                    "wrongAnswers": self.task.wrong_answers,
                    "cmdRounds": self.task.cmd_history.len(),
                    "bestAnswer": crate::log::brief(&self.task.best_answer, 120),
                    "head": crate::log::brief(&self.task.description, 120),
                }),
            );
        }
        if self.task.active && !self.task.description.is_empty() {
            // Cache the SOP when we did produce a working command.
            if let TaskStage::HavePlan { .. } = &self.task.stage {
            } else if let Some(entry) = self.extract_sop() {
                if !self.sop_cache.iter().any(|old| old.script == entry.script) {
                    self.sop_cache.push(entry);
                }
                if self.sop_cache.len() > 32 {
                    self.sop_cache.remove(0);
                }
            }
        }
        self.task = TaskSession::default();
    }

    fn extract_sop(&self) -> Option<SopEntry> {
        let script = self.task.cmd_history.last()?.clone();
        if script.is_empty() {
            return None;
        }
        let keywords = keywords_of(&self.task.description);
        if keywords.is_empty() {
            return None;
        }
        Some(SopEntry { keywords, script })
    }

    pub fn find_sop(&self, description: &str) -> Option<&SopEntry> {
        let keywords = keywords_of(description);
        if keywords.is_empty() {
            return None;
        }
        self.sop_cache
            .iter()
            .filter(|entry| {
                entry.keywords.iter().filter(|kw| keywords.contains(*kw)).count() * 2
                    >= entry.keywords.len()
            })
            .max_by_key(|entry| {
                entry.keywords.iter().filter(|kw| keywords.contains(*kw)).count()
            })
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
                // Not the right time yet: push the opening day forward.
                if let Some(plan) = &mut self.treasure.plan {
                    plan.open_day = turn.day.max(plan.open_day + 1);
                }
                self.treasure.summon_attempts = self.treasure.summon_attempts.saturating_add(1);
                if self.treasure.summon_attempts > 6 {
                    self.treasure.phase = Done;
                } else if !matches!(self.treasure.phase, Done) {
                    self.treasure.phase = HavePlan;
                }
            }
            3 => {
                // Wrong sacrifice items: ask the LLM again with feedback.
                self.treasure.wrong_item_rounds = self.treasure.wrong_item_rounds.saturating_add(1);
                self.treasure.plan = None;
                if self.treasure.wrong_item_rounds >= 3 {
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
    let stop = ["的", "了", "请", "你", "我", "在", "和", "与", "是", "个", "任务", "查询"];
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
