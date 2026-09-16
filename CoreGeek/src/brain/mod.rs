//! Decision entry point and shared helpers.

pub mod coach;
pub mod combat;
pub mod day;
pub mod economy;
pub mod news;
pub mod night;
pub mod route;
pub mod task;
pub mod treasure;
pub mod verify;

use std::collections::{HashMap, HashSet};

use crate::model::{chebyshev, footprint_distance, neighbours, station_footprint, Turn, Unit};
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
    let mut state = BotState::locked();
    decide_with(&mut state, raw_body)
}

/// `decide` with the cross-round memory supplied by the caller.
///
/// The server is single-bot and uses the process-wide `BotState` singleton,
/// but a simulation that drives a whole match round by round must NOT: two
/// simulations sharing one state observe round numbers that jump backwards,
/// `BotState::observe` reads that as a new half and wipes the memory, and the
/// result is a match whose outcome depends on which test ran first. Tests get
/// their own `BotState` through this entry point (issues #12/#13/#14 all came
/// out of a day-1 simulation, so it has to be reproducible).
pub fn decide_with(state: &mut BotState, raw_body: &[u8]) -> Result<String, String> {
    let started = std::time::Instant::now();
    let req: Request = serde_json::from_slice(raw_body).map_err(|err| err.to_string())?;
    let turn = Turn::from_request(req);
    state.observe(&turn);

    let mut plan = if turn.is_day {
        day::plan(&turn, state)
    } else {
        night::plan(&turn, state)
    };

    // executeCmd is only accepted by the judger while a task is active.
    if !state.task.active {
        plan.execute_cmd = None;
    }

    let sanitized = crate::validate::sanitize(&turn, plan.commands);

    // What today earned, counted off the commands that are about to go out. See
    // [`crate::state::DayEarn`] for why it is counted here and not off
    // `mine_pick`. The tally is written out once, on the day's last round, so
    // the day-1 question 「挖了什么、卖了多少钱」 has one line to answer it
    // instead of a `mine_pick` per round walked.
    let day_earned: Vec<RoleCommand> = sanitized.values().cloned().collect();
    state.earn.note(&turn, &day_earned);
    if turn.is_day && turn.in_day_round == crate::model::DAY_ROUNDS - 1 {
        let record = state.earn.record(&turn);
        crate::log::event("day_earn", record);
    }

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
        state,
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
        // The sandbox prelude is prepended HERE, at the boundary where bytes
        // leave for the judger, rather than inside the task loop: `Plan`
        // stays the model's script (which is what the SOP cache stores and
        // replays) and every caller that inspects `execute_cmd` keeps seeing
        // the script rather than the environment. See `task::sandbox_prelude`
        // for the three sandbox failures it removes.
        executeCmd: plan
            .execute_cmd
            .as_deref()
            .map(task::sandbox_command)
            .unwrap_or_default(),
    };
    serde_json::to_string(&response).map_err(|err| err.to_string())
}

/// `score3` — the survival objective — exactly as chapter 6 of the task book
/// defines it: `Σ 10 × day` over the days the base has stood, with 存活系数
/// dropping to 0 on the day the base falls and every day after.
///
/// It is a **sum**, and getting that wrong is quiet. At day 2 the objective is
/// worth 10 + 20 = 30, not 20; a base that fell on day 3 keeps 10 + 20 = 30
/// rather than losing everything; and surviving the full ten days is 550, not
/// 100. `kill` beside it in `scoreAttr` is cumulative, and `residual` subtracts
/// both from the running total — so a survival term that is not cumulative does
/// not merely mislabel one column, it pushes the entire discrepancy into the
/// residual, which is the column `abreport` reads as "task score plus estimate
/// error". At day 10 the mistake is 450 points, larger than any task score this
/// team has ever recorded, which is why the A/B report could show the task
/// system improving while the task score stayed at zero.
///
/// `fell_day` is `None` while the base stands, and the day it went missing
/// afterwards — the coefficient is 0 for that day, so the sum stops one day
/// short of it.
pub fn survival_score(day: i64, fell_day: Option<i64>) -> i64 {
    match fell_day {
        None => 5 * day * (day + 1),
        Some(fell) => 5 * (fell - 1) * fell,
    }
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
                "target": cmd.targetPos.as_ref().and_then(|list| list.first()).map(|pos| crate::log::xy(*pos)),
                "controller": cmd.controllerId,
            })
        })
        .collect();
    let towers: serde_json::Value = turn
        .towers()
        .iter()
        .map(|tower| {
            json!({"id": tower.id, "lvl": tower.level, "hp": tower.health, "cd": tower.cooldown})
        })
        .collect();
    let roles: serde_json::Value = turn
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
    // The opponent's side of the same two numbers. Their score, gold and task
    // submissions are invisible to us (see `Turn::enemy_station`), but their
    // base HP is in every request — logging it makes "our base fell on day 2,
    // theirs on day 5" answerable from OUR log alone, so the analysis workflow
    // is not the only source of that fact.
    let enemy_wall_hp: i64 = turn.enemy_walls().iter().map(|wall| wall.health).sum();
    // Score attribution. Robots that leave the board score `score2`
    // (small/middle/large/BOSS = 1/2/4/10); the survival rule of chapter 6 is
    // `Σ 10 x day` over the days the base has stood. Both are accumulated here
    // so the residual — task score (`score1`) plus estimate error — separates
    // the three objectives and shows WHICH one a change actually moved. See
    // [`survival_score`] for why the survival term is a sum and not a term.
    // DAWN IS NOT A KILL (issues #131-#135). 任务书 4.7.3: "黑夜结束后，在第二天
    // 早上的第一个回合，残余机器人自动清除" — the wave that survives the night is
    // removed by the rules, not shot. Counting that removal as a kill inflated
    // `cum_kill_score` by one night's survivors every single morning, and
    // `residual` is `total - kill - survival`, so the phantom landed entirely in
    // the one column the analysis workflow reads as "score_1 plus estimate
    // error". 表 8 of those five matches shows the result: a residual of -106 to
    // -409, read for five batches as "the task line is losing us 400 points" —
    // impossible, since 任务书 ch.6 gives `score_1 = 奖励 × 通过率` and neither
    // factor is ever negative. The `scoreAttr` split has to be honest before any
    // task fix can be judged by it.
    //
    // The dawn round is the first day round: robots only ever appear at the first
    // night round, so `turn.robots` is empty there and every robot that was alive
    // last round was cleared. `dawn_clear` records the displaced count beside the
    // score delta for the same round, so the next batch can check the two against
    // each other and settle whether the judger scores them.
    let vanished: Vec<i64> = state
        .prev_robot_hp
        .iter()
        .filter(|(id, hp)| **hp > 0 && !robot_hp.contains_key(id))
        .map(|(id, _)| *id)
        .collect();
    let score_of = |id: &i64| state.prev_robot_kind.get(id).copied().unwrap_or(0);
    let dawn_clear = turn.is_day && turn.in_day_round <= 1;
    let killed: i64 = if dawn_clear {
        0
    } else {
        vanished.iter().map(score_of).sum()
    };
    if dawn_clear && !vanished.is_empty() {
        crate::log::event(
            "dawn_clear",
            json!({
                "round": turn.round_no,
                "day": turn.day,
                "cleared": vanished.len(),
                "clearedScore": vanished.iter().map(score_of).sum::<i64>(),
                "scoreDelta": score_delta,
                "score": turn.total_score,
            }),
        );
    }
    state.cum_kill_score = state.cum_kill_score.saturating_add(killed);
    // `score3` freezes on the day the base falls, so the day it happened has to
    // be latched the first time we see the station gone: every round after it
    // must report the same figure, and none of them can work it out from the
    // current day alone.
    if turn.station().is_none() {
        state.station_fell_day.get_or_insert(turn.day);
    }
    let survival_score = survival_score(turn.day, state.station_fell_day);
    let residual = turn.total_score - state.cum_kill_score - survival_score;
    let pairs: serde_json::Value = state
        .night_pairs
        .iter()
        .map(|(controller, tower)| json!({"controller": controller, "tower": tower}))
        .collect();
    // `policy` and `scoreAttr` are written every round on purpose: the first is
    // the documented way to tell which coach stance was in force at a given
    // moment (WORKFLOW_REQUEST §5), the second is how `abreport` splits the
    // running total into its three objectives, and `residual` moves whenever
    // `score` does. Everything else below is change-gated — see `log::changed`.
    // `stationHp`/`enemyStationHp` keep their flat names rather than becoming a
    // `station` block: `abreport` reads `stationHp` and the A/B report's
    // base-fall detection is downstream of it. A base that is gone writes
    // `Null`, which `prune` drops; the signature still flips, so the round it
    // fell is marked in `chg`.
    // Our own base reports `0` rather than `null` once it is gone: `turn.station()`
    // returning `None` IS the base having fallen, and `0` is the value the loss
    // rule reads. The opponent's base is `null` instead, because `None` there
    // usually means the base is out of our vision rather than destroyed.
    let station_hp = turn.station().map(|station| station.health).unwrap_or(0);
    let station_lvl = turn.station().map(|station| station.level).unwrap_or(0);
    let enemy_station_hp = turn.enemy_station().map(|station| station.health);
    let enemy_station_lvl = turn.enemy_station().map(|station| station.level);
    let wall = json!({"count": turn.walls().len(), "hp": wall_hp, "hpDelta": wall_hp_delta});
    let enemy_wall = json!({"count": turn.enemy_walls().len(), "hp": enemy_wall_hp});
    // The opponent's armed forces, change-gated like every other roster block.
    // `enemyWall` above and `enemyStationLvl` already say how much wall and how
    // big a base they have; WITHOUT this line the log cannot say how many guns
    // are behind that wall, which is exactly the question "do they build
    // weapons first?" asks — and the only other place the answer could come
    // from is a receipt field the delivery workflow has reported unobtainable
    // twice (WORKFLOW_REQUEST §3.4, §13).
    let enemy_towers = json!({
        "count": turn.enemy_towers().len(),
        "kinds": turn
            .enemy_towers()
            .iter()
            .map(|tower| format!("{:?}", tower.kind).to_lowercase())
            .collect::<Vec<_>>(),
    });
    let task = json!({
        "active": state.task.active,
        "session": state.task.session_id,
        "stage": format!("{:?}", state.task.stage),
        "wrong": state.task.wrong_answers,
        "submittedRound": state.task.submitted_round,
        "phaseMissing": state.task.phase_missing_rounds,
        "pointClosedRound": state.task.point_closed_round,
    });
    let treasure = json!({
        "phase": format!("{:?}", state.treasure.phase),
        "legends": state.treasure.legends.len(),
        "summons": state.treasure.summon_attempts,
    });
    let mut data = json!({
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
        // P2-4 归因看板：三个 scoreAttr 分量之外，还缺的一格是"任务线到底挣了多少
        // 钱"。判题器按 奖励×通过率 结算、从不告诉我们通过率，所以任务点自己的
        // goldReward 是本日志能诚实携带的上界——但没有它，"任务修复有没有让任务线
        // 开始挣钱"在对局日志里根本没有答案，而它正是每一次任务修复唯一要被检验
        // 的东西。每回合写、与 `scoreAttr` 同级：这是一条累计曲线，只在变化时写
        // 就只剩下变化点，读不出比例。
        "taskGoldEarned": state.task_gold_earned,
        "robotCount": robot_hp.len(),
        "robotEvents": robot_events,
        "cmds": cmds,
        "failures": failures,
        "volley": volley.summary(),
        "errors": turn.error_codes,
        // The judger's own words for each code (e.g. `MissingNamedInput`) —
        // the one authoritative schema signal for task answers, requested
        // by the analysis workflow (WORKFLOW_REQUEST 请求四).
        "errorDescs": turn.error_descriptions.iter().map(|text| crate::log::brief(text, 120)).collect::<Vec<_>>(),
        "phaseTask": crate::log::brief(&turn.phase_task, 160),
        "lastCmdResult": crate::log::brief(&turn.last_cmd_result, 160),
        "promptChars": prompt.as_ref().map(String::len).unwrap_or(0),
        "execChars": execute_cmd.as_ref().map(String::len).unwrap_or(0),
        "llmToday": state.llm_used_today,
        // 内置教练当前档位：让分析侧能把结果与"当时是哪一档"对上，
        // 不必等一个跑不起来的 A/B（WORKFLOW_REQUEST 请求七）。
        "policy": {
            "stationPressure": state.coach.policy().station_pressure,
            "gapFunding": state.coach.policy().gap_funding,
            "harass": state.coach.policy().harass.as_str(),
        },
        "ms": started.elapsed().as_micros() as f64 / 1000.0,
    });
    // Blocks that move a handful of times a match: the base and wall lines,
    // the tower roster, the controller roster, the night pairing, the task
    // session and the treasure plan. Writing them every round cost ~500 of the
    // record's ~1400 bytes and answered nothing — the question a reader has is
    // always "when did this change", and a change is exactly what gets written.
    // The names of the blocks re-sent land in `chg`, so a round where something
    // moved is marked as such and a round without `chg` is a quiet round.
    let mut chg: Vec<&str> = Vec::new();
    let object = data.as_object_mut().expect("round data is an object");
    let base = json!([station_hp, station_lvl]);
    let enemy_base = json!([enemy_station_hp, enemy_station_lvl]);
    let gated: [(&str, &serde_json::Value, &mut Option<String>); 10] = [
        ("base", &base, &mut state.log_sigs.station),
        ("enemyBase", &enemy_base, &mut state.log_sigs.enemy_station),
        ("wall", &wall, &mut state.log_sigs.wall),
        ("enemyWall", &enemy_wall, &mut state.log_sigs.enemy_wall),
        ("enemyTowers", &enemy_towers, &mut state.log_sigs.enemy_towers),
        ("towers", &towers, &mut state.log_sigs.towers),
        ("roles", &roles, &mut state.log_sigs.roles),
        ("pairs", &pairs, &mut state.log_sigs.pairs),
        ("task", &task, &mut state.log_sigs.task),
        ("treasure", &treasure, &mut state.log_sigs.treasure),
    ];
    for (name, block, slot) in gated {
        if !crate::log::changed(slot, block) {
            continue;
        }
        match name {
            // One `chg` entry for the two halves of a base, and the keys they
            // land under are the ones `abreport` already reads.
            "base" => {
                object.insert("stationHp".into(), json!(station_hp));
                object.insert("stationLvl".into(), json!(station_lvl));
                chg.push("stationHp");
            }
            "enemyBase" => {
                object.insert("enemyStationHp".into(), json!(enemy_station_hp));
                object.insert("enemyStationLvl".into(), json!(enemy_station_lvl));
                chg.push("enemyStationHp");
            }
            _ => {
                object.insert(name.to_string(), block.clone());
                chg.push(name);
            }
        }
    }
    if !chg.is_empty() {
        object.insert("chg".to_string(), json!(chg));
    }
    crate::log::event("round", data);
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
    // The LLM traffic. `round`/`day` are here because they were NOT: these two
    // records carried a head and a `ts` and nothing else, so the one question a
    // reader has of them — "which round did the prompt actually go out on, and
    // did the script run that round or three rounds later" — could not be asked.
    // `chars` is the whole length against the head's 300-char window: the round
    // record's `promptChars`/`execChars` say a prompt went out, these two say
    // what was in it, and neither is much use without the other.
    // Drained here rather than read: the purpose belongs to the round that asked
    // for it, so a later round with no prompt must not inherit one.
    let purpose = state.last_prompt.take().map(|winner| winner.word());
    if let Some(text) = prompt {
        crate::log::event(
            "prompt_sent",
            json!({
                "round": turn.round_no,
                "day": turn.day,
                "purpose": purpose,
                "chars": text.chars().count(),
                "head": crate::log::brief(text, 300),
            }),
        );
    }
    if let Some(cmd) = execute_cmd {
        crate::log::event(
            "cmd_sent",
            json!({
                "round": turn.round_no,
                "day": turn.day,
                "chars": cmd.chars().count(),
                "head": crate::log::brief(cmd, 300),
            }),
        );
    }
}

/// Walkable cells around `target` (for "stand next to X" actions).
pub fn stand_cells(turn: &Turn, target: Pos) -> Vec<Pos> {
    neighbours(target)
        .into_iter()
        .filter(|pos| turn.is_land(*pos))
        .collect()
}

/// Can `role` reach any of `stands` from where it is? A role already standing
/// on one counts as reachable. Walls, robots and the map edge all block.
pub fn can_reach_any(turn: &Turn, role: &Unit, stands: &[Pos]) -> bool {
    if stands.iter().any(|stand| *stand == role.pos) {
        return true;
    }
    let blocked = turn.blocked_for(role.id);
    crate::path::step_toward_stands(turn, role.pos, stands, &blocked).is_some()
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

/// Walkable cells at footprint distance <= 1 from the station — the band
/// between the base and the radius-2 wall ring. This is the set
/// `update_wall_gate` measures "everyone is inside" against, so it is also the
/// right target for a role that has nowhere else useful to be: hugging the
/// station is both the safest cell on the board and the one that lets the last
/// ring cell be sealed.
///
/// The station's own four cells are excluded: they satisfy the distance test
/// but no role can ever stand on them, and offering them as a retreat target
/// makes `walk_toward` path at a wall of its own base.
pub fn interior_cells(turn: &Turn) -> Vec<Pos> {
    let Some(station) = turn.station() else {
        return Vec::new();
    };
    let footprint = station_footprint(station.pos);
    let mut cells: Vec<Pos> = footprint
        .iter()
        .flat_map(|cell| neighbours(*cell))
        .filter(|pos| {
            turn.is_land(*pos)
                && !footprint.contains(pos)
                && footprint_distance(*pos, &footprint) <= 1
        })
        .collect();
    cells.sort_by_key(|pos| (pos.x, pos.y));
    cells.dedup();
    cells
}

/// Walkable cells from which a controller can OPERATE a tower, restricted to
/// the inside of the wall ring. Plain `stand_cells` would also return the
/// tower's outer neighbours (footprint distance 2) — those sit exactly on the
/// wall ring and get walled over, which is what trapped workers outside.
///
/// An inner cell is used whenever one exists at all, not merely when two do.
/// The old "two or nothing" fallback handed the controller an outer cell the
/// moment a gun had only one inner neighbour, and an outer cell is the wall
/// line: the operator then stands in front of the wall it is supposed to be
/// behind, and the wall crew can never fill the cell under its feet. That is
/// issue #15 — "操控者在墙的外侧…直接暴露在怪物攻击范围内". A gun manned from a
/// single inner cell still fires; a gun manned from the wall ring is a hole in
/// the ring. `all` remains the answer only for a tower with no inner cell at
/// all, where standing outside is the only way to shoot at all.
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
    if inner.is_empty() {
        all
    } else {
        inner
    }
}

/// The escape of last resort for a role our own ring has shut out.
///
/// [`walk_or_remove_wall`] only ever tears down a wall whose removal makes the
/// goal reachable **this round**. That is right when the pocket is one wall
/// deep and wrong when it is not: with no single removal that opens the route
/// the predicate is false for every candidate, no command is issued at all, and
/// the role stands on the same cell until the night ends. Measured across
/// issues #126-#130, which is exactly what the `stuck` triple added in v17 was
/// built to expose:
///
///   * 126 tower 20040 `controller_stuck@27,13/5/1` — **11 consecutive rounds**
///     on one cell with one adjacent own wall, its gun silent for the whole of
///     day 3's night, and the same role the one `wall_gate_open` named on all 11
///     dusk rounds of that day (the seal happened only on the hard deadline);
///   * 129 tower 20020 `controller_stuck@32,10/3/3` — **14 rounds**, three
///     adjacent own walls, none of which alone reopened the route;
///   * 128 `20040@34,5/5/0` x3 and 130 `20020@30,11/3/0` x5 — the same freeze
///     with nothing adjacent to cut at all, which is the half of the batch the
///     fix must stay silent on;
///   * 127's 19 `controller_stuck` rounds, from before v17's triple existed to
///     say which cell they were on.
///
/// §15.3 of the workflow spec predicted the reading: a repeated `@x,y` is a
/// pocket, not slowness, and `M > 0` with the role still stuck is the shape of
/// the wall line rather than a missing demolition. So the post is approached by
/// MOVEMENT when no route exists, in two steps, whichever makes progress:
///
///   1. cut an adjacent wall of ours that strictly shortens the distance to the
///      nearest operating cell — one wall per round, the same rate the ring crew
///      builds them, and the only move that changes the position at all;
///   2. failing that, STEP onto a neighbour that strictly shortens the same
///      distance. Without this the trick above is a one-round cure: the role
///      cuts `(6,5)`, and on the next round it has no adjacent wall left to cut
///      and no route to walk, so it freezes one cell short of the gap it just
///      opened. The step is the half that actually gets it home.
///
/// Deliberately narrow. Every candidate must strictly reduce the distance to the
/// post, so a role in a pocket whose only walls lead nowhere still reports
/// `controller_stuck` rather than chewing the ring for nothing; the search is
/// monotone, so it cannot oscillate; and the day planner keeps
/// `walk_or_remove_wall`'s stricter predicate untouched — the demolition hatch
/// that protects the dusk seal is not widened by this.
pub fn break_out(
    turn: &Turn,
    role: &Unit,
    stands: &[Pos],
    claimed: &mut HashSet<Pos>,
) -> Option<RoleCommand> {
    if stands.is_empty() {
        return None;
    }
    let distance = |from: Pos| {
        stands
            .iter()
            .map(|stand| chebyshev(from, *stand))
            .min()
            .unwrap_or(i32::MAX)
    };
    let here = distance(role.pos);
    if let Some(pos) = turn
        .walls()
        .into_iter()
        .map(|wall| wall.pos)
        .filter(|pos| chebyshev(role.pos, *pos) == 1)
        .filter(|pos| distance(*pos) < here)
        .min_by_key(|pos| (distance(*pos), pos.x, pos.y))
    {
        return Some(RoleCommand::remove(pos));
    }
    let blocked = turn.blocked_for(role.id);
    let closer: Vec<Pos> = neighbours(role.pos)
        .into_iter()
        .filter(|pos| turn.is_land(*pos) && !blocked.contains(pos))
        .filter(|pos| distance(*pos) < here)
        .collect();
    if !closer.is_empty() {
        return walk_toward(turn, role, &closer, claimed);
    }
    // LAST RESORT (pk616181: tower 20040 `controller_stuck@32,9/3/3` for 33
    // rounds — a whole gun silent for a whole night). The controller is in a
    // sealed pocket of its own walls: no adjacent wall leads strictly closer,
    // and no adjacent land cell leads strictly closer. The monotone search
    // above correctly refuses to wander, but 33 rounds of silence is worse
    // than a temporary hole the day crew can re-wall at dawn. Cut an adjacent
    // wall at the SAME distance (a lateral move) — it might open a path around
    // the obstacle that the strict `<` filter missed. Walls that lead strictly
    // AWAY from the post are still rejected: a wall farther out is not a path
    // home, it is a hole in the ring for nothing (the `M = 0` / far-wall
    // shapes measured in issues #128 and #130).
    if let Some(pos) = turn
        .walls()
        .into_iter()
        .map(|wall| wall.pos)
        .filter(|pos| chebyshev(role.pos, *pos) == 1)
        .filter(|pos| distance(*pos) <= here)
        .min_by_key(|pos| (distance(*pos), pos.x, pos.y))
    {
        return Some(RoleCommand::remove(pos));
    }
    None
}

/// Walk toward `stands`; when the pathfinder finds no route (our own wall
/// ring has sealed us out), demolish an adjacent wall of ours to reopen the
/// way. A role already standing on a usable cell returns None (no movement
/// needed) rather than tearing down a wall needlessly.
///
/// "Reopen the way" is meant literally: a candidate wall is only torn down
/// when its removal makes `stands` reachable. Tearing one down otherwise spends
/// a cell of the day's ring — and therefore a hole the night can be entered
/// through — on a role that still cannot get home afterwards. A ring whose
/// inner band is occupied by towers and teammates has walls like that: the two
/// cells beside the door lead into a gun and into the operator standing on it,
/// so demolishing them opens nothing, while the ring ends the day two cells
/// short and the gate seal waits on a role that was never let in.
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
    // On the very last day round, a wall cut here can never be re-walled —
    // the next round is night and the seal step is done. The controller
    // holds outside rather than opening a hole the night must keep. The
    // unconditional night recall (night.rs) brings it in after dark.
    // (Measured in day1_sim: R200 demolished (11,26), the removal was
    // never re-walled, and the door stayed open all night.)
    if turn.is_day && turn.in_day_round >= crate::model::DAY_ROUNDS - 1 {
        return None;
    }
    // A route that exists once this round's CLAIMS are ignored is a route
    // that exists next round: hold rather than tear a wall down for a
    // one-round collision with a teammate's reservation. Claims are intents
    // (the claimer moves on); a genuinely sealed route — a parked teammate,
    // a closed ring — shows up in the static set too, and only that earns a
    // demolition. Measured on the day-2 board: the gatling operator's way
    // home crossed one claimed cell for one round, the hatch cut (13,23) for
    // it, and the cut then had to be re-walled from outside with stone the
    // seal did not have.
    let static_blocked = turn.blocked_for(role.id);
    if crate::path::step_toward_stands(turn, role.pos, stands, &static_blocked).is_some() {
        return None;
    }
    turn.walls()
        .into_iter()
        .map(|wall| wall.pos)
        .filter(|pos| chebyshev(role.pos, *pos) == 1)
        .filter(|pos| {
            let mut opened = static_blocked.clone();
            opened.remove(pos);
            crate::path::step_toward_stands(turn, role.pos, stands, &opened).is_some()
        })
        .min_by_key(|pos| {
            stands
                .iter()
                .map(|stand| chebyshev(*pos, *stand))
                .min()
                .unwrap_or(i32::MAX)
        })
        .map(|pos| RoleCommand::remove(pos))
}
