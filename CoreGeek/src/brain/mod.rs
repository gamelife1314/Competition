//! Decision entry point and shared helpers.

pub mod combat;
pub mod day;
pub mod economy;
pub mod news;
pub mod night;
pub mod task;
pub mod treasure;

use std::collections::{HashMap, HashSet};

use crate::model::{neighbours, Turn, Unit};
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

    let mut plan = if turn.is_day { day::plan(&turn, &mut state) } else { night::plan(&turn, &mut state) };

    // executeCmd is only accepted by the judger while a task is active.
    if !state.task.active {
        plan.execute_cmd = None;
    }

    let sanitized = crate::validate::sanitize(&turn, plan.commands);

    // Join last round's failures with the commands that caused them before
    // overwriting `last_issued` — key feedback for post-match tuning.
    let failures: Vec<serde_json::Value> = turn
        .last_action_results
        .iter()
        .filter(|(_id, ok)| !**ok)
        .filter_map(|(id, _ok)| {
            state.last_issued.get(id).map(|cmd| {
                serde_json::json!({
                    "role": id,
                    "action": cmd.action,
                    "target": cmd.target,
                    "name": cmd.name,
                })
            })
        })
        .collect();

    // Remember what we actually sent for next round's failure feedback.
    let mut issued: HashMap<i64, IssuedCmd> = HashMap::new();
    for (id_key, cmd) in &sanitized {
        if let Ok(id) = id_key.parse::<i64>() {
            issued.insert(
                id,
                IssuedCmd {
                    action: cmd.action.clone(),
                    target: cmd.targetPos.as_ref().and_then(|list| list.first().copied()),
                    name: cmd.name.clone(),
                },
            );
        }
    }
    state.last_issued = issued;

    log_round(&turn, &state, &sanitized, &failures, &plan.prompt, &plan.execute_cmd, started);

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
    state: &BotState,
    sanitized: &std::collections::BTreeMap<String, RoleCommand>,
    failures: &[serde_json::Value],
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
    crate::log::event(
        "round",
        json!({
            "round": turn.round_no,
            "day": turn.day,
            "isDay": turn.is_day,
            "gold": turn.gold,
            "score": turn.total_score,
            "stationHp": turn.station().map(|station| station.health),
            "stationLvl": turn.station().map(|station| station.level),
            "robots": turn.robots.iter().filter(|robot| robot.health > 0).count(),
            "towers": towers,
            "roles": roles,
            "cmds": cmds,
            "failures": failures,
            "errors": turn.error_codes,
            "promptChars": prompt.as_ref().map(String::len).unwrap_or(0),
            "execChars": execute_cmd.as_ref().map(String::len).unwrap_or(0),
            "llmToday": state.llm_used_today,
            "task": {
                "active": state.task.active,
                "stage": format!("{:?}", state.task.stage),
                "wrong": state.task.wrong_answers,
            },
            "treasure": {
                "phase": format!("{:?}", state.treasure.phase),
                "legends": state.treasure.legends.len(),
                "summons": state.treasure.summon_attempts,
            },
            "ms": started.elapsed().as_micros() as f64 / 1000.0,
        }),
    );
    if let Some(text) = prompt {
        crate::log::event("prompt_sent", json!({"head": crate::log::brief(text, 300)}));
    }
    if let Some(cmd) = execute_cmd {
        crate::log::event("cmd_sent", json!({"head": crate::log::brief(cmd, 300)}));
    }
}

/// Walkable cells around `target` (for "stand next to X" actions).
pub fn stand_cells(turn: &Turn, target: Pos) -> Vec<Pos> {
    neighbours(target).into_iter().filter(|pos| turn.is_land(*pos)).collect()
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
