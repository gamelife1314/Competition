//! Build-time configuration. The internal workflow cannot set environment
//! variables, so anything the owner wants to change is a constant here: edit,
//! repack, ship.
//!
//! Nothing in this module reads the board, the clock or the purse — it is the
//! one file a human edits between matches, and every reader of it is a planner
//! that would otherwise hard-code the same decision at the call site.

/// Tower build line. Slot i is built when tower slot i (0-based, absolute among
/// all `TOWER_CAP` slots) comes up — the i-th entry is always built at the i-th
/// tower position, regardless of what is already standing.
///
/// Slots are positional, NOT per-kind — repeats are allowed and meaningful
/// (`["rocket", "rocket", "railgun"]` is two rockets then a railgun). The index
/// is ABSOLUTE: slot N of `TOWER_CAP` always reads `TOWER_BUILD_ORDER[N]`, so a
/// three-entry line builds all three entries in order as the three slots fill.
///
/// A line shorter than the empty-slot count simply builds fewer towers — that is
/// intended, not an error. See `brain::day::tower_gaps` for the site selection
/// that consumes this.
///
/// Battle analysis (pk616181/pk616182, 2026-09-17): opponents that build 0
/// towers and rush our base with 70 robots destroy it on night 1 (1500→0 HP).
/// Two towers (rocket 3-round cooldown + railgun single-target) cannot kill
/// robots fast enough. Adding a gatling (fires every round, 90° cone, level =
/// bullets) as the third tower gives close-range swarm defense — the exact
/// range band (3 cells) robots occupy when they reach our wall ring. The rocket
/// stays first for long-range early picks; the railgun stays second for
/// medium-range pierce; the gatling rounds out the coverage at the wall line.
pub const TOWER_BUILD_ORDER: &[&str] = &["rocket", "railgun", "gatling"];

/// Hard cap on towers (game rule: 3 total). Never build past it, whatever
/// [`TOWER_BUILD_ORDER`] says.
pub const TOWER_CAP: usize = 3;
