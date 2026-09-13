//! The stdout log is the only witness a match leaves behind.
//!
//! A match is 2600 rounds and the log is handed to an analysis agent that has
//! to read it, so `round` records are compressed three ways: nothing empty,
//! nothing repeated, and no clock where the round number is already the clock.
//! Compression is the one kind of change that can destroy evidence silently —
//! a pruned field and a field that was never true look identical afterwards —
//! so the rules that could delete a fact are pinned here.
//!
//! The change-gating itself cannot drop a *moved* block by construction: a
//! block's signature IS the block, so anything that would have been written
//! differently is a different signature and is re-sent. What these tests check
//! is that the wiring holds — that a quiet round really does skip the block and
//! a moved one really does re-send it.

use serde_json::json;

use coregeek::brain::decide_with;
use coregeek::log::{changed, prune, xy, LogSigs};
use coregeek::model::Turn;
use coregeek::protocol::{Pos, Request, RoleCommand};
use coregeek::state::BotState;

fn turn_from(payload: serde_json::Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

#[test]
fn nothing_empty_survives_a_record() {
    let pruned = prune(json!({
        "errors": [],
        "failures": [],
        "lastCmdResult": "",
        "phaseTask": "",
        "promptChars": 0,
        "missing": null,
        "volley": {"rejected": [], "fired": 0, "noRobotDamage": false},
    }));
    assert_eq!(
        pruned,
        json!({
            "promptChars": 0,
            "volley": {"fired": 0, "noRobotDamage": false},
        }),
        "nulls, empty strings, empty arrays and empty objects all say what a \
         missing key says, and on a quiet round they are most of the record"
    );
}

#[test]
fn a_zero_or_a_false_is_a_fact_and_is_kept() {
    // The distinction the whole rule turns on: `gold: 0` means we are broke,
    // `fired: 0` means no gun fired, `noRobotDamage: false` is a claim about
    // the round — while `"errors": []` means only that there were no errors.
    // Pruning the first three would turn "we were broke" into "the record does
    // not mention gold", which reads as a logging bug rather than a fact.
    let pruned = prune(json!({
        "gold": 0,
        "score": 0,
        "fired": 0,
        "kills": 0,
        "noRobotDamage": false,
        "active": false,
    }));
    assert_eq!(
        pruned,
        json!({
            "gold": 0, "score": 0, "fired": 0, "kills": 0,
            "noRobotDamage": false, "active": false,
        })
    );
}

#[test]
fn pruning_reaches_inside_arrays_without_renumbering_them() {
    // An array is positional. Dropping the `null` out of `["unknown", null]`
    // would slide the second element into the first's place, so `null` stays
    // where it is — only the objects inside get pruned.
    let pruned = prune(json!({
        "target": [3, 7],
        "rows": [{"tower": 1, "note": ""}, {"tower": 2, "fired": 1}],
        "sparse": [null, 4],
    }));
    assert_eq!(
        pruned,
        json!({
            "target": [3, 7],
            "rows": [{"tower": 1}, {"tower": 2, "fired": 1}],
            "sparse": [null, 4],
        })
    );
}

#[test]
fn an_object_that_prunes_to_nothing_takes_its_key_with_it() {
    // `volley` on a round where nothing fired is all zeros, empty lists and
    // nulls except its two booleans — this is the case that decides whether the
    // key survives at all.
    assert_eq!(prune(json!({"volley": {"rejected": [], "volleys": []}})), json!({}));
}

#[test]
fn a_block_is_written_when_it_appears_and_when_it_moves_but_not_otherwise() {
    let mut sigs = LogSigs::default();
    let towers = json!([{"id": 30000, "lvl": 1, "hp": 300, "cd": 0}]);

    assert!(
        changed(&mut sigs.towers, &towers),
        "the match's first record has to carry the block"
    );
    assert!(
        !changed(&mut sigs.towers, &towers),
        "the second identical round must not repeat it"
    );

    let damaged = json!([{"id": 30000, "lvl": 1, "hp": 299, "cd": 0}]);
    assert!(
        changed(&mut sigs.towers, &damaged),
        "one point of damage is a change and has to be written"
    );
    assert!(!changed(&mut sigs.towers, &damaged));

    // Slots are independent: the wall line moving must not re-send the towers.
    let mut sigs = LogSigs::default();
    changed(&mut sigs.towers, &towers);
    assert!(changed(&mut sigs.wall, &json!({"count": 8, "hp": 4000})));
    assert!(
        !changed(&mut sigs.towers, &towers),
        "one block moving does not un-gate another"
    );
}

#[test]
fn a_position_costs_two_numbers_not_two_key_names() {
    assert_eq!(xy(Pos { x: 36, y: 4 }), json!([36, 4]));
}

#[test]
fn a_sale_records_the_purse_it_was_made_from() {
    // WORKFLOW_REQUEST §7.1 question 6 reads this event against the
    // `round.gold` series, so the field names are an interface: drop `round`
    // and the two streams stop joining, drop `gold` and "the purse is pinned
    // at 25" stops being answerable. Until this round the event did not exist
    // at all and the recipe matched zero lines.
    let turn = turn_from(json!({
        "roundNo": 42,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 25, "totalScore": 0, "playerTasks": [],
            "roles": [
                {"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                 "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []},
                {"id": 10002, "pos": {"x": 14, "y": 24}, "roleType": "worker",
                 "health": 220, "attackPower": 0, "attackRange": 0,
                 "level": 1, "backPackCapability": 100, "backpack": ["iron", "iron"]},
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    }));
    let role = turn.role_by_id(10002).expect("the worker exists");

    let record = coregeek::log::sell_record(&turn, role, &RoleCommand::sell("iron", 2));
    assert_eq!(
        record,
        json!({"round": 42, "role": 10002, "ore": "iron", "num": 2, "gold": 25})
    );
}

/// A board with one wall whose health the caller controls, so the same payload
/// can be replayed twice and then perturbed in exactly one field.
fn board(round_no: i64, wall_hp: i64) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 75, "totalScore": 0,
            "playerTasks": [],
            "roles": [
                {"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                 "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []},
                {"id": 10002, "pos": {"x": 14, "y": 24}, "roleType": "worker",
                 "health": 220, "attackPower": 0, "attackRange": 0,
                 "level": 1, "backPackCapability": 100, "backpack": []},
                {"id": 30000, "pos": {"x": 11, "y": 22}, "roleType": "gatling",
                 "health": 300, "attackPower": 50, "attackRange": 5,
                 "level": 1, "backPackCapability": 0, "backpack": []},
                {"id": 40000, "pos": {"x": 12, "y": 22}, "roleType": "wall",
                 "health": wall_hp, "level": 1, "backPackCapability": 0, "backpack": []},
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    }))
    .expect("payload serialises")
}

#[test]
fn a_quiet_round_leaves_the_gates_shut_and_a_moved_one_opens_them() {
    let mut state = BotState::default();

    decide_with(&mut state, &board(1, 500)).expect("round 1 decides");
    let towers = state.log_sigs.towers.clone();
    let base = state.log_sigs.station.clone();
    assert!(towers.is_some(), "the first round writes the tower block");
    assert!(state.log_sigs.wall.is_some(), "and the wall block");
    assert!(base.is_some(), "and the base");

    // The same board again. The wall moves once more — its `hpDelta` has no
    // value to report on round 1 (`null`) and reports `0` from round 2 on, and
    // a block whose signature changed is a block that gets written. The towers
    // and the base, which carry no such running number, are already quiet.
    decide_with(&mut state, &board(2, 500)).expect("round 2 decides");
    assert_eq!(state.log_sigs.towers, towers, "an unchanged round is silent");
    assert_eq!(state.log_sigs.station, base);
    let wall = state.log_sigs.wall.clone();

    // Round 3 is identical to round 2 in every field the record logs, so this
    // is the round where nothing at all may be re-sent.
    decide_with(&mut state, &board(3, 500)).expect("round 3 decides");
    assert_eq!(state.log_sigs.wall, wall, "a quiet round re-sends nothing");
    assert_eq!(state.log_sigs.towers, towers);
    assert_eq!(state.log_sigs.station, base);

    // One wall takes damage. Only the wall block may re-open.
    decide_with(&mut state, &board(4, 460)).expect("round 4 decides");
    assert_ne!(
        state.log_sigs.wall, wall,
        "a wall that lost 40 HP is a change and has to be written"
    );
    assert_eq!(
        state.log_sigs.towers, towers,
        "and the towers, which did not move, stay shut"
    );
}
