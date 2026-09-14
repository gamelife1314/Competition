//! Domain model: a `Turn` view over the raw request, with game-rule helpers.

use std::collections::{HashMap, HashSet};

use crate::protocol::{Pos, Request, RoleRaw};

pub const DAY_ROUNDS: i64 = 70;
pub const ROUNDS_PER_DAY: i64 = DAY_ROUNDS + 60;

pub const WEAPON_BUILD_COST: i64 = 25;
pub const STONE: &str = "stone";
pub const IRON: &str = "iron";
pub const COPPER: &str = "copper";
pub const ORES: [&str; 3] = [STONE, IRON, COPPER];

/// Attack range per weapon level (index 0 = level1). Rocket level3 = whole map.
pub const FULL_MAP_RANGE: i32 = i32::MAX / 2;
pub fn tower_range(kind: UnitKind, level: i32) -> i32 {
    let table: [i32; 3] = match kind {
        UnitKind::Gatling => [3, 5, 7],
        UnitKind::Railgun => [6, 8, 10],
        UnitKind::Rocket => [10, 15, FULL_MAP_RANGE],
        _ => [0, 0, 0],
    };
    let idx = level.clamp(1, 3) as usize - 1;
    table[idx]
}

/// Number of projectiles/targets allowed = weapon level.
pub fn tower_projectiles(kind: UnitKind, level: i32) -> i32 {
    match kind {
        UnitKind::Gatling | UnitKind::Rocket => level.clamp(1, 3),
        UnitKind::Railgun => 1,
        _ => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitKind {
    Station,
    Gatling,
    Railgun,
    Rocket,
    Wall,
    Pioneer,
    Worker,
    Unknown,
}

impl UnitKind {
    pub fn parse(s: &str) -> Self {
        match s {
            "station" => UnitKind::Station,
            "gatling" => UnitKind::Gatling,
            "railgun" => UnitKind::Railgun,
            "rocket" => UnitKind::Rocket,
            "wall" => UnitKind::Wall,
            "pioneer" => UnitKind::Pioneer,
            "worker" => UnitKind::Worker,
            _ => UnitKind::Unknown,
        }
    }
    pub fn is_tower(self) -> bool {
        matches!(
            self,
            UnitKind::Gatling | UnitKind::Railgun | UnitKind::Rocket
        )
    }
    pub fn is_controllable(self) -> bool {
        matches!(self, UnitKind::Pioneer | UnitKind::Worker)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RobotKind {
    Small,
    Middle,
    Large,
    Boss,
    Unknown,
}

impl RobotKind {
    pub fn parse(s: &str) -> Self {
        match s {
            "smallRobot" => RobotKind::Small,
            "middleRobot" => RobotKind::Middle,
            "largeRobot" => RobotKind::Large,
            "bossRobot" => RobotKind::Boss,
            _ => RobotKind::Unknown,
        }
    }
    /// Score awarded per kill (task book 4.7.2).
    pub fn score(self) -> i64 {
        match self {
            RobotKind::Small => 1,
            RobotKind::Middle => 2,
            RobotKind::Large => 4,
            RobotKind::Boss => 10,
            RobotKind::Unknown => 0,
        }
    }
}

pub fn chebyshev(a: Pos, b: Pos) -> i32 {
    (a.x - b.x).abs().max((a.y - b.y).abs())
}

pub fn neighbours(pos: Pos) -> [Pos; 8] {
    let mut out = [Pos { x: 0, y: 0 }; 8];
    let mut i = 0;
    for dx in [-1, 0, 1] {
        for dy in [-1, 0, 1] {
            if dx == 0 && dy == 0 {
                continue;
            }
            out[i] = Pos {
                x: pos.x + dx,
                y: pos.y + dy,
            };
            i += 1;
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Unit {
    pub id: i64,
    pub pos: Pos,
    pub kind: UnitKind,
    pub health: i64,
    pub attack_power: i64,
    pub attack_range_raw: i32,
    pub level: i32,
    pub cooldown: i32,
    pub capacity: i64,
    pub backpack: Vec<String>,
}

impl Unit {
    fn from_raw(raw: &RoleRaw) -> Self {
        Self {
            id: raw.id,
            pos: raw.pos,
            kind: UnitKind::parse(&raw.role_type),
            health: raw.health,
            attack_power: raw.attack_power,
            attack_range_raw: raw.attack_range.clamp(i64::MIN, FULL_MAP_RANGE as i64) as i32,
            level: raw.level.max(0) as i32,
            cooldown: raw.cooldown.max(0) as i32,
            capacity: raw.backpack_capability,
            backpack: raw.backpack.clone(),
        }
    }

    pub fn alive(&self) -> bool {
        self.health > 0
    }
    pub fn backpack_full(&self) -> bool {
        self.capacity > 0 && self.backpack.len() as i64 >= self.capacity
    }
    pub fn count_item(&self, name: &str) -> usize {
        self.backpack
            .iter()
            .filter(|item| item.as_str() == name)
            .count()
    }
    /// Effective attack range: trust the request field when set, else fall
    /// back to the per-kind level table.
    pub fn range_of_attack(&self) -> i32 {
        if self.attack_range_raw > 0 {
            self.attack_range_raw
        } else {
            tower_range(self.kind, self.level)
        }
    }
    /// 2x2 footprint for stations (pos = one corner), single cell otherwise.
    pub fn footprint(&self) -> Vec<Pos> {
        if self.kind == UnitKind::Station {
            station_footprint(self.pos)
        } else {
            vec![self.pos]
        }
    }
}

pub fn station_footprint(pos: Pos) -> Vec<Pos> {
    vec![
        pos,
        Pos {
            x: pos.x + 1,
            y: pos.y,
        },
        Pos {
            x: pos.x,
            y: pos.y - 1,
        },
        Pos {
            x: pos.x + 1,
            y: pos.y - 1,
        },
    ]
}

pub fn footprint_distance(pos: Pos, footprint: &[Pos]) -> i32 {
    footprint
        .iter()
        .map(|cell| chebyshev(pos, *cell))
        .min()
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct Robot {
    pub id: i64,
    pub pos: Pos,
    pub kind: RobotKind,
    pub health: i64,
    pub dizzy: bool,
    pub target_team: String,
}

#[derive(Debug, Clone)]
pub struct PlayerTask {
    pub task_type: String,
    pub pos: Pos,
    pub cooldown_rounds: i64,
    pub score_reward: i64,
    pub gold_reward: i64,
    pub is_valid: bool,
    pub timeout_rounds: i64,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub round_no: i64,
    /// 1-based day index.
    pub day: i64,
    /// 0-based round index within the day.
    pub in_day_round: i64,
    pub is_day: bool,
    pub width: i32,
    pub height: i32,
    pub zones: HashMap<Pos, String>,
    pub ours: Vec<Unit>,
    pub enemy: Vec<Unit>,
    pub robots: Vec<Robot>,
    pub gold: i64,
    pub total_score: i64,
    pub team_type: String,
    pub player_tasks: Vec<PlayerTask>,
    pub vendor_prices: HashMap<String, i64>,
    pub weapon_shop: HashMap<String, i64>,
    pub phase_task: String,
    pub llm_resp: String,
    pub last_cmd_result: String,
    pub last_summon_result: i64,
    pub last_action_results: HashMap<i64, bool>,
    pub official_news: String,
    pub folk_legends: String,
    pub error_codes: Vec<i64>,
    /// The judger's text for each error code — e.g. the `MissingNamedInput`
    /// rejection reason the opponent used to fix their task answers (issue
    /// #10). Parsed since protocol.rs day one but DROPPED here until now;
    /// kept for logging so the analysis workflow finally sees WHY a
    /// submission was rejected instead of only that it was.
    pub error_descriptions: Vec<String>,
}

impl Turn {
    pub fn from_request(req: Request) -> Self {
        let round_no = req.round_no.max(1);
        let in_day_round = (round_no - 1) % ROUNDS_PER_DAY;
        let mut zones = HashMap::new();
        for zone in &req.map_info.zones {
            zones.insert(zone.pos, zone.neutral_type.clone());
        }
        let mut vendor_prices = HashMap::new();
        for item in &req.vendor_shop_list {
            vendor_prices.insert(item.name.clone(), item.price);
        }
        let mut weapon_shop = HashMap::new();
        for item in &req.weapon_shop_list {
            weapon_shop.insert(item.name.clone(), item.price);
        }
        let mut last_action_results = HashMap::new();
        for (key, ok) in &req.last_round_role_action_results {
            if let Ok(id) = key.parse::<i64>() {
                last_action_results.insert(id, *ok);
            }
        }
        Self {
            round_no,
            day: (round_no - 1) / ROUNDS_PER_DAY + 1,
            in_day_round,
            is_day: in_day_round < DAY_ROUNDS,
            width: req.map_info.width.max(1),
            height: req.map_info.height.max(1),
            zones,
            ours: req.team_our.roles.iter().map(Unit::from_raw).collect(),
            enemy: req.team_enemy.roles.iter().map(Unit::from_raw).collect(),
            robots: req
                .robot
                .roles
                .iter()
                .map(|raw| Robot {
                    id: raw.id,
                    pos: raw.pos,
                    kind: RobotKind::parse(&raw.role_type),
                    health: raw.health,
                    dizzy: raw.abnormal_state == "dizzy",
                    target_team: raw.target_team.clone(),
                })
                .collect(),
            gold: req.team_our.gold_num,
            total_score: req.team_our.total_score,
            team_type: req.team_our.team_type,
            player_tasks: req
                .team_our
                .player_tasks
                .iter()
                .map(|raw| PlayerTask {
                    task_type: raw.task_type.clone(),
                    pos: raw.task_position,
                    cooldown_rounds: raw.cold_down_rounds,
                    score_reward: raw.score_reward,
                    gold_reward: raw.gold_reward,
                    is_valid: raw.is_valid,
                    timeout_rounds: raw.timeout_rounds,
                })
                .collect(),
            vendor_prices,
            weapon_shop,
            phase_task: req.phase_task,
            llm_resp: req.llm_resp,
            last_cmd_result: req.last_cmd_result,
            last_summon_result: req.last_summon_treasure_result,
            last_action_results,
            official_news: req.world_news.official_news,
            folk_legends: req.world_news.folk_legends,
            error_codes: req.errors.iter().map(|e| e.error_code).collect(),
            error_descriptions: req.errors.iter().map(|e| e.description.clone()).collect(),
        }
    }

    pub fn station(&self) -> Option<&Unit> {
        self.ours.iter().find(|unit| unit.kind == UnitKind::Station)
    }
    pub fn workers(&self) -> Vec<&Unit> {
        let mut out: Vec<&Unit> = self
            .ours
            .iter()
            .filter(|unit| unit.kind == UnitKind::Worker && unit.alive())
            .collect();
        out.sort_by_key(|unit| unit.id);
        out
    }
    pub fn pioneer(&self) -> Option<&Unit> {
        self.ours
            .iter()
            .find(|unit| unit.kind == UnitKind::Pioneer && unit.alive())
    }
    pub fn controllable(&self) -> Vec<&Unit> {
        let mut out: Vec<&Unit> = self
            .ours
            .iter()
            .filter(|unit| unit.kind.is_controllable() && unit.alive())
            .collect();
        out.sort_by_key(|unit| unit.id);
        out
    }
    pub fn towers(&self) -> Vec<&Unit> {
        let mut out: Vec<&Unit> = self
            .ours
            .iter()
            .filter(|unit| unit.kind.is_tower() && unit.alive())
            .collect();
        out.sort_by_key(|unit| (unit.pos.x, unit.pos.y));
        out
    }
    pub fn walls(&self) -> Vec<&Unit> {
        self.ours
            .iter()
            .filter(|unit| unit.kind == UnitKind::Wall && unit.alive())
            .collect()
    }
    /// The opponent's station. The request carries their whole roster
    /// (`teamEnemy.roles`) but nothing about how they are doing — their score,
    /// gold and task submissions are invisible to us; their base's HP is not,
    /// and logging it per round is what lets a captured match say which day a
    /// base fell without needing the judger's own match record.
    pub fn enemy_station(&self) -> Option<&Unit> {
        self.enemy
            .iter()
            .find(|unit| unit.kind == UnitKind::Station)
    }
    pub fn enemy_walls(&self) -> Vec<&Unit> {
        self.enemy
            .iter()
            .filter(|unit| unit.kind == UnitKind::Wall && unit.alive())
            .collect()
    }
    /// The opponent's guns, in a stable order. Their gold, score and task
    /// submissions are invisible to us, but their ARMED FORCES are in every
    /// request, and `enemy_walls()` was the only half of that we ever logged.
    /// Logging the other half is what makes "the opponent builds weapons first"
    /// answerable from our own match record instead of from a report about a
    /// report (WORKFLOW_REQUEST §13).
    pub fn enemy_towers(&self) -> Vec<&Unit> {
        let mut towers: Vec<&Unit> = self
            .enemy
            .iter()
            .filter(|unit| unit.kind.is_tower() && unit.alive())
            .collect();
        towers.sort_by_key(|unit| (unit.pos.x, unit.pos.y));
        towers
    }
    pub fn role_by_id(&self, id: i64) -> Option<&Unit> {
        self.ours.iter().find(|unit| unit.id == id)
    }

    pub fn zone_positions(&self, kind: &str) -> Vec<Pos> {
        self.zones
            .iter()
            .filter(|(_pos, value)| value.as_str() == kind)
            .map(|(pos, _value)| *pos)
            .collect()
    }

    pub fn all_mines(&self) -> Vec<(Pos, String)> {
        self.zones
            .iter()
            .filter(|(_pos, kind)| ORES.contains(&kind.as_str()))
            .map(|(pos, kind)| (*pos, kind.clone()))
            .collect()
    }

    pub fn vendors(&self) -> Vec<Pos> {
        self.zone_positions("vendor")
    }

    pub fn weapon_shops(&self) -> Vec<Pos> {
        self.zone_positions("weaponShop")
    }

    pub fn in_bounds(&self, pos: Pos) -> bool {
        pos.x >= 0 && pos.x < self.width && pos.y >= 0 && pos.y < self.height
    }

    /// Cell is plain walkable/buildable land (no neutral zone element).
    pub fn is_land(&self, pos: Pos) -> bool {
        self.in_bounds(pos) && !self.zones.contains_key(&pos)
    }

    /// Cells a moving unit cannot enter this round.
    pub fn blocked_for(&self, moving_id: i64) -> HashSet<Pos> {
        let mut cells: HashSet<Pos> = self.zones.keys().copied().collect();
        for unit in &self.ours {
            if unit.id == moving_id {
                continue;
            }
            if unit.kind == UnitKind::Station || unit.alive() {
                cells.extend(unit.footprint());
            }
        }
        for unit in &self.enemy {
            cells.extend(unit.footprint());
        }
        for robot in &self.robots {
            cells.insert(robot.pos);
        }
        cells
    }
}
