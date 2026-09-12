//! Shared helpers for reading the box's real state.

use rultra_evolve::policy::Applier;
use rultra_sense::{Backend, DeviceId, Value};

/// Where the cycle keeps its policy and chain. Mirrors the `rultra` binary so
/// the console and the CLI cannot disagree about what is in force.
pub fn state_dir() -> std::path::PathBuf {
    std::env::var("RULTRA_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/rultra"))
}

pub fn applier() -> Applier {
    Applier::new(state_dir().join("policy.json"))
}

pub fn backend() -> Box<dyn Backend> {
    #[cfg(all(target_os = "linux", feature = "hardware"))]
    {
        if let Ok(b) = rultra_sense::backend::linux::LinuxBackend::open() {
            return Box::new(b);
        }
    }
    Box::new(rultra_sense::backend::mock::MockBackend::crowpi())
}

/// Read one scalar, or `None` if the device is absent or not a scalar.
pub fn scalar(b: &mut dyn Backend, id: DeviceId) -> Option<f64> {
    match b.read(id).ok()?.value {
        Value::Scalar { n, .. } => Some(n),
        _ => None,
    }
}
