//! Action executors shared by the three role mainlines (issue #221).
//!
//! These modules hold the "how" — how to pair a controller with a gun, how to
//! walk to a vein and dig, how to sell, how to build — while `role/` holds the
//! "what next" of each person. Extracted from `day.rs`/`night.rs` so both the
//! legacy planners (until phase 5 deletes them) and the new mainlines call the
//! same code.
pub mod base_layout;
pub mod build;
pub mod fight;
pub mod geometry;
pub mod mine;
pub mod sell;
pub mod shop;
pub mod tower_site;
