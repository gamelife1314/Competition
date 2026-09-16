//! P2-1: treasure-line completion — the pioneer holds the altar while waiting
//! for opening day, the treasure LLM call waits until day 2 (day 1 belongs to
//! the wall ring), and the sacrifice shopping never eats the night's medicine
//! money.

use std::collections::HashSet;

use serde_json::{json, Value};

use coregeek::brain::treasure::{holds_altar, plan_pioneer};
use coregeek::brain::Plan;
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::{BotState, TreasurePhase, TreasurePlan, TreasureState};

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// A bare board at `round_no` with a pioneer at `pioneer` and a weapon shop.
fn world(round_no: i64, pioneer: (i32, i32)) -> Value {
    json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 28, "y": 20}, "neutralType": "weaponShop"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [],
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

fn with_gold(payload: &mut Value, gold: i64) {
    payload["teamOur"]["goldNum"] = json!(gold);
}

fn waiting_state(open_day: i64) -> BotState {
    let mut state = BotState::default();
    state.current_day = 2;
    state.treasure.phase = TreasurePhase::HavePlan;
    state.treasure.plan = Some(TreasurePlan {
        pos: coregeek::protocol::Pos { x: 30, y: 5 },
        items: vec!["StarSand".to_string()],
        open_day,
    });
    state
}

#[test]
fn the_pioneer_holds_the_altar_while_waiting_for_opening_day() {
    // Day 2, plan opens day 3, pioneer next to the altar: HOLD. Loitering at
    // the task point or retreating inside would drag it a cell away every
    // round and back the next — the oscillation that used to spend the whole
    // opening window commuting.
    let turn = turn_from(world(140, (29, 6)));
    let state = waiting_state(3);
    let pioneer = turn.pioneer().unwrap();
    assert!(holds_altar(&turn, &state, pioneer));

    // Opening day arrived: stop holding, go summon.
    let turn = turn_from(world(270, (29, 6)));
    assert!(!holds_altar(&turn, &state, pioneer));

    // Too far from the altar: nothing to hold for, walk first.
    let turn = turn_from(world(140, (20, 10)));
    let pioneer = turn.pioneer().unwrap();
    assert!(!holds_altar(&turn, &state, pioneer));

    // No plan yet: the treasure phase owns no cell.
    let turn = turn_from(world(140, (29, 6)));
    let state = BotState::default();
    assert!(!holds_altar(&turn, &state, pioneer));
}

#[test]
fn the_treasure_llm_waits_for_day_two() {
    // Two legends in hand, prompt budget free — but day 1 belongs to the wall
    // ring and the towers, so the ask waits.
    let mut state = BotState::default();
    state.current_day = 1;
    state.treasure.legends = vec![(1, "传闻一".into()), (1, "传闻二".into())];
    let turn = turn_from(world(10, (13, 26)));
    let pioneer = turn.pioneer().unwrap().clone();
    let mut claimed = HashSet::new();
    let mut plan = Plan::default();
    plan_pioneer(&turn, &mut state, &pioneer, &mut claimed, &mut plan);
    assert!(plan.prompt.is_none(), "day 1 spends no LLM call on treasure");
    assert!(matches!(state.treasure.phase, TreasurePhase::Idle));

    // Day 2 with two legends: the ask fires and the budget is consumed.
    let turn = turn_from(world(140, (13, 26)));
    let pioneer = turn.pioneer().unwrap().clone();
    let mut plan = Plan::default();
    plan_pioneer(&turn, &mut state, &pioneer, &mut claimed, &mut plan);
    assert!(plan.prompt.is_some(), "day 2 asks the LLM for the altar plan");
    assert!(matches!(state.treasure.phase, TreasurePhase::AskedLlm { .. }));
    assert_eq!(state.llm_used_today, 1);
}

#[test]
fn the_sacrifice_shopping_leaves_the_medicine_money_intact() {
    // Plan in hand, one StarSand missing, pioneer at the shop stand.
    let mut state = waiting_state(3);
    let turn = turn_from(world(140, (28, 21)));
    let pioneer = turn.pioneer().unwrap().clone();
    let mut claimed = HashSet::new();

    // 24 gold: the 15-gold item is "affordable" — and would leave 9, one gold
    // short of the night's first Medicine. A wrong summon consumes the item
    // for nothing (result code 3), so the gamble is only taken from surplus.
    let mut payload = world(140, (28, 21));
    with_gold(&mut payload, 24);
    let turn = turn_from(payload);
    let mut plan = Plan::default();
    let cmd = plan_pioneer(&turn, &mut state, &pioneer, &mut claimed, &mut plan);
    assert!(cmd.is_none(), "24 gold does not cover 15 + the medicine floor");

    // 25 gold: item bought, floor intact.
    let mut payload = world(140, (28, 21));
    with_gold(&mut payload, 25);
    let turn = turn_from(payload);
    let cmd = plan_pioneer(&turn, &mut state, &pioneer, &mut claimed, &mut plan)
        .expect("25 gold buys the item and keeps the floor");
    assert_eq!(cmd.action, "buy");
    assert_eq!(cmd.name.as_deref(), Some("StarSand"));
}

/// The floor is the DOSE, not the item's price — and the test above only says
/// so while the two happen to be 15 and 10. The owner's correction is that the
/// sacrifice's money must be flexible (「改成灵活运用吧」), which only works if
/// the floor is the one number that is *not* flexible: a cheap sacrifice must
/// not cheapen the medicine, or the flexibility becomes a way to spend it.
#[test]
fn the_medicine_floor_is_one_dose_whatever_the_item_costs() {
    let mut payload = world(140, (28, 21));
    with_shop_items(&mut payload, &[("IronWhistle", 5)]);
    let mut state = BotState::default();
    state.current_day = 2;
    state.treasure.phase = TreasurePhase::HavePlan;
    state.treasure.plan = Some(TreasurePlan {
        pos: Pos { x: 30, y: 5 },
        items: vec!["IronWhistle".into()],
        open_day: 3,
    });

    // 14 gold is 5 for the item and 9 left: one gold short of a Medicine, so
    // the purchase is refused even though the item itself is trivially
    // affordable. A floor that tracked the item's price would have bought it.
    with_gold(&mut payload, 14);
    let turn = turn_from(payload.clone());
    let pioneer = turn.pioneer().unwrap().clone();
    let mut claimed = HashSet::new();
    let mut plan = Plan::default();
    assert!(
        plan_pioneer(&turn, &mut state, &pioneer, &mut claimed, &mut plan).is_none(),
        "14 gold is 5 + 9: the item is affordable, the dose is not"
    );

    // 15 is the first purse that leaves a dose behind, so it buys.
    with_gold(&mut payload, 15);
    let turn = turn_from(payload.clone());
    let pioneer = turn.pioneer().unwrap().clone();
    let cmd = plan_pioneer(&turn, &mut state, &pioneer, &mut claimed, &mut plan)
        .expect("15 gold leaves exactly one Medicine");
    assert_eq!(cmd.action, "buy");
}

#[test]
fn day_plan_never_drags_the_waiting_pioneer_off_the_altar() {
    // The wiring behind holds_altar: with the sacrifice already in the pack
    // and opening day tomorrow, the treasure step has nothing to emit — and
    // the loiter/retreat fall-throughs in pioneer_day must NOT take over.
    let mut state = waiting_state(3);
    let mut payload = world(140, (29, 6));
    payload["teamOur"]["roles"][1]["backpack"] = json!(["StarSand"]);
    let turn = turn_from(payload);
    let mut day_state = BotState::default();
    day_state.current_day = 2;
    day_state.treasure = state.treasure.clone();
    let plan = coregeek::brain::day::plan(&turn, &mut day_state);
    assert!(
        plan.commands.get(&10004).is_none(),
        "a pioneer waiting out the altar's last day holds its cell: {:?}",
        plan.commands.get(&10004)
    );

    // Opening day: the same board summons immediately.
    let mut day_state = BotState::default();
    day_state.current_day = 3;
    day_state.treasure = state.treasure.clone();
    let mut payload = world(270, (29, 6));
    payload["teamOur"]["roles"][1]["backpack"] = json!(["StarSand"]);
    let turn = turn_from(payload);
    let plan = coregeek::brain::day::plan(&turn, &mut day_state);
    let cmd = plan
        .commands
        .get(&10004)
        .expect("opening day summons, it does not hold");
    assert_eq!(cmd.action, "summonTreasure");
    let _ = &mut state;
}

// ---------------------------------------------------------------------------
// P2-2: reviving the treasure line — the gold reserve, and a summon that is
// issued once and waited for.
// ---------------------------------------------------------------------------

use coregeek::brain::treasure::{gold_reserve, TREASURE_RESERVE_CAP};
use coregeek::protocol::Pos;

/// `world` with a second shop item on the list.
fn with_shop_items(payload: &mut Value, items: &[(&str, i64)]) {
    payload["weaponShopList"] = json!(items
        .iter()
        .map(|(name, price)| json!({"name": name, "price": price}))
        .collect::<Vec<_>>());
}

#[test]
fn the_sacrifice_gold_is_reserved_out_of_the_shopping_list() {
    // The whole reason the treasure line never ran: `intent_list` spent every
    // coin on vouchers, so the pioneer reached the shop with an empty purse and
    // stood at the counter while the opening day went past. While the plan is
    // live and the items are missing, the gold they cost is reserved.
    let mut payload = world(140, (28, 21));
    with_shop_items(&mut payload, &[("StarSand", 15), ("FlameBreath", 15)]);
    with_gold(&mut payload, 60);
    let turn = turn_from(payload.clone());

    let mut state = BotState::default();
    state.current_day = 2;
    state.treasure.phase = TreasurePhase::HavePlan;
    state.treasure.plan = Some(TreasurePlan {
        pos: Pos { x: 30, y: 5 },
        items: vec!["StarSand".into(), "FlameBreath".into()],
        open_day: 3,
    });
    assert_eq!(
        gold_reserve(&turn, &state),
        30,
        "two 15-gold items are two items' worth of gold"
    );

    // Holding one of them already halves the reserve.
    payload["teamOur"]["roles"][1]["backpack"] = json!(["StarSand"]);
    let turn = turn_from(payload.clone());
    assert_eq!(gold_reserve(&turn, &state), 15);

    // Both in the pack: the errand needs no reservation at all.
    payload["teamOur"]["roles"][1]["backpack"] = json!(["StarSand", "FlameBreath"]);
    let turn = turn_from(payload.clone());
    assert_eq!(gold_reserve(&turn, &state), 0);
}

/// The reserve is the PLAN's arithmetic, in both directions — the owner's
/// 「有时候宝藏开启可能不止 15 币」 and its mirror image, a sacrifice cheaper
/// than the old constant. A fixed 15 was wrong for a three-item list (45 gold)
/// and wrong for a five-gold item (it held back three times what the errand
/// needed, out of a defence that was still short of vouchers).
#[test]
fn the_item_money_follows_the_plan_and_not_a_constant() {
    // Three items at 15 apiece: the reserve is 45, three times the constant.
    let mut payload = world(140, (28, 21));
    with_shop_items(&mut payload, &[("StarSand", 15)]);
    with_gold(&mut payload, 200);
    let mut state = BotState::default();
    state.current_day = 2;
    state.treasure.phase = TreasurePhase::HavePlan;
    state.treasure.plan = Some(TreasurePlan {
        pos: Pos { x: 30, y: 5 },
        items: vec!["StarSand".into(); 3],
        open_day: 3,
    });
    let turn = turn_from(payload.clone());
    assert_eq!(
        gold_reserve(&turn, &state),
        TREASURE_RESERVE_CAP,
        "a three-item sacrifice is 45 gold, not 15"
    );

    // A five-gold item is reserved at five — the reserve reads the shop's
    // quote, not a guess about what a sacrifice costs.
    with_shop_items(&mut payload, &[("IronWhistle", 5)]);
    state.treasure.plan = Some(TreasurePlan {
        pos: Pos { x: 30, y: 5 },
        items: vec!["IronWhistle".into()],
        open_day: 3,
    });
    let turn = turn_from(payload.clone());
    assert_eq!(
        gold_reserve(&turn, &state),
        5,
        "the reserve is the quote, not the constant"
    );
}

#[test]
fn the_reserve_can_never_freeze_the_whole_purse() {
    let mut payload = world(140, (28, 21));
    with_shop_items(&mut payload, &[("StarSand", 15)]);
    let mut state = BotState::default();
    state.current_day = 2;
    state.treasure.phase = TreasurePhase::HavePlan;
    // A six-item sacrifice is 90 gold at 15 apiece: more than the cap allows
    // the line to hold.
    state.treasure.plan = Some(TreasurePlan {
        pos: Pos { x: 30, y: 5 },
        items: vec!["StarSand".into(); 6],
        open_day: 3,
    });

    with_gold(&mut payload, 200);
    let turn = turn_from(payload.clone());
    assert_eq!(
        gold_reserve(&turn, &state),
        TREASURE_RESERVE_CAP,
        "the reserve is capped so the defence keeps the rest"
    );

    // Gold that is not in the purse cannot be reserved: a reserve larger than
    // the purse is not a reserve, it is a spending freeze.
    with_gold(&mut payload, 20);
    let turn = turn_from(payload.clone());
    assert_eq!(
        gold_reserve(&turn, &state),
        0,
        "20 gold does not cover 45 + the medicine floor"
    );

    // And a reserve that would eat the night's medicine money is not taken.
    with_gold(&mut payload, 50);
    let turn = turn_from(payload.clone());
    assert_eq!(gold_reserve(&turn, &state), 0);

    // Past the opening day the altar will not open and the gold goes home.
    with_gold(&mut payload, 200);
    let mut expired = BotState::default();
    expired.treasure.phase = TreasurePhase::HavePlan;
    expired.treasure.plan = Some(TreasurePlan {
        pos: Pos { x: 30, y: 5 },
        items: vec!["StarSand".into()],
        open_day: 2,
    });
    let later = turn_from(world(210, (28, 21))); // day 3
    assert_eq!(gold_reserve(&later, &expired), 0);
}

#[test]
fn the_day_planner_hands_the_reserve_to_the_build_budget() {
    // End to end: the reserve reaches `economy::budget` as part of the day's
    // reserve, which is what stops the shopping list spending it.
    use coregeek::brain::economy;

    let mut payload = world(140, (28, 21));
    with_shop_items(&mut payload, &[("StarSand", 15)]);
    with_gold(&mut payload, 60);
    let turn = turn_from(payload.clone());
    let mut plan_state = BotState::default();
    plan_state.current_day = 2;
    plan_state.treasure = TreasureState {
        phase: TreasurePhase::HavePlan,
        plan: Some(TreasurePlan {
            pos: Pos { x: 30, y: 5 },
            items: vec!["StarSand".into()],
            open_day: 3,
        }),
        ..Default::default()
    };
    let reserved = gold_reserve(&turn, &plan_state);
    assert_eq!(reserved, 15);

    // The budget the planner builds sees the floor: with 60 gold and a 15-gold
    // reserve, a 50-gold voucher is not affordable.
    let budget = economy::budget(&turn, &BotState::default(), 15);
    assert!(
        budget.shopping.iter().all(|need| need.name != "WeaponUpgradeVoucher1"),
        "the reserved gold was spent on a voucher"
    );
}

#[test]
fn a_summon_is_issued_once_and_its_verdict_waited_for() {
    // (Round 270 is day 3 of a 130-round day.)
    // The state machine that made the treasure line unable to survive its own
    // first summon: `HavePlan` stayed set after the summon, so the next round
    // the pioneer — still beside the altar, items still in the pack, day still
    // open — fired the same summon again. `summon_attempts` counted those
    // repeats, so the four-summon cap was reached in four rounds of ONE gamble
    // and a result of 2 ("not open yet", which pushes the day and retries in
    // place) never got its retry.
    let mut payload = world(270, (29, 6)); // opening day
    payload["teamOur"]["roles"][1]["backpack"] = json!(["StarSand"]);
    with_gold(&mut payload, 100);
    let turn = turn_from(payload);
    let pioneer = turn.pioneer().unwrap().clone();

    let mut state = waiting_state(3);
    let mut claimed = HashSet::new();
    let mut plan = Plan::default();
    let cmd = plan_pioneer(&turn, &mut state, &pioneer, &mut claimed, &mut plan)
        .expect("opening day summons");
    assert_eq!(cmd.action, "summonTreasure");
    assert_eq!(state.treasure.summon_attempts, 1);

    // The next round: the verdict has not come back, so nothing is re-issued
    // and the pioneer holds its cell.
    let follow_up = turn_from(world(271, (29, 6)));
    let mut plan = Plan::default();
    let cmd = plan_pioneer(&follow_up, &mut state, &pioneer, &mut claimed, &mut plan);
    assert!(cmd.is_none(), "a summon in flight is not re-issued: {cmd:?}");
    assert_eq!(state.treasure.summon_attempts, 1, "and not counted twice");
    assert!(holds_altar(&follow_up, &state, &pioneer));
}

#[test]
fn a_not_open_yet_verdict_gets_its_in_place_retry() {
    // Result code 2: push the opening day out and retry in place, then re-ask
    // the LLM, then stop at four REAL summons. The cap counts gambles taken,
    // not verdicts received — a legal summon consumes the sacrifice items
    // whatever the outcome, so the two are the same number only if the verdict
    // is not counted as well.
    let mut state = waiting_state(3);
    state.treasure.summon_attempts = 1; // the summon already went out
    state.treasure.phase = TreasurePhase::Summoned { round: 270 };

    let outcome = |state: &mut BotState, round_no: i64, code: i64| {
        let mut payload = world(round_no, (29, 6));
        payload["lastSummonTreasureResult"] = json!(code);
        let turn = turn_from(payload);
        state.observe(&turn);
    };

    outcome(&mut state, 271, 2);
    assert_eq!(state.treasure.summon_attempts, 1, "the verdict is not a summon");
    assert!(
        matches!(state.treasure.phase, TreasurePhase::HavePlan),
        "one in-place retry: {:?}",
        state.treasure.phase
    );
    assert_eq!(
        state.treasure.plan.as_ref().map(|plan| plan.open_day),
        Some(4),
        "the opening day is pushed out by one"
    );

    // Four real gambles is the hard cap, and the fifth never happens.
    state.treasure.phase = TreasurePhase::Summoned { round: 272 };
    state.treasure.summon_attempts = 4;
    outcome(&mut state, 273, 2);
    assert!(
        matches!(state.treasure.phase, TreasurePhase::Done),
        "the 4-attempt cap still ends the line: {:?}",
        state.treasure.phase
    );
}

#[test]
fn a_wrong_item_verdict_still_re_asks_with_feedback() {
    let mut state = waiting_state(3);
    state.treasure.phase = TreasurePhase::Summoned { round: 270 };
    let mut payload = world(271, (29, 6));
    payload["lastSummonTreasureResult"] = json!(3);
    let turn = turn_from(payload);
    state.observe(&turn);
    assert!(matches!(state.treasure.phase, TreasurePhase::Idle));
    assert!(state.treasure.plan.is_none());
    assert_eq!(state.treasure.summon_attempts, 0, "code 3 is a verdict, not a summon");
}

#[test]
fn a_lost_verdict_resumes_instead_of_freezing_the_line() {
    // A task running through the night can swallow the summon channel. The
    // line must not stay `Summoned` for the rest of the match.
    let mut state = waiting_state(3);
    state.treasure.phase = TreasurePhase::Summoned { round: 270 };
    let turn = turn_from(world(275, (29, 6))); // four rounds later
    let pioneer = turn.pioneer().unwrap().clone();
    let mut claimed = HashSet::new();
    let mut plan = Plan::default();
    plan_pioneer(&turn, &mut state, &pioneer, &mut claimed, &mut plan);
    assert!(
        matches!(state.treasure.phase, TreasurePhase::HavePlan),
        "the line resumed: {:?}",
        state.treasure.phase
    );
}
