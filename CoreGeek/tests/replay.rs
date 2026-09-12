//! End-to-end replays of the sample request from the interface doc.
//! Kept as ONE test function: the bot has process-global cross-round state,
//! so scenarios must run in a deterministic sequence.

use serde_json::{json, Value};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/request.txt");

fn load_fixture() -> Value {
    let raw = std::fs::read_to_string(FIXTURE).expect("fixture readable");
    serde_json::from_str(&raw).expect("fixture is valid JSON")
}

fn post(payload: &Value) -> Value {
    let body = serde_json::to_vec(payload).unwrap();
    let out = coregeek::brain::respond(&body);
    serde_json::from_str(&out).expect("response is valid JSON")
}

fn set_round(payload: &mut Value, round: i64) {
    payload["roundNo"] = json!(round);
}

#[test]
fn replay_scenarios() {
    // --- 1. Night round (85 → in-day round 84 ≥ 70): man the towers. ---
    let mut night = load_fixture();
    set_round(&mut night, 85);
    let resp = post(&night);
    let map = &resp["roleCommandMap"];
    assert!(resp["prompt"].as_str() == Some(""));
    assert!(resp["executeCmd"].as_str() == Some(""));
    // Three controllers, three towers: each should get at most one command,
    // robots are far away so all commands are moves toward the towers.
    let ids: Vec<&str> = map
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        ids.iter()
            .all(|id| ["10010", "10011", "10012"].contains(id)),
        "{ids:?}"
    );
    for (_id, cmd) in map.as_object().unwrap() {
        let action = cmd["action"].as_str().unwrap();
        assert!(
            action == "move" || action == "attack",
            "unexpected {action}"
        );
        if action == "move" {
            let t = &cmd["targetPos"][0];
            assert!(t["x"].is_number() && t["y"].is_number());
        }
    }

    // --- 2. Night round with robots in range and a worker next to the
    //        gatling: expect an attack keyed by the TOWER id. ---
    let mut fight = load_fixture();
    set_round(&mut fight, 86);
    // worker 10010 stands adjacent to gatling 10020 at (9,24)
    for role in fight["teamOur"]["roles"].as_array_mut().unwrap() {
        if role["id"] == json!(10010) {
            role["pos"] = json!({"x": 8, "y": 23});
        }
    }
    // small robot within gatling range 3 of (9,24)
    fight["robot"]["roles"].as_array_mut().unwrap()[0]["pos"] = json!({"x": 11, "y": 25});
    let resp = post(&fight);
    let attack = &resp["roleCommandMap"]["10020"];
    assert_eq!(attack["action"].as_str(), Some("attack"));
    assert_eq!(attack["controllerId"].as_str(), Some("10010"));
    // gatling level1 → exactly one target position, the robot cell
    assert_eq!(attack["targetPos"].as_array().unwrap().len(), 1);
    assert_eq!(attack["targetPos"][0]["x"].as_i64(), Some(11));
    assert_eq!(attack["targetPos"][0]["y"].as_i64(), Some(25));

    // --- 3. Day round (day 2 morning): economy commands. ---
    let mut day = load_fixture();
    set_round(&mut day, 135); // day2, in-day round 4 → daytime
    let resp = post(&day);
    let map = resp["roleCommandMap"].as_object().unwrap();
    assert!(!map.is_empty(), "day round must issue commands");
    for (id, cmd) in map {
        let action = cmd["action"].as_str().unwrap();
        assert!(
            [
                "move",
                "collect",
                "build",
                "sell",
                "buy",
                "use",
                "acceptTask",
                "submitAnswer",
                "summonTreasure",
            ]
            .contains(&action),
            "role {id}: illegal daytime action {action}"
        );
        assert!(action != "attack", "attack is illegal during day");
    }

    // --- 4. Garbage input must never crash nor return malformed JSON. ---
    let out = coregeek::brain::respond(b"{");
    assert_eq!(out, r#"{"roleCommandMap":{},"prompt":"","executeCmd":""}"#);
    let out = coregeek::brain::respond(b"[]");
    assert!(out.contains("roleCommandMap"));
    let out = coregeek::brain::respond(b"");
    assert!(out.contains("roleCommandMap"));
}

#[test]
fn response_schema_matches_doc() {
    // A minimal but complete request; ensures optional fields serialize away.
    let payload = json!({
        "roundNo": 1,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "teamId": "1", "teamName": "t",
            "goldNum": 0, "totalScore": 0, "playerTasks": [],
            "roles": [{
                "id": 10010, "pos": {"x": 1, "y": 1}, "roleType": "worker",
                "health": 220, "attackPower": 0, "attackRange": 0,
                "backPackCapability": 100, "backpack": []
            }]
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
    });
    let resp = post(&payload);
    assert_eq!(resp["prompt"], json!(""));
    assert_eq!(resp["executeCmd"], json!(""));
    let map = resp["roleCommandMap"].as_object().unwrap();
    for cmd in map.values() {
        let obj = cmd.as_object().unwrap();
        assert!(obj.contains_key("action"));
        // No null-valued optional fields may leak into the response.
        for (key, value) in obj {
            assert!(!value.is_null(), "field {key} serialized as null");
        }
    }
}
