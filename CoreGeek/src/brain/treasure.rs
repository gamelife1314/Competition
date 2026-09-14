//! Treasure hunt: accumulate folk legends, spend one LLM call to infer the
//! altar (position / sacrifice items / opening day), buy the items, walk the
//! pioneer over and `summonTreasure`. Feedback via `lastSummonTreasureResult`
//! (2 = too early → push the day; 3 = wrong items → re-ask with feedback).

use std::collections::HashSet;

use crate::brain::task::truncate;
use crate::brain::Plan;
use crate::model::{chebyshev, Turn, Unit};
use crate::protocol::{Pos, RoleCommand};
use crate::state::{BotState, TreasurePhase, TreasurePlan};

const ALL_ITEMS: [&str; 6] = [
    "AcientTablet",
    "StarSand",
    "FlameBreath",
    "FrostPotion",
    "ThornAmulet",
    "IronWhistle",
];

/// Gold that must survive a treasure-item purchase (P2-1). The summon is a
/// gamble — result code 3 consumes the sacrifice items for nothing — so it
/// may never eat the night's medicine money. Ten gold is one Medicine.
const TREASURE_GOLD_FLOOR: i64 = 10;

/// Shop price assumed for a sacrifice item the shop has not quoted.
const TREASURE_ITEM_PRICE: i64 = 15;

/// Ceiling on the gold the treasure line may hold back from the shopping list
/// (P2-2). Three items is the common shape of a sacrifice, and 45 gold is a
/// reserve the defence can survive losing; a longer list is funded over
/// several days rather than by freezing the purse. The summon is a gamble
/// (code 3 consumes the items for nothing), so the line gets a share of the
/// surplus and never a lien on the whole treasury.
pub const TREASURE_RESERVE_CAP: i64 = 45;

/// Gold the treasure line needs in the purse right now (P2-2).
///
/// The framework — altar position, sacrifice list, opening day, the four-summon
/// cap — was all in place, and none of it could run: `intent_list` spent every
/// coin on upgrade vouchers, so the pioneer walked to the shop, found an empty
/// purse, and waited there while the opening day went past. This is the gold
/// the *buyer* must leave alone, returned as an addition to the day's build
/// reserve.
///
/// Three conditions keep the reserve from becoming a spending freeze, which
/// would be a worse bug than the one it fixes:
///
/// * the plan must be live and the opening day not yet past — after it, the
///   altar will not open and the gold belongs back in the defence;
/// * the reserve is capped ([`TREASURE_RESERVE_CAP`]) and only ever holds gold
///   that is actually in the purse (`turn.gold >= cost + floor`), so it can
///   never reserve money the team does not have;
/// * the night's medicine floor is untouched — see [`TREASURE_GOLD_FLOOR`].
pub fn gold_reserve(turn: &Turn, state: &BotState) -> i64 {
    if !matches!(state.treasure.phase, TreasurePhase::HavePlan) {
        return 0;
    }
    let Some(plan) = &state.treasure.plan else {
        return 0;
    };
    let Some(pioneer) = turn.pioneer() else {
        return 0;
    };
    if turn.day > plan.open_day {
        return 0;
    }
    // Items are a multiset: buy the missing COUNT per distinct name.
    let mut distinct: Vec<&String> = Vec::new();
    for item in &plan.items {
        if !distinct.iter().any(|old| *old == item) {
            distinct.push(item);
        }
    }
    let mut cost = 0i64;
    for item in distinct {
        let need = plan.items.iter().filter(|other| *other == item).count() as i64;
        let have = pioneer.count_item(item) as i64;
        let missing = (need - have).max(0);
        if missing > 0 {
            let price = turn
                .weapon_shop
                .get(item)
                .copied()
                .unwrap_or(TREASURE_ITEM_PRICE);
            cost = cost.saturating_add(price.saturating_mul(missing));
        }
    }
    let cost = cost.min(TREASURE_RESERVE_CAP);
    if cost <= 0 || turn.gold < cost + TREASURE_GOLD_FLOOR {
        return 0;
    }
    cost
}

/// Should the pioneer HOLD its current cell instead of falling through to
/// loiter/retreat? Yes while it waits beside the altar for the opening day
/// (P2-1). `plan_pioneer` returns no command for that wait — and every
/// fall-through below it in `pioneer_day` (loiter at the task point, retreat
/// inside the ring) walks the pioneer AWAY, so next round this walk drags it
/// back: a two-step oscillation that can spend the whole opening window
/// mid-commute, and that keeps the gate seal waiting on a role that is never
/// home. Holding is one explicit predicate shared by the caller.
pub fn holds_altar(turn: &Turn, state: &BotState, pioneer: &Unit) -> bool {
    let Some(plan) = &state.treasure.plan else {
        return false;
    };
    // "Beside the altar" is the same cell either way; only the day test below
    // differs between the two phases.
    let beside = chebyshev(pioneer.pos, plan.pos) <= 3;
    match state.treasure.phase {
        // Waiting for opening day: hold only while the day is still ahead.
        // Once it arrives, `plan_pioneer` walks the last cells and summons,
        // and holding here would freeze the pioneer short of the altar.
        TreasurePhase::HavePlan => beside && turn.day < plan.open_day,
        // `Summoned` holds for a different reason, and the day no longer
        // enters into it: the summon is out, its verdict has not come back,
        // and the altar is open NOW. A result code 2 asks for the retry from
        // this very cell, so `turn.day < plan.open_day` cannot be the test —
        // the opening day has by definition arrived, and gating on it here
        // made the hold unreachable in exactly the phase that documents it.
        TreasurePhase::Summoned { .. } => beside,
        _ => false,
    }
}

/// Consume a fresh `llmResp` addressed to the treasure hunt.
pub fn on_llm_resp(state: &mut BotState, resp: &str, round_no: i64) {
    if !matches!(state.treasure.phase, TreasurePhase::AskedLlm { .. }) {
        return;
    }
    match parse_plan(resp, state.current_day) {
        Some(plan) => {
            crate::log::event(
                "treasure_plan",
                serde_json::json!({"pos": plan.pos, "items": plan.items, "openDay": plan.open_day}),
            );
            state.treasure.plan = Some(plan);
            state.treasure.phase = TreasurePhase::HavePlan;
        }
        None => {
            crate::log::event(
                "treasure_parse_failed",
                serde_json::json!({"attempts": state.treasure.ask_attempts, "head": crate::log::brief(resp, 200)}),
            );
            state.treasure.ask_attempts = state.treasure.ask_attempts.saturating_add(1);
            state.treasure.phase = if state.treasure.ask_attempts >= 3 {
                TreasurePhase::Done
            } else {
                TreasurePhase::Idle
            };
        }
    }
    let _ = round_no;
}

/// Extract `{"pos":{"x":..,"y":..},"items":[..],"openDay":N}` from free text.
pub fn parse_plan(resp: &str, current_day: i64) -> Option<TreasurePlan> {
    let start = resp.find('{')?;
    let end = resp.rfind('}')?;
    if end <= start {
        return None;
    }
    let candidate = &resp[start..=end];
    let value: serde_json::Value = serde_json::from_str(candidate).ok()?;
    let pos_value = value.get("pos").or_else(|| value.get("altarPos"))?;
    let x = pos_value.get("x")?.as_i64()? as i32;
    let y = pos_value.get("y")?.as_i64()? as i32;
    if !(0..41).contains(&x) || !(0..32).contains(&y) {
        return None;
    }
    // Items are a MULTISET: the legends may ask for e.g. three StarSand
    // ("门需三钥"). Keep duplicates; the judger checks 不能多、不能少.
    let mut items: Vec<String> = Vec::new();
    if let Some(list) = value.get("items").and_then(|v| v.as_array()) {
        for entry in list {
            if let Some(name) = entry.as_str() {
                if ALL_ITEMS.contains(&name) && items.len() < 6 {
                    items.push(name.to_string());
                }
            }
        }
    }
    if items.is_empty() {
        return None;
    }
    let open_day = value
        .get("openDay")
        .and_then(|v| v.as_i64())
        .unwrap_or(current_day)
        .max(current_day);
    Some(TreasurePlan {
        pos: Pos { x, y },
        items,
        open_day,
    })
}

fn build_prompt(state: &BotState) -> String {
    let mut prompt = String::new();
    prompt.push_str("以下是游戏世界中逐日流传的民间传闻，其中隐藏着一个祭坛宝藏的线索。\n");
    prompt.push_str("请推理出：宝藏祭坛坐标（地图 41x32，原点左下角）、开启所需的献祭物品组合、以及宝藏开启的游戏日。\n");
    prompt.push_str("可用献祭物品（英文名必须原样使用）：AcientTablet(古符石板), StarSand(星辰之沙), FlameBreath(烈焰之息), FrostPotion(寒霜药剂), ThornAmulet(荆棘护符), IronWhistle(回音铁哨)。\n");
    prompt.push_str("献祭物品是多重集合：若线索指向同一种物品的多个（例如“门需三钥”指三件同类物品），请在 items 中重复列出该物品名。\n\n");
    prompt.push_str("全部传闻：\n");
    for (day, text) in &state.treasure.legends {
        prompt.push_str(&format!("DAY{day}: {}\n", truncate(text, 400)));
    }
    if state.treasure.wrong_item_rounds > 0 {
        prompt.push_str("\n注意：上次献祭的物品组合被判定错误（结果码3）。请重新审视传闻中关于物品数量与种类的全部细节，给出不同的组合。\n");
    }
    if state.treasure.time_feedback {
        prompt.push_str("\n注意：上次按你给出的坐标与物品献祭，被判定“无宝藏或宝藏暂未开启”（结果码2）。可能是坐标错误或开启时间未到，请重新推理祭坛位置与开启日。\n");
    }
    prompt.push_str(
        "\n只输出一个 JSON 对象，不要输出其它内容，格式：\n{\"pos\":{\"x\":20,\"y\":16},\"items\":[\"StarSand\",\"IronWhistle\"],\"openDay\":5}\n",
    );
    prompt
}

/// Pioneer behaviour for the treasure hunt. Returns a movement/action command
/// or None when the pioneer has nothing treasure-related to do this round.
pub fn plan_pioneer(
    turn: &Turn,
    state: &mut BotState,
    pioneer: &Unit,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) -> Option<RoleCommand> {
    match state.treasure.phase.clone() {
        TreasurePhase::Done => None,
        TreasurePhase::Idle => {
            // Ask the LLM once we have enough legends and spare budget. Day 1
            // is excluded (P2-1): the first day belongs to the wall ring and
            // the towers, and a legend is still arriving every morning — an
            // answer inferred from one more day of clues is an answer that
            // does not burn 15-gold sacrifices on a wrong guess (code 3).
            let enough = state.treasure.legends.len() >= 2 && turn.day >= 2;
            if enough && state.is_prompt_free() && plan.prompt.is_none() {
                plan.prompt = Some(build_prompt(state));
                state.consume_prompt_budget();
                state.treasure.time_feedback = false;
                state.treasure.phase = TreasurePhase::AskedLlm {
                    round: turn.round_no,
                };
            }
            None
        }
        TreasurePhase::AskedLlm { round } => {
            // Response lost (LLM error etc.): allow a re-ask later.
            if turn.round_no.saturating_sub(round) > 4 && state.treasure.ask_attempts < 3 {
                state.treasure.phase = TreasurePhase::Idle;
            }
            None
        }
        TreasurePhase::Summoned { round } => {
            // The summon is out and `lastSummonTreasureResult` has not come
            // back. Hold: re-issuing it here is what burned the four-summon
            // cap in four rounds of a single gamble. If no verdict ever
            // arrives (a task was running and swallowed the channel), resume
            // rather than freeze the line for the rest of the match.
            if turn.round_no.saturating_sub(round) > 3 {
                state.treasure.phase = TreasurePhase::HavePlan;
            }
            None
        }
        TreasurePhase::HavePlan => {
            let treasure_plan = state.treasure.plan.clone()?;
            // 1. Collect the sacrifice items (multiset: buy the missing COUNT
            //    per distinct item, not just one).
            let mut missing: Vec<(String, i64)> = Vec::new();
            for item in &treasure_plan.items {
                if missing.iter().any(|(name, _)| name == item) {
                    continue;
                }
                let need = treasure_plan
                    .items
                    .iter()
                    .filter(|other| *other == item)
                    .count() as i64;
                let have = pioneer.count_item(item) as i64;
                if have < need {
                    missing.push((item.clone(), need - have));
                }
            }
            if !missing.is_empty() {
                let shops = turn.weapon_shops();
                let stand = shops
                    .iter()
                    .flat_map(|shop| crate::model::neighbours(*shop))
                    .filter(|pos| turn.is_land(*pos))
                    .collect::<Vec<_>>();
                if stand.iter().any(|pos| *pos == pioneer.pos) {
                    // Buying one kind per round; gold is team-shared. The
                    // purchase must leave the night's medicine money intact
                    // (P2-1): a wrong sacrifice consumes the items for
                    // nothing (result code 3), so the gamble is only taken
                    // from surplus.
                    let (name, num) = &missing[0];
                    let price = turn
                        .weapon_shop
                        .get(name)
                        .copied()
                        .unwrap_or(TREASURE_ITEM_PRICE);
                    if turn.gold >= price.saturating_mul(*num) + TREASURE_GOLD_FLOOR {
                        return Some(RoleCommand::buy(name, *num));
                    }
                    // Waiting for the reserve to do its job (P2-2). Emitted
                    // once per round on purpose: "the pioneer stood at the
                    // counter while the opening day went past" is the failure
                    // this line exists to make visible, and it is a *duration*.
                    crate::log::event(
                        "treasure_wait_gold",
                        serde_json::json!({
                            "round": turn.round_no,
                            "item": name,
                            "need": num,
                            "price": price,
                            "gold": turn.gold,
                            "reserve": gold_reserve(turn, state),
                        }),
                    );
                    return None; // wait for gold
                }
                return crate::brain::walk_toward(turn, pioneer, &stand, claimed);
            }
            // 2. Head to the altar; summon when open day arrived.
            let altar = treasure_plan.pos;
            let stands = crate::model::neighbours(altar)
                .iter()
                .copied()
                .filter(|pos| turn.is_land(*pos))
                .collect::<Vec<_>>();
            let adjacent = chebyshev(pioneer.pos, altar) <= 1;
            if turn.day >= treasure_plan.open_day {
                if adjacent {
                    // Counted HERE, on the summon that actually goes out, and
                    // deliberately not again when its verdict comes back
                    // (state::absorb_treasure_events): a legal summon consumes
                    // the sacrifice items whatever the outcome, so `attempts`
                    // has to mean "gambles taken" for the 4-attempt cap to
                    // bound the gold it costs. The `Summoned` phase is what
                    // stops this arm firing once per round.
                    state.treasure.summon_attempts =
                        state.treasure.summon_attempts.saturating_add(1);
                    state.treasure.phase = TreasurePhase::Summoned {
                        round: turn.round_no,
                    };
                    crate::log::event(
                        "treasure_summon",
                        serde_json::json!({
                            "round": turn.round_no,
                            "altar": altar,
                            "items": treasure_plan.items,
                            "attempt": state.treasure.summon_attempts,
                        }),
                    );
                    let items: Vec<String> = treasure_plan.items.clone();
                    return Some(RoleCommand::summon_treasure(altar, items));
                }
                return crate::brain::walk_toward(turn, pioneer, &stands, claimed);
            }
            // Early: wait nearby (within 3 cells) so we can act on the day.
            if chebyshev(pioneer.pos, altar) > 3 {
                return crate::brain::walk_toward(turn, pioneer, &stands, claimed);
            }
            None
        }
    }
}
