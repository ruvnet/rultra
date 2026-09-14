//! Backends: where readings come from.
//!
//! The mock backend is the default so that tests, CI and development on a
//! workstation all work with no hardware attached. Hardware support is behind
//! the `hardware` feature and only compiles on Linux.

pub mod cache;
pub mod mock;

#[cfg(all(target_os = "linux", feature = "hardware"))]
pub mod linux;
