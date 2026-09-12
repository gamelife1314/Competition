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
}
