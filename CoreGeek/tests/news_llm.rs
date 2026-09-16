//! The day's official news goes to the LLM, and the price trend it reads is
//! what prices the mining (P1-1).
//!
//! The owner, verbatim: 「推理类任务和世界新闻可以提交有大模型推测……先提交大模型
//! 推理矿的价格趋势，接下来几天应该采集哪些矿……都由大模型来判断，辅助关键词判断。
//! 价格趋势直接决定了我们采集哪些矿，至关重要。」
//!
//! Before this, `news::price_outlook` was a keyword count and nothing else: the
//! miner's whole idea of where prices were going came from `text.contains("上涨")`.
//! What is asserted here is the three-way contract that replaces it —
//!
//! * the model's reading is the one in force when it answers, including when it
//!   contradicts the words (that is the whole point of asking it);
//! * the keyword scan still produces the old answer when the model does not
//!   answer, so the LLM path is an upgrade and never a regression;
//! * the news read takes the round's prompt from the self-evolution task line,
//!   and gives it straight back the next round.
//!
//! The mining half is asserted through `economy::choose_mine`, not through the
//! outlook: "the stored direction changed" is only worth something if the
//! worker walks somewhere else because of it.

use std::collections::HashSet;

use serde_json::{json, Value};

use coregeek::brain::{day, Plan};
use coregeek::brain::economy;
use coregeek::brain::news::{self, Direction, ReadPhase};
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::{BotState, PromptPurpose, TaskStage};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// A daytime board with an iron vein two steps from the worker and a copper one
/// further out at a better price, priced 8 and 12 by the vendor — the tie
/// `choose_mine` breaks on gold per round. With no news at all the copper wins;
/// the news is the only thing that can move the pick.
fn world(round_no: i64, official_news: Option<&str>) -> Value {
    let mut payload = json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 5, "y": 9}, "neutralType": "iron"},
            {"pos": {"x": 8, "y": 10}, "neutralType": "copper"},
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0, "playerTasks": [],
            "roles": [
                {"id": 10010, "pos": {"x": 5, "y": 5}, "roleType": "worker",
                 "health": 220, "attackPower": 0, "attackRange": 0,
                 "level": 1, "backPackCapability": 4, "backpack": []},
                // The pioneer is not decoration: the news read is asked for in
                // the pioneer's step of the day plan, so a board without one
                // never asks anything.
                {"id": 10011, "pos": {"x": 14, "y": 14}, "roleType": "pioneer",
                 "health": 200, "attackPower": 0, "attackRange": 0,
                 "level": 1, "backPackCapability": 40, "backpack": []},
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "vendorShopList": [
            {"name": "stone", "price": 2},
            {"name": "iron", "price": 8},
            {"name": "copper", "price": 12},
        ],
        "weaponShopList": [],
    });
    if let Some(text) = official_news {
        // 接口文档: the day's news arrives inside `worldNews`.
        payload["worldNews"] = json!({"officialNews": text});
    }
    payload
}

/// A market-only announcement — no outage word in it, so the copper vein stays
/// workable and the only thing that can move the pick is the price reading.
/// The keyword scan reads it as copper falling (库存充足 / 需求下降 / 下跌).
const COPPER_FALLS: &str = "矿业协会通告：铜矿库存充足，需求下降，预计价格下跌。";

/// The keyword scan's own reading of `COPPER_FALLS`, spelled out: three fall
/// signals is `SIGNAL_STEP * 5`. Asserted rather than assumed — the rows below
/// are only evidence if the words really do point the other way.
const COPPER_FALLS_KEYWORD: (Direction, i64) = (Direction::Fall, 75);

/// What the model answers when it disagrees with the words: copper up, for
/// three days, at 90%.
const THE_MODEL_DISAGREES: &str =
    r#"{"outlooks":[{"ore":"copper","direction":"rise","days":3,"confidence":90}]}"#;

fn worker_pick(turn: &Turn, state: &BotState) -> Option<String> {
    economy::choose_mine(
        turn,
        state,
        turn.role_by_id(10010).expect("the worker is on the board"),
        0,
        &HashSet::new(),
    )
    .map(|(_, ore)| ore)
}

/// One round: absorb, then plan, exactly as the loop does it.
fn round(round_no: i64, news_text: Option<&str>, llm_resp: Option<&str>, state: &mut BotState) -> Plan {
    let mut payload = world(round_no, news_text);
    if let Some(resp) = llm_resp {
        payload["llmResp"] = json!(resp);
    }
    let turn = turn_from(payload);
    state.observe(&turn);
    day::plan(&turn, state)
}

// ---------------------------------------------------------------------------
// The model's reading wins, and the miner follows it
// ---------------------------------------------------------------------------

#[test]
fn the_models_reading_is_what_prices_the_vein() {
    let mut state = BotState::default();

    // Round 1 of the day: the news lands and the read goes out.
    let ask = turn_from(world(5, Some(COPPER_FALLS)));
    state.observe(&ask);
    let plan = day::plan(&ask, &mut state);
    let prompt = plan
        .prompt
        .as_deref()
        .expect("the day's news never went to the LLM");
    assert!(
        prompt.contains(COPPER_FALLS),
        "the prompt does not carry the day's news:\n{prompt}"
    );
    assert!(
        prompt.contains("outlooks") && prompt.contains("direction"),
        "the prompt does not state the answer shape:\n{prompt}"
    );
    // 「辅助关键词判断」: the words' own reading goes in as the hint the model is
    // free to disagree with. Without it the model is asked to re-derive a
    // keyword count it cannot see.
    assert!(
        prompt.contains("copper fall (75%)"),
        "the keyword reading is not in the prompt as the assist it is:\n{prompt}"
    );
    assert!(
        state.news.awaits_response(),
        "the read was asked for and is not waiting for an answer"
    );

    // The keyword reading is in force until the model answers, and on this text
    // it sends the worker to the iron: copper priced at 12 × 63% = 7 is worth
    // less per round than iron at 8, two steps closer.
    assert_eq!(
        worker_pick(&ask, &state),
        Some("iron".to_string()),
        "test setup: the keyword reading does not move the pick off the copper"
    );

    // The model's answer, next round.
    round(6, None, Some(THE_MODEL_DISAGREES), &mut state);

    let outlook = state
        .price_outlook("copper", 1)
        .expect("the model's reading was not stored");
    assert_eq!(
        outlook.direction,
        Direction::Rise,
        "the stored reading is the keyword's — the model answered and was ignored"
    );
    assert_eq!(
        outlook.confidence, 90,
        "the stored reading kept the keyword's confidence"
    );
    assert_eq!(outlook.days, 3, "the horizon the model named was dropped");
    assert_eq!(
        state
            .outlooks
            .iter()
            .filter(|o| o.ore == "copper" && o.day == 1)
            .count(),
        1,
        "the model's reading was added beside the keyword's instead of replacing it"
    );
    // ...and the mining decision follows it. This is the assertion the whole
    // change is for: copper reversed by the model is copper worth walking to,
    // where the words alone had the worker crossing to the iron.
    assert_eq!(
        worker_pick(&ask, &state),
        Some("copper".to_string()),
        "the model reversed the copper and the miner still walked past it"
    );
}

// ---------------------------------------------------------------------------
// The control: no answer, or an answer that is not a reading
// ---------------------------------------------------------------------------

#[test]
fn without_an_answer_the_keyword_reading_is_the_one_in_force() {
    // The regression control. A lost response, a spent budget, a model that
    // never replies — the day has to read exactly as it read before any of this
    // existed, or the LLM path is a way to lose a signal rather than gain one.
    let mut state = BotState::default();
    let ask = turn_from(world(5, Some(COPPER_FALLS)));
    state.observe(&ask);
    day::plan(&ask, &mut state);
    assert!(state.news.awaits_response(), "test setup: nothing was asked");

    // No `llmResp` in any of them.
    for round_no in 6..=13 {
        round(round_no, None, None, &mut state);
    }

    let outlook = state
        .price_outlook("copper", 1)
        .expect("no reading for copper at all");
    assert_eq!(
        (outlook.direction, outlook.confidence),
        COPPER_FALLS_KEYWORD,
        "the keyword reading was not the one left in force"
    );
    assert_eq!(
        outlook.days, 0,
        "a keyword reading claims a horizon it cannot know"
    );
    assert_eq!(
        worker_pick(&ask, &state),
        Some("iron".to_string()),
        "the fallback reading no longer moves the pick the way it always did"
    );
    // And the read gives the day up rather than asking forever: the day's news
    // is worth two asks, and the mining it prices is happening now.
    assert_eq!(
        state.news.attempts, 2,
        "the read did not stop at its attempt budget"
    );
    assert_eq!(
        state.news.phase,
        ReadPhase::Done,
        "the read is still trying after its attempts ran out"
    );
}

#[test]
fn an_answer_that_is_not_a_reading_leaves_the_keyword_reading_standing() {
    // The other half of the control, and the likelier one: the model answers,
    // in prose, about the news — and the answer is not the shape that was asked
    // for. It must not be read as a reading of zero ores ("no ore moves") nor
    // as a reading of the answer ("copper rise, because the sentence says
    // 上涨"): both would be the miner acting on something nobody said.
    let mut state = BotState::default();
    let ask = turn_from(world(5, Some(COPPER_FALLS)));
    state.observe(&ask);
    day::plan(&ask, &mut state);

    round(6, None, Some("根据新闻，铜矿供应充足，铜价应该会下跌。"), &mut state);
    assert!(
        state.news.awaits_response(),
        "an unparseable answer was taken as the reading"
    );
    let outlook = state.price_outlook("copper", 1).expect("no reading");
    assert_eq!(
        (outlook.direction, outlook.confidence),
        COPPER_FALLS_KEYWORD,
        "an answer that is not a reading displaced the keyword reading"
    );

    // The retry at round 9 (the ask at 5 is lost after `NEWS_WAIT_ROUNDS`) says
    // what went wrong, so the second question is not the first one repeated.
    for round_no in 7..=8 {
        round(round_no, None, None, &mut state);
    }
    let plan = round(9, None, None, &mut state);
    let prompt = plan
        .prompt
        .as_deref()
        .expect("the retry never went out");
    assert!(
        prompt.contains("注意："),
        "the retry carries no correction from the failed answer:\n{prompt}"
    );
    assert!(
        prompt.contains(COPPER_FALLS) || prompt.contains("铜矿"),
        "the retry lost the news it is about:\n{prompt}"
    );
    assert_eq!(
        (state.news.attempts, state.news.phase == ReadPhase::Done),
        (2, false),
        "the retry was not counted, or the read gave up one attempt early"
    );
}

// ---------------------------------------------------------------------------
// The budget: the price trend outranks the self-evolution task line
// ---------------------------------------------------------------------------

#[test]
fn the_news_read_takes_the_rounds_prompt_from_the_self_evolution_task() {
    // The owner's ranking, at the only place it is scarce: `plan.prompt` is one
    // slot per round, and both lines want it. 「价格趋势直接决定了我们采集哪些矿，
    // 至关重要」 comes first; 「自进化任务可以晚点接」 waits a round.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.accepted_round = 3;
    state.task.timeout_round = 40;
    state.task.task_type = "自进化类1".into();
    state.task.description = "统计 /tmp/selfEvolutionTask 下的文件数量".into();
    state.task.stage = TaskStage::Planning;

    let turn = turn_from(world(5, Some(COPPER_FALLS)));
    state.observe(&turn);
    let plan = day::plan(&turn, &mut state);
    let prompt = plan.prompt.as_deref().expect("no prompt went out at all");
    assert!(
        prompt.contains(COPPER_FALLS),
        "the round's prompt is not the news read:\n{prompt}"
    );
    assert!(
        !prompt.contains("/tmp/selfEvolutionTask/"),
        "the task line took the round the price trend asked for first:\n{prompt}"
    );
    assert_eq!(
        state.task.llm_request_round, None,
        "the task line recorded a request it was never granted"
    );

    // The control: deferred, not starved. The news read asks once a day, and the
    // very next round's prompt is the task line's own.
    let next = turn_from(world(6, None));
    state.observe(&next);
    let plan = day::plan(&next, &mut state);
    let prompt = plan
        .prompt
        .as_deref()
        .expect("the task line never got its prompt back");
    assert!(
        prompt.contains("/tmp/selfEvolutionTask/"),
        "the task line's own prompt was never sent:\n{prompt}"
    );
    assert_eq!(
        state.task.llm_request_round,
        Some(6),
        "the task line's request round was not recorded"
    );
}

// ---------------------------------------------------------------------------
// The horizon the model names
// ---------------------------------------------------------------------------

#[test]
fn a_reading_stops_pricing_the_vein_when_its_horizon_ends() {
    // The owner asked for 「接下来几天」, so the answer carries a horizon and the
    // horizon has to mean something: a reading that claims two days is evidence
    // about two days. Past it the ore prices at the vendor's number again —
    // which is what the miner did before the model was asked, and the honest
    // answer once nobody has said anything about today.
    let mut state = BotState::default();
    let ask = turn_from(world(5, Some(COPPER_FALLS)));
    state.observe(&ask);
    day::plan(&ask, &mut state);
    round(6, None, Some(THE_MODEL_DISAGREES), &mut state);

    // Day 1 and 2 are inside the horizon (3 days from day 1); day 4 is not.
    assert!(state.price_outlook("copper", 1).is_some(), "day 1 lost");
    assert!(state.price_outlook("copper", 3).is_some(), "day 3 lost");
    assert!(
        state.price_outlook("copper", 4).is_none(),
        "a three-day reading is still pricing the fourth day"
    );
    assert_eq!(
        news::expected_price(12, state.price_outlook("copper", 4)),
        12,
        "the expired reading is still moving the price"
    );
    // A keyword reading, which claims no horizon, is untouched by any of this.
    assert_eq!(
        news::expected_price(12, state.outlooks.iter().find(|o| o.days == 0)),
        12,
        "the no-horizon path stopped being the vendor price"
    );
}

// ---------------------------------------------------------------------------
// WHEN the ask fires: the first round that carries the news
// ---------------------------------------------------------------------------
//
// The owner, after the first round of the match: 「第一回合收到新闻之后我发现并没有
// 第一时间向大模型请求」. The read must not wait for a nicer round — the outlook
// prices the mining that is happening this afternoon, and the afternoon starts
// at round 1 of the day. What must be true is: **the round that first sees the
// news is the round that asks.**
//
// Every other test in this file starts at round 5, so nothing here ever pinned
// the opening round. These two do.

#[test]
fn the_round_that_first_sees_the_news_is_the_round_that_asks() {
    let mut state = BotState::default();
    // Round 1 of the match: the first board of the first day, and the day's
    // official news is already on it. This is the exact shape the owner was
    // looking at.
    let ask = turn_from(world(1, Some(COPPER_FALLS)));
    assert_eq!(ask.day, 1, "test setup: round 1 is not day 1");
    assert_eq!(ask.in_day_round, 0, "test setup: round 1 is not the day's first");
    assert_eq!(ask.round_no, 1, "test setup");
    state.observe(&ask);
    let plan = day::plan(&ask, &mut state);
    let prompt = plan
        .prompt
        .as_deref()
        .expect("the news arrived on round 1 and the round went by without asking");
    assert!(
        prompt.contains(COPPER_FALLS),
        "the first round's prompt is not the news read:\n{prompt}"
    );
    assert_eq!(
        state.news.attempts, 1,
        "the first ask was not counted"
    );
    assert_eq!(
        state.news.phase,
        ReadPhase::AskedLlm { round: 1 },
        "the ask did not go out on the round the news first arrived"
    );
}

#[test]
fn the_first_days_read_is_asked_once_and_not_once_a_round() {
    // The other half of the timing contract, on the same opening board: the ask
    // is per DAY, so the round after it does not ask again — and an answer that
    // never comes costs the day one retry, not a prompt every round.
    let mut state = BotState::default();
    let first = turn_from(world(1, Some(COPPER_FALLS)));
    state.observe(&first);
    day::plan(&first, &mut state);
    assert_eq!(state.news.attempts, 1, "test setup: round 1 did not ask");

    // Round 2: the same news is still on the board and the ask is out. No
    // second prompt, and the attempt count does not move.
    let second = round(2, Some(COPPER_FALLS), None, &mut state);
    assert!(
        second.prompt.is_none(),
        "the read asked again one round later:\n{:?}",
        second.prompt
    );
    assert_eq!(state.news.attempts, 1, "the second round billed an ask");

    // Round 4: the wait has expired (`NEWS_WAIT_ROUNDS` is 2 and the ask went
    // out at round 1). This is the round that GIVES UP on the first ask — it
    // notes the loss and stops there, so the retry is the next round's.
    let lost = round(4, Some(COPPER_FALLS), None, &mut state);
    assert!(
        lost.prompt.is_none(),
        "the round that gave up on the first ask also asked again in it:\n{:?}",
        lost.prompt
    );
    assert_eq!(state.news.attempts, 1, "giving up billed an ask");

    // Round 5: the retry. It carries the day's news again and names what the
    // first ask got wrong, so the second question is not the same question.
    let retry = round(5, Some(COPPER_FALLS), None, &mut state);
    let prompt = retry
        .prompt
        .as_deref()
        .expect("a lost answer was never retried");
    assert!(prompt.contains(COPPER_FALLS), "the retry is not the news read");
    assert!(
        prompt.contains("上次没有收到回答"),
        "the retry does not tell the model what went wrong:\n{prompt}"
    );
    assert_eq!(state.news.attempts, 2, "the retry was not counted");

    // And that is the day's budget: `NEWS_MAX_ATTEMPTS` is two, so from here
    // the keyword scan has the day to itself. Round 9 is past the retry's own
    // wait window, and the read is `Done` rather than asking a third time.
    let third = round(9, Some(COPPER_FALLS), None, &mut state);
    assert!(
        third.prompt.is_none(),
        "a third ask went out after the day's two were spent:\n{:?}",
        third.prompt
    );
    assert_eq!(state.news.attempts, 2, "a spent budget was spent again");
    assert_eq!(
        state.news.phase,
        ReadPhase::Done,
        "the spent day's read is not finished with the model"
    );
}

// ---------------------------------------------------------------------------
// Who got the round's prompt slot (issue #207 §5)
// ---------------------------------------------------------------------------

#[test]
fn the_round_that_asks_records_that_the_news_was_the_consumer_that_won() {
    // 「第一回合收到新闻之后我发现并没有第一时间向大模型请求」 — the second half of
    // issue #207 §5. The behaviour was already right (`the_round_that_first_sees_
    // the_news_is_the_round_that_asks`), but nothing said WHO the prompt was for:
    // `request_prompt` ranks News over Treasure over Task as a function of the
    // round, so with only the prompt text a reader cannot tell a news ask from a
    // task ask that happens to quote the news. `prompt_sent` now carries the
    // winner, and this is the record's value on the round that asks.
    let mut state = BotState::default();
    let asked = round(1, Some(COPPER_FALLS), None, &mut state);
    assert!(asked.prompt.is_some(), "test setup: round 1 did not ask");
    assert_eq!(
        state.last_prompt,
        Some(PromptPurpose::News),
        "the round that asked the news read does not name the news as the \
         consumer that won the prompt slot"
    );
    assert_eq!(
        state.last_prompt.map(PromptPurpose::word),
        Some("news"),
        "the word the log writes for the news purpose is not `news`, so 表 12's \
         `问给谁` column cannot be read"
    );

    // The other end: a round that asks for no prompt must not be carrying the
    // last one's purpose, or every quiet round would be attributed to whoever
    // asked last.
    let mut quiet_state = BotState::default();
    let quiet = round(1, None, None, &mut quiet_state);
    assert!(
        quiet.prompt.is_none(),
        "test setup: a day with no official news asked anyway"
    );
    assert_eq!(
        quiet_state.last_prompt, None,
        "a round with no prompt is recorded as having been asked for by {:?}",
        quiet_state.last_prompt
    );
}

#[test]
fn a_starved_prompt_budget_does_not_record_a_consumer() {
    // `request_prompt` has two ways to refuse: the purpose is stood down by a
    // higher one, or the day's budget is spent. Neither is an ask, so neither
    // may leave a purpose behind — otherwise the log would report a consumer
    // that never got a prompt, and 「第一回合有没有发问」 would read as yes.
    let mut state = BotState::default();
    let turn = turn_from(world(1, Some(COPPER_FALLS)));
    // `observe` resets the day's budgets on the day rollover, so the starvation
    // has to be set AFTER it — set before, and the reset quietly hands the round
    // its three prompts back and this test would be measuring nothing.
    state.observe(&turn);
    state.llm_used_today = 3;
    let refused = day::plan(&turn, &mut state);
    assert!(
        refused.prompt.is_none(),
        "test setup: a spent budget still produced a prompt:\n{:?}",
        refused.prompt
    );
    assert_eq!(
        state.last_prompt, None,
        "a refused ask recorded {:?} as the consumer that won the slot",
        state.last_prompt
    );
}
