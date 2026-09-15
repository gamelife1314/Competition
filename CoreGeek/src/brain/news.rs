//! Official-news parsing: mine outages shift collection feasibility and vendor
//! prices.
//!
//! Two readers, in this order: the model ([`build_prompt`] / [`parse_outlook`])
//! and the keyword scan ([`price_outlook`], no regex dependency). The scan is
//! the fallback — it is what a day reads as when the model never answered — and
//! a hint inside the prompt when it does. See the section comment above
//! [`build_prompt`].

use crate::brain::Plan;
use crate::model::{Turn, COPPER, IRON, STONE};
use crate::state::{BotState, Outage, PromptPurpose};

const OUTAGE_WORDS: [&str; 9] = [
    "塌方", "停工", "停产", "关停", "封闭", "检修", "暂停", "事故", "管制",
];

const CN_NUM: [(char, i64); 10] = [
    ('一', 1),
    ('二', 2),
    ('两', 2),
    ('三', 3),
    ('四', 4),
    ('五', 5),
    ('六', 6),
    ('七', 7),
    ('八', 8),
    ('九', 9),
];

/// Parse the day's official news into mine outages. `day` is the 1-based day
/// the news was published.
pub fn parse_official(day: i64, text: &str) -> Vec<Outage> {
    let mut out = Vec::new();
    if !OUTAGE_WORDS.iter().any(|word| text.contains(word)) {
        return out;
    }
    for (keyword, ore) in [("铁", IRON), ("石", STONE), ("铜", COPPER)] {
        if !text.contains(keyword) {
            continue;
        }
        // "今天浅层矿面还能抢采" style: mining still OK today → start tomorrow.
        let start = if text.contains("即日") || text.contains("今日起") || text.contains("今天起")
        {
            day
        } else {
            day + 1
        };
        let duration = find_duration_days(text).unwrap_or(2).clamp(1, 5);
        out.push(Outage {
            ore: ore.to_string(),
            from_day: start,
            to_day: start + duration - 1,
        });
    }
    out
}

/// Which way the news points an ore's price.
///
/// `Flat` is only ever the model's: a keyword count cannot produce it (a text
/// with no direction words has no reading at all), but "the news moves nothing"
/// is a judgement about the news, and it has to be sayable — otherwise the only
/// way for the model to contradict a keyword reading would be to invert it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Rise,
    Fall,
    Flat,
}

/// One ore's expected price move, as a day's official news reads it.
///
/// The economy already reacts to the news through [`parse_official`]: an outage
/// takes a vein off the list. That is the *physical* half of the news. This is
/// the other half — the market's — and it points the opposite way for the veins
/// that are still workable: a shortage raises what the ore sells for, and a
/// resumption lowers it. Without it the miner prices every ore at today's
/// vendor price and walks past the vein whose price is about to double.
#[derive(Debug, Clone, PartialEq)]
pub struct Outlook {
    pub ore: String,
    pub direction: Direction,
    /// 0..=100. Two readings produce this field and they mean the same thing by
    /// it: how far the expected price may move off today's. The keyword scan
    /// counts the words that point one way (one signal is a rumour, three is a
    /// policy); the model states its own confidence and is clamped to the same
    /// range.
    pub confidence: i64,
    /// The day the news was published. A move is priced in from then on: the
    /// outage it describes may start tomorrow, but the market reads the
    /// announcement today, which is exactly why waiting costs gold.
    pub day: i64,
    /// How many days the move lasts, counted from `day`. `0` is "no horizon":
    /// the reading stands until a later one replaces it.
    ///
    /// That is what every keyword reading says, and it is what the keyword
    /// readings did before the model was asked — so `0` keeps their behaviour
    /// byte for byte. The model is asked for a horizon because the owner asked
    /// for one (「接下来几天应该采集哪些矿」), and a claim about the next two
    /// days that keeps pricing the vein on the fifth is a claim nothing can
    /// falsify.
    pub days: i64,
}

/// Words that mean the ore is about to get scarcer — or dearer to buy.
const RISE_WORDS: [&str; 14] = [
    "塌方", "停工", "停产", "关停", "封闭", "检修", "暂停", "事故", "管制", "短缺", "紧缺",
    "供不应求", "涨价", "上涨",
];
/// Words that mean supply is coming back or demand is going away.
const FALL_WORDS: [&str; 12] = [
    "复产", "复工", "恢复开采", "恢复生产", "增产", "产量提升", "供应充足", "库存充足", "滞销",
    "需求下降", "降价", "下跌",
];

/// How long a keyword reading claims to last. `0` is "no horizon": the words
/// in today's news are evidence about today's price until a later reading
/// replaces them, and that is what they have always been. See [`Outlook::days`].
const KEYWORD_HORIZON: i64 = 0;
/// One signal is worth this much confidence, and each further one adds to it.
const SIGNAL_STEP: i64 = 15;
/// Confidence never reaches 100: the news is a claim about the future, and the
/// miner still has to walk somewhere today.
const CONFIDENCE_CAP: i64 = 95;

/// Read the day's official news as expected price moves, one per ore the text
/// names.
///
/// Deliberately a keyword scan like [`parse_official`] — no model, no regex —
/// so the same text yields the same reading every round it is replayed. An ore
/// the text does not name gets no outlook: "矿石价格将上涨" without naming iron
/// or copper is not evidence about either, and guessing is how a miner ends up
/// committed to a vein nobody priced. When a text carries both directions the
/// louder one wins, and a tie goes to the rise.
pub fn price_outlook(day: i64, text: &str) -> Vec<Outlook> {
    let mut out = Vec::new();
    let rise = RISE_WORDS.iter().filter(|word| text.contains(*word)).count() as i64;
    let fall = FALL_WORDS.iter().filter(|word| text.contains(*word)).count() as i64;
    if rise == 0 && fall == 0 {
        return out;
    }
    let (direction, signals) = if rise >= fall {
        (Direction::Rise, rise)
    } else {
        (Direction::Fall, fall)
    };
    let confidence = (SIGNAL_STEP * (signals + 2)).min(CONFIDENCE_CAP);
    for ore in [IRON, STONE, COPPER] {
        if !names_ore(text, ore) {
            continue;
        }
        out.push(Outlook {
            ore: ore.to_string(),
            direction,
            confidence,
            day,
            days: KEYWORD_HORIZON,
        });
    }
    out
}

/// Does the text name this ore as a RESOURCE?
///
/// Stricter than [`parse_official`]'s single-character scan, and it has to be:
/// 矿石 is the word for "ore", so `"铁矿石"` contains 石 and an outage parser
/// that flags stone there is merely over-cautious — but a price outlook that
/// does it sends the miner to a stone vein on news about iron. Stone has to be
/// named as stone, not as the second character of somebody else's ore.
fn names_ore(text: &str, ore: &str) -> bool {
    match ore {
        STONE => ["石矿", "石头", "石料"].iter().any(|word| text.contains(word)),
        IRON => text.contains("铁"),
        _ => text.contains("铜"),
    }
}

/// What one ore should be priced at today, given the day's news.
///
/// `confidence` scales the move, so a single keyword nudges the price and a
/// whole paragraph of them moves it by the full amount. The result is a
/// *comparison* price — it decides which vein is worth the walk — and never
/// what the vendor actually pays.
pub fn expected_price(current: i64, outlook: Option<&Outlook>) -> i64 {
    let Some(outlook) = outlook else {
        return current;
    };
    let lift = match outlook.direction {
        Direction::Rise => outlook.confidence,
        Direction::Fall => -outlook.confidence,
        // "Nothing moves" prices the ore at today's vendor price, which is also
        // the answer with no reading at all — but it is an ANSWER, and it
        // displaces the keyword reading it replaced.
        Direction::Flat => 0,
    };
    // Half the confidence, so one keyword is a 7% move and a full-confidence
    // reading is 47% — enough to change which vein wins, not enough to invent a
    // price the vendor has never paid.
    (current * (100 + lift / 2) / 100).max(1)
}

// ---------------------------------------------------------------------------
// The LLM read: the price trend is the model's call, the words are the assist
// ---------------------------------------------------------------------------
//
// The owner, verbatim: 「推理类任务和世界新闻可以提交有大模型推测……先提交大模型
// 推理矿的价格趋势，接下来几天应该采集哪些矿……都由大模型来判断，辅助关键词判断。
// 价格趋势直接决定了我们采集哪些矿，至关重要。」
//
// `price_outlook` above counts words. It cannot read 恢复开采 as a fall when the
// text also says 停工, it cannot price an ore the sentence implies but never
// names, and it has no way to say "this announcement is about road repairs, not
// about ore". The model is asked for that reading, and the keyword count keeps
// two jobs: it is a hint inside the prompt (「辅助关键词判断」), and it is the
// reading that stands whenever the model has not answered — a lost response, a
// budget spent elsewhere, an answer in the wrong shape. The model can therefore
// only ever REPLACE a reading it actually made.

/// How many rounds to wait for an answer before the ask is treated as lost.
/// Short on purpose: this reading prices the mining that is happening now.
const NEWS_WAIT_ROUNDS: i64 = 2;
/// Asks per day. After this the keyword scan has the day to itself.
const NEWS_MAX_ATTEMPTS: i64 = 2;
/// The horizon a reading gets when it does not name one. The owner's 「接下来
/// 几天」 is two or three, and a reading with no stated horizon has to expire
/// somewhere or it prices the vein for the rest of the match.
const DEFAULT_HORIZON_DAYS: i64 = 2;
/// An ore's price does not move for a week on one paragraph of news.
const MAX_HORIZON_DAYS: i64 = 5;
/// The confidence a reading gets when it does not name one: under the keyword
/// scan's weakest reading (three signals), because a model that did not say how
/// sure it is has not said anything the words did not.
const DEFAULT_CONFIDENCE: i64 = 45;

/// The prompt that hands the day's official news to the model.
///
/// Shaped like `treasure::build_prompt` — the task, the raw input, the
/// vocabulary the answer has to use, what the last answer got wrong, and the
/// exact JSON object wanted — with one addition: the keyword scan's own reading
/// of the same text goes in as a hint. The model is free to disagree with it,
/// and disagreeing is the reason it is asked.
pub fn build_prompt(day: i64, text: &str, keyword: &[Outlook], correction: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str("以下是游戏世界中今天的官方新闻。请推理它对矿石价格走势的影响。\n");
    prompt.push_str("矿石共三种：iron(铁)、stone(石)、copper(铜)。\n");
    prompt.push_str(
        "请对每种受影响的矿石给出：direction（rise 涨价 / fall 跌价 / flat 不变）、\
         days（该走势预计持续几个游戏日，1-5）、confidence（0-100）。\n",
    );
    prompt.push_str(
        "判据取自新闻本身：矿区停工、塌方、检修、减产、供应紧缺 → 涨；\
         复产、复工、增产、供应充足、需求下降 → 跌。\n\
         新闻与矿石价格无关时，outlooks 给空数组。\n",
    );
    prompt.push_str(&format!(
        "\n新闻（第{day}天）：\n{}\n",
        crate::log::brief(text, 600)
    ));
    if keyword.is_empty() {
        prompt.push_str("\n关键词初判：这段文字里没有读出方向。\n");
    } else {
        prompt.push_str("\n关键词初判（仅供参考，你可以不同意）：\n");
        for outlook in keyword {
            prompt.push_str(&format!(
                "  {} {} ({}%)\n",
                outlook.ore,
                direction_word(outlook.direction),
                outlook.confidence
            ));
        }
    }
    if let Some(note) = correction {
        prompt.push_str(&format!("\n注意：{note}\n"));
    }
    prompt.push_str(
        "\n只输出一个 JSON 对象，不要输出其它内容，格式：\n\
         {\"outlooks\":[{\"ore\":\"iron\",\"direction\":\"rise\",\"days\":3,\"confidence\":80}]}\n",
    );
    prompt
}

/// What the model answered, or `None` when the answer is not a price reading.
///
/// `None` is the fallback's trigger: the keyword scan's reading stands. An empty
/// `outlooks` array is NOT `None` — it is the model saying the news moves
/// nothing, which is an answer, and it replaces the keyword reading.
pub fn parse_outlook(day: i64, resp: &str) -> Option<Vec<Outlook>> {
    let start = resp.find('{')?;
    let end = resp.rfind('}')?;
    if end <= start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&resp[start..=end]).ok()?;
    let list = value
        .get("outlooks")
        .or_else(|| value.get("prices"))
        .or_else(|| value.get("trends"))?
        .as_array()?;
    let mut out: Vec<Outlook> = Vec::new();
    for entry in list {
        let Some(ore) = entry.get("ore").and_then(|v| v.as_str()).and_then(ore_of) else {
            continue;
        };
        let Some(direction) = entry.get("direction").and_then(|v| v.as_str()).and_then(direction_of)
        else {
            continue;
        };
        // One reading per ore: the last one would win by `confidence` anyway,
        // and two rows for the same ore in one answer is the model hedging.
        if out.iter().any(|old| old.ore == ore) {
            continue;
        }
        let confidence = entry
            .get("confidence")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_CONFIDENCE)
            .clamp(0, 100);
        let days = entry
            .get("days")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_HORIZON_DAYS)
            .clamp(1, MAX_HORIZON_DAYS);
        out.push(Outlook {
            ore: ore.to_string(),
            direction,
            confidence,
            day,
            days,
        });
    }
    Some(out)
}

/// `iron` / `IRON` / `铁` / `铁矿` … → the ore the rest of the code names.
fn ore_of(name: &str) -> Option<&'static str> {
    let lower = name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "iron" | "fe" | "铁" | "铁矿" | "铁矿石" => Some(IRON),
        "stone" | "rock" | "石" | "石矿" | "石头" | "石料" => Some(STONE),
        "copper" | "cu" | "铜" | "铜矿" | "铜矿石" => Some(COPPER),
        _ => None,
    }
}

/// `rise` / `up` / `涨` … → the direction. Unknown words are not a reading, so
/// the row is dropped rather than defaulted: `flat` and a missing answer are
/// not the same claim.
fn direction_of(word: &str) -> Option<Direction> {
    let lower = word.trim().to_ascii_lowercase();
    match lower.as_str() {
        "rise" | "rising" | "up" | "increase" | "涨" | "上涨" | "涨价" | "看涨" => {
            Some(Direction::Rise)
        }
        "fall" | "falling" | "down" | "decrease" | "跌" | "下跌" | "降价" | "看跌" => {
            Some(Direction::Fall)
        }
        "flat" | "stable" | "unchanged" | "no_change" | "平" | "持平" | "不变" => {
            Some(Direction::Flat)
        }
        _ => None,
    }
}

/// The one spelling of a direction the log uses, for both readings' records.
pub fn direction_word(direction: Direction) -> &'static str {
    match direction {
        Direction::Rise => "rise",
        Direction::Fall => "fall",
        Direction::Flat => "flat",
    }
}

/// Where the day's news read has got to. One read per day, and the day's news
/// arrives once, so the phase is per day and reset at the day rollover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadPhase {
    /// No ask is outstanding: the read is either not started or retrying.
    #[default]
    Idle,
    /// The prompt went out at `round`; an answer is outstanding.
    AskedLlm { round: i64 },
    /// Answered, or given up on for today. The keyword scan has the day.
    Done,
}

/// The state of one day's news read.
#[derive(Debug, Clone, Default)]
pub struct NewsRead {
    pub phase: ReadPhase,
    /// Asks made today. The read is retried once and then left to the scan:
    /// the mining this reading prices is happening now.
    pub attempts: i64,
    /// The day the model answered for — the day whose keyword readings it
    /// replaced, and the day the scan must not write over again.
    pub read_day: Option<i64>,
    /// What the last answer got wrong, handed back to the model in the retry's
    /// prompt, exactly as `treasure::build_prompt` does with its corrections.
    pub correction: Option<String>,
}

impl NewsRead {
    /// An ask is out and unanswered.
    pub fn awaits_response(&self) -> bool {
        matches!(self.phase, ReadPhase::AskedLlm { .. })
    }

    /// The read still has an ask to make today.
    pub fn wants_reading(&self) -> bool {
        self.phase == ReadPhase::Idle
    }

    /// The ask was not answered. Retry, or leave the rest of the day to the
    /// keyword scan.
    fn note_lost(&mut self, note: &str) {
        self.correction = Some(note.to_string());
        self.phase = if self.attempts >= NEWS_MAX_ATTEMPTS {
            ReadPhase::Done
        } else {
            ReadPhase::Idle
        };
    }

    /// The day is read: the model answered and its readings are in force.
    fn note_read(&mut self, day: i64) {
        self.read_day = Some(day);
        self.correction = None;
        self.phase = ReadPhase::Done;
    }
}

/// Take the model's answer: its readings REPLACE the keyword scan's for the day.
///
/// The model was shown the same text, so silence about an ore is a judgement
/// about that ore rather than an omission to be filled in from the words — which
/// is what 「都由大模型来判断」 asks for. A day the model never answered for is
/// untouched, and there the scan's reading is the answer, exactly as before.
pub fn on_llm_resp(state: &mut BotState, day: i64, resp: &str) {
    if !state.news.awaits_response() {
        return;
    }
    let Some(outlooks) = parse_outlook(day, resp) else {
        crate::log::event(
            "news_read_failed",
            serde_json::json!({
                "day": day,
                "attempts": state.news.attempts,
                "head": crate::log::headline(resp, 200),
            }),
        );
        state.news.note_lost("上次的回答不是可解析的 JSON，请只输出规定的 JSON 对象。");
        return;
    };
    let replaced = state
        .outlooks
        .iter()
        .filter(|old| old.day == day)
        .count();
    state.outlooks.retain(|old| old.day != day);
    for outlook in &outlooks {
        crate::log::event(
            "news_outlook",
            serde_json::json!({
                "ore": outlook.ore,
                "direction": direction_word(outlook.direction),
                "confidence": outlook.confidence,
                "days": outlook.days,
                "day": outlook.day,
                "source": "llm",
            }),
        );
    }
    crate::log::event(
        "news_read",
        serde_json::json!({
            "day": day,
            "readings": outlooks.len(),
            "replaced": replaced,
        }),
    );
    state.outlooks.extend(outlooks);
    state.news.note_read(day);
}

/// The pioneer's news step: ask the model about the day's official news.
///
/// Runs before every other consumer of the round's prompt — see
/// `BotState::request_prompt` for the ranking and the day's budget.
pub fn plan_prompt(turn: &Turn, state: &mut BotState, plan: &mut Plan) {
    if plan.prompt.is_some() {
        return;
    }
    // One day's news, read once. No official news today, nothing to ask about.
    let Some(text) = state.official_seen.get(&turn.day).cloned() else {
        return;
    };
    if let ReadPhase::AskedLlm { round } = state.news.phase {
        if turn.round_no.saturating_sub(round) > NEWS_WAIT_ROUNDS {
            state.news.note_lost("上次没有收到回答，请直接输出规定的 JSON 对象。");
        }
        return;
    }
    if !state.news.wants_reading() {
        return;
    }
    if !state.request_prompt(PromptPurpose::News, turn) {
        return;
    }
    let keyword = price_outlook(turn.day, &text);
    let correction = state.news.correction.clone();
    crate::log::event(
        "news_read_ask",
        serde_json::json!({
            "day": turn.day,
            "round": turn.round_no,
            "attempt": state.news.attempts + 1,
            "keyword": keyword.len(),
        }),
    );
    plan.prompt = Some(build_prompt(
        turn.day,
        &text,
        &keyword,
        correction.as_deref(),
    ));
    state.news.attempts += 1;
    state.news.phase = ReadPhase::AskedLlm {
        round: turn.round_no,
    };
}

/// Scan for "<number>天" duration hints ("需要2天左右", "停工三天").
fn find_duration_days(text: &str) -> Option<i64> {
    let chars: Vec<char> = text.chars().collect();
    for (idx, ch) in chars.iter().enumerate() {
        let next_is_day = chars.get(idx + 1) == Some(&'天');
        if !next_is_day {
            continue;
        }
        if let Some(digit) = ch.to_digit(10) {
            return Some(digit as i64);
        }
        for (cn, value) in CN_NUM {
            if *ch == cn {
                return Some(value);
            }
        }
    }
    // "十" alone
    if text.contains("十天") {
        return Some(10);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iron_collapse_sample() {
        let text = "矿业管理局紧急通报：北部铁矿区昨夜发生严重矿井塌方事故，主巷道结构受损。矿区将于明日全面停工，进行巷道加固和主矿脉修复。矿区领班表示：'今天浅层矿面还能抢采一些，明天的全面停工不可避免。'工程队评估：类似规模的塌方事故，修复工程通常需要2天左右才能完成并恢复开采。";
        let outages = parse_official(3, text);
        assert_eq!(outages.len(), 1);
        assert_eq!(outages[0].ore, IRON);
        assert_eq!(outages[0].from_day, 4);
        assert_eq!(outages[0].to_day, 5);
    }

    #[test]
    fn ignores_plain_news() {
        assert!(parse_official(1, "今日无重大新闻").is_empty());
    }

    #[test]
    fn chinese_numeral_duration() {
        let outages = parse_official(2, "铜矿检修，明日起停产三天。");
        assert_eq!(outages.len(), 1);
        assert_eq!(outages[0].ore, COPPER);
        assert_eq!(outages[0].from_day, 3);
        assert_eq!(outages[0].to_day, 5);
    }

    /// Every row is a whole reading: text in, (ore, direction, confidence) out.
    /// The samples are the three shapes the judger actually publishes — a
    /// shortage, a resumption, and a day with no news about ore at all.
    #[test]
    fn price_outlook_reads_the_three_shapes_of_news() {
        let rise = "矿业管理局紧急通报：北部铁矿区昨夜发生严重矿井塌方事故，主巷道结构受损。\
                    矿区将于明日全面停工，进行巷道加固和主矿脉修复。";
        let fall = "北部铁矿区完成复产检修，今日起恢复开采，产量提升，供应充足。";
        let unrelated = "市政公告：今日无重大新闻，请各单位正常作业。";
        let rows: [(&str, i64, &str, Option<(Direction, i64)>); 3] = [
            ("rise", 3, rise, Some((Direction::Rise, 3 * SIGNAL_STEP + 30))),
            ("fall", 4, fall, Some((Direction::Fall, 4 * SIGNAL_STEP + 30))),
            ("unrelated", 5, unrelated, None),
        ];
        for (name, day, text, expected) in rows {
            let outlooks = price_outlook(day, text);
            match expected {
                None => assert!(
                    outlooks.is_empty(),
                    "{name}: news that names no ore produced {outlooks:?}"
                ),
                Some((direction, wanted)) => {
                    assert_eq!(outlooks.len(), 1, "{name}: {outlooks:?}");
                    let outlook = &outlooks[0];
                    assert_eq!(outlook.ore, IRON, "{name}: wrong ore");
                    assert_eq!(outlook.direction, direction, "{name}: wrong way");
                    assert_eq!(
                        outlook.confidence,
                        wanted.min(CONFIDENCE_CAP),
                        "{name}: confidence is not the signal count"
                    );
                    assert_eq!(outlook.day, day, "{name}: wrong publication day");
                }
            }
        }
    }

    #[test]
    fn a_text_naming_several_ores_prices_each_of_them() {
        let text = "铁矿石与铜矿石同步减产，供应紧缺，价格将上涨。";
        let outlooks = price_outlook(2, text);
        let mut ores: Vec<&str> = outlooks.iter().map(|o| o.ore.as_str()).collect();
        ores.sort();
        assert_eq!(ores, vec![COPPER, IRON], "named ores, and only those");
        assert!(outlooks.iter().all(|o| o.direction == Direction::Rise));
    }

    #[test]
    fn a_falling_price_is_not_read_as_a_rising_one() {
        // The failure this guards: "恢复开采" contains 开 and 采 and would be
        // read by a naive `contains("开采")`-style rise rule. Direction has to
        // come out of the words that mean MORE supply, not fewer.
        let outlooks = price_outlook(1, "铜矿已恢复开采，供应充足，价格下跌。");
        assert_eq!(outlooks.len(), 1);
        assert_eq!(outlooks[0].direction, Direction::Fall);
    }

    #[test]
    fn the_expected_price_follows_the_outlook_and_falls_back_to_today() {
        let current = 12;
        let rise = Outlook {
            ore: COPPER.to_string(),
            direction: Direction::Rise,
            confidence: 80,
            day: 1,
            days: KEYWORD_HORIZON,
        };
        let fall = Outlook {
            direction: Direction::Fall,
            ..rise.clone()
        };
        assert_eq!(expected_price(current, None), current, "no news, today's price");
        assert!(
            expected_price(current, Some(&rise)) > current,
            "a shortage did not lift the price"
        );
        assert!(
            expected_price(current, Some(&fall)) < current,
            "a resumption did not cut the price"
        );
        // Replayed every round, so it has to be a function of the text alone.
        assert_eq!(
            expected_price(current, Some(&rise)),
            expected_price(current, Some(&rise))
        );
        // A reading that claims nothing moves prices the ore at today's
        // number — the same answer as no reading, from an answer that is one.
        let flat = Outlook {
            ore: COPPER.to_string(),
            direction: Direction::Flat,
            confidence: 90,
            day: 1,
            days: 2,
        };
        assert_eq!(expected_price(current, Some(&flat)), current);
    }

    // -----------------------------------------------------------------------
    // The model's answer
    // -----------------------------------------------------------------------

    /// Every row is a whole answer: text in, readings out. The shapes are the
    /// ones a model actually produces — the asked-for object, the same thing in
    /// Chinese, one row missing its optional fields, one row with a number the
    /// range does not allow — plus the two texts that are NOT readings.
    #[test]
    fn parse_outlook_reads_the_shape_it_asked_for() {
        let rows: [(&str, &str, &str, Option<(Direction, i64, i64)>); 7] = [
            (
                "the asked-for object",
                r#"{"outlooks":[{"ore":"iron","direction":"rise","days":3,"confidence":80}]}"#,
                IRON,
                Some((Direction::Rise, 80, 3)),
            ),
            (
                "the same answer in Chinese, wrapped in prose",
                "我认为：\n{\"outlooks\":[{\"ore\":\"铜\",\"direction\":\"跌\",\"days\":2,\"confidence\":70}]}\n以上。",
                COPPER,
                Some((Direction::Fall, 70, 2)),
            ),
            (
                "no horizon named",
                r#"{"outlooks":[{"ore":"stone","direction":"flat","confidence":55}]}"#,
                STONE,
                Some((Direction::Flat, 55, DEFAULT_HORIZON_DAYS)),
            ),
            (
                "numbers outside the range",
                r#"{"outlooks":[{"ore":"iron","direction":"rise","days":99,"confidence":300}]}"#,
                IRON,
                Some((Direction::Rise, 100, MAX_HORIZON_DAYS)),
            ),
            (
                "a negative confidence",
                r#"{"outlooks":[{"ore":"iron","direction":"fall","days":1,"confidence":-40}]}"#,
                IRON,
                Some((Direction::Fall, 0, 1)),
            ),
            (
                "an ore nobody mines",
                r#"{"outlooks":[{"ore":"gold","direction":"rise","days":2,"confidence":90}]}"#,
                IRON,
                None,
            ),
            (
                "a direction nobody defined",
                r#"{"outlooks":[{"ore":"iron","direction":"maybe","days":2,"confidence":90}]}"#,
                IRON,
                None,
            ),
        ];
        for (name, resp, ore, expected) in rows {
            let readings = parse_outlook(1, resp).unwrap_or_else(|| panic!("{name}: not read"));
            match expected {
                None => assert!(
                    readings.iter().all(|o| o.ore != ore),
                    "{name}: {readings:?}"
                ),
                Some((direction, confidence, days)) => {
                    assert_eq!(readings.len(), 1, "{name}: {readings:?}");
                    let reading = &readings[0];
                    assert_eq!(reading.ore, ore, "{name}: wrong ore");
                    assert_eq!(reading.direction, direction, "{name}: wrong way");
                    assert_eq!(reading.confidence, confidence, "{name}: wrong confidence");
                    assert_eq!(reading.days, days, "{name}: wrong horizon");
                    assert_eq!(reading.day, 1, "{name}: wrong day");
                }
            }
        }
    }

    #[test]
    fn an_answer_that_is_not_a_reading_is_not_a_reading() {
        // `None` is the fallback's trigger, and it has to stay narrow: prose
        // with no object in it, and a JSON object that is about something else.
        // Both are answers, and neither says what any ore will do.
        assert!(parse_outlook(1, "根据新闻，铜价应该会下跌。").is_none());
        assert!(parse_outlook(1, r#"{"city":"北京","total_count":15}"#).is_none());
        assert!(parse_outlook(1, "").is_none());
        // ...and the empty list is NOT one of them: it is the model saying the
        // news moves nothing, which is an answer and replaces the words.
        assert_eq!(parse_outlook(1, r#"{"outlooks":[]}"#), Some(Vec::new()));
    }
}
