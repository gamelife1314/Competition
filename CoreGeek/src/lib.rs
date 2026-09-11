//! CoreGeek: 《未来战争》 competition bot.
//!
//! HTTP service (hyper) that receives the round state as JSON and answers
//! with per-role commands. All game logic lives in [`brain`].

pub mod brain;
pub mod log;
pub mod model;
pub mod path;
pub mod protocol;
pub mod server;
pub mod state;
pub mod validate;
