//! P2-3: the altar's window is a ONE-SHOT RACE, and the plan's order is a
//! function of the board rather than a fixed chain.
//!
//! The owner, re-reading 任务书: 「再读一遍任务书，世界新闻可能没有，宝藏只能召唤
//! 一次要抢，顺序上不能固定。」
//!
//! Three things were wrong with what the previous rounds left behind.
//!
//! 1. `day::pioneer_day` ordered its steps so the task accept came BEFORE the
//!    treasure, and `next_task_point` returns a command the moment a point is
//!    acceptable — so a pioneer that took a self-evolution task on the altar's
//!    opening day never summoned at all. It did not arrive a round late; it
//!    never went. 任务书 5.2 is the reason that is fatal: 一张地图宝藏只有一个,
//!    a legal summon consumes the offering whatever the outcome, and 「若同一回合
//!    内双方均满足宝藏开启条件并且都正确使用了召唤宝藏指令，则双方均获得宝藏奖励」
//!    — a round spent at a task point is a round the opponent can take it in.
//! 2. Nothing in `treasure.rs` carried urgency: the window was walked to when
//!    the pioneer happened to get there and not a round sooner. `window_due` is
//!    the missing urgency, measured in the pioneer's own walk.
//! 3. The order was a constant — News > Treasure > Task, enforced by position.
//!    「顺序上不能固定」: it is now read off the round, and the altar moves it.
//!
//! What is asserted here is the pair that decides the race — the COMMAND the
//! pioneer issues, and whether the round's prompt was spent on the right thing.
//! A flag would not have caught (1): the treasure step was reachable, it simply
//! ran second.

use serde_json::{json, Value};

use coregeek::brain::{day, Plan};
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request};
use coregeek::state::{BotState, PromptPurpose, TaskStage, TreasurePhase, TreasurePlan};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// The altar, five cells from a pioneer standing at (29,6): adjacent enough to
/// summon from, far enough that reaching it is a walk.
const ALTAR: (i32, i32) = (30, 5);
/// The self-evolution point, one cell from that same pioneer — 任务书 5.3's
/// whole allowance, so `next_task_point` accepts it on sight.
const POINT: (i32, i32) = (29, 7);
/// The weapon shop the sacrifice is bought from, fourteen cells out.
const SHOP: (i32, i32) = (28, 20);

/// Day 3's first round: `(3 - 1) * 130 + 1`.
const OPENING_ROUND: i64 = 261;
/// Day 2's first round.
const EVE_ROUND: i64 = 131;

/// A daytime board: a station at (10,24), a pioneer at `pioneer`, a weapon shop
/// and a task point that is acceptable from the pioneer's cell.
fn world(round_no: i64, pioneer: (i32, i32)) -> Value {
    json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": SHOP.0, "y": SHOP.1}, "neutralType": "weaponShop"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": 100, "totalScore": 0,
            "playerTasks": [{
                "taskType": "自进化类1",
                "taskPosition": {"x": POINT.0, "y": POINT.1},
                "coldDownRounds": 0,
                "scoreReward": 10,
                "goldReward": 0,
                "isValid": true,
                "timeoutRounds": 250,
            }],
            "roles": [
                {"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                 "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []},
                {"id": 10004, "pos": {"x": pioneer.0, "y": pioneer.1}, "roleType": "pioneer",
                 "health": 200, "attackPower": 0, "attackRange": 0,
                 "level": 1, "backPackCapability": 40, "backpack": []},
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "weaponShopList": [{"name": "StarSand", "price": 15}],
    })
}

/// A live plan: the altar is known, the window opens on `open_day`, and the
/// offering is whatever the payload puts in the pioneer's pack.
fn planned_state(open_day: i64) -> BotState {
    let mut state = BotState::default();
    state.current_day = open_day;
    state.treasure.phase = TreasurePhase::HavePlan;
    state.treasure.plan = Some(TreasurePlan {
        pos: Pos {
            x: ALTAR.0,
            y: ALTAR.1,
        },
        items: vec!["StarSand".to_string()],
        open_day,
    });
    state
}

fn pioneer_command(plan: &Plan) -> Option<&str> {
    plan.commands.get(&10004).map(|cmd| cmd.action.as_str())
}

/// The pioneer has issued a command with a target — a walk, of some kind.
fn pioneer_target(plan: &Plan) -> Option<Pos> {
    plan.commands
        .get(&10004)
        .and_then(|cmd| cmd.targetPos.as_ref())
        .and_then(|targets| targets.first())
        .copied()
}

// ---------------------------------------------------------------------------
// 1. The altar outranks the task point
// ---------------------------------------------------------------------------

#[test]
fn the_altar_outranks_the_task_point_on_the_opening_round() {
    // The board the bug needed: a live window, the offering already in the
    // pack, the pioneer beside the altar — and a task point one cell away that
    // `next_task_point` is only too happy to accept. The old order reached the
    // accept first and the summon never happened.
    let mut payload = world(OPENING_ROUND, (29, 6));
    payload["teamOur"]["roles"][1]["backpack"] = json!(["StarSand"]);
    let turn = turn_from(payload);

    // Control first, so the assertion below is about the window and not about a
    // point that was never acceptable: with no plan at all, the same board
    // accepts the task.
    let mut closed = BotState::default();
    let plan = day::plan(&turn, &mut closed);
    assert_eq!(
        pioneer_command(&plan),
        Some("acceptTask"),
        "test setup: the task point is not acceptable from this cell"
    );

    let mut state = planned_state(3);
    let plan = day::plan(&turn, &mut state);
    assert_eq!(
        pioneer_command(&plan),
        Some("summonTreasure"),
        "the task point took the round the altar's window was open in"
    );
    assert_eq!(
        state.treasure.summon_attempts, 1,
        "the summon went out without being counted as a gamble"
    );
}

#[test]
fn the_altar_walks_the_last_cells_instead_of_taking_the_point() {
    // The same race one step out: the window is open, the offering is in the
    // pack, and the altar is five cells away. The pioneer walks there — it does
    // not accept the point and come back later, because 5.2's window does not
    // wait and the opponent's summon can land on any round of it.
    //
    // The point is put on the FAR side of the pioneer from the altar, so the two
    // walks are opposite and the command can only be read one way: a step toward
    // the altar is a step away from the point.
    const FAR_POINT: (i32, i32) = (20, 15);
    let mut payload = world(OPENING_ROUND, (25, 10));
    payload["teamOur"]["roles"][1]["backpack"] = json!(["StarSand"]);
    payload["teamOur"]["playerTasks"][0]["taskPosition"] =
        json!({"x": FAR_POINT.0, "y": FAR_POINT.1});
    let turn = turn_from(payload);

    let mut closed = BotState::default();
    let plan = day::plan(&turn, &mut closed);
    assert_eq!(
        pioneer_command(&plan),
        Some("move"),
        "test setup: the task point draws no walk from this cell"
    );
    let control = pioneer_target(&plan).expect("the walk has a target");
    assert!(
        coregeek::model::chebyshev(control, Pos { x: FAR_POINT.0, y: FAR_POINT.1 })
            < coregeek::model::chebyshev(Pos { x: 25, y: 10 }, Pos { x: FAR_POINT.0, y: FAR_POINT.1 }),
        "test setup: the task point's own walk does not head for the point"
    );

    let mut state = planned_state(3);
    let plan = day::plan(&turn, &mut state);
    assert_eq!(
        pioneer_command(&plan),
        Some("move"),
        "the pioneer did not start walking to the altar"
    );
    let target = pioneer_target(&plan).expect("the walk has a target");
    let here = Pos { x: 25, y: 10 };
    let altar = Pos { x: ALTAR.0, y: ALTAR.1 };
    let point = Pos { x: FAR_POINT.0, y: FAR_POINT.1 };
    assert!(
        coregeek::model::chebyshev(target, altar) < coregeek::model::chebyshev(here, altar),
        "the walk ({target:?}) is not toward the altar"
    );
    assert!(
        coregeek::model::chebyshev(target, point) > coregeek::model::chebyshev(here, point),
        "the walk ({target:?}) still serves the task point"
    );
    assert_eq!(
        state.treasure.summon_attempts, 0,
        "a summon went out from five cells away"
    );
}

// ---------------------------------------------------------------------------
// 2. The window's own clock: buying the offering is what the eve is for
// ---------------------------------------------------------------------------

#[test]
fn the_eve_of_the_window_is_spent_at_the_counter() {
    // The window opens tomorrow and the offering is still missing. The counter
    // trip is the one part of the errand that has to happen BEFORE the window
    // rather than in it — and it is also the only progress the night does not
    // undo, because the dusk recall walks the pioneer home either way. Task
    // point one cell away, and the pioneer goes shopping instead.
    let turn = turn_from(world(EVE_ROUND, (29, 6)));
    assert_eq!(turn.day, 2, "the eve of a day-3 window");

    let mut closed = BotState::default();
    let plan = day::plan(&turn, &mut closed);
    assert_eq!(
        pioneer_command(&plan),
        Some("acceptTask"),
        "test setup: the task point is not acceptable from this cell"
    );

    let mut state = planned_state(3);
    let plan = day::plan(&turn, &mut state);
    assert_eq!(
        pioneer_command(&plan),
        Some("move"),
        "the eve of the window was spent on something other than the offering"
    );
    let target = pioneer_target(&plan).expect("the walk has a target");
    let here = Pos { x: 29, y: 6 };
    let shop = Pos { x: SHOP.0, y: SHOP.1 };
    assert!(
        coregeek::model::chebyshev(target, shop) < coregeek::model::chebyshev(here, shop),
        "the pioneer is not walking to the counter the sacrifice is bought from: {target:?}"
    );
}

#[test]
fn a_window_two_days_out_leaves_the_task_point_alone() {
    // The other half of 「顺序上不能固定」, and the reason the eve arm is
    // narrow: nothing the pioneer does today is still true the day after
    // tomorrow — the recall undoes every step of it — so a window that far out
    // does not stand the task line down. 「自进化任务可以晚点接」, not 「不接」.
    let turn = turn_from(world(EVE_ROUND, (29, 6)));

    let mut state = planned_state(4);
    let plan = day::plan(&turn, &mut state);
    assert_eq!(
        pioneer_command(&plan),
        Some("acceptTask"),
        "a window two days away was treated as imminent"
    );
}

// ---------------------------------------------------------------------------
// 3. A running session: the altar takes the pioneer off it
// ---------------------------------------------------------------------------

#[test]
fn an_open_window_takes_the_pioneer_off_a_running_session() {
    // A session holds the pioneer at its point (任务书 5.3 ends the task on
    // 「离开己方任务点周围一格内」), so a pioneer mid-session when the window
    // opens is a pioneer that watches the altar be taken. The two errands are
    // not the same bet: the point comes back after a 30-round refresh and pays
    // its own reward, the altar does not come back at all.
    let mut payload = world(OPENING_ROUND, (29, 6));
    payload["teamOur"]["roles"][1]["backpack"] = json!(["StarSand"]);
    let turn = turn_from(payload);

    let mut state = planned_state(3);
    state.task.active = true;
    state.task.session_id = 7;
    state.task.accepted_round = 250;
    state.task.timeout_round = 400;
    state.task.point = Some(Pos { x: POINT.0, y: POINT.1 });
    state.task.task_type = "自进化类1".into();
    state.task.stage = TaskStage::Planning;

    let plan = day::plan(&turn, &mut state);
    assert_eq!(
        pioneer_command(&plan),
        Some("summonTreasure"),
        "the session kept the pioneer off the altar it was standing next to"
    );
    assert!(
        !state.task.active,
        "the session is still running while the pioneer is at the altar"
    );
}

// ---------------------------------------------------------------------------
// 4. 「世界新闻可能没有」
// ---------------------------------------------------------------------------

#[test]
fn a_day_with_no_official_news_reserves_no_prompt_and_blocks_nobody() {
    // 任务书 4.8 publishes the world news every morning, and an uneventful one
    // is published as nothing at all. `absorb_news` already ignores both
    // spellings; what this pins is that the day then RESERVES nothing — the
    // treasure's ask goes out in the round the news read would have taken, and
    // the read itself makes no attempt at all. Without the guard, the read's
    // reservation (`attempts == 0 && Idle`) is true on every quiet day and the
    // first ask of the round is refused for a question nobody asked.
    for (name, news) in [
        ("no worldNews at all", None),
        ("今日无重大新闻", Some("今日无重大新闻")),
    ] {
        let mut state = BotState::default();
        state.current_day = 3;
        state.treasure.legends = vec![
            (1, "传闻其一".to_string()),
            (2, "传闻其二".to_string()),
        ];
        assert_eq!(state.treasure.phase, TreasurePhase::Idle);

        let mut payload = world(OPENING_ROUND, (29, 6));
        if let Some(text) = news {
            payload["worldNews"] = json!({"officialNews": text});
        }
        let turn = turn_from(payload);
        state.observe(&turn);
        assert!(
            !state.official_seen.contains_key(&turn.day),
            "{name}: an empty news day was stored as news"
        );

        let plan = day::plan(&turn, &mut state);
        let prompt = plan
            .prompt
            .as_deref()
            .unwrap_or_else(|| panic!("{name}: the round produced no prompt at all"));
        assert!(
            prompt.contains("民间传闻"),
            "{name}: the round's prompt is not the treasure's ask:\n{prompt}"
        );
        assert_eq!(
            state.news.attempts, 0,
            "{name}: the news read asked about a day with no news"
        );
        assert_eq!(
            state.llm_used_today, 1,
            "{name}: the day's budget was charged for more than the one ask that went out"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. The same ranking, on the prompt slot the actions are ranked against
// ---------------------------------------------------------------------------

#[test]
fn the_task_line_does_not_take_the_prompt_while_the_race_is_on() {
    // The prompt slot is one per round and the ranking has to hold for it too —
    // 「顺序上不能固定」 is about the plan, not only about `RoleCommand`s. With no
    // window the task line may have the round; with the altar's window live the
    // round's prompt belongs to the altar, and the refusal must not be charged
    // to the day's budget either.
    let mut payload = world(OPENING_ROUND, (29, 6));
    payload["teamOur"]["roles"][1]["backpack"] = json!(["StarSand"]);
    let turn = turn_from(payload);

    let mut closed = BotState::default();
    assert!(
        closed.request_prompt(PromptPurpose::Task, &turn),
        "a closed window refused the task line the prompt"
    );
    assert_eq!(closed.llm_used_today, 1, "a granted ask was not charged");

    let mut state = planned_state(3);
    assert!(
        !state.request_prompt(PromptPurpose::Task, &turn),
        "the task line took the prompt in the round the altar's window opened in"
    );
    assert_eq!(
        state.llm_used_today, 0,
        "a refused ask was charged to the day's budget anyway"
    );
}

#[test]
fn the_altars_ask_does_not_take_the_prompt_off_the_days_news() {
    // The other clause, and the one that keeps 「价格趋势……至关重要」 true on a
    // day the treasure also wants to ask: while the day's news has not been read
    // yet, the treasure's ask is refused and the read takes the slot.
    let mut payload = world(OPENING_ROUND, (29, 6));
    payload["worldNews"] = json!({"officialNews": COPPER_FALLS});
    let turn = turn_from(payload);

    let mut state = BotState::default();
    state.observe(&turn);
    assert!(
        state.request_prompt(PromptPurpose::News, &turn),
        "the day's news was refused its own read"
    );
    assert!(
        !state.request_prompt(PromptPurpose::Treasure, &turn),
        "the treasure took the prompt off the day's news"
    );
    assert_eq!(
        state.llm_used_today, 1,
        "the refused ask was charged to the day's budget"
    );
}

// ---------------------------------------------------------------------------
// 6. The dynamic half: where the order was right, it is unchanged
// ---------------------------------------------------------------------------

/// A market announcement that names an ore and points its price down.
const COPPER_FALLS: &str = "矿业协会通告：铜矿库存充足，需求下降，预计价格下跌。";

#[test]
fn with_no_window_the_news_still_goes_first() {
    // 「顺序上不能固定」 is not "the treasure always wins". With the altar out of
    // the picture the order is what it was and what it should be: the day's
    // price trend takes the round's prompt (「价格趋势直接决定了我们采集哪些矿，
    // 至关重要」), and the task point is accepted normally.
    //
    // The treasure is given a full set of legends so that it HAS an ask to make
    // this round. The test is therefore about the ranking and not about the
    // treasure being unready: a window that is not live leaves the day's news
    // first, and the treasure's ask waits for a round the news does not want.
    let mut payload = world(OPENING_ROUND, (29, 6));
    payload["worldNews"] = json!({"officialNews": COPPER_FALLS});
    let turn = turn_from(payload);

    let mut state = BotState::default();
    state.treasure.legends = vec![
        (1, "传闻其一".to_string()),
        (2, "传闻其二".to_string()),
    ];
    state.observe(&turn);
    let plan = day::plan(&turn, &mut state);

    let prompt = plan
        .prompt
        .as_deref()
        .expect("the day's news never went to the LLM");
    assert!(
        prompt.contains(COPPER_FALLS),
        "the round's prompt is not the news read:\n{prompt}"
    );
    assert!(
        !prompt.contains("民间传闻"),
        "the treasure took the prompt off the day's news:\n{prompt}"
    );
    assert_eq!(
        state.news.attempts, 1,
        "the news read did not make its ask"
    );
    assert_eq!(
        state.treasure.phase,
        TreasurePhase::Idle,
        "the treasure asked in the round the news owned"
    );
    assert_eq!(
        pioneer_command(&plan),
        Some("acceptTask"),
        "a task point that a closed window leaves alone was not accepted"
    );
}
