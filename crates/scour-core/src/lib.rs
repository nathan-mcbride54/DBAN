//! scour-core — the sanitization engine behind Scour.
//!
//! This crate is deliberately UI-free. Everything destructive funnels through
//! [`engine::spawn_wipe`], which cannot be called without an [`safety::ArmToken`],
//! and a token can only be minted by walking the full [`safety::SafetyGate`]
//! state machine (typed confirmation phrase + abortable countdown).

pub mod algorithm;
pub mod buffer;
pub mod demo;
pub mod device;
pub mod engine;
pub mod prng;
pub mod report;
pub mod safety;
pub mod sysinfo;

#[cfg(target_os = "linux")]
pub mod linux;

mod error;
pub use error::CoreError;

/// Crate version, surfaced in the UI and in erasure reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
