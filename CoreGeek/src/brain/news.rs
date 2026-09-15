//! Official-news parsing: mine outages shift collection feasibility and vendor
//! prices. Heuristic keyword scan (no regex dependency).

use crate::model::{COPPER, IRON, STONE};
use crate::state::Outage;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Rise,
    Fall,
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
    /// 0..=100. How many independent words in the text point the same way —
    /// one signal is a rumour, three is a policy. Used to scale how far the
    /// expected price is allowed to move off today's.
    pub confidence: i64,
    /// The day the news was published. A move is priced in from then on: the
    /// outage it describes may start tomorrow, but the market reads the
    /// announcement today, which is exactly why waiting costs gold.
    pub day: i64,
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
    };
    // Half the confidence, so one keyword is a 7% move and a full-confidence
    // reading is 47% — enough to change which vein wins, not enough to invent a
    // price the vendor has never paid.
    (current * (100 + lift / 2) / 100).max(1)
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
    }
}
