//! The sandbox prelude (issues #126-#130).
//!
//! `表 4d` counts the commands that came back with no `ANSWER:` line at all,
//! and across the five matches it is the single largest block of wasted rounds:
//! `8 exit_nonzero` + `6 no_answer_marker` in 126, `5 + 4` in 128, `7 + 3` in
//! 129, `2 + 7` in 127, `3` in 130. A session has a 24-46 round budget and each
//! failed command costs about four of them (prompt → answer → command →
//! verdict), so three failures is a session: every one of the batch's 27
//! sessions ended `success: false`.
//!
//! The named causes in `表 4c` are environment, not arithmetic:
//!
//! ```text
//! 127 r18, r173   /bin/bash: ./check: /bin/sh^M: bad interpreter: No such file or directory
//! 127 r39         'ascii' codec can't encode characters in position 33-34: ordinal not in range(128)
//! ```
//!
//! The task files are authored on Windows; the sandbox is Linux. A `check`
//! script whose shebang ends in CR cannot be executed by any interpreter, and a
//! Python that defaults to ASCII cannot print the Chinese field names the tasks
//! ask for. Neither is anything the model should be spending a round on, and
//! the prompt has told it to fix the first since v14 (rule 12) without the
//! round ever coming back. So the fix is deterministic: a fixed block prepended
//! to every command, asserting the three properties that matter — it is
//! silent, it cannot fail the script, and it does not move the working
//! directory.

use serde_json::json;

use coregeek::brain::decide_with;
use coregeek::brain::task::{sandbox_command, sandbox_prelude};
use coregeek::state::{BotState, TaskStage};

#[test]
fn the_prelude_goes_on_the_wire_in_front_of_the_script() {
    let script = "find /tmp/selfEvolutionTask -maxdepth 4\n";
    let sent = sandbox_command(script);
    assert!(
        sent.starts_with(sandbox_prelude()),
        "the environment is set up before the model's script runs"
    );
    assert!(
        sent.ends_with(script),
        "and the model's script is untouched at the end: {sent:?}"
    );
    assert_eq!(sent.len(), sandbox_prelude().len() + script.len());
}

#[test]
fn the_prelude_fixes_the_two_named_causes() {
    let prelude = sandbox_prelude();
    // 127 r18 and r173: `/bin/sh^M: bad interpreter`.
    assert!(
        prelude.contains("check") && prelude.contains("sed -i 's/\\r$//'"),
        "the task's own scripts get their CR stripped before anything runs"
    );
    // 127 r39: `'ascii' codec can't encode characters`.
    for setting in ["PYTHONIOENCODING=utf-8", "PYTHONUTF8=1", "LC_ALL=C.UTF-8"] {
        assert!(prelude.contains(setting), "a UTF-8 sandbox needs {setting}");
    }
}

#[test]
fn the_prelude_can_never_be_what_fails_the_command() {
    let prelude = sandbox_prelude();
    // Rule 1: it prints nothing, so `ANSWER:`/`FIELDS:`/`SCHEMA:` scanning is
    // exactly as it was. A stray `echo` here would be read as the answer.
    for line in prelude.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        assert!(
            !line.contains("echo ") && !line.starts_with("echo"),
            "the prelude must not print: {line:?}"
        );
    }
    // Rule 2: no `set -e`, and every statement swallows its own failure — a
    // sandbox without `find`/`sed`/`chmod` still runs the model's script.
    assert!(
        !prelude.contains("set -e"),
        "`set -e` is the exact defect prompt rule 10 warns the model about"
    );
    for line in prelude.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("export ") {
            continue;
        }
        assert!(
            line.ends_with("|| true"),
            "every prelude statement must be non-fatal: {line:?}"
        );
    }
    // Rule 3: the working directory is not moved. A relative path the model was
    // told to verify has to resolve the way the model expects — the batch's
    // `cd: .../ws_1: No such file or directory` followed by
    // `mkdir: cannot create directory 'logs': Permission denied` is what a
    // redirected CWD looks like when it goes wrong.
    assert!(
        !prelude.contains("cd "),
        "the prelude never changes directory"
    );
}

#[test]
fn the_prelude_is_actually_on_the_execute_cmd_we_send() {
    // The helper above can be right while the wiring is not: `Plan::execute_cmd`
    // is composed in one place in `decide_with`, and a prelude that never leaves
    // the process is worth nothing. Drive a real round through the entry point
    // the server uses, with a session holding a script, and read the bytes back
    // off the response.
    let mut state = BotState::default();
    state.task.active = true;
    state.task.session_id = 1;
    state.task.timeout_round = 999;
    // The description has already landed, so `absorb_task_events` does not read
    // this board's `phaseTask` as a fresh session starting and reset the stage.
    state.task.description = "请阅读task_1_alpha.md，获取任务信息".into();
    state.task.description_round = 4;
    state.task.stage = TaskStage::HavePlan {
        cmd: "echo hello\n".into(),
    };

    let body = serde_json::to_vec(&json!({
        "roundNo": 5,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 75, "totalScore": 0,
            "playerTasks": [],
            "roles": [
                {"id": 10001, "pos": {"x": 10, "y": 24}, "roleType": "station",
                 "health": 1500, "level": 1, "backPackCapability": 0, "backpack": []},
                {"id": 10011, "pos": {"x": 10, "y": 23}, "roleType": "pioneer",
                 "health": 200, "attackPower": 0, "attackRange": 0,
                 "level": 1, "backPackCapability": 100, "backpack": []},
            ],
        },
        "teamEnemy": {"roles": []},
        "robot": {"roles": []},
        "phaseTask": "请阅读task_1_alpha.md，获取任务信息",
    }))
    .expect("payload serialises");

    let out = decide_with(&mut state, &body).expect("round 5 decides");
    let resp: serde_json::Value = serde_json::from_str(&out).expect("response is JSON");
    let sent = resp["executeCmd"].as_str().expect("a command goes out");
    assert!(
        sent.starts_with(sandbox_prelude()),
        "the environment block leads every command on the wire: {sent:?}"
    );
    assert!(
        sent.ends_with("echo hello\n"),
        "and the model's script is what follows it: {sent:?}"
    );
}
