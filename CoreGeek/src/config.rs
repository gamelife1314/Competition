//! Build-time configuration. The internal workflow cannot set environment
//! variables, so anything the owner wants to change is a constant here: edit,
//! repack, ship.
//!
//! Nothing in this module reads the board, the clock or the purse — it is the
//! one file a human edits between matches, and every reader of it is a planner
//! that would otherwise hard-code the same decision at the call site.

/// Tower build line. Slot i is built when the i-th empty tower slot comes up.
///
/// Slots are positional, NOT per-kind — repeats are allowed and meaningful
/// (`["rocket", "rocket", "railgun"]` is two rockets then a railgun). The index
/// is also RELATIVE, not absolute: the empty slots are `TOWER_CAP` minus the
/// towers standing right now, and slot `i` of THAT list is built as
/// `TOWER_BUILD_ORDER[i]`. So the line is re-read from the top every time a slot
/// frees up, and that is what lets a two-entry line express the owner's tactic
/// (issue #206 §5, "2 导弹 / 全导弹"): day 1 raises a rocket and a railgun, and
/// the next slot to free reads the line from the start again and raises a second
/// rocket — two missiles and a railgun, never a gatling, without spelling three
/// entries out here. Under absolute indexing the same line would have stopped at
/// two guns and could not express it at all.
///
/// A line shorter than the empty-slot count simply builds fewer towers — that is
/// intended, not an error. See `brain::day::tower_gaps` for the site selection
/// that consumes this.
pub const TOWER_BUILD_ORDER: &[&str] = &["rocket", "railgun"];

/// Hard cap on towers (game rule: 3 total). Never build past it, whatever
/// [`TOWER_BUILD_ORDER`] says.
pub const TOWER_CAP: usize = 3;
