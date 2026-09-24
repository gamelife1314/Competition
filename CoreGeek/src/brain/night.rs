//! Night helpers shared by the role mainlines (issue #221 phase 4b-3).
//!
//! The legacy night dispatch — the controller↔tower pairing and its firing
//! loop — is gone: design D14-D16's L-shape model has Worker A run all three
//! weapons from the single operator cell
//! ([`crate::brain::role::wall_worker::plan_night`]), B never comes home
//! ([`crate::brain::role::economy_worker`]), and the pioneer takes the
//! repair-duty chain below. What survives here: the spare chain (comment 1
//! §3.3 `night_repair`, Q5's default), the night medicine and withdrawal
//! thresholds, the reload-window masonry and the shelter walk. Phase 5 folds
//! these into `role::pioneer` and deletes this file.

use std::collections::HashSet;

use super::action::fight::THREAT_RADIUS;
use super::action::shop::voucher_flow;
use crate::brain::{break_out, combat, economy, task, walk_or_remove_wall, walk_toward, Plan};
use crate::model::{chebyshev, Turn, Unit, UnitKind};
use crate::protocol::{Pos, RoleCommand};
use crate::state::BotState;

// The fight executors moved to `action::fight` (issue #221 phase 2a). Re-export
// them under their old names so the test crates compile untouched.
pub use super::action::fight::{
    defense_needs_pioneer, operator_shortage, pairing, pairing_controllers, stable_pairs,
    unpaired_of, unpaired_towers, withdrawing, WITHDRAW_HEALTH_TENTHS,
};

/// The spare's night chain — since phase 4b-3 the PIONEER's night (and the
/// fallback for any controllable the roster does not name).
///
/// Comment 1 §3.3 (`night_repair`) and Q5: a WallFixer in the pack is a
/// standing shift inside the wall — the repair duty owns the role, and the
/// task point (outside the ring) may not pull it away. With the kit spent
/// (or never held) an active task keeps working through the night as before,
/// and a role with neither falls through the duties: heal, shelter, the
/// base vouchers, the wall, the items.
pub(crate) fn plan_spare(
    turn: &Turn,
    state: &mut BotState,
    role: &Unit,
    claimed: &mut HashSet<Pos>,
    plan: &mut Plan,
) {
    // Q5: 有围墙修复包时站在墙内准备维修围墙 — the repair kit outranks the
    // task; 没有或者使用完…撤离到安全区域 is what the shelter below does.
    let repair_duty = role.count_item("WallFixer") > 0;
    if role.kind == UnitKind::Pioneer && state.task.active && !repair_duty {
        if let Some(cmd) = task::plan_pioneer(turn, state, role, plan) {
            plan.push(role.id, cmd);
        }
        return;
    }
    // Self-heal — survival outranks every other spare duty.
    if let Some(cmd) = night_medicine(turn, role) {
        plan.push(role.id, cmd);
        return;
    }
    // Retreat inside the wall ring next to the station BEFORE anything that
    // keeps the role out in the open — a lone spare next to a mine or wall is
    // easy to focus down. Once inside we fall through to the repair duties.
    if shelter(turn, role, claimed, plan) {
        return;
    }
    // `night_repair`, first half (comment 1 §3.3: 夜间回基地使用升级券): at
    // the base, the upgrade vouchers go on before the masonry — an upgrade is
    // permanent value, and the mend below still owns every round the wall is
    // actually being chewed.
    if let Some(cmd) = voucher_flow(turn, role, claimed) {
        plan.push(role.id, cmd);
        return;
    }
    // THE SPARE'S MASON ROUND. The operator only gets a mend on a reload
    // round (`cooldown_repair`), and issues #131-#135 measured the ring
    // losing 2465-15135 HP a night with the base behind it falling on night 1-3.
    // A spare standing on the inner band — which is where `shelter` just put it,
    // and the band is adjacent to the ring — is the second pair of hands the
    // night never used: `repair_target(.., 3)` below is false for every wall
    // under attack (see `combat::night_mend_target`). Deliberately placed AFTER
    // the shelter move, so the walk home still owns the round, and BEFORE the
    // items, because a wall restored to full is worth more than a Bomb that
    // may not land.
    if let Some(wall_pos) = combat::night_mend_target(turn, role) {
        crate::log::event(
            "wall_mend",
            serde_json::json!({
                "round": turn.round_no,
                "role": role.id,
                "target": [wall_pos.x, wall_pos.y],
                "duty": "spare",
            }),
        );
        plan.push(role.id, RoleCommand::use_item_at("WallFixer", wall_pos));
        return;
    }
    // Burn summon orders (harassment works at night too).
    const ORDERS: [&str; 4] = [
        "BossRobotSummonOrder",
        "LargeRobotSummonOrder",
        "MiddleRobotSummonOrder",
        "SmallRobotSummonOrder",
    ];
    for order in ORDERS {
        if role.count_item(order) > 0 && state.summon_orders_today < 10 {
            state.consume_summon_order();
            state.harass_done_today = true;
            // P2-3 压制窗口: the boss order is first in ORDERS, so a spare that
            // carries one already fires it ahead of the smaller waves. What
            // this records is WHY it was worth buying today — a wave aimed at
            // a base that is nearly down or at towers inside one blast radius
            // is the shot the window opened for, and the round it lands is the
            // only place that can be confirmed from the log.
            if order == "BossRobotSummonOrder" && economy::boss_suppression_window(turn) {
                crate::log::event(
                    "boss_suppression",
                    serde_json::json!({
                        "round": turn.round_no,
                        "role": role.id,
                        "enemyStationHp": turn.enemy_station().map(|station| station.health),
                        "enemyTowers": turn
                            .enemy
                            .iter()
                            .filter(|unit| unit.kind.is_tower() && unit.alive())
                            .count(),
                    }),
                );
            }
            plan.push(role.id, RoleCommand::use_item(order));
            return;
        }
    }
    // Bomb: worth it against clusters (>= 2 robots) or big targets.
    if role.count_item("Bomb") > 0 {
        if let Some(impact) = combat::bomb_impact(turn) {
            let clustered = turn
                .robots
                .iter()
                .filter(|robot| robot.health > 0 && chebyshev(impact, robot.pos) <= 1)
                .count();
            let big = turn.robots.iter().any(|robot| {
                robot.health > 0
                    && chebyshev(impact, robot.pos) <= 1
                    && combat::is_big_threat(robot.kind)
            });
            if clustered >= 2 || big {
                plan.push(role.id, RoleCommand::use_item_at("Bomb", impact));
                return;
            }
        }
    }
    // Dizzy: stall a wave pressing our base.
    if role.count_item("DizzyWeapon") > 0 {
        if let Some(impact) = combat::dizzy_impact(turn) {
            plan.push(role.id, RoleCommand::use_item_at("DizzyWeapon", impact));
            return;
        }
    }
    // (Shelter already ran above; reaching here means the role is inside the
    // ring and has nothing else to throw at the robots.)
}

/// Heal at night, while the role is still worth saving.
///
/// `day::use_medicine` waits for 30% health, which is tuned for the day: a role
/// that takes a hit at noon has hours to walk it off, and a potion spent early
/// is 10 gold never coming back. At night there is no walking it off — a
/// focused controller goes from 30% to dead inside the round it is shot in, and
/// the tower it was manning goes silent with it. Issue #18 lost 20011 in six
/// rounds (HP 200→0) and 20010 in TWO (HP 220→0); issue #19 lost all three on
/// D2 night and the base fell from 1500 to 105 HP behind them. Medicine
/// restores FULL health, so a potion spent at 60% buys a gun that fires all
/// night and a survival score that keeps paying — the potion saved buys
/// nothing.
///
/// Below the threat radius (no robot close enough to finish the job this
/// round) the day threshold still applies: a scratch at 3 a.m. can wait.
pub(crate) fn night_medicine(turn: &Turn, role: &Unit) -> Option<RoleCommand> {
    if role.count_item("Medicine") < 1 {
        return None;
    }
    let max_hp = match role.kind {
        UnitKind::Worker => 220,
        UnitKind::Pioneer => 200,
        _ => return None,
    };
    let threatened = turn
        .robots
        .iter()
        .any(|robot| robot.health > 0 && chebyshev(robot.pos, role.pos) <= THREAT_RADIUS);
    let threshold = if threatened { 7 } else { 3 };
    if role.health * 10 < max_hp * threshold {
        return Some(RoleCommand::use_item("Medicine"));
    }
    None
}

/// Mend the weakest wall this operator is standing beside, if the gun it mans
/// cannot fire this round anyway.
///
/// See the call sites for why a round the gun cannot fire is a round the wall
/// gets. There are two such windows and both call this: the reload
/// (`tower.cooldown != 0`), and a ready gun whose trigger came up empty because
/// every robot hunting us is outside every tower's reach
/// (`no_target_reserved_for_robots` — 表 6a's second-largest silence bucket in
/// issues #131-#135, 18-42 rounds a match). Neither trades a shot for a mend.
///
/// The predicates live in [`combat::night_mend_target`]:
///
///   * it carries a `WallFixer` — the item is the whole errand, and a role
///     without one has nothing to mend with;
///   * no live robot is ADJACENT to the operator (chebyshev <= 1). Robots shoot
///     from three cells (任务书 4.7.2), so a wall being chewed from two cells
///     out is still repairable; a robot on the operator's own cell means it is
///     being meleed and the heal/withdraw rules own that round;
///   * an own wall of ours is ADJACENT and below its level's maximum, taken
///     weakest first — the same ranking the day's `repair_target` uses, and
///     the wall the night is closest to losing.
///
/// Bounded to one mend per round, which is what one action per role allows.
pub(crate) fn cooldown_repair(turn: &Turn, role: &Unit) -> Option<RoleCommand> {
    let target = combat::night_mend_target(turn, role)?;
    crate::log::event(
        "wall_mend",
        serde_json::json!({
            "round": turn.round_no,
            "role": role.id,
            "target": [target.x, target.y],
            "duty": "cooldown",
        }),
    );
    Some(RoleCommand::use_item_at("WallFixer", target))
}

/// Move a spare role inside the wall ring, right next to the station. Returns
/// true when a movement command was issued (the caller should stop planning
/// this round). Uses only the cells at footprint distance <= 1 — hugging the
/// station — so the role never stops on the wall line or out near the mines.
pub(crate) fn shelter(turn: &Turn, role: &Unit, claimed: &mut HashSet<Pos>, plan: &mut Plan) -> bool {
    // The same set the day planner retreats to and the wall gate measures
    // "everyone is inside" against; sharing it keeps the three in step.
    let mut stands: Vec<Pos> = crate::brain::interior_cells(turn);
    if stands.is_empty() {
        return false;
    }
    if stands.iter().any(|stand| *stand == role.pos) {
        return false; // already hugging the station
    }
    // Prefer the corner furthest from the nearest robot.
    stands.sort_by_cached_key(|stand| {
        let nearest = turn
            .robots
            .iter()
            .filter(|robot| robot.health > 0)
            .map(|robot| chebyshev(*stand, robot.pos))
            .min()
            .unwrap_or(i32::MAX);
        std::cmp::Reverse(nearest)
    });
    if let Some(cmd) = walk_toward(turn, role, &stands, claimed) {
        plan.push(role.id, cmd);
        return true;
    }
    // A SPARE ROLE THE RING CLOSED ON HAS NO WAY BACK IN (issues #176-#185).
    //
    // This walk was plain `walk_toward`, and it is the last resort of a role
    // that holds no tower. The paired controller had a three-rung ladder for
    // exactly this (claims, then no claims, then `break_out`), but a spare has
    // nothing — `night_goal` returns None for it, so it never reached that
    // block. `gate_open_record` is just as strict about it: a spare counts as
    // home only by standing within one cell of the station footprint, with no
    // `interior_cells` substitute.
    //
    // 178 day 2 is the case: `wall_gate_forced` seals the ring with 20011 on
    // (24,16) and 20012 on (28,6), both outside, and that match carries the
    // batch's worst `tower_unpaired` count (78) — two of three guns lost on the
    // night the base started dying. Being behind the ring is the whole of a
    // spare's night duty (任务书: the three valid night duties are operate,
    // heal, retreat), so a spare that cannot walk in must cut its way in, the
    // same as the operator next to it.
    let mut ignored = HashSet::new();
    if let Some(cmd) = walk_or_remove_wall(turn, role, &stands, &mut ignored) {
        plan.push(role.id, cmd);
        return true;
    }
    if let Some(cmd) = break_out(turn, role, &stands, &mut ignored) {
        plan.push(role.id, cmd);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32, y: i32) -> Pos {
        Pos { x, y }
    }

    fn unit(id: i64, kind: &str, at: Pos, pack: Vec<&str>) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": kind,
            "health": 1000, "level": 1, "backPackCapability": 100, "backpack": pack
        })
    }

    fn wall_at(id: i64, at: Pos, hp: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "pos": {"x": at.x, "y": at.y}, "roleType": "wall",
            "health": hp, "level": 1, "backPackCapability": 0, "backpack": []
        })
    }

    /// A night board (roundNo 71+ = day 1's night) with the station at
    /// (10,24); its footprint is {(10,24),(11,24),(10,23),(11,23)}, so the
    /// interior band includes (9,22) — which is adjacent to the ring's
    /// bottom-row wall cells (9,21) and (10,21).
    fn night_board(round_no: i64, roles: Vec<serde_json::Value>) -> Turn {
        let mut all = vec![unit(10001, "station", pos(10, 24), vec![])];
        all.extend(roles);
        let payload = serde_json::json!({
            "roundNo": round_no,
            "mapInfo": {"width": 41, "height": 32, "zones": []},
            "teamOur": {
                "type": "challenger", "goldNum": 0, "totalScore": 0,
                "playerTasks": [], "roles": all
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
        });
        let req: crate::protocol::Request =
            serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    fn spare_plan(turn: &Turn, state: &mut BotState, id: i64) -> Plan {
        let mut claimed = HashSet::new();
        let mut plan = Plan::default();
        let role = turn.role_by_id(id).expect("the spare exists");
        plan_spare(turn, state, role, &mut claimed, &mut plan);
        plan
    }

    fn target(cmd: &RoleCommand) -> Pos {
        cmd.targetPos.as_ref().expect("a target")[0]
    }

    /// Q5: 有围墙修复包时站在墙内准备维修围墙 — a WallFixer in the pack is a
    /// standing shift inside the wall, and it outranks the task: the pioneer
    /// walks INTO the ring, never out to a task point, while it carries one.
    #[test]
    fn the_repair_kit_outranks_the_task() {
        let turn = night_board(
            75,
            vec![unit(20001, "pioneer", pos(20, 10), vec!["WallFixer"])],
        );
        assert!(!turn.is_day);
        let mut state = BotState::default();
        state.task.active = true;
        let plan = spare_plan(&turn, &mut state, 20001);
        let cmd = plan.commands.get(&20001).expect("the repair duty walks");
        assert_eq!(cmd.action, "move");
        let station = pos(10, 24);
        assert!(
            chebyshev(target(cmd), station) < chebyshev(pos(20, 10), station),
            "the step closes on the base, not on the task point"
        );
    }

    /// Q5's other half and comment 1 §3.3 `night_repair`: inside the ring, a
    /// mender with a damaged wall beside it spends the round on the WEAKEST
    /// adjacent wall — the shelter walk is already done and no voucher, item
    /// or summon steals the mend.
    #[test]
    fn the_mender_mends_the_weakest_adjacent_wall() {
        let turn = night_board(
            75,
            vec![
                unit(20001, "pioneer", pos(9, 22), vec!["WallFixer"]),
                wall_at(20010, pos(9, 21), 700),
                wall_at(20011, pos(10, 21), 400),
            ],
        );
        let mut state = BotState::default();
        let plan = spare_plan(&turn, &mut state, 20001);
        let cmd = plan.commands.get(&20001).expect("the mender mends");
        assert_eq!(cmd.action, "use");
        assert_eq!(cmd.name.as_deref(), Some("WallFixer"));
        assert_eq!(target(&cmd), pos(10, 21), "the weakest wall first");
    }
}
