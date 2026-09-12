//! Decision entry point and shared helpers.

pub mod combat;
pub mod day;
pub mod economy;
pub mod news;
pub mod night;
pub mod task;
pub mod treasure;
pub mod verify;

use std::collections::{HashMap, HashSet};

use crate::model::{chebyshev, neighbours, Turn, Unit};
use crate::protocol::{Pos, Request, Response, RoleCommand};
use crate::state::{BotState, IssuedCmd};

const EMPTY_RESPONSE: &str = r#"{"roleCommandMap":{},"prompt":"","executeCmd":""}"#;

/// Everything the planners can emit in one round.
#[derive(Default)]
pub struct Plan {
    pub commands: HashMap<i64, RoleCommand>,
    pub prompt: Option<String>,
    pub execute_cmd: Option<String>,
}

impl Plan {
    /// First command per role wins (priority = insertion order).
    pub fn push(&mut self, id: i64, cmd: RoleCommand) {
        self.commands.entry(id).or_insert(cmd);
    }
}

/// Entry point called by the HTTP layer. Never panics; always returns a
/// well-formed JSON response string.
pub fn respond(raw_body: &[u8]) -> String {
    match decide(raw_body) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("decision failed: {err}");
            EMPTY_RESPONSE.to_owned()
        }
    }
}

fn decide(raw_body: &[u8]) -> Result<String, String> {
    let started = std::time::Instant::now();
    let req: Request = serde_json::from_slice(raw_body).map_err(|err| err.to_string())?;
    let turn = Turn::from_request(req);
    let mut state = BotState::locked();
    state.observe(&turn);

    let mut plan = if turn.is_day {
        day::plan(&turn, &mut state)
    } else {
        night::plan(&turn, &mut state)
    };

    // executeCmd is only accepted by the judger while a task is active.
    if !state.task.active {
        plan.execute_cmd = None;
    }

    let sanitized = crate::validate::sanitize(&turn, plan.commands);

    // Last round's command map, captured before it is overwritten: both the
    // failure join below and the volley review read it.
    let previous_issued = std::mem::take(&mut state.last_issued);

    // Join last round's failures with the commands that caused them — key
    // feedback for post-match tuning.
    let failures: Vec<serde_json::Value> = turn
        .last_action_results
        .iter()
        .filter(|(_id, ok)| !**ok)
        .filter_map(|(id, _ok)| {
            previous_issued.get(id).map(|cmd| {
                serde_json::json!({
                    "role": id,
                    "action": cmd.action,
                    "target": cmd.target,
                    "name": cmd.name,
                })
            })
        })
        .collect();

    // Attack-result verification: the judger's verdict on last night's volleys
    // joined with the robot HP the same round left behind. `prev_robot_hp` is
    // still last round's snapshot here — `log_round` overwrites it at the tail.
    let volley = verify::review_volley(&turn, &previous_issued, &state.prev_robot_hp);

    // Remember what we actually sent for next round's failure feedback.
    let mut issued: HashMap<i64, IssuedCmd> = HashMap::new();
    for (id_key, cmd) in &sanitized {
        if let Ok(id) = id_key.parse::<i64>() {
            issued.insert(
                id,
                IssuedCmd {
                    action: cmd.action.clone(),
                    target: cmd
                        .targetPos
                        .as_ref()
                        .and_then(|list| list.first().copied()),
                    name: cmd.name.clone(),
                },
            );
        }
    }
    state.last_issued = issued;

    log_round(
        &turn,
        &mut state,
        &sanitized,
        &failures,
        &volley,
        &plan.prompt,
        &plan.execute_cmd,
        started,
    );

    let response = Response {
        roleCommandMap: sanitized,
        prompt: plan.prompt.unwrap_or_default(),
        executeCmd: plan.execute_cmd.unwrap_or_default(),
    };
    serde_json::to_string(&response).map_err(|err| err.to_string())
}

#[allow(clippy::too_many_arguments)]
fn log_round(
    turn: &Turn,
    state: &mut BotState,
    sanitized: &std::collections::BTreeMap<String, RoleCommand>,
    failures: &[serde_json::Value],
    volley: &verify::VolleyReview,
    prompt: &Option<String>,
    execute_cmd: &Option<String>,
    started: std::time::Instant,
) {
    use serde_json::json;
    let cmds: Vec<serde_json::Value> = sanitized
        .iter()
        .map(|(id, cmd)| {
            json!({
                "id": id,
                "action": cmd.action,
                "name": cmd.name,
                "target": cmd.targetPos.as_ref().map(|list| list.first()),
                "controller": cmd.controllerId,
            })
        })
        .collect();
    let towers: Vec<serde_json::Value> = turn
        .towers()
        .iter()
        .map(|tower| {
            json!({"id": tower.id, "lvl": tower.level, "hp": tower.health, "cd": tower.cooldown})
        })
        .collect();
    let roles: Vec<serde_json::Value> = turn
        .controllable()
        .iter()
        .map(|role| json!({"id": role.id, "hp": role.health, "pack": role.backpack.len()}))
        .collect();
    let score_delta = state
        .prev_total_score
        .map(|previous| turn.total_score - previous);
    let robot_hp: HashMap<i64, i64> = turn
        .robots
        .iter()
        .filter(|robot| robot.health > 0)
        .map(|robot| (robot.id, robot.health))
        .collect();
    let robot_events: Vec<serde_json::Value> = robot_hp
        .iter()
        .filter_map(|(id, hp)| match state.prev_robot_hp.get(id) {
            Some(previous) if previous != hp => Some(json!({"id": id, "hpDelta": hp - previous})),
            None => Some(json!({"id": id, "spawnHp": hp})),
            _ => None,
        })
        .chain(
            state
                .prev_robot_hp
                .iter()
                .filter(|(id, _)| !robot_hp.contains_key(id))
                .map(|(id, previous)| json!({"id": id, "goneFromHp": previous})),
        )
        .collect();
    let wall_hp: i64 = turn.walls().iter().map(|wall| wall.health).sum();
    let wall_hp_delta = state.prev_wall_hp.map(|previous| wall_hp - previous);
    // Score attribution. Robots that leave the board score `score2`
    // (small/middle/large/BOSS = 1/2/4/10); the survival rule of chapter 6 is
    // `10 x day` while the station stands. Both are accumulated here so the
    // residual — task score (`score1`) plus estimate error — separates the
    // three objectives and shows WHICH one a change actually moved.
    let killed: i64 = state
        .prev_robot_hp
        .iter()
        .filter(|(id, hp)| **hp > 0 && !robot_hp.contains_key(id))
        .map(|(id, _)| state.prev_robot_kind.get(id).copied().unwrap_or(0))
        .sum();
    state.cum_kill_score = state.cum_kill_score.saturating_add(killed);
    let survival_score = if turn.station().is_some() {
        turn.day * 10
    } else {
        0
    };
    let residual = turn.total_score - state.cum_kill_score - survival_score;
    let pairs: Vec<serde_json::Value> = state
        .night_pairs
        .iter()
        .map(|(controller, tower)| json!({"controller": controller, "tower": tower}))
        .collect();
    crate::log::event(
        "round",
        json!({
            "round": turn.round_no,
            "day": turn.day,
            "isDay": turn.is_day,
            "gold": turn.gold,
            "score": turn.total_score,
            "scoreDelta": score_delta,
            "scoreAttr": {
                "kill": state.cum_kill_score,
                "killThisRound": killed,
                "survival": survival_score,
                "residual": residual,
            },
            "stationHp": turn.station().map(|station| station.health),
            "stationLvl": turn.station().map(|station| station.level),
            "robotCount": robot_hp.len(),
            "robotEvents": robot_events,
            "wall": {"count": turn.walls().len(), "hp": wall_hp, "hpDelta": wall_hp_delta},
            "pairs": pairs,
            "towers": towers,
            "roles": roles,
            "cmds": cmds,
            "failures": failures,
            "volley": volley.summary(),
            "errors": turn.error_codes,
            "phaseTask": crate::log::brief(&turn.phase_task, 160),
            "lastCmdResult": crate::log::brief(&turn.last_cmd_result, 160),
            "promptChars": prompt.as_ref().map(String::len).unwrap_or(0),
            "execChars": execute_cmd.as_ref().map(String::len).unwrap_or(0),
            "llmToday": state.llm_used_today,
            "task": {
                "active": state.task.active,
                "session": state.task.session_id,
                "stage": format!("{:?}", state.task.stage),
                "wrong": state.task.wrong_answers,
                "submittedRound": state.task.submitted_round,
                "phaseMissing": state.task.phase_missing_rounds,
                "pointClosedRound": state.task.point_closed_round,
            },
            "treasure": {
                "phase": format!("{:?}", state.treasure.phase),
                "legends": state.treasure.legends.len(),
                "summons": state.treasure.summon_attempts,
            },
            "ms": started.elapsed().as_micros() as f64 / 1000.0,
        }),
    );
    // Two actionable volley outcomes get their own record so a captured match
    // can be grepped for them directly instead of reconstructed from `round`.
    if !volley.rejected_towers().is_empty() || volley.no_robot_damage_round() {
        crate::log::event(
            "volley_review",
            json!({
                "round": turn.round_no,
                "day": turn.day,
                "rejected": volley.rejected_towers(),
                "noRobotDamage": volley.no_robot_damage_round(),
                "volleys": volley.summary()["volleys"].clone(),
                "damage": volley.robot_damage(),
                "kills": volley.kills,
            }),
        );
    }
    state.prev_total_score = Some(turn.total_score);
    state.prev_robot_hp = robot_hp;
    state.prev_robot_kind = turn
        .robots
        .iter()
        .filter(|robot| robot.health > 0)
        .map(|robot| (robot.id, robot.kind.score()))
        .collect();
    state.prev_wall_hp = Some(wall_hp);
    if let Some(text) = prompt {
        crate::log::event("prompt_sent", json!({"head": crate::log::brief(text, 300)}));
    }
    if let Some(cmd) = execute_cmd {
        crate::log::event("cmd_sent", json!({"head": crate::log::brief(cmd, 300)}));
    }
}

/// Walkable cells around `target` (for "stand next to X" actions).
pub fn stand_cells(turn: &Turn, target: Pos) -> Vec<Pos> {
    neighbours(target)
        .into_iter()
        .filter(|pos| turn.is_land(*pos))
        .collect()
}

/// Compute the next step for `role` toward any of `stands`, avoiding cells
/// already claimed by teammates this round. Returns a move command and
/// reserves the chosen step.
pub fn walk_toward(
    turn: &Turn,
    role: &Unit,
    stands: &[Pos],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    let mut blocked = turn.blocked_for(role.id);
    blocked.extend(claimed.iter().copied());
    let usable: Vec<Pos> = stands
        .iter()
        .copied()
        .filter(|pos| *pos == role.pos || (!blocked.contains(pos) && turn.is_land(*pos)))
        .collect();
    let step = crate::path::step_toward_stands(turn, role.pos, &usable, &blocked)?;
    claimed.insert(step);
    Some(RoleCommand::move_to(step))
}

/// Walkable cells from which a controller can OPERATE a tower, restricted to
/// the inside of the wall ring. Plain `stand_cells` would also return the
/// tower's outer neighbours (footprint distance 2) — those sit exactly on the
/// wall ring and get walled over, which is what trapped workers outside. The
/// wall layout leaves at least two inner cells per tower, so a weapon can
/// never be fully enclosed.
pub fn tower_stand_cells(turn: &Turn, tower_pos: Pos) -> Vec<Pos> {
    let all = stand_cells(turn, tower_pos);
    let Some(station) = turn.station() else {
        return all;
    };
    let footprint = station.footprint();
    let inner: Vec<Pos> = all
        .iter()
        .copied()
        .filter(|pos| crate::model::footprint_distance(*pos, &footprint) <= 1)
        .collect();
    if inner.len() >= 2 {
        inner
    } else {
        all
    }
}

/// Walk toward `stands`; when the pathfinder finds no route (our own wall
/// ring has sealed us out), demolish an adjacent wall of ours to reopen the
/// way. A role already standing on a usable cell returns None (no movement
/// needed) rather than tearing down a wall needlessly.
pub fn walk_or_remove_wall(
    turn: &Turn,
    role: &Unit,
    stands: &[Pos],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if stands.iter().any(|stand| *stand == role.pos) {
        return None;
    }
    if let Some(cmd) = walk_toward(turn, role, stands, claimed) {
        return Some(cmd);
    }
    turn.walls()
        .into_iter()
        .map(|wall| wall.pos)
        .filter(|pos| chebyshev(role.pos, *pos) == 1)
        .min_by_key(|pos| {
            stands
                .iter()
                .map(|stand| chebyshev(*pos, *stand))
                .min()
                .unwrap_or(i32::MAX)
        })
        .map(|pos| RoleCommand::remove(pos))
}
