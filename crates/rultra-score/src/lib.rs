//! Parent-versus-child scoring, producing an [`agl_types::FitnessVector`].
//!
//! # Why this exists in Rust
//!
//! MetaHarness defines the methodology this module implements — five gates plus
//! statistical promotion, where a child must beat its parent by a margin whose
//! lower 95% bootstrap confidence bound is above zero. That methodology is
//! sound and portable. Its implementation is TypeScript, and its evolve loop
//! assumes a sandbox and a git tree, which is a CI workflow rather than
//! something that runs on a Pi in the field (ADR-0003).
//!
//! So the *rules* are ported and the *code* is not linked. Nothing here shells
//! out to `npx`.
//!
//! # Why a hand-rolled PRNG
//!
//! Reproducibility is a gate, not a nicety: a promotion decision that cannot be
//! replayed is not auditable. A small seeded xorshift keeps the bootstrap
//! deterministic across machines and architectures without depending on
//! `rand`'s version-to-version distribution changes.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use agl_types::{FitnessVector, HardGates};

/// Deterministic xorshift64*. Seeded, portable, and stable across releases.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Zero is a fixed point of xorshift; nudge it.
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform index in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Mean of a slice. Returns 0.0 for an empty slice rather than NaN, so a
/// missing measurement degrades to "no improvement" instead of poisoning the
/// comparison.
fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// The 95% bootstrap confidence interval of `mean(child) - mean(parent)`.
///
/// Resamples both groups with replacement `iters` times. Returns
/// `(lower, upper)`. A child is only an improvement when `lower > 0.0` — that
/// is, when the interval excludes "no difference" entirely.
pub fn bootstrap_delta_ci(parent: &[f64], child: &[f64], iters: usize, seed: u64) -> (f64, f64) {
    if parent.is_empty() || child.is_empty() || iters == 0 {
        return (0.0, 0.0);
    }
    let mut rng = Rng::new(seed);
    let mut deltas = Vec::with_capacity(iters);
    for _ in 0..iters {
        let p: f64 = (0..parent.len())
            .map(|_| parent[rng.below(parent.len())])
            .sum::<f64>()
            / parent.len() as f64;
        let c: f64 = (0..child.len())
            .map(|_| child[rng.below(child.len())])
            .sum::<f64>()
            / child.len() as f64;
        deltas.push(c - p);
    }
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo = deltas[(iters as f64 * 0.025) as usize];
    let hi = deltas[((iters as f64 * 0.975) as usize).min(iters - 1)];
    (lo, hi)
}

/// Everything measured about one candidate configuration on the box.
#[derive(Debug, Clone, Default)]
pub struct Measurement {
    /// Primary objective samples, higher is better (e.g. successful reads/sec).
    pub quality: Vec<f64>,
    /// Added p99 latency in milliseconds, lower is better.
    pub p99_overhead_ms: f64,
    /// Fraction of readings that were wrong or implausible, lower is better.
    pub false_positive_rate: f64,
    /// Behaviours that worked on the parent and stopped working. Must be zero.
    pub regression_count: u32,
    /// Did a rollback actually execute and get confirmed in this environment?
    /// Not "is a rollback available" — was one performed.
    pub rollback_verified: bool,
    /// Safety score in `0..=1`.
    pub safety: f64,
    /// Governance score in `0..=1`.
    pub governance: f64,
}

/// The outcome of comparing a child against its parent.
#[derive(Debug, Clone)]
pub struct Verdict {
    /// The fitness vector handed to autogenous.
    pub fitness: FitnessVector,
    /// Lower bound of the 95% CI on the quality delta.
    pub delta_lo: f64,
    /// Upper bound of the 95% CI on the quality delta.
    pub delta_hi: f64,
    /// Did the child beat the parent by the required margin, statistically?
    pub beats_parent: bool,
    /// Did it clear autogenous's hard AND-gate?
    pub passes_gates: bool,
}

impl Verdict {
    /// Promotion requires **both**: a statistically real improvement, and the
    /// hard gate. Neither substitutes for the other.
    pub fn promotable(&self) -> bool {
        self.beats_parent && self.passes_gates
    }
}

/// Score a child against its parent.
///
/// `min_delta` is the margin the child must clear (MetaHarness's default is
/// 0.05); `seed` makes the bootstrap reproducible.
pub fn score(
    parent: &Measurement,
    child: &Measurement,
    min_delta: f64,
    seed: u64,
    gates: &HardGates,
) -> Verdict {
    let (delta_lo, delta_hi) = bootstrap_delta_ci(&parent.quality, &child.quality, 2000, seed);
    let observed = mean(&child.quality) - mean(&parent.quality);

    // Both conditions, deliberately: a large point estimate with a CI that
    // straddles zero is noise, and a tight CI around a trivial delta is not
    // worth a deployment.
    let beats_parent = delta_lo > 0.0 && observed >= min_delta;

    let fitness = FitnessVector {
        task_quality: mean(&child.quality),
        safety: child.safety,
        governance: child.governance,
        reliability: 1.0 - child.false_positive_rate,
        p99_overhead_ms: child.p99_overhead_ms,
        false_positive_rate: child.false_positive_rate,
        regression_count: child.regression_count,
        rollback_verified: child.rollback_verified,
    };
    let passes_gates = fitness.passes_hard_gates(gates);

    Verdict {
        fitness,
        delta_lo,
        delta_hi,
        beats_parent,
        passes_gates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good(quality: Vec<f64>) -> Measurement {
        Measurement {
            quality,
            p99_overhead_ms: 1.0,
            false_positive_rate: 0.001,
            regression_count: 0,
            rollback_verified: true,
            safety: 1.0,
            governance: 1.0,
        }
    }

    #[test]
    fn bootstrap_is_reproducible_for_a_fixed_seed() {
        let p = vec![1.0, 1.1, 0.9, 1.05];
        let c = vec![2.0, 2.1, 1.9, 2.05];
        assert_eq!(
            bootstrap_delta_ci(&p, &c, 500, 42),
            bootstrap_delta_ci(&p, &c, 500, 42)
        );
    }

    #[test]
    fn a_clear_improvement_has_a_ci_above_zero() {
        let (lo, _) = bootstrap_delta_ci(&[1.0; 20], &[2.0; 20], 2000, 7);
        assert!(lo > 0.0, "expected a positive lower bound, got {lo}");
    }

    /// The case the methodology exists to catch: the child's mean is higher,
    /// but the spread is so wide the difference could easily be noise.
    #[test]
    fn noise_does_not_count_as_improvement() {
        let p = vec![1.0, 5.0, 0.5, 4.0, 2.0, 3.0, 0.1, 6.0];
        let c = vec![1.2, 5.2, 0.4, 4.4, 2.1, 3.3, 0.2, 6.1];
        let v = score(&good(p), &good(c), 0.05, 11, &HardGates::default());
        assert!(!v.beats_parent, "wide-spread noise was treated as a win");
    }

    #[test]
    fn a_safety_failure_cannot_be_offset_by_quality() {
        let mut child = good(vec![100.0; 10]);
        child.safety = 0.5; // below min_safety 0.99
        let v = score(&good(vec![1.0; 10]), &child, 0.05, 3, &HardGates::default());
        assert!(v.beats_parent, "quality really did improve");
        assert!(!v.passes_gates, "but the AND-gate must still refuse it");
        assert!(!v.promotable());
    }

    /// `rollback_verified` is about an executed rollback, not an available one.
    #[test]
    fn an_unverified_rollback_blocks_promotion() {
        let mut child = good(vec![10.0; 10]);
        child.rollback_verified = false;
        let v = score(&good(vec![1.0; 10]), &child, 0.05, 5, &HardGates::default());
        assert!(!v.promotable());
    }

    #[test]
    fn any_regression_blocks_promotion() {
        let mut child = good(vec![10.0; 10]);
        child.regression_count = 1;
        let v = score(&good(vec![1.0; 10]), &child, 0.05, 5, &HardGates::default());
        assert!(!v.promotable());
    }

    #[test]
    fn empty_measurements_degrade_to_no_improvement() {
        let v = score(&good(vec![]), &good(vec![]), 0.05, 1, &HardGates::default());
        assert!(!v.beats_parent);
    }
}
