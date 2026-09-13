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
use coregeek::state::{BotState, TreasurePhase, TreasurePlan};

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
