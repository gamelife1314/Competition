//! Economy-worker (B) mainline — the behavior spec of issue #221 comment 1
//! §5/§6, pinned per person instead of per time of day.
//!
//! One priority chain, walked every round: robot distance → HP → day-1
//! weapon help → stone delivery / camp → sell triggers (T1-T5, deferred by
//! the day-1 ring hunger) → the ROI vein loop with its latch.
//! `is_day` is a threat parameter (the 4-cell robot clearance), never a fork:
//! the night does NOT recall B — comment 1 §6 keeps the economy worker out
//! among the veins and makes distance its only defence.
//!
//! The boards are minimal synthetic payloads driven through the real
//! scheduler (`brain::orchestrate::plan`), so the Worker-A planner runs
//! alongside exactly as it does in a match. The full multi-round ring
//! dynamics (the spare board's trap-veto oscillation) live in `day1_sim.rs`;
//! the tests here pin the single-round decisions those dynamics are built
//! from.
//!
//! The station stands at (10,20) on these boards, so comment 1's fixed L
//! puts the weapon sites at (9,18),(10,18),(9,20), the operator stand at
//! (9,19), the wall order on the front column x=13 and the rows y=22/y=17,
//! and the permanent entrance at (8,18)..(8,21).

use serde_json::{json, Value};

use coregeek::brain::orchestrate;
use coregeek::brain::Plan;
use coregeek::model::{chebyshev, Turn};
use coregeek::protocol::{Pos, Request, RoleCommand};
use coregeek::state::{BotState, TreasurePhase, TreasurePlan, TreasureState};

/// Worker A — the wall mainline (still legacy in phase 3; keeps claim
/// priority over B, exactly as `orchestrate::plan` orders it).
const A: i64 = 10002;
/// Worker B — the economy mainline under test.
const B: i64 = 10003;
/// The pioneer.
const P: i64 = 10004;

const VENDOR_PRICES: &[(&str, i64)] = &[("stone", 2), ("iron", 8), ("copper", 12)];
const SHOP_PRICES: &[(&str, i64)] = &[("Medicine", 10), ("Bomb", 100)];
/// Iron priced like a late-game spike — used to prove the vein latch
/// outranks a fresh ROI re-pick.
const RICH_IRON: &[(&str, i64)] = &[("stone", 2), ("iron", 100), ("copper", 12)];

fn pos(x: i32, y: i32) -> Pos {
    Pos { x, y }
}

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn worker_items(id: i64, x: i32, y: i32, cap: i64, items: Vec<&str>) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "backPackCapability": cap, "backpack": items
    })
}

fn worker(id: i64, x: i32, y: i32) -> Value {
    worker_items(id, x, y, 100, vec![])
}

fn worker_hp(id: i64, x: i32, y: i32, hp: i64) -> Value {
    let mut v = worker(id, x, y);
    v["health"] = json!(hp);
    v
}

fn pioneer(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "pioneer",
        "health": 200, "attackPower": 0, "attackRange": 0,
        "backPackCapability": 40, "backpack": []
    })
}

fn station(x: i32, y: i32) -> Value {
    json!({
        "id": 10001, "pos": {"x": x, "y": y}, "roleType": "station",
        "health": 10000, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn wall(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "wall",
        "health": 1000, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn gatling(id: i64, x: i32, y: i32) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "gatling",
        "health": 1000, "attackPower": 10, "attackRange": 3,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn robot(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "smallRobot",
        "health": hp, "abnormalState": "", "targetTeam": "challenger"
    })
}

fn zone(x: i32, y: i32, kind: &str) -> Value {
    json!({"pos": {"x": x, "y": y}, "neutralType": kind})
}

fn price_list(prices: &[(&str, i64)]) -> Value {
    Value::Array(
        prices
            .iter()
            .map(|(name, price)| json!({"name": name, "price": price}))
            .collect(),
    )
}

/// The full request payload: round, purse, our roles, robots, zones, prices.
fn world_at(
    round_no: i64,
    gold: i64,
    roles: Vec<Value>,
    robots: Vec<Value>,
    zones: Vec<Value>,
    vendor: &[(&str, i64)],
    shop: &[(&str, i64)],
) -> Value {
    json!({
        "roundNo": round_no,
        "mapInfo": {"width": 41, "height": 32, "zones": zones},
        "teamOur": {
            "type": "challenger", "goldNum": gold, "totalScore": 0,
            "playerTasks": [], "roles": roles
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": robots},
        "vendorShopList": price_list(vendor),
        "weaponShopList": price_list(shop),
    })
}

/// The 20-cell ring perimeter around a station at (10,20) (footprint
/// (10,19),(11,19),(10,20),(11,20)): x ∈ 8..13, y ∈ 17..22, edges only.
fn ring_perimeter() -> Vec<Pos> {
    let mut cells = Vec::new();
    for x in 8..=13 {
        cells.push(pos(x, 17));
        cells.push(pos(x, 22));
    }
    for y in 18..=21 {
        cells.push(pos(8, y));
        cells.push(pos(13, y));
    }
    cells
}

/// Ring walls on every perimeter cell except `skip`; ids derive from the
/// cell so repeated calls on one board never collide.
fn ring_walls(skip: &[Pos]) -> Vec<Value> {
    ring_perimeter()
        .into_iter()
        .filter(|cell| !skip.contains(cell))
        .map(|cell| wall(20000 + (cell.x * 100 + cell.y) as i64, cell.x, cell.y))
        .collect()
}

fn first_target(cmd: &RoleCommand) -> Option<Pos> {
    cmd.targetPos.as_ref().and_then(|targets| targets.first()).copied()
}

fn cmd_of<'p>(plan: &'p Plan, id: i64) -> &'p RoleCommand {
    plan.commands
        .get(&id)
        .unwrap_or_else(|| panic!("no command for role {id}"))
}

// ---------------------------------------------------------------------------
// Night: no recall (comment 1 §6) — the clearance is a threat parameter, not
// a curfew.
// ---------------------------------------------------------------------------

#[test]
fn night_keeps_b_mining_instead_of_recalling_it() {
    // Round 85 = day-1 night. No robots anywhere: the 4-cell clearance never
    // fires, and B keeps earning at the vein instead of walking home like
    // the legacy night plan did.
    let turn = turn_from(world_at(
        85,
        0,
        vec![station(10, 20), worker(A, 11, 21), worker(B, 30, 20)],
        vec![],
        vec![zone(30, 21, "iron")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "collect");
    assert_eq!(first_target(cmd), Some(pos(30, 21)));
    assert_eq!(state.vein_latch, Some(pos(30, 21)));
    assert_eq!(state.vein_hits.get(&pos(30, 21)).copied(), Some(1));
}

#[test]
fn night_robot_inside_the_clearance_pushes_b_off() {
    // The robot stands 2 cells from B — inside the 4-cell night clearance.
    // Distance is B's only defence: it steps away instead of collecting.
    let turn = turn_from(world_at(
        85,
        0,
        vec![station(10, 20), worker(A, 11, 21), worker(B, 30, 20)],
        vec![robot(30001, 32, 20, 40)],
        vec![zone(30, 21, "iron")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "move");
    // (29,19)/(29,20)/(29,21) all reach chebyshev 3 from the robot at
    // (32,20); the deterministic tie-break takes the lexicographic first.
    assert_eq!(first_target(cmd), Some(pos(29, 19)));
}

#[test]
fn night_refuses_veins_inside_the_robot_ring_but_keeps_safe_ones() {
    // The robot is 8 cells from B — no evasion. But the iron at (30,20) is
    // only 2 cells from the ROBOT: mining it would walk B into the clearance
    // ring. The copper at (20,24) is far from the robot, so the night stays
    // productive — on a different vein.
    let turn = turn_from(world_at(
        85,
        0,
        vec![station(10, 20), worker(A, 11, 21), worker(B, 20, 20)],
        vec![robot(30001, 28, 20, 40)],
        vec![zone(30, 20, "iron"), zone(20, 24, "copper")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "move"); // copper is 4 cells off: walk, not collect
    assert_eq!(state.vein_latch, Some(pos(20, 24)));
    let t = first_target(cmd).unwrap();
    assert!(chebyshev(t, pos(20, 24)) < 4, "must close on the safe vein, got {:?}", t);
}

// ---------------------------------------------------------------------------
// The vein latch (comment 1 §5 step 2): one vein at a time, revalidated
// every round, released at the 10-collect limit.
// ---------------------------------------------------------------------------

#[test]
fn vein_latch_survives_a_richer_offer_and_releases_at_the_collect_limit() {
    // Day 2, round 40 (roundNo 170): B opens on the copper at (21,20) and
    // latches it. An iron vein worth 100 sits one cell further — richer by
    // ROI from round one, but the latch outranks the re-pick until the copper
    // hits the 10-collect limit. The stone at (6,6) is A's: it is not
    // deliverable inside any dusk budget from out here, so B never sees it.
    let roles = || vec![station(10, 20), worker(A, 5, 5), worker(B, 20, 20)];
    let zones = || {
        vec![
            zone(6, 6, "stone"),
            zone(21, 20, "copper"),
            zone(21, 21, "iron"),
        ]
    };

    // Round 1: copper (12/cell) beats iron (8/cell) at equal trips — latch.
    let turn = turn_from(world_at(170, 0, roles(), vec![], zones(), VENDOR_PRICES, SHOP_PRICES));
    let mut state = BotState::default();
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "collect");
    assert_eq!(first_target(cmd), Some(pos(21, 20)));
    assert_eq!(state.vein_latch, Some(pos(21, 20)));

    // Round 2: iron's price spikes to 100 — an 8× ROI offer. The latch holds.
    state.vein_hits.insert(pos(21, 20), 3);
    let turn = turn_from(world_at(171, 0, roles(), vec![], zones(), RICH_IRON, SHOP_PRICES));
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "collect");
    assert_eq!(
        first_target(cmd),
        Some(pos(21, 20)),
        "the latch must beat the richer iron offer"
    );

    // Round 3: the copper is mined out (10 hits) — the latch releases and
    // the richer iron wins the fresh pick.
    state.vein_hits.insert(pos(21, 20), 10);
    let turn = turn_from(world_at(172, 0, roles(), vec![], zones(), RICH_IRON, SHOP_PRICES));
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "collect");
    assert_eq!(first_target(cmd), Some(pos(21, 21)));
    assert_eq!(state.vein_latch, Some(pos(21, 21)));
}

// ---------------------------------------------------------------------------
// The sell triggers of comment 1 §6, in spec order T4 → T1 → T2 → T3 → T5.
// ---------------------------------------------------------------------------

#[test]
fn t4_full_pack_turns_to_selling() {
    // Zero free slots — nothing more can be dug, so the pack must be cashed.
    // Far from the vendor: walk. Standing on a vendor stand: sell now.
    let pack = || vec!["copper", "copper", "iron"];
    let far = turn_from(world_at(
        170,
        0,
        vec![
            station(10, 20),
            worker(A, 5, 5),
            worker_items(B, 20, 20, 3, pack()),
        ],
        vec![],
        vec![zone(6, 6, "stone"), zone(30, 26, "vendor")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    let plan = orchestrate::plan(&far, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "move");
    let t = first_target(cmd).unwrap();
    assert!(
        chebyshev(t, pos(30, 26)) < chebyshev(pos(20, 20), pos(30, 26)),
        "T4 must walk B toward the vendor, got {:?}",
        t
    );

    let at = turn_from(world_at(
        170,
        0,
        vec![
            station(10, 20),
            worker(A, 5, 5),
            worker_items(B, 29, 26, 3, pack()),
        ],
        vec![],
        vec![zone(6, 6, "stone"), zone(30, 26, "vendor")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    let plan = orchestrate::plan(&at, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "sell");
}

#[test]
fn t1_buy_deadline_cashes_ore_before_the_shop_closes() {
    // The pioneer's synced buy deadline has arrived: even a half-empty pack
    // and a distant vendor become a sell errand — the gold must exist before
    // the buyer stands at the counter.
    let turn = turn_from(world_at(
        170,
        0,
        vec![
            station(10, 20),
            worker(A, 5, 5),
            worker_items(B, 20, 20, 100, vec!["copper"]),
        ],
        vec![],
        vec![zone(6, 6, "stone"), zone(35, 28, "vendor")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    state.buy_deadline = Some(170); // round_no + vendor_travel >= deadline
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "move");
    let t = first_target(cmd).unwrap();
    assert!(
        chebyshev(t, pos(35, 28)) < chebyshev(pos(20, 20), pos(35, 28)),
        "T1 must head for the vendor, got {:?}",
        t
    );
}

#[test]
fn t2_hurt_teammate_without_medicine_gold_sells_for_it() {
    // A is at 60/220 HP (below 30%) and the purse cannot buy the Medicine
    // (gold 0 < 10) — B's copper becomes the medicine fund. T2 is one of
    // the two triggers the ring hunger may NOT defer, so this holds even on
    // a day-1 board; here the plain day-2 board keeps the unit minimal.
    let turn = turn_from(world_at(
        170,
        0,
        vec![
            station(10, 20),
            worker_hp(A, 30, 30, 60),
            worker_items(B, 20, 20, 100, vec!["copper"]),
        ],
        vec![],
        vec![zone(35, 28, "vendor")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "move");
    let t = first_target(cmd).unwrap();
    assert!(
        chebyshev(t, pos(35, 28)) < chebyshev(pos(20, 20), pos(35, 28)),
        "T2 must head for the vendor, got {:?}",
        t
    );
}

#[test]
fn t3_passing_the_vendor_cashes_sellable_ore() {
    // Day 2, the vendor is one step off B's cell — well inside the 3-round
    // window: the detour pays for itself, so the ore becomes gold now.
    let turn = turn_from(world_at(
        170,
        0,
        vec![
            station(10, 20),
            worker(A, 5, 5),
            worker_items(B, 20, 20, 100, vec!["iron"]),
        ],
        vec![],
        vec![zone(6, 6, "stone"), zone(22, 21, "vendor")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "move");
    let t = first_target(cmd).unwrap();
    assert!(
        chebyshev(t, pos(22, 21)) <= 1,
        "T3 must step onto a vendor stand, got {:?}",
        t
    );
}

#[test]
fn t5_treasure_floor_sells_when_the_purse_is_under_the_plan_cost() {
    // A live treasure plan wants a Bomb (shop price 100, reserve-capped to
    // 45) and the purse holds 20 — under TREASURE_GOLD_FLOOR. No buy
    // deadline, no hurt teammate, vendor far beyond the 3-round window: T5
    // is the only trigger that can own this round. It is also the trigger
    // the wallet-gated `gold_reserve` could never fire — a purse below the
    // floor is exactly the case that gate zeroes — hence `plan_cost`.
    let turn = turn_from(world_at(
        170,
        20,
        vec![
            station(10, 20),
            worker(A, 5, 5),
            worker_items(B, 20, 20, 100, vec!["copper"]),
            pioneer(P, 11, 21),
        ],
        vec![],
        vec![zone(6, 6, "stone"), zone(35, 28, "vendor")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    state.treasure = TreasureState {
        phase: TreasurePhase::HavePlan,
        plan: Some(TreasurePlan {
            pos: pos(20, 5),
            items: vec!["Bomb".into()],
            open_day: 2,
        }),
        ..Default::default()
    };
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "move");
    let t = first_target(cmd).unwrap();
    assert!(
        chebyshev(t, pos(35, 28)) < chebyshev(pos(20, 20), pos(35, 28)),
        "T5 must head for the vendor, got {:?}",
        t
    );
}

// ---------------------------------------------------------------------------
// Day 1: the ring outranks the money loop (issues #12/#13/#14 — "the earners
// left").
// ---------------------------------------------------------------------------

#[test]
fn day1_ring_hunger_defers_the_vendor_errand_to_the_stone_run() {
    // Day 1 round 40: two ring cells still open, 15 rounds to dusk, and A —
    // even with two stones in its pack — is 21 cells away: it cannot close
    // the row in time, so the ring is hungry. B stands one cell from a
    // vendor (T3 would fire) with sellable copper, but a stone vein that is
    // still deliverable before dusk sits six cells west. The ring comes
    // first: B latches the stone and walks west, NOT to the vendor.
    let mut roles = vec![
        station(10, 20),
        worker_items(A, 30, 30, 100, vec!["stone", "stone"]),
        worker_items(B, 20, 20, 100, vec!["copper"]),
    ];
    roles.extend(ring_walls(&[pos(8, 22), pos(9, 22)]));
    let turn = turn_from(world_at(
        40,
        0,
        roles,
        vec![],
        vec![zone(14, 21, "stone"), zone(21, 20, "vendor")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    state.d1_weapon_helped = true; // the help already fired this morning
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "move");
    let t = first_target(cmd).unwrap();
    assert!(
        chebyshev(t, pos(14, 21)) < 6,
        "B must walk toward the stone vein, got {:?}",
        t
    );
    assert!(t.x < 20, "and not east toward the vendor, got {:?}", t);
    assert_eq!(state.vein_latch, Some(pos(14, 21)));
}

#[test]
fn day1_weapon_help_outranks_the_money_loop_and_fires_once() {
    // Day 1 round 10, 75 gold, no towers: the one-time weapon help must own
    // B even with copper adjacent at (10,17) — the money loop would collect
    // it. B stands at (9,17), one cell from the L's first two sites (9,18)
    // and (10,18); A is across the map and claims at most one site, so B
    // keeps an adjacent one to build on.
    let turn = turn_from(world_at(
        10,
        75,
        vec![station(10, 20), worker(A, 40, 31), worker(B, 9, 17)],
        vec![],
        vec![zone(10, 17, "copper")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_ne!(cmd.action, "collect", "day-1 help must outrank the money loop");
    assert!(cmd.action == "build" || cmd.action == "move");
    if cmd.action == "build" {
        assert!(state.d1_weapon_helped, "the help flag latches on the build round");
        let t = first_target(cmd).unwrap();
        assert!(
            matches!((t.x, t.y), (9, 18) | (10, 18)),
            "B builds one of its adjacent L sites, got {:?}",
            t
        );
        assert!(
            matches!(cmd.name.as_deref(), Some("rocket" | "railgun" | "gatling")),
            "the help builds a weapon, got {:?}",
            cmd.name
        );
    }

    // Round 2: the help has fired (flag set) — the money loop takes over and
    // collects the copper sitting next to B.
    let turn = turn_from(world_at(
        11,
        50,
        vec![
            station(10, 20),
            worker(A, 40, 31),
            worker(B, 10, 22),
            gatling(30000, 9, 18),
        ],
        vec![],
        vec![zone(10, 23, "copper")],
        VENDOR_PRICES,
        SHOP_PRICES,
    ));
    let mut state = BotState::default();
    state.d1_weapon_helped = true;
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "collect");
    assert_eq!(first_target(cmd), Some(pos(10, 23)));
}

#[test]
fn batched_stone_builds_the_gap_when_the_trap_check_passes() {
    // Day 1 round 50: one open cell (8,22), five rounds to dusk. A is home
    // inside but cannot close the cell in time (3 walk + 4 build > 5 left) —
    // the ring is hungry even with the pool covered. B already stands on the
    // build position with the stone in its pack, and the trap check passes
    // (A is home, nothing else owes a gap): the batch delivers as a build.
    let mut roles = vec![
        station(10, 20),
        worker(A, 11, 21),
        worker_items(B, 9, 23, 100, vec!["stone"]),
    ];
    roles.extend(ring_walls(&[pos(8, 22)]));
    let turn = turn_from(world_at(50, 0, roles, vec![], vec![], VENDOR_PRICES, SHOP_PRICES));
    let mut state = BotState::default();
    state.d1_weapon_helped = true;
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "build");
    assert_eq!(first_target(cmd), Some(pos(8, 22)));
    assert_eq!(cmd.name.as_deref(), Some("wall"));
}

#[test]
fn camp_holds_b_at_a_vetoed_gap_instead_of_releasing_the_money_loop() {
    // Day 1 round 60: the ring is complete except (8,22) — this synthetic
    // board walled even the permanent entrance, so the last open cell really
    // is the last way in. A is outside to the north and (8,22) is its ONLY
    // route home — `wall_would_trap` vetoes the build until A gets in or the
    // HARD_SEAL override lands at round 66. The old code released B to the
    // money loop here and it paced between the vetoed gap and a distant
    // vein for the rest of the day (the spare-board oscillation). Now B
    // camps: walk to the gap's mouth, then hold — no command at all, even
    // with a copper vein in plain view.
    let walls = ring_walls(&[pos(8, 22)]);
    let seed = |state: &mut BotState| {
        state.d1_weapon_helped = true;
    };
    // Far south-east: the other end of the old oscillation.
    let zones = || vec![zone(35, 28, "copper")];

    // Round 1: B is three cells from the vetoed gap — the camp walks it in.
    let mut roles = vec![
        station(10, 20),
        worker_items(A, 20, 8, 100, vec!["stone"]),
        worker_items(B, 11, 25, 100, vec!["stone"]),
    ];
    roles.extend(walls.clone());
    let turn = turn_from(world_at(60, 0, roles, vec![], zones(), VENDOR_PRICES, SHOP_PRICES));
    let mut state = BotState::default();
    seed(&mut state);
    let plan = orchestrate::plan(&turn, &mut state);
    let cmd = cmd_of(&plan, B);
    assert_eq!(cmd.action, "move");
    let t = first_target(cmd).unwrap();
    assert!(
        chebyshev(t, pos(8, 22)) < 3,
        "the camp must close on the vetoed gap, got {:?}",
        t
    );

    // Round 2: B at the gap's mouth — the hold. Its absence from the plan
    // is the proof: had the money loop taken the round, B would be walking
    // south-east toward the copper instead.
    let mut roles = vec![
        station(10, 20),
        worker_items(A, 20, 8, 100, vec!["stone"]),
        worker_items(B, 9, 23, 100, vec!["stone"]),
    ];
    roles.extend(walls);
    let turn = turn_from(world_at(61, 0, roles, vec![], zones(), VENDOR_PRICES, SHOP_PRICES));
    let mut state = BotState::default();
    seed(&mut state);
    let plan = orchestrate::plan(&turn, &mut state);
    assert!(
        !plan.commands.contains_key(&B),
        "at the mouth B camps: no command, and the money loop does not take the round"
    );
}
