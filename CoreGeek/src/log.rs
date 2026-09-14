//! JSONL stdout logging for post-match analysis.
//!
//! We never see a live match, so every round writes one compact `round`
//! record (state digest + issued commands + timing) and strategic events
//! (news parsing, blacklisting, task/treasure transitions, LLM traffic)
//! append their own records. Records are JSONL lines on stdout so the
//! judger's captured process output contains the full match history.
//! Logging never fails the round: every error path is ignored.
//!
//! # What a record is allowed to cost
//!
//! A whole match is 2600 rounds, and the log is handed to an analysis agent
//! that has to read it — so the budget is not "does this fit on disk", it is
//! "how much of this does anybody read". Three rules keep the record to the
//! facts that move:
//!
//! * **Nothing empty.** A `null`, `""`, `[]` or `{}` says what a missing key
//!   says, and on a quiet round they are the majority of the line. See
//!   [`prune`].
//! * **Nothing repeated.** The per-round records are state digests, and most
//!   of the state does not move most rounds — three towers at full health, a
//!   wall line of the same length, the same controller-to-tower pairs. A block
//!   is written when it differs from the last one written, and skipped when it
//!   does not. See [`changed`].
//! * **No clock where the round number is the clock.** Line order is the
//!   timeline and `round` is already in the record; see [`event`].

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Append one JSONL record to stdout.
///
/// The envelope is `{"event":...,"data":{...}}`, and `data` has been through
/// [`prune`]. A record that already carries a `round` field does not also get
/// a wall clock: the round number is the match's own timeline, line order
/// breaks ties, and the ~2600 records of a match would spend 50 KB on
/// timestamps nothing reads. Lifecycle records — `startup`, `build_info`, the
/// coach checkpoints — have no round to anchor to and keep theirs.
///
/// `std::io::Stdout` is internally synchronized and line-buffered, so the
/// record is flushed on the trailing newline without an explicit lock.
pub fn event(name: &str, data: serde_json::Value) {
    let data = prune(data);
    let record = if data.get("round").is_some() {
        serde_json::json!({ "event": name, "data": data })
    } else {
        serde_json::json!({ "ts": now_ms(), "event": name, "data": data })
    };
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{record}");
}

/// Drop every `null`, `""`, `[]` and `{}`, at any depth.
///
/// These are the "nothing to report" values, and on a quiet round they are
/// most of the record: `errors:[]`, `failures:[]`, `lastCmdResult:""`,
/// `phaseTask:""`, the `controller` and `name` of every command that has
/// neither, and the ten zeroed fields of a `volley` that fired nothing.
///
/// `0` and `false` are deliberately **not** pruned: `gold: 0` means we are
/// broke and `noRobotDamage: false` is a claim about the round, while an empty
/// list means only that the list is empty. An array keeps its `null`s — an
/// array is positional, and dropping an element renumbers the rest.
pub fn prune(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, prune(value)))
                .filter(|(_, value)| !is_blank(value))
                .collect(),
        ),
        serde_json::Value::Array(list) => {
            serde_json::Value::Array(list.into_iter().map(prune).collect())
        }
        other => other,
    }
}

fn is_blank(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(text) => text.is_empty(),
        serde_json::Value::Array(list) => list.is_empty(),
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Has `signature` moved away from the last one written for this block?
///
/// The caller passes the block's own JSON as its signature, so "unchanged"
/// needs no hand-written comparison per block. The first call for a slot always
/// reports `true`, which is what puts the block in the match's first record.
///
/// A skipped block is not a missing fact: the last record that *did* write it
/// still describes the current state, so a reader carries it forward. The
/// record names what it re-sent in `chg`, which also marks the round as one
/// where something moved.
pub fn changed(slot: &mut Option<String>, signature: &serde_json::Value) -> bool {
    let signature = signature.to_string();
    if slot.as_deref() == Some(signature.as_str()) {
        return false;
    }
    *slot = Some(signature);
    true
}

/// The last-written signatures of the `round` record's change-gated blocks.
///
/// One slot per block that is expensive to repeat and rarely moves; the cheap
/// scalars that carry the round's story (`gold`, `score`, `scoreDelta`) are
/// written unconditionally and have no slot here — and so are the events whose
/// *repetition is the finding*, like `night_debug` counting the rounds a gun
/// spent silent.
#[derive(Debug, Default)]
pub struct LogSigs {
    pub station: Option<String>,
    pub enemy_station: Option<String>,
    pub wall: Option<String>,
    pub enemy_wall: Option<String>,
    pub towers: Option<String>,
    pub roles: Option<String>,
    pub pairs: Option<String>,
    pub task: Option<String>,
    pub treasure: Option<String>,
}

/// A position as `[x, y]`.
///
/// A two-element array is a coordinate and nothing else, so `{"x":..,"y":..}`
/// spends fourteen bytes per position restating its own type — and a round
/// with four commands and a `night_debug` line carries a dozen of them.
pub fn xy(pos: crate::protocol::Pos) -> serde_json::Value {
    serde_json::json!([pos.x, pos.y])
}

/// The record for one day's folk legend — the text the altar is inferred from.
///
/// The legend is judger-authored prose, and it is the only input to the single
/// LLM call that infers the treasure altar (`treasure.rs` sends
/// `state.treasure.legends` wholesale). Nothing else in the log carries what it
/// said, so a `treasure_plan` that comes back with the wrong sacrifice items or
/// the wrong opening day is unreadable: there is no way to tell a misread
/// legend from a wrong inference. `day` is the join key that places the text
/// beside the plan it produced.
pub fn legend_record(day: i64, text: &str) -> serde_json::Value {
    serde_json::json!({"day": day, "head": brief(text, 200)})
}

/// The record for one thing bought or sold — `buy` and `sell` share it.
///
/// They are the two directions of one ledger, and the question they exist to
/// answer is asked of the ledger, not of either entry: is the purse pinned
/// because nothing is coming in, or because everything that comes in goes
/// straight back out? That is read by joining both directions against the
/// `round.gold` series, so a row with no `round` cannot be placed on the
/// timeline and a row keyed differently from its opposite cannot be joined to
/// it. `item` is the ore, weapon or voucher the money moved for, and `gold` is
/// the purse at the moment the order went out — the same frame `round.gold`
/// uses. The event name carries the direction; the record does not repeat it.
///
/// A named function rather than inline JSON at two call sites because the
/// field list is an interface WORKFLOW_REQUEST §7.3 reads, and an interface is
/// worth a test.
pub fn ledger_record(
    turn: &crate::model::Turn,
    role: &crate::model::Unit,
    cmd: &crate::protocol::RoleCommand,
) -> serde_json::Value {
    serde_json::json!({
        "round": turn.round_no,
        "role": role.id,
        "item": cmd.name,
        "num": cmd.num,
        "gold": turn.gold,
    })
}

/// Truncate long strings (LLM traffic, task texts) before logging.
pub fn brief(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect::<String>() + "…"
    }
}

/// The first `max_chars` characters of a multi-line blob, on one line.
///
/// [`brief`] keeps the newlines, which in a JSON string are `\n` escapes —
/// two bytes each, so a captured script or a sandbox transcript spends a third
/// of its budget restating its own line structure without carrying any more of
/// the text. Runs of whitespace collapse to one space first, so the window
/// covers as much of the content as the cap allows. This is the shape a log
/// record wants when the question is "what did it actually say", which is what
/// `cmd_result.head` and `task_cmd_failed.head` are for.
pub fn headline(text: &str, max_chars: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    brief(&collapsed, max_chars)
}
