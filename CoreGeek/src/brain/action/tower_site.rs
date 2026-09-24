//! Tower siting: the three fixed L-shape weapon cells of comment 1 §1,
//! paired with the weapon kind each empty slot builds (issue #221 phase 4b).
//!
//! The legacy siting searched the ring-1 corridor by distance to the map
//! centre and proved every candidate MANABLE against the day's gate — a BFS
//! over the sealed shell, an operating-cell reservation set and a corridor-
//! stranding veto (`guns_stay_mannable`/`strands_corridor`). Comment 1 §1
//! makes all of that machinery unnecessary: the sites are FIXED cells of an L
//! around one operator stand ([`super::base_layout`]), every site is within
//! Chebyshev 1 of that stand by construction, and the stand itself is one step
//! from the permanent entrance column — so mannability is a property of the
//! layout, pinned by `base_layout`'s unit tests, not a per-candidate proof.

use std::collections::HashSet;

use super::base_layout;
use crate::model::Turn;
use crate::protocol::Pos;
use crate::state::BotState;

/// Desired tower cells — the L-shape weapon sites of comment 1 §1, in layout
/// order — paired with the weapon kind that should stand there. Cells already
/// holding one of our units, blacklisted cells and non-land are excluded.
///
/// One entry per EMPTY tower slot, in ABSOLUTE line order: the i-th empty slot
/// (0-based among empties) builds `config::TOWER_BUILD_ORDER[existing + i]`,
/// where `existing` is the number of towers already standing. Slot N of
/// `TOWER_CAP` therefore always reads `TOWER_BUILD_ORDER[N]`, whatever is
/// already standing (pk616181/pk616182: relative indexing re-read index 0 for
/// the third slot and raised a second rocket instead of the gatling — 70
/// robots then walked through two long-cooldown guns on night 1). The list is
/// at most `config::TOWER_CAP - towers standing` long and may be shorter when
/// the configured line runs out (issue #206 §5) or a site is blocked.
pub fn tower_gaps(turn: &Turn, state: &BotState) -> Vec<(Pos, String)> {
    let existing = turn.towers().len();
    if existing >= crate::config::TOWER_CAP || turn.station().is_none() {
        return Vec::new();
    }
    let empty_slots = crate::config::TOWER_CAP - existing;
    // A unit standing on a site hides it this round — the judger refuses a
    // build under anyone's footprint. Transient by nature: the site returns
    // the round the cell frees up.
    let occupied: HashSet<Pos> = turn.ours.iter().flat_map(|unit| unit.footprint()).collect();
    let mut gaps: Vec<(Pos, String)> = Vec::new();
    for site in base_layout::weapon_sites(turn) {
        if gaps.len() >= empty_slots {
            break;
        }
        if !turn.is_land(site) || occupied.contains(&site) {
            continue;
        }
        let Some(&kind) = crate::config::TOWER_BUILD_ORDER.get(existing + gaps.len()) else {
            break; // the configured line ran out before the slots did
        };
        if state
            .blacklisted_builds
            .contains(&(site, kind.to_string()))
        {
            continue;
        }
        gaps.push((site, kind.to_string()));
    }
    gaps
}
