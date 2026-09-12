//! Wire protocol: request deserialization and response serialization.
//!
//! All request structs are lenient (`#[serde(default)]`) so missing or extra
//! fields never fail parsing — a parse failure would mean an empty response
//! for the whole round.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Request side
// ---------------------------------------------------------------------------

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(default)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RoleRaw {
    pub id: i64,
    pub pos: Pos,
    #[serde(rename = "roleType")]
    pub role_type: String,
    pub health: i64,
    #[serde(rename = "attackPower")]
    pub attack_power: i64,
    #[serde(rename = "attackRange")]
    pub attack_range: i64,
    #[serde(rename = "backPackCapability")]
    pub backpack_capability: i64,
    pub backpack: Vec<String>,
    pub level: i64,
    pub cooldown: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RobotRaw {
    pub id: i64,
    pub pos: Pos,
    #[serde(rename = "roleType")]
    pub role_type: String,
    pub health: i64,
    #[serde(rename = "abnormalState")]
    pub abnormal_state: String,
    #[serde(rename = "targetTeam")]
    pub target_team: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ZoneRaw {
    pub pos: Pos,
    #[serde(rename = "neutralType")]
    pub neutral_type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MapInfo {
    pub width: i32,
    pub height: i32,
    pub zones: Vec<ZoneRaw>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayerTaskRaw {
    #[serde(rename = "taskType")]
    pub task_type: String,
    #[serde(rename = "taskPosition")]
    pub task_position: Pos,
    #[serde(rename = "coldDownRounds")]
    pub cold_down_rounds: i64,
    #[serde(rename = "scoreReward")]
    pub score_reward: i64,
    #[serde(rename = "goldReward")]
    pub gold_reward: i64,
    #[serde(rename = "isValid")]
    pub is_valid: bool,
    #[serde(rename = "timeoutRounds")]
    pub timeout_rounds: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TeamOur {
    #[serde(rename = "type")]
    pub team_type: String,
    #[serde(rename = "teamId")]
    pub team_id: String,
    #[serde(rename = "teamName")]
    pub team_name: String,
    #[serde(rename = "goldNum")]
    pub gold_num: i64,
    #[serde(rename = "totalScore")]
    pub total_score: i64,
    #[serde(rename = "playerTasks")]
    pub player_tasks: Vec<PlayerTaskRaw>,
    pub roles: Vec<RoleRaw>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TeamEnemy {
    pub roles: Vec<RoleRaw>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RobotGroup {
    pub roles: Vec<RobotRaw>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorldNews {
    #[serde(rename = "officialNews")]
    pub official_news: String,
    #[serde(rename = "folkLegends")]
    pub folk_legends: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ShopItem {
    pub name: String,
    pub price: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ErrorRaw {
    #[serde(rename = "errorCode")]
    pub error_code: i64,
    pub description: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Request {
    #[serde(rename = "roundNo")]
    pub round_no: i64,
    #[serde(rename = "mapInfo")]
    pub map_info: MapInfo,
    #[serde(rename = "teamOur")]
    pub team_our: TeamOur,
    #[serde(rename = "teamEnemy")]
    pub team_enemy: TeamEnemy,
    pub robot: RobotGroup,
    #[serde(rename = "phaseTask")]
    pub phase_task: String,
    #[serde(rename = "lastRoundRoleActionResults")]
    pub last_round_role_action_results: BTreeMap<String, bool>,
    #[serde(rename = "lastSummonTreasureResult")]
    pub last_summon_treasure_result: i64,
    #[serde(rename = "llmResp")]
    pub llm_resp: String,
    #[serde(rename = "worldNews")]
    pub world_news: WorldNews,
    #[serde(rename = "lastCmdResult")]
    pub last_cmd_result: String,
    #[serde(rename = "vendorShopList")]
    pub vendor_shop_list: Vec<ShopItem>,
    #[serde(rename = "weaponShopList")]
    pub weapon_shop_list: Vec<ShopItem>,
    pub errors: Vec<ErrorRaw>,
}

// ---------------------------------------------------------------------------
// Response side
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
#[allow(non_snake_case)]
pub struct RoleCommand {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controllerId: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targetPos: Option<Vec<Pos>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taskAnswer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<Vec<String>>,
}

impl RoleCommand {
    pub fn move_to(pos: Pos) -> Self {
        Self {
            action: "move".into(),
            targetPos: Some(vec![pos]),
            ..Default::default()
        }
    }
    pub fn collect(pos: Pos) -> Self {
        Self {
            action: "collect".into(),
            targetPos: Some(vec![pos]),
            ..Default::default()
        }
    }
    pub fn build(pos: Pos, name: &str) -> Self {
        Self {
            action: "build".into(),
            targetPos: Some(vec![pos]),
            name: Some(name.into()),
            ..Default::default()
        }
    }
    #[allow(dead_code)] // wall demolition: reserved for future tactics
    pub fn remove(pos: Pos) -> Self {
        Self {
            action: "remove".into(),
            targetPos: Some(vec![pos]),
            ..Default::default()
        }
    }
    pub fn attack(controller_id: i64, targets: Vec<Pos>) -> Self {
        Self {
            action: "attack".into(),
            controllerId: Some(controller_id.to_string()),
            targetPos: Some(targets),
            ..Default::default()
        }
    }
    pub fn sell(name: &str, num: i64) -> Self {
        Self {
            action: "sell".into(),
            name: Some(name.into()),
            num: Some(num),
            ..Default::default()
        }
    }
    pub fn buy(name: &str, num: i64) -> Self {
        Self {
            action: "buy".into(),
            name: Some(name.into()),
            num: Some(num),
            ..Default::default()
        }
    }
    pub fn use_item(name: &str) -> Self {
        Self {
            action: "use".into(),
            name: Some(name.into()),
            ..Default::default()
        }
    }
    pub fn use_item_at(name: &str, pos: Pos) -> Self {
        Self {
            action: "use".into(),
            name: Some(name.into()),
            targetPos: Some(vec![pos]),
            ..Default::default()
        }
    }
    #[allow(dead_code)] // reserved for backpack management tactics
    pub fn drop_item(name: &str) -> Self {
        Self {
            action: "drop".into(),
            name: Some(name.into()),
            ..Default::default()
        }
    }
    pub fn accept_task() -> Self {
        Self {
            action: "acceptTask".into(),
            ..Default::default()
        }
    }
    pub fn submit_answer(answer: &str) -> Self {
        Self {
            action: "submitAnswer".into(),
            taskAnswer: Some(answer.into()),
            ..Default::default()
        }
    }
    pub fn summon_treasure(pos: Pos, items: Vec<String>) -> Self {
        Self {
            action: "summonTreasure".into(),
            targetPos: Some(vec![pos]),
            item: Some(items),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[allow(non_snake_case)]
pub struct Response {
    pub roleCommandMap: BTreeMap<String, RoleCommand>,
    pub prompt: String,
    pub executeCmd: String,
}
