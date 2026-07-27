use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

pub mod benchmarking;
pub mod data;
pub mod information;
pub mod meta;
pub mod simulator;
pub mod state;
#[cfg(test)]
mod tests;
pub mod user;

pub static VERBOSITY: OnceLock<u8> = OnceLock::new();
pub static SHARED_MULTIHIT_DAMAGE_ROLLS: AtomicBool = AtomicBool::new(false);
