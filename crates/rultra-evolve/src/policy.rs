//! The mutable policy, and the applier that puts it on a real box.
//!
//! This is seam 5 from ADR-0003. autogenous models canary and rollback over an
//! abstract content-addressed artifact with a health check; it has no opinion
//! about what "roll back" means for a running sensor loop. That translation
//! lives here.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Never sample faster than this, whatever the thermals suggest.
pub const MIN_POLL_MS: u64 = 100;
/// Never sample slower than this, or the box stops being a sensor.
pub const MAX_POLL_MS: u64 = 60_000;
/// Above this die temperature the box is considered thermally stressed.
/// Chosen below the Pi 5's throttle point so the policy reacts *before* the
/// firmware does — this board has already recorded `get_throttled` bit 19.
pub const THERMAL_CEILING_C: f64 = 75.0;
/// Below this, there is thermal headroom to spend on sampling more often.
pub const THERMAL_COMFORT_C: f64 = 55.0;

/// The configuration the box is allowed to change about itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensePolicy {
    /// Milliseconds between sampling rounds.
    pub poll_interval_ms: u64,
}

impl Default for SensePolicy {
    fn default() -> Self {
        Self {
            poll_interval_ms: 1000,
        }
    }
}

/// What the box measured over one evaluation window.
#[derive(Debug, Clone, Default)]
pub struct Observation {
    /// Mean die temperature over the window, Celsius.
    pub die_temp_c: f64,
    /// Fraction of reads that failed or returned implausible values.
    pub read_error_rate: f64,
    /// How many samples the window covered. A tiny window is not evidence.
    pub samples: u32,
}

impl Observation {
    /// Is the box running hot enough that sampling should back off?
    pub fn thermally_stressed(&self) -> bool {
        self.die_temp_c >= THERMAL_CEILING_C
    }
}

impl SensePolicy {
    /// Content hash of the canonical encoding, used as the genome hash.
    ///
    /// Canonical means the JSON encoding of this struct, whose field order is
    /// fixed by the type. Two boxes with the same policy therefore derive the
    /// same hash, which is what makes a rollback target meaningful across them.
    pub fn content_hash(&self) -> String {
        let canonical = serde_json::to_string(self).unwrap_or_default();
        let mut h = Sha256::new();
        h.update(canonical.as_bytes());
        format!("{:x}", h.finalize())
    }

    /// Propose an adjusted policy, or `None` when the observation does not
    /// justify a change.
    ///
    /// Returning `None` is the common and desirable case. A box that mutates
    /// on every window is not optimizing, it is oscillating.
    pub fn adjust_for(&self, obs: &Observation) -> Option<Self> {
        // Too few samples is not evidence. Refusing to act on a thin window is
        // the same discipline as requiring a confidence interval in scoring.
        if obs.samples < 10 {
            return None;
        }
        let next = if obs.thermally_stressed() {
            // Back off multiplicatively: thermal problems compound, so the
            // response should too.
            (self.poll_interval_ms * 2).min(MAX_POLL_MS)
        } else if obs.die_temp_c <= THERMAL_COMFORT_C && obs.read_error_rate < 0.01 {
            // Approach additively. Fast to retreat, slow to advance — the
            // standard shape for a controller that must not oscillate.
            self.poll_interval_ms.saturating_sub(100).max(MIN_POLL_MS)
        } else {
            return None;
        };
        if next == self.poll_interval_ms {
            return None;
        }
        Some(Self {
            poll_interval_ms: next,
        })
    }
}

/// Reads and writes the policy on disk, so a promotion survives a restart and
/// a rollback is a real operation rather than an in-memory undo.
pub struct Applier {
    path: std::path::PathBuf,
}

impl Applier {
    /// Point the applier at a policy file.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Load the active policy, falling back to the default when absent.
    pub fn load(&self) -> SensePolicy {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Write a policy, returning the previous one so the caller holds a real
    /// rollback value rather than trusting a hash alone.
    pub fn apply(&self, next: &SensePolicy) -> anyhow::Result<SensePolicy> {
        let previous = self.load();
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write-then-rename: a power cut mid-write must not leave a truncated
        // policy file on an unattended box.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(next)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(previous)
    }

    /// Restore a policy and confirm it actually landed.
    ///
    /// autogenous's contract is a *verified* rollback — the receipt asserts the
    /// artifact was restored, not merely that a restore was attempted. So this
    /// reads the file back and compares content hashes before returning `Ok`.
    pub fn rollback(&self, to: &SensePolicy) -> anyhow::Result<()> {
        self.apply(to)?;
        let observed = self.load();
        if observed.content_hash() != to.content_hash() {
            anyhow::bail!(
                "rollback did not take effect: wanted {}, found {}",
                to.content_hash(),
                observed.content_hash()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(t: f64) -> Observation {
        Observation {
            die_temp_c: t,
            read_error_rate: 0.0,
            samples: 100,
        }
    }

    #[test]
    fn a_thin_window_is_not_evidence() {
        let p = SensePolicy::default();
        let thin = Observation {
            die_temp_c: 85.0,
            read_error_rate: 0.0,
            samples: 3,
        };
        assert!(p.adjust_for(&thin).is_none());
    }

    #[test]
    fn backoff_is_multiplicative_and_advance_is_additive() {
        let p = SensePolicy {
            poll_interval_ms: 1000,
        };
        assert_eq!(p.adjust_for(&obs(80.0)).unwrap().poll_interval_ms, 2000);
        assert_eq!(p.adjust_for(&obs(45.0)).unwrap().poll_interval_ms, 900);
    }

    #[test]
    fn bounds_are_respected_in_both_directions() {
        let hot = SensePolicy {
            poll_interval_ms: MAX_POLL_MS,
        };
        assert!(
            hot.adjust_for(&obs(90.0)).is_none(),
            "should not exceed max"
        );
        let fast = SensePolicy {
            poll_interval_ms: MIN_POLL_MS,
        };
        assert!(
            fast.adjust_for(&obs(40.0)).is_none(),
            "should not go below min"
        );
    }

    #[test]
    fn errors_block_speeding_up_even_when_cool() {
        let p = SensePolicy::default();
        let noisy = Observation {
            die_temp_c: 40.0,
            read_error_rate: 0.2,
            samples: 100,
        };
        assert!(p.adjust_for(&noisy).is_none());
    }

    #[test]
    fn content_hash_is_stable_and_distinguishes_policies() {
        let a = SensePolicy {
            poll_interval_ms: 1000,
        };
        let b = SensePolicy {
            poll_interval_ms: 2000,
        };
        assert_eq!(a.content_hash(), a.clone().content_hash());
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn apply_returns_the_previous_policy_and_rollback_verifies() {
        let dir = std::env::temp_dir().join(format!("rultra-test-{}", std::process::id()));
        let path = dir.join("policy.json");
        let app = Applier::new(&path);

        let first = SensePolicy {
            poll_interval_ms: 1000,
        };
        app.apply(&first).unwrap();

        let second = SensePolicy {
            poll_interval_ms: 4000,
        };
        let previous = app.apply(&second).unwrap();
        assert_eq!(
            previous, first,
            "apply must hand back a real rollback value"
        );
        assert_eq!(app.load(), second);

        app.rollback(&previous).unwrap();
        assert_eq!(app.load(), first, "rollback must actually land on disk");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
