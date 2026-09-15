//! Issue #206 §5 item 8: the pack has a way out.
//!
//! `protocol::RoleCommand::drop_item` has existed since the first day and had
//! zero callers — the plan's row 8 is exactly that ("定义了但零调用"), and the
//! rule it asks for is one line: the pack is full AND a vein worth more than
//! the cheapest thing carried is within reach → drop the cheapest.
//!
//! Three things this file is careful about, because each of them is a way the
//! wiring could be wrong while the test stayed green:
//!
//! 1. **The sale comes first.** A pack the vendor will take is SOLD, and that
//!    order is not negotiable — the collect→sell→buy loop is what pays for the
//!    towers. So a full pack on a board with a reachable vendor must still walk
//!    to the vendor, and `a_pack_that_can_be_sold_is_sold_not_dumped` is that
//!    lock. It is also why the drop sits at step 10 rather than beside the sale.
//! 2. **The ring's stone is not the miner's to throw away.** Stone the wall
//!    line is still holding back is not droppable at all, so the one board where
//!    the pack can be full and nothing can be sold — a carrier whose load the
//!    ring still owes walls for — drops nothing and behaves as it did before.
//! 3. **The comparison is per ore, not per trip.** `choose_mine` prices a trip
//!    as `value × free / rounds`, and a full pack is `free == 0`, so it prices
//!    every vein on the board at zero. The question the drop answers is what a
//!    SLOT is worth, and a slot is worth whatever fills it.

use serde_json::{json, Value};

use coregeek::brain::economy::discard_command;
use coregeek::model::{chebyshev, Turn, Unit, COPPER, IRON, STONE};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn zone(x: i32, y: i32, kind: &str) -> Value {
    json!({"pos": {"x": x, "y": y}, "neutralType": kind})
}

/// What the vendor pays this round. Iron is the cheap ore the miner is meant to
/// trade away and copper the dear one it is meant to trade it for; stone is the
/// ring's own material and the cheapest thing on the board.
fn prices() -> Value {
    json!([
        {"name": "stone", "price": 2},
        {"name": "iron", "price": 8},
        {"name": "copper", "price": 12},
    ])
}

/// Round 25 of day 1 — well before the dusk cash-out, so the mining steps are
/// live. The copper vein is NEAR (14,20) and the iron is FAR (20,20): the drop
/// only pays for itself if the vein it makes room for is the one a free slot
/// would actually be spent on, and gold-per-round has to agree. `extra_zones`
/// is where a vendor goes, or does not.
fn board(pack: Vec<&str>, extra_zones: Vec<Value>) -> Value {
    let mut zones = vec![zone(14, 20, COPPER), zone(20, 20, IRON)];
    zones.extend(extra_zones);
    json!({
        "roundNo": 25,
        "mapInfo": {"width": 41, "height": 32, "zones": zones},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0, "playerTasks": [],
            "roles": [
                json!({"id": 10001, "pos": {"x": 10, "y": 20}, "roleType": "station",
                       "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []}),
                json!({"id": 10002, "pos": {"x": 12, "y": 20}, "roleType": "worker",
                       "health": 220, "attackPower": 0, "attackRange": 0,
                       "level": 1, "backPackCapability": 100, "backpack": pack}),
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "vendorShopList": prices(),
        "weaponShopList": [],
    })
}

/// A hundred items in a hundred slots — `backpack_full()` is the gate the whole
/// rule hangs on, so every board here is exactly at the brim.
fn full(items: Vec<&'static str>) -> Vec<&'static str> {
    assert_eq!(items.len(), 100, "the test board is not a full pack");
    items
}

fn worker_of(turn: &Turn) -> Unit {
    turn.role_by_id(10002).expect("worker 10002").clone()
}

// ---------------------------------------------------------------------------
// The rule, on the pack alone
// ---------------------------------------------------------------------------

#[test]
fn the_cheapest_ore_buys_the_slot_for_the_dearer_vein() {
    // All three ores in the pack, and the ring complete (demand 0, so even the
    // stone is surplus). Copper at 12 is worth more than the 2-gold stone, so a
    // stone goes over the side — not the iron the worker could sell for four
    // times as much.
    let mut pack = vec![STONE; 40];
    pack.extend(vec![IRON; 30]);
    pack.extend(vec![COPPER; 30]);
    let turn = turn_from(board(full(pack), vec![]));
    let role = worker_of(&turn);

    let cmd = discard_command(&turn, &BotState::default(), &role, 0)
        .expect("a slot is worth buying on this board");
    assert_eq!(cmd.action, "drop");
    assert_eq!(
        cmd.name.as_deref(),
        Some(STONE),
        "the wrong ore was thrown away: {cmd:?}"
    );
    // One item, named and nothing else: no target, no count.
    assert!(
        cmd.targetPos.is_none() && cmd.num.is_none(),
        "a drop names an item and nothing else: {cmd:?}"
    );
}

#[test]
fn nothing_is_dropped_when_every_vein_is_worth_less_than_the_pack() {
    // The gate the plan states: a HIGHER-value vein. A pack of copper with only
    // iron and copper vein left on the board is a pack whose cheapest item is
    // worth as much as anything the walk could replace it with — throwing one
    // away would be paying gold for nothing.
    let turn = turn_from(board(full(vec![COPPER; 100]), vec![]));
    let role = worker_of(&turn);
    assert!(
        discard_command(&turn, &BotState::default(), &role, 0).is_none(),
        "copper was thrown away for a vein that does not pay for it"
    );
}

#[test]
fn the_rings_stone_is_not_the_miners_to_throw_away() {
    // A carrier whose whole load is stone, with the ring still owing every cell
    // of itself: there is nothing in the pack the vendor would take and nothing
    // the ring is not going to build with, so the answer is "drop nothing" and
    // the role behaves exactly as it did before this step existed.
    //
    // `stone_demand` is the only variable between the two halves. At 0 the ring
    // is complete and the load is a surplus the miner may spend on a slot; at
    // 100 — every cell of a ring the pool holds exactly enough for — the same
    // load is the wall line and it is not the miner's to throw away. A cheaper
    // rule, "drop the least valuable ore" with no veto, sends the stone over
    // the side in both.
    let turn = turn_from(board(full(vec![STONE; 100]), vec![]));
    let role = worker_of(&turn);
    let state = BotState::default();

    assert!(
        discard_command(&turn, &state, &role, 0).is_some(),
        "test setup: surplus stone is not droppable at all"
    );
    assert!(
        discard_command(&turn, &state, &role, 100).is_none(),
        "the ring's stone was thrown away to buy a slot"
    );
}

#[test]
fn a_pack_with_room_is_left_alone() {
    // The gate itself. `drop_item` exists for a FULL pack; a worker with a slot
    // free has nothing to fix and must never be handed a `drop` — that would be
    // ore given away on every round of the match.
    let turn = turn_from(board(vec![STONE; 99], vec![]));
    let role = worker_of(&turn);
    assert!(
        discard_command(&turn, &BotState::default(), &role, 0).is_none(),
        "a pack with a free slot was raided anyway"
    );
}

// ---------------------------------------------------------------------------
// The wiring: what the day planner actually sends
// ---------------------------------------------------------------------------

/// One role's command out of `brain::decide_with`, as (action, name, target).
fn command_for(payload: Value, role_id: i64) -> (String, Option<String>, Option<Pos>) {
    let body = payload.to_string();
    let raw = coregeek::brain::decide_with(&mut BotState::default(), body.as_bytes())
        .expect("the board decides");
    let response: Value = serde_json::from_str(&raw).expect("a JSON response");
    let cmd = response["roleCommandMap"][role_id.to_string()].clone();
    assert!(!cmd.is_null(), "no command for role {role_id}: {raw}");
    let target = cmd["targetPos"]
        .as_array()
        .and_then(|list| list.first())
        .map(|pos| Pos {
            x: pos["x"].as_i64().unwrap_or_default() as i32,
            y: pos["y"].as_i64().unwrap_or_default() as i32,
        });
    (
        cmd["action"].as_str().unwrap_or_default().to_string(),
        cmd["name"].as_str().map(str::to_string),
        target,
    )
}

#[test]
fn a_pack_that_can_be_sold_is_sold_not_dumped() {
    // THE ORDER. A full pack of iron with a vendor on the board is a pack on its
    // way to the vendor: the ore becomes gold, the slot frees itself, and the
    // collect→sell→buy loop keeps running. A drop here would buy the same freed
    // slot with 8 gold. So the worker walks, and no `drop` is ever emitted.
    // The vendor stands next to the worker, so the round is decided rather than
    // started: a `sell` names the ore it converts.
    let payload = board(full(vec![IRON; 100]), vec![zone(12, 21, "vendor")]);
    let (action, name, _target) = command_for(payload, 10002);
    assert_ne!(action, "drop", "the pack was dumped instead of sold: {name:?}");
    assert_eq!(action, "sell", "the worker did not sell at the counter");
    assert_eq!(
        name.as_deref(),
        Some(IRON),
        "the wrong stack was taken to the vendor"
    );
}

#[test]
fn a_full_pack_that_cannot_be_sold_trades_the_cheapest_ore_for_the_better_vein() {
    // The seam the drop is for. There is no vendor on this board, so the sale
    // has had its turn and produced nothing, and the worker would otherwise
    // stand still for the rest of the day with a pack it cannot add to. The
    // iron goes (8 gold, the cheapest thing it carries) and the copper at 12 is
    // what the freed slot is for.
    let payload = board(full(vec![IRON; 100]), vec![]);
    let (action, name, _target) = command_for(payload, 10002);
    assert_eq!(action, "drop", "the worker stood still with a full pack");
    assert_eq!(
        name.as_deref(),
        Some(IRON),
        "the cheapest carried ore is the one that leaves"
    );
}

#[test]
fn after_the_drop_the_worker_walks_to_the_vein_it_made_room_for() {
    // A drop is worth a round only if the trip follows, so this plays the two
    // rounds the judger would: the drop, then the same board with one fewer
    // item in the pack. With a slot free the miner is back in `choose_mine`,
    // and copper at 12 two steps away beats iron at 8 eight steps away — which
    // is the whole reason the slot was worth paying for.
    let (action, _, _) = command_for(board(full(vec![IRON; 100]), vec![]), 10002);
    assert_eq!(action, "drop", "test setup: no drop to follow up on");

    let mut lighter = vec![IRON; 98];
    lighter.push(COPPER); // the slot the drop bought, filled as the judger would
    let (action, _, target) = command_for(board(lighter, vec![]), 10002);
    let at = target.unwrap_or_else(|| panic!("nothing followed the drop (sent {action})"));
    assert!(
        chebyshev(at, Pos { x: 14, y: 20 }) <= 1,
        "after making room the worker walked to {at:?}, not to the copper vein \
         at (14, 20)"
    );
}
