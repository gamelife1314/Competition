//! Role identification — the three people the whole plan is organized around
//! (issue #221). The unified scheduler dispatches PER PERSON, never per time
//! of day: every controllable unit maps to exactly one mainline
//! ([`RoleKind`]) and that mainline is the only place its commands come from.
//!
//! The mapping mirrors what the legacy planners already did, so the refactor
//! renames the concept without changing who is who:
//!
//! * **pioneer** — the first alive Pioneer (`Turn::pioneer`): tasks, shopping,
//!   night wall repair;
//! * **wall_worker** (Worker A) — `workers.first()`, the lowest worker id:
//!   weapons and walls first, surplus mining second;
//! * **economy_worker** (Worker B) — `workers.last()` when two or more workers
//!   are alive: the full-time mine→sell loop. With a single worker there is no
//!   economy worker — that one worker carries the wall mainline, exactly like
//!   `day::plan`'s `economy_id` being `None` below two workers.

use crate::model::Turn;

// Role mainline modules land here as the phases replace the legacy planners:
// `wall_worker` (phase 4 — 4a delegates to the legacy dispatch), `pioneer`
// (phase 5).
pub(crate) mod economy_worker;
pub(crate) mod wall_worker;

/// Which mainline a controllable unit belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleKind {
    /// Task runner, buyer, night wall repairer.
    Pioneer,
    /// Worker A: builds/repairs walls, builds and operates the weapons,
    /// mines and sells only with the rounds its wall duties leave over.
    WallWorker,
    /// Worker B: the team economy — mines and sells around the clock.
    EconomyWorker,
}

/// The three slots of the team, resolved once per round.
///
/// Fields are unit ids rather than `&Unit` borrows so the roster stays usable
/// while `&mut BotState` is passed through the scheduler.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Roles {
    pub pioneer: Option<i64>,
    pub wall_worker: Option<i64>,
    pub economy_worker: Option<i64>,
}

impl Roles {
    /// Resolve the roster for this round. Deterministic: `Turn::workers` is
    /// sorted by id, so first/last are stable across calls.
    pub fn of(turn: &Turn) -> Roles {
        let workers = turn.workers();
        let (wall_worker, economy_worker) = match workers.len() {
            0 => (None, None),
            1 => (workers.first().map(|unit| unit.id), None),
            _ => (
                workers.first().map(|unit| unit.id),
                workers.last().map(|unit| unit.id),
            ),
        };
        Roles {
            pioneer: turn.pioneer().map(|unit| unit.id),
            wall_worker,
            economy_worker,
        }
    }

    /// The mainline of one unit, or `None` when it is not one of the three
    /// people (station, tower — units that never receive movement orders).
    pub fn kind_of(&self, id: i64) -> Option<RoleKind> {
        if self.pioneer == Some(id) {
            return Some(RoleKind::Pioneer);
        }
        if self.wall_worker == Some(id) {
            return Some(RoleKind::WallWorker);
        }
        if self.economy_worker == Some(id) {
            return Some(RoleKind::EconomyWorker);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Turn;
    use crate::protocol::Request;
    use serde_json::{json, Value};

    fn unit(id: i64, kind: &str, health: i64) -> Value {
        json!({
            "id": id, "pos": {"x": 20, "y": 15}, "roleType": kind,
            "health": health, "attackPower": 0, "attackRange": 0,
            "level": 1, "backPackCapability": 4, "backpack": []
        })
    }

    fn turn_with(roles: Vec<Value>) -> Turn {
        let payload = json!({
            "roundNo": 1,
            "mapInfo": {"width": 41, "height": 32, "zones": []},
            "teamOur": {
                "type": "challenger", "goldNum": 0, "totalScore": 0,
                "playerTasks": [], "roles": roles
            },
            "teamEnemy": {"roles": []},
            "robot": {"roles": []},
        });
        let req: Request = serde_json::from_value(payload).expect("payload parses");
        Turn::from_request(req)
    }

    #[test]
    fn full_team_maps_to_three_slots() {
        let turn = turn_with(vec![
            unit(10001, "station", 1500),
            unit(10002, "worker", 100),
            unit(10003, "worker", 100),
            unit(10004, "pioneer", 100),
        ]);
        let roles = Roles::of(&turn);
        assert_eq!(roles.pioneer, Some(10004));
        assert_eq!(roles.wall_worker, Some(10002), "A is the lowest worker id");
        assert_eq!(roles.economy_worker, Some(10003), "B is the highest worker id");
        assert_eq!(roles.kind_of(10002), Some(RoleKind::WallWorker));
        assert_eq!(roles.kind_of(10003), Some(RoleKind::EconomyWorker));
        assert_eq!(roles.kind_of(10004), Some(RoleKind::Pioneer));
        assert_eq!(roles.kind_of(10001), None, "the station has no mainline");
    }

    #[test]
    fn a_lone_worker_carries_the_wall_line() {
        let turn = turn_with(vec![unit(10001, "station", 1500), unit(10002, "worker", 100)]);
        let roles = Roles::of(&turn);
        assert_eq!(roles.wall_worker, Some(10002));
        assert_eq!(
            roles.economy_worker, None,
            "below two workers there is no economy slot — same rule as day::plan's economy_id"
        );
    }

    #[test]
    fn a_dead_pioneer_leaves_the_slot_empty() {
        let turn = turn_with(vec![
            unit(10001, "station", 1500),
            unit(10002, "worker", 100),
            unit(10003, "worker", 100),
            unit(10004, "pioneer", 0),
        ]);
        let roles = Roles::of(&turn);
        assert_eq!(roles.pioneer, None);
        assert_eq!(roles.kind_of(10004), None);
    }
}
