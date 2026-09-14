//! Pocket escape at night, and the reload window (issues #126-#130).
//!
//! Two defects with the same signature in the batch's tables — a tower silent
//! and an operator that never moved.
//!
//! **1. `controller_stuck` is a pocket, not slowness.** v17's `stuck` triple
//! (`@x,y/N/M` = the cell it is frozen on, how many operating cells it was
//! walking to, how many of our own walls it could cut) was added precisely so
//! this could be read, and the batch answers it:
//!
//! ```text
//! 126  20040  controller_stuck@27,13/5/1   x11   (the role wall_gate_open named, 11/11 dusk rounds)
//! 129  20020  controller_stuck@32,10/3/3   x14   (three adjacent walls, none of them enough)
//! 128  20040  controller_stuck@34,5/5/0    x3    (M = 0: nothing adjacent to cut)
//! 130  20020  controller_stuck@30,11/3/0   x5    (M = 0 as well)
//! ```
//!
//! The same cell for eleven and fourteen consecutive rounds is not a walk, and
//! `M > 0` with the role still frozen is the wall LINE, not a missing
//! demolition: `walk_or_remove_wall` tears a wall down only when that one cut
//! makes the post reachable *this round*, so in a pocket two walls deep — the
//! shapes above, with one or three walls but no single cut that opens the route
//! — it issues **no command at all**. `brain::break_out` cuts toward the post
//! instead, and steps into the gap it opened on the round after.
//!
//! **2. The reload is mason time.** `cooldown` is the largest idle bucket in
//! the batch after `fired`: 51 tower-rounds for 126's 20040, 36 for 129's 20040,
//! 33 for 127's 10040 — a healthy operator standing next to a loaded
//! `WallFixer` doing nothing, on the nights `ourWallLost` ran 2000-14335 and
//! the station behind the wall fell. `night::plan` now mends during it.

use serde_json::{json, Value};

use coregeek::brain::{break_out, walk_or_remove_wall};
use coregeek::model::{chebyshev, Turn};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

const STATION: (i32, i32) = (10, 20);
/// Where the operator is trying to get to — a gun's operating cell, far to the
/// east so the only useful direction is +x.
const POST: Pos = Pos { x: 20, y: 5 };
const START: Pos = Pos { x: 5, y: 5 };

fn station() -> Value {
    json!({
        "id": 10001, "pos": {"x": STATION.0, "y": STATION.1}, "roleType": "station",
        "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn operator(id: i64, pos: Pos) -> Value {
    json!({
        "id": id, "pos": {"x": pos.x, "y": pos.y}, "roleType": "worker",
        "health": 220, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 100, "backpack": []
    })
}

fn wall(id: i64, pos: Pos, health: i64) -> Value {
    json!({
        "id": id, "pos": {"x": pos.x, "y": pos.y}, "roleType": "wall",
        "health": health, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn robot(id: i64, pos: Pos) -> Value {
    json!({
        "id": id, "pos": {"x": pos.x, "y": pos.y}, "roleType": "smallRobot",
        "health": 40, "abnormalState": "", "targetTeam": "challenger"
    })
}

fn board(round: i64, roles: Vec<Value>, robots: Vec<Value>) -> Value {
    json!({
        "roundNo": round,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {"type": "challenger", "goldNum": 0, "totalScore": 0,
                    "playerTasks": [], "roles": roles},
        "teamEnemy": {"roles": []},
        "robot": {"roles": robots},
    })
}

/// A night board (round 71+) with the operator at `pos` and one wall per cell.
/// `mapInfo.zones` is empty: the whole board is walkable land, so the only
/// thing standing between the operator and its post is our own wall line.
fn world(round: i64, pos: Pos, walls: &[Pos], robots: Vec<Value>) -> Value {
    let mut roles = vec![station()];
    for (index, cell) in walls.iter().enumerate() {
        roles.push(wall(40000 + index as i64, *cell, 1000));
    }
    roles.push(operator(10010, pos));
    board(round, roles, robots)
}

/// Every cell of the full-height column at `x`. Movement is 8-directional and
/// the board is 32 tall, so a column that spans it cannot be walked around —
/// which is what makes the operator's post genuinely unreachable.
fn column(x: i32) -> Vec<Pos> {
    (0..32).map(|y| Pos { x, y }).collect()
}

/// The cut or step a command describes, for assertions that read the same way
/// whether the escape decided to demolish or to walk.
fn target(cmd: &coregeek::protocol::RoleCommand) -> Pos {
    cmd.targetPos.as_ref().expect("every escape command is aimed")[0]
}

#[test]
fn a_pocket_two_walls_deep_is_cut_toward_the_post() {
    // Two full columns between the operator and its gun. `walk_or_remove_wall`
    // can cut one wall, and cutting one wall opens nothing — the second column
    // is still there. So it refuses, and this is the 126/129 freeze.
    let mut walls = column(6);
    walls.extend(column(7));
    let turn = turn_from(world(71, START, &walls, vec![]));
    let role = turn.role_by_id(10010).unwrap();
    let stands = vec![POST];

    assert!(
        walk_or_remove_wall(&turn, role, &stands, &mut Default::default()).is_none(),
        "no single cut reopens the route, which is exactly the dead end"
    );

    let cmd = break_out(&turn, role, &stands, &mut Default::default())
        .expect("the escape of last resort issues a command");
    let cut = target(&cmd);
    assert_eq!(cmd.action, "remove", "the pocket is opened by cutting, not walking");
    assert_eq!(cut.x, 6, "the column on the post's side of the pocket: {cut:?}");
    assert!(
        chebyshev(cut, POST) < chebyshev(START, POST),
        "{cut:?} must be progress toward the post"
    );
}

#[test]
fn the_operator_steps_into_the_gap_it_just_opened() {
    // Round two of the same escape: the first column is gone, the second still
    // stands, and the operator is one cell short of its own hole. Cutting alone
    // is a one-round cure — with no route and no adjacent wall left it froze
    // again, one cell from the gap. It has to walk into it.
    let turn = turn_from(world(72, START, &column(7), vec![]));
    let role = turn.role_by_id(10010).unwrap();
    let stands = vec![POST];

    assert!(
        walk_or_remove_wall(&turn, role, &stands, &mut Default::default()).is_none(),
        "the second column still blocks, and there is no adjacent wall to cut"
    );
    let cmd = break_out(&turn, role, &stands, &mut Default::default())
        .expect("a step toward the post is progress");
    let step = target(&cmd);
    assert_eq!(cmd.action, "move", "there is nothing left to cut: the round is a step");
    assert_eq!(step.x, 6, "into the hole it just opened: {step:?}");
    assert!(
        chebyshev(step, POST) < chebyshev(START, POST),
        "{step:?} must be progress toward the post"
    );
}

#[test]
fn the_operator_tunnels_all_the_way_to_its_post() {
    // The whole escape, round by round, on a board three walls deep. Every
    // command `break_out` issues is applied and the operator must arrive. A cut
    // that is not followed by a step, or a step that does not reduce the
    // distance, shows up here as a role that never reaches its gun — which is
    // the failure the batch actually logged.
    let mut walls: Vec<Pos> = column(6);
    walls.extend(column(7));
    walls.extend(column(8));
    let mut pos = START;
    let stands = vec![POST];
    // The state signature, not the position: a round spent cutting does not move
    // the operator, and that is not the oscillation this guards against — a
    // repeated (cell, wall count) is.
    let mut seen: Vec<(Pos, usize)> = Vec::new();

    for round in 71..140 {
        if stands.contains(&pos) {
            break;
        }
        let signature = (pos, walls.len());
        assert!(
            !seen.contains(&signature),
            "round {round}: {signature:?} came back — the escape must be monotone"
        );
        seen.push(signature);

        let turn = turn_from(world(round, pos, &walls, vec![]));
        let role = turn.role_by_id(10010).unwrap();
        let cmd = break_out(&turn, role, &stands, &mut Default::default()).unwrap_or_else(|| {
            panic!("round {round}: nothing to do at {pos:?}, the freeze the batch logged")
        });
        match cmd.action.as_str() {
            "move" => {
                let next = target(&cmd);
                assert!(
                    chebyshev(next, POST) < chebyshev(pos, POST),
                    "round {round}: {pos:?} -> {next:?} is not progress"
                );
                pos = next;
            }
            "remove" => {
                let cell = target(&cmd);
                assert!(
                    chebyshev(cell, pos) == 1,
                    "round {round}: {cell:?} is not adjacent to {pos:?}"
                );
                let before = walls.len();
                walls.retain(|wall| *wall != cell);
                assert_eq!(before - 1, walls.len(), "round {round}: {cell:?} was not ours");
            }
            other => panic!("round {round}: unexpected action {other}"),
        }
    }

    assert_eq!(pos, POST, "the operator reaches the gun it mans");
}

#[test]
fn a_pocket_with_nothing_useful_to_cut_still_reports_a_stuck_controller() {
    // The narrowness that keeps the fix honest, and the shape 128's
    // `@34,5/5/0` and 130's `@30,11/3/0` actually logged: `M = 0`, nothing
    // adjacent to cut at all, and the way out held by the robots themselves.
    // There is no
    // move to make, and inventing one — chewing a wall that leads away from the
    // post, or pacing the cells it can already stand on — would spend the
    // night's rounds and still leave the gun unmanned. `break_out` returns
    // None, the caller reports `controller_stuck`, and the log keeps saying the
    // truth.
    let far_side = vec![Pos { x: 4, y: 5 }]; // west of the operator: away from the post
    let robots = vec![
        robot(50001, Pos { x: 6, y: 4 }),
        robot(50002, Pos { x: 6, y: 5 }),
        robot(50003, Pos { x: 6, y: 6 }),
    ];
    let turn = turn_from(world(71, START, &far_side, robots));
    let role = turn.role_by_id(10010).unwrap();
    assert!(
        break_out(&turn, role, &[POST], &mut Default::default()).is_none(),
        "a wall that leads away from the post is not worth a round, \
         and the robots hold every cell that would be"
    );

    // And a role already on its post has nothing to break out of.
    let sealed = turn_from(world(71, POST, &[], vec![]));
    let role = sealed.role_by_id(10010).unwrap();
    assert!(
        break_out(&sealed, role, &[POST], &mut Default::default()).is_none(),
        "already there: no command is the correct command"
    );
}

// ------------------------------------------------------- the reload window

/// Station at (10,20), so the footprint is (10,19)-(11,20) and the interior
/// band is the twelve cells hugging it. A railgun sits at (9,19) — an interior
/// cell — and its operator mans it from (10,18), the neighbouring interior
/// cell. The wall the operator can reach is (9,17): one cell away, outside the
/// interior band (so it is the ring's business, not the station's), and not a
/// cell the gun can be operated from.
const GUN: Pos = Pos { x: 9, y: 19 };
const OPERATOR: Pos = Pos { x: 10, y: 18 };
const WALL_CELL: Pos = Pos { x: 9, y: 17 };
/// A robot inside the gun's range, and not on the operator.
const FAR_ROBOT: Pos = Pos { x: 12, y: 19 };
/// A robot on top of the operator.
const NEAR_ROBOT: Pos = Pos { x: 9, y: 18 };

fn reload_world(round: i64, cooldown: i64, pack: Vec<&str>, wall_health: i64, robots: Vec<Value>) -> Value {
    let gun = json!({
        "id": 30000, "pos": {"x": GUN.x, "y": GUN.y}, "roleType": "gatling",
        "health": 1000, "attackPower": 10, "attackRange": 5, "level": 1,
        "cooldown": cooldown, "backPackCapability": 0, "backpack": []
    });
    let mut crew = operator(10010, OPERATOR);
    crew["backpack"] = json!(pack);
    board(
        round,
        vec![
            station(),
            gun,
            wall(40001, WALL_CELL, wall_health),
            crew,
        ],
        robots,
    )
}

#[test]
fn a_reloading_gun_mends_the_wall_beside_it() {
    let mut state = BotState::default();
    let turn = turn_from(reload_world(71, 2, vec!["WallFixer"], 400, vec![]));
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let cmd = plan
        .commands
        .get(&10010)
        .expect("the operator spends the reload on the wall");
    assert_eq!(cmd.action, "use");
    assert_eq!(cmd.name.as_deref(), Some("WallFixer"));
    assert_eq!(
        cmd.targetPos.as_ref().map(|list| list[0]),
        Some(WALL_CELL),
        "and it is the chipped wall it is standing beside"
    );
}

#[test]
fn a_ready_gun_is_never_traded_for_a_mend() {
    // The whole gate. A gun that CAN fire this round fires — the wall is not
    // worth a volley, and the batch's `fired` rounds are the score.
    let mut state = BotState::default();
    let turn = turn_from(reload_world(
        71,
        0,
        vec!["WallFixer"],
        400,
        vec![robot(50001, FAR_ROBOT)],
    ));
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    // An attack is keyed by the TOWER (`RoleCommand::attack(controller, …)` is
    // pushed under the tower's id); a mend is keyed by the operator.
    let cmd = plan
        .commands
        .get(&30000)
        .expect("a ready gun shoots a robot in range");
    assert_eq!(cmd.action, "attack", "the shot outranks the mason's round");
    assert_eq!(
        cmd.controllerId.as_deref(),
        Some("10010"),
        "and it is a crewed gun: the operator is the one acting"
    );
}

#[test]
fn a_robot_on_the_operator_is_not_a_mason_s_round() {
    // A robot within one cell means the operator is being meleed: the heal and
    // withdraw rules own that round, not the wall. The gun is reloading, so
    // nothing else is competing for it — the robot is the only reason not to
    // mend.
    let mut state = BotState::default();
    let turn = turn_from(reload_world(
        71,
        3,
        vec!["WallFixer"],
        400,
        vec![robot(50001, NEAR_ROBOT)],
    ));
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    let mend = plan
        .commands
        .get(&10010)
        .map(|cmd| cmd.name.as_deref() == Some("WallFixer"))
        .unwrap_or(false);
    assert!(!mend, "no reaching out while a robot is on the operator");
}

#[test]
fn a_whole_wall_and_an_empty_backpack_are_both_no_ops() {
    // Nothing to mend: the wall is at full HP.
    let mut state = BotState::default();
    let turn = turn_from(reload_world(71, 2, vec!["WallFixer"], 1000, vec![]));
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    assert!(
        plan.commands.get(&10010).is_none(),
        "a full-HP wall is not an errand"
    );

    // Nothing to mend WITH: no WallFixer in the backpack.
    let mut state = BotState::default();
    let turn = turn_from(reload_world(71, 2, vec![], 400, vec![]));
    let plan = coregeek::brain::night::plan(&turn, &mut state);
    assert!(
        plan.commands.get(&10010).is_none(),
        "the item is the whole errand"
    );
}
