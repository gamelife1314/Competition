//! Treasure hunt: accumulate folk legends, spend one LLM call to infer the
//! altar (position / sacrifice items / opening day), buy the items, walk the
//! pioneer over and `summonTreasure`. Feedback via `lastSummonTreasureResult`
//! (2 = too early → push the day; 3 = wrong items → re-ask with feedback).

use std::collections::HashSet;

use crate::brain::{economy, task::truncate};
use crate::brain::Plan;
use crate::model::{chebyshev, Turn, Unit};
use crate::protocol::{Pos, RoleCommand};
use crate::state::{BotState, PromptPurpose, TreasurePhase, TreasurePlan};

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

// ---------------------------------------------------------------------------
// The window: 「宝藏只能召唤一次要抢」
// ---------------------------------------------------------------------------
//
// 任务书 5.2: 一张地图宝藏只有一个, 宝藏地点/开启条件/开启时间 all have to be
// inferred, a legal summon CONSUMES the offering whatever the outcome, and
// 「若同一回合内双方均满足宝藏开启条件并且都正确使用了召唤宝藏指令，则双方均获得
// 宝藏奖励」. Every one of those is a reason the window is a RACE and not an
// errand: a round the pioneer spends on something else is a round the opponent
// can take the altar in, and there is no second altar.
//
// The planner had no urgency at all — `plan_pioneer` walks and summons, and
// nothing anywhere said "now". Worse, `day::pioneer_day` reaches the task
// accept before it reaches the treasure, so a pioneer that took a
// self-evolution task on the opening day did not arrive late; it never went.
// `window_due` is the missing urgency, and it is measured in the pioneer's own
// walk rather than in a round number chosen by hand.

/// Day-rounds of working daylight left in the current day.
///
/// `in_day_round` is 0-based over `ROUNDS_PER_DAY` and `economy::DUSK_ROUND` is
/// where the day's errands stop: from dusk the recall owns the evening and the
/// pioneer is already walking home, so a walk that cannot finish before it is a
/// walk the night takes back.
fn daylight_left(turn: &Turn) -> i64 {
    (economy::DUSK_ROUND - turn.in_day_round).max(0)
}

/// The sacrifice the pioneer still has to buy: one `(name, count)` per distinct
/// item it is short of. Items are a MULTISET — 任务书 5.2's 不能多、不能少 is
/// checked by the judger — so the count is per name, not per list entry.
fn missing_items(pioneer: &Unit, plan: &TreasurePlan) -> Vec<(String, i64)> {
    let mut missing: Vec<(String, i64)> = Vec::new();
    for item in &plan.items {
        if missing.iter().any(|(name, _)| name == item) {
            continue;
        }
        let need = plan.items.iter().filter(|other| *other == item).count() as i64;
        let have = pioneer.count_item(item) as i64;
        if have < need {
            missing.push((item.clone(), need - have));
        }
    }
    missing
}

/// The nearest cell the pioneer can shop from — the same stand set
/// `plan_pioneer` buys from, so the two cannot disagree about where the counter
/// is.
fn nearest_shop_stand(turn: &Turn, from: Pos) -> Option<Pos> {
    turn.weapon_shops()
        .iter()
        .flat_map(|shop| crate::model::neighbours(*shop))
        .filter(|pos| turn.is_land(*pos))
        .min_by_key(|pos| (chebyshev(from, *pos), pos.x, pos.y))
}

/// Rounds the pioneer needs to be standing beside the altar with the sacrifice
/// in its pack, from where it stands now: the walk, or — when the offering is
/// short — the detour through the counter.
fn rounds_to_summon(turn: &Turn, pioneer: &Unit, plan: &TreasurePlan) -> i64 {
    let missing = missing_items(pioneer, plan);
    if missing.is_empty() {
        return chebyshev(pioneer.pos, plan.pos) as i64;
    }
    match nearest_shop_stand(turn, pioneer.pos) {
        Some(shop) => {
            chebyshev(pioneer.pos, shop) as i64
                + missing.len() as i64
                + chebyshev(shop, plan.pos) as i64
        }
        // No counter on the board: what the pioneer carries is all there is.
        None => chebyshev(pioneer.pos, plan.pos) as i64,
    }
}

/// Rounds of SHOPPING the sacrifice still owes: the walk to the counter and one
/// round per kind bought. `0` when the pack already holds the offering.
///
/// This is the half of the errand that survives the night. The dusk recall
/// walks the pioneer home, so a step taken toward the altar today is a step it
/// takes again tomorrow; the items in its pack are the only progress that is
/// still there in the morning.
fn shopping_rounds(turn: &Turn, pioneer: &Unit, plan: &TreasurePlan) -> i64 {
    let missing = missing_items(pioneer, plan);
    if missing.is_empty() {
        return 0;
    }
    match nearest_shop_stand(turn, pioneer.pos) {
        Some(shop) => chebyshev(pioneer.pos, shop) as i64 + missing.len() as i64,
        None => 0,
    }
}

/// Is the altar's window open — or close enough that walking there NOW is the
/// difference between taking the treasure and losing it?
///
/// The horizon is a WALK measured against the daylight that is left, because
/// the night is a wall: the dusk recall walks the pioneer home, so a step taken
/// toward the altar today is a step it takes again tomorrow and the only
/// progress the night does not undo is the offering in its pack. Two arms fall
/// out of that, and both are the pioneer's own numbers rather than a round
/// count chosen by hand:
///
/// * **The window is open** (`open_day` has arrived). Due while the whole
///   errand — the counter trip included (`rounds_to_summon`) — still fits in
///   the daylight that is left before the recall (`daylight_left`). A live
///   window is therefore due from the first round of its opening day, which is
///   the point: 任务书 5.2's same-round rule means the opponent's summon can
///   land on any of those rounds.
/// * **The eve of it** (`open_day` is tomorrow). Due while the sacrifice is
///   still short and the counter trip fits in the daylight that is left
///   (`shopping_rounds`): buying the offering is the one part of the errand
///   that has to happen BEFORE the window rather than in it, and the day before
///   is the last day on which it can happen at all. A window further out is not
///   imminent — nothing the pioneer does today would still be true tomorrow —
///   so those days leave the task line alone.
///
/// `Idle` is deliberately not due: a `code 3` verdict drops the plan, so the
/// line has no altar to race for and the round belongs to the re-ask. `Done` is
/// the treasure taken — or emptied by the other side, which is the same thing
/// from here — or the line out of attempts.
pub fn window_due(turn: &Turn, state: &BotState, pioneer: &Unit) -> bool {
    if !matches!(
        state.treasure.phase,
        TreasurePhase::HavePlan | TreasurePhase::Summoned { .. }
    ) {
        return false;
    }
    let Some(plan) = &state.treasure.plan else {
        return false;
    };
    if turn.day >= plan.open_day {
        return rounds_to_summon(turn, pioneer, plan) <= daylight_left(turn);
    }
    let shopping = shopping_rounds(turn, pioneer, plan);
    plan.open_day - turn.day == 1 && shopping >= 1 && shopping <= daylight_left(turn)
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

/// The ASK half of the treasure line, split out of `plan_pioneer` so it can be
/// ranked by position instead of by accident.
///
/// The question is the same one, in the same window, on the same budget. What
/// changed (「顺序上不能固定」) is where it sits: as step 6's first arm it sat
/// BELOW the task accept in `day::pioneer_day`, and `next_task_point` returns a
/// command the moment a point is acceptable — so a pioneer standing beside a
/// task point swallowed the round before the ask was ever reached. That does
/// not just delay the answer; the ask is the only thing that produces a plan,
/// and a plan is what `window_due` needs to see a window at all. The line could
/// be held off the altar by a task point it was never going to finish.
///
/// `day::pioneer_day` now calls this immediately after `news::plan_prompt`, so
/// the ranking the prompt slot has always had is the ranking the round actually
/// applies: the day's price trend when there is a trend to read (the news read
/// reserves the slot and `request_prompt` refuses the treasure behind it), the
/// altar when there is not, the task line last — and the task line loses only
/// the rounds those two need, because the ask is made once per plan.
///
/// It issues no command and moves nobody. The ACTIONS stay in `plan_pioneer`.
pub fn plan_ask(turn: &Turn, state: &mut BotState, plan: &mut Plan) {
    // A response lost to an LLM error: allow a re-ask later. Folding the stale
    // reset into the same call is deliberate — the reset alone would hand the
    // round back to whatever else wanted the slot, and the re-ask would wait on
    // the same accident that swallowed the first one.
    if let TreasurePhase::AskedLlm { round } = state.treasure.phase {
        if turn.round_no.saturating_sub(round) > 4 && state.treasure.ask_attempts < 3 {
            state.treasure.phase = TreasurePhase::Idle;
        }
    }
    if state.treasure.phase != TreasurePhase::Idle {
        return;
    }
    // Day 1 is excluded (P2-1): the first day belongs to the wall ring and the
    // towers, and a legend is still arriving every morning — an answer inferred
    // from one more day of clues is an answer that does not burn 15-gold
    // sacrifices on a wrong guess (code 3).
    let enough = state.treasure.legends.len() >= 2 && turn.day >= 2;
    // The budget and the ranking both live in `request_prompt`. Outside a task
    // session the ask needs a free slot of the day's three; inside one the
    // channel is free and uncounted.
    if enough && plan.prompt.is_none() && state.request_prompt(PromptPurpose::Treasure, turn) {
        plan.prompt = Some(build_prompt(state));
        state.treasure.time_feedback = false;
        state.treasure.phase = TreasurePhase::AskedLlm {
            round: turn.round_no,
        };
    }
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
        // Both prompt-side phases live in `plan_ask`, which `day::pioneer_day`
        // runs above the task accept. Delegating rather than duplicating keeps
        // this function correct for a caller that reaches it first — the ask is
        // idempotent (`plan.prompt.is_none()` and the phase change), so the
        // second call in a round is a no-op.
        TreasurePhase::Idle | TreasurePhase::AskedLlm { .. } => {
            plan_ask(turn, state, plan);
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
            //    per distinct item, not just one). The same list `window_due`
            //    prices the countdown with, so the urgency and the errand can
            //    never disagree about what is still owed.
            let missing = missing_items(pioneer, &treasure_plan);
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
