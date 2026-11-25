//! 战斗计算核心，提供独立的 Rust 接口。

pub use proc::bake_pool;
pub mod co;
pub mod counters;
pub mod fx;
pub mod heat;
pub mod mods;
pub mod proc;
pub mod pymath;
pub mod replay;
pub mod spatial;
pub mod stackable;
pub mod sustained;
pub mod tables;

#[cfg(test)]
mod tests;
