use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

pub mod data;
pub mod information;
pub mod simulator;
pub mod state;
#[cfg(test)]
mod tests;
pub mod user;

pub static VERBOSITY: OnceLock<u8> = OnceLock::new();
pub static SHARED_MULTIHIT_DAMAGE_ROLLS: AtomicBool = AtomicBool::new(false);
