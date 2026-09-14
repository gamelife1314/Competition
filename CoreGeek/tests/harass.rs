//! P2-3: summon-order rhythm — a boss wave is a half-ender against a
//! wall-less enemy base. The rich gate (500 gold + three towers) is
//! untouched; the finisher trigger arms only while the enemy ring is thin
//! (≤3 walls, globally visible) and keeps two orders in stock so the
//! pressure runs across nights.

use serde_json::{json, Value};

use coregeek::brain::economy::intent_list;
use coregeek::model::Turn;
use coregeek::protocol::Request;
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

/// Day-2 board: three towers, `gold`, a worker carrying `orders` boss
/// orders, and an enemy station ringed by `enemy_walls` walls.
fn board(gold: i64, orders: usize, enemy_walls: usize) -> Value {
    let mut enemy = vec![json!({
        "id": 20013, "pos": {"x": 30, "y": 6}, "roleType": "station",
        "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []
    })];
    for index in 0..enemy_walls {
        enemy.push(json!({
            "id": 41000 + index as i64, "pos": {"x": 28, "y": 4 + index as i32},
            "roleType": "wall", "health": 1000, "level": 1,
            "backPackCapability": 0, "backpack": []
        }));
    }
    json!({
        "roundNo": 140,
        "mapInfo": {"width": 41, "height": 32, "zones": [
            {"pos": {"x": 28, "y": 20}, "neutralType": "weaponShop"}
        ]},
        "teamOur": {
            "type": "challenger", "goldNum": gold, "totalScore": 0,
            "playerTasks": [],
            "roles": [
                {"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                 "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []},
                {"id": 10020, "pos": {"x": 10, "y": 22}, "roleType": "gatling",
                 "health": 1000, "attackPower": 10, "attackRange": 3, "level": 1,
                 "backPackCapability": 0, "backpack": []},
                {"id": 10030, "pos": {"x": 11, "y": 22}, "roleType": "railgun",
                 "health": 1000, "attackPower": 10, "attackRange": 6, "level": 1,
                 "backPackCapability": 0, "backpack": []},
                {"id": 10040, "pos": {"x": 12, "y": 22}, "roleType": "rocket",
                 "health": 1000, "attackPower": 20, "attackRange": 10, "level": 1,
                 "backPackCapability": 0, "backpack": []},
                {"id": 10002, "pos": {"x": 13, "y": 24}, "roleType": "worker",
                 "health": 220, "attackPower": 0, "attackRange": 0,
                 "backPackCapability": 100,
                 "backpack": vec!["BossRobotSummonOrder"; orders]},
            ],
        },
        "teamEnemy": {"roles": enemy},
        "robot": {"roles": []},
        "weaponShopList": [{"name": "BossRobotSummonOrder", "price": 200}],
    })
}

fn has_boss_order(needs: &[coregeek::brain::economy::Need]) -> bool {
    needs.iter().any(|need| need.name == "BossRobotSummonOrder")
}

#[test]
fn a_wall_less_enemy_arms_the_finisher_from_300_gold() {
    let turn = turn_from(board(300, 0, 2));
    let needs = intent_list(&turn, &BotState::default(), 0);
    assert!(
        has_boss_order(&needs),
        "two enemy walls: the boss wave can end the half"
    );
}

#[test]
fn a_walled_enemy_keeps_the_old_500_gold_gate() {
    // Ten walls: the finisher stays off, and 300 gold never bought harassment.
    let turn = turn_from(board(300, 0, 10));
    assert!(!has_boss_order(&intent_list(&turn, &BotState::default(), 0)));
    // The rich gate is untouched: 500 gold and three towers still buy it.
    let turn = turn_from(board(500, 0, 10));
    assert!(has_boss_order(&intent_list(&turn, &BotState::default(), 0)));
}

#[test]
fn the_stock_caps_at_two_so_the_pressure_is_a_rhythm() {
    let turn = turn_from(board(300, 2, 2));
    assert!(
        !has_boss_order(&intent_list(&turn, &BotState::default(), 0)),
        "two already in stock: tonight's pressure is paid for"
    );
    let turn = turn_from(board(300, 1, 2));
    assert!(
        has_boss_order(&intent_list(&turn, &BotState::default(), 0)),
        "one in stock: top up for tomorrow night"
    );
}

// ---------------------------------------------------------------------------
// P2-3 压制窗口: the boss wave armed by a stronger reason than "we are rich".
//
// The two gates above answer "can we afford a wave". This one answers "is the
// wave aimed at something that cannot absorb it" — their base is nearly down
// (the win condition and the score2 amplifier are the same event) or their
// towers stand inside one blast radius.
// ---------------------------------------------------------------------------

use coregeek::brain::economy::{boss_suppression_window, ENEMY_STATION_LOW_HP, ENEMY_TOWER_CLUSTER};

/// The enemy station is `enemy`'s first role; set its HP.
fn with_enemy_station_hp(payload: &mut Value, hp: i64) {
    payload["teamEnemy"]["roles"][0]["health"] = json!(hp);
}

/// Append enemy towers at `towers` to `teamEnemy`.
fn with_enemy_towers(payload: &mut Value, towers: &[(i32, i32)]) {
    for (index, (x, y)) in towers.iter().enumerate() {
        payload["teamEnemy"]["roles"]
            .as_array_mut()
            .expect("roles is an array")
            .push(json!({
                "id": 42000 + index as i64, "pos": {"x": x, "y": y},
                "roleType": "gatling", "health": 1000, "attackPower": 10,
                "attackRange": 3, "level": 1, "backPackCapability": 0, "backpack": []
            }));
    }
}

/// Nothing of the enemy is visible.
fn with_hidden_enemy(payload: &mut Value) {
    payload["teamEnemy"]["roles"] = json!([]);
}

#[test]
fn a_low_enemy_base_opens_the_suppression_window() {
    // A base is 1500 HP at level 1. The window opens on the last 40% — the
    // point at which a boss wave is a finisher rather than harassment.
    let mut payload = board(500, 0, 10);
    with_enemy_station_hp(&mut payload, ENEMY_STATION_LOW_HP + 1);
    let high = turn_from(payload.clone());
    assert!(!boss_suppression_window(&high), "1201 HP is not a finisher");

    with_enemy_station_hp(&mut payload, ENEMY_STATION_LOW_HP);
    let low = turn_from(payload.clone());
    assert!(boss_suppression_window(&low), "the threshold is inclusive");

    with_enemy_station_hp(&mut payload, 100);
    let dying = turn_from(payload);
    assert!(boss_suppression_window(&dying));
}

#[test]
fn a_hidden_enemy_base_is_not_a_low_one() {
    // `enemy_station()` returning None means OUT OF VISION, not destroyed — a
    // base we cannot see must never read as a base about to fall, or the wave
    // is bought on a blank map. Their towers are globally visible (接口文档
    // 1.4), which is why the tower trigger is the one that carries a hidden
    // base.
    let mut payload = board(500, 0, 10);
    with_hidden_enemy(&mut payload);
    let turn = turn_from(payload);
    assert!(turn.enemy_station().is_none());
    assert!(
        !boss_suppression_window(&turn),
        "an unseen base fails closed"
    );
}

#[test]
fn enemy_towers_in_one_blast_radius_open_the_window() {
    // A BOSS wave is area damage. Against towers packed inside one blast it is
    // the only order that pays for itself twice — and their base is at full HP,
    // so only the tower trigger can fire here.
    let mut payload = board(500, 0, 10);
    with_enemy_station_hp(&mut payload, 1500);
    with_enemy_towers(&mut payload, &[(28, 8), (28, 8 + ENEMY_TOWER_CLUSTER)]);
    let clustered = turn_from(payload.clone());
    assert!(
        boss_suppression_window(&clustered),
        "two towers {} apart are one blast radius",
        ENEMY_TOWER_CLUSTER
    );

    // One cell further apart is a different story: the wave hits one tower and
    // the gold was better spent on the defence. Built on a fresh board — the
    // clustered pair above would still be standing on this one.
    let mut payload = board(500, 0, 10);
    with_enemy_station_hp(&mut payload, 1500);
    with_enemy_towers(&mut payload, &[(28, 8), (28, 9 + ENEMY_TOWER_CLUSTER)]);
    let spread = turn_from(payload);
    assert!(
        !boss_suppression_window(&spread),
        "a spread-out ring is not a cluster"
    );
}

#[test]
fn the_suppression_window_relaxes_the_once_a_day_gate() {
    // Ten enemy walls keep the thin-ring finisher off, so the only boss order
    // that can appear on this board is the harassment one — and the day's one
    // wave has already been fired.
    let mut payload = board(500, 0, 10);
    let mut state = BotState::default();
    state.harass_done_today = true;

    let turn = turn_from(payload.clone());
    assert!(!boss_suppression_window(&turn));
    assert!(
        !has_boss_order(&intent_list(&turn, &state, 0)),
        "the day's one wave is spent"
    );

    // The enemy base drops into the window. A small order fired at dawn must
    // not use up the day's chance at the wave the evening just opened — so the
    // same board buys it now, and the need says why.
    with_enemy_station_hp(&mut payload, 400);
    let turn = turn_from(payload);
    let needs = intent_list(&turn, &state, 0);
    let need = needs
        .iter()
        .find(|need| need.name == "BossRobotSummonOrder")
        .expect("the window buys the wave even though the day's is spent");
    assert_eq!(need.reason, "boss_suppression");
    assert_eq!(
        need.priority, 7,
        "the window outranks the ordinary harass order it replaces"
    );
}

#[test]
fn the_window_never_loosens_the_rich_gate_or_the_three_towers() {
    // The window changes WHEN the wave may be bought, never WHETHER we can
    // afford it. `gold` is already net of the build reserve, so a cheap wave
    // here can never divert a coin the defence is holding.
    let mut payload = board(499, 0, 10);
    with_enemy_station_hp(&mut payload, 200);
    let turn = turn_from(payload.clone());
    assert!(boss_suppression_window(&turn), "the window is open");
    assert!(
        !has_boss_order(&intent_list(&turn, &BotState::default(), 0)),
        "499 gold is still 499 gold"
    );

    // And a summons we cannot defend behind is a wave paid for twice: two
    // towers is not enough, however low their base is.
    let mut payload = board(500, 0, 10);
    with_enemy_station_hp(&mut payload, 200);
    payload["teamOur"]["roles"]
        .as_array_mut()
        .expect("roles is an array")
        .retain(|role| role["roleType"] != "rocket");
    let turn = turn_from(payload);
    assert_eq!(turn.towers().len(), 2);
    assert!(
        !has_boss_order(&intent_list(&turn, &BotState::default(), 0)),
        "two towers: the wave would be answered with our own base open"
    );
}
