//! Mine: the shared stone-demand bound (issue #221 phase 4b).
//!
//! The shared executor (`mine_flow`) died with the legacy `worker_day`
//! dispatch: each worker mainline now latches its own single vein and digs
//! it out — comment 1 §5's 每个矿只能采集10次, 尽可能一次性将单个矿采集完.
//! [`crate::brain::role::wall_worker`] mines the day's stone quota on
//! `wall_vein_latch`; [`crate::brain::role::economy_worker`] mines its ROI
//! loop on `vein_latch`. What the two still share is the bound that says
//! when stone stops being wanted at all.

use crate::brain::economy;
use crate::model::{Turn, STONE};

/// Does the team already carry the stone the day owes the ring?
///
/// THE one bound on the stone-first rule. `stone_demand` is what the day
/// still owes — the gaps, capped at the day's fortification budget — and
/// stone is dug while the TEAM is short of that number, not one fetch
/// longer. The bound is what keeps "dig stone first" from becoming "dig
/// stone all day": the wall worker's quota latch releases on it and its
/// surplus pick refuses stone past it, and a team with the ring's stone in
/// its packs is a team whose workers belong at the ore. Two workers
/// splitting a 20-cell ring hold 10 each and neither pack ever reaches a
/// batch, so the test is the TEAM's stone and never one pack's.
pub(crate) fn stone_covered(turn: &Turn, stone_demand: i64) -> bool {
    stone_demand <= 0 || economy::team_ores(turn, STONE) >= stone_demand
}
