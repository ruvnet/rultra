//! The governed self-optimization loop.
//!
//! Closes two of the seams named in ADR-0003: generating a typed mutation from
//! telemetry, and applying or reversing one on a real box. autogenous owns
//! admission and the fitness gate; this crate owns the domain translation on
//! either side of it.
//!
//! # A documented impedance mismatch
//!
//! `agl_types::MutationScope` has ten variants and **none of them describe
//! device or hardware policy** — they are software concerns (`PromptContext`,
//! `RoutingBudget`, `SchemaMigration`, `CompilerIr`, …). autogenous has so far
//! only been pointed at security-antibody use cases, so this is expected, but
//! it means every scope choice here is an interpretation rather than a fit.
//!
//! A sensor poll interval is modelled as [`MutationScope::RoutingBudget`]:
//! it allocates a sampling budget against a thermal constraint, which is the
//! closest honest reading, and it is one of the four auto-promotable scopes.
//! This is recorded as a finding worth taking upstream, not smoothed over.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod policy;

pub use policy::{Observation, SensePolicy};

use agl_types::{Applicability, Authority, Genome, HardInvariant, Mutation, MutationScope};

/// The invariant this box refuses to violate: the die must stay below its
/// thermal ceiling. `get_throttled` on this board has already reported bit 19
/// (a soft temperature limit *has* occurred), so this is a live constraint.
pub const THERMAL_INVARIANT: &str = "die_temp_below_ceiling";

/// Build the root genome for a policy.
///
/// `capability_ceiling` is [`Authority::AutoReversible`]: this box may adjust
/// its own sampling budget automatically, and may never grant a descendant
/// more than that. Anything needing `Governed` or above is a human's decision.
pub fn genome_for(policy: &SensePolicy, thermal_ok: bool) -> Genome {
    Genome {
        hash: policy.content_hash(),
        identity: "rultra-sense-policy".to_string(),
        // Hash-pinned and externally governed: the constitution is outside the
        // loop and this code never generates or edits it.
        constitution: "rultra-constitution-v1".to_string(),
        capability_ceiling: Authority::AutoReversible,
        hard_invariants: vec![HardInvariant {
            name: THERMAL_INVARIANT.to_string(),
            holds: thermal_ok,
        }],
        lineage: vec![],
    }
}

/// Propose a change to the sampling policy from an observation.
///
/// Returns `None` when nothing warrants a change — the common case, and the
/// one that keeps a self-optimizing box from thrashing.
pub fn propose(
    parent: &Genome,
    current: &SensePolicy,
    obs: &Observation,
    now: u64,
) -> Option<(Mutation, SensePolicy)> {
    let candidate = current.adjust_for(obs)?;
    let mutation = Mutation {
        id: format!("poll-{}-{}", candidate.poll_interval_ms, now),
        parent_genome_hash: parent.hash.clone(),
        // See the module docs: an interpretation, not a fit.
        scope: MutationScope::RoutingBudget,
        requested_authority: Authority::AutoReversible,
        applicability: Applicability {
            workloads: vec!["sense-stream".to_string()],
            environments: vec!["crowpi-v3.2".to_string()],
            jurisdictions: vec![],
        },
        preserved_invariants: vec![HardInvariant {
            name: THERMAL_INVARIANT.to_string(),
            // A proposal that raises sampling while already hot does not get to
            // claim it preserves the thermal invariant.
            holds: candidate.poll_interval_ms >= current.poll_interval_ms
                || !obs.thermally_stressed(),
        }],
        // Reversibility is structural in autogenous: a mutation with no
        // rollback target is inadmissible, so the parent hash always goes here.
        rollback_target: Some(parent.hash.clone()),
        // Expiry is a safety property, not bookkeeping. A tuning decision made
        // from an hour-old thermal reading should not still be in force a day
        // later, so every proposal times out.
        expires_at: Some(now + 86_400),
        signature: None,
    };
    Some((mutation, candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agl_types::AdmissionError;

    fn base() -> (Genome, SensePolicy) {
        let p = SensePolicy::default();
        (genome_for(&p, true), p)
    }

    #[test]
    fn a_hot_box_proposes_to_sample_less_often() {
        let (g, p) = base();
        let obs = Observation {
            die_temp_c: 79.0,
            read_error_rate: 0.0,
            samples: 100,
        };
        let (m, next) = propose(&g, &p, &obs, 1000).expect("should propose");
        assert!(
            next.poll_interval_ms > p.poll_interval_ms,
            "expected backing off, got {} -> {}",
            p.poll_interval_ms,
            next.poll_interval_ms
        );
        assert!(m.admissible(&g, 1000).is_ok());
    }

    #[test]
    fn a_cool_reliable_box_proposes_to_sample_more_often() {
        let (g, p) = base();
        let obs = Observation {
            die_temp_c: 45.0,
            read_error_rate: 0.0,
            samples: 100,
        };
        let (_, next) = propose(&g, &p, &obs, 1000).expect("should propose");
        assert!(next.poll_interval_ms < p.poll_interval_ms);
    }

    #[test]
    fn a_steady_box_proposes_nothing() {
        let (g, p) = base();
        let obs = Observation {
            die_temp_c: 60.0,
            read_error_rate: 0.0,
            samples: 100,
        };
        assert!(propose(&g, &p, &obs, 1000).is_none(), "should not thrash");
    }

    /// autogenous refuses a mutation requesting more authority than its parent
    /// allows. Verified here against the real upstream check, not a local copy.
    #[test]
    fn authority_cannot_expand_beyond_the_ceiling() {
        let (g, p) = base();
        let obs = Observation {
            die_temp_c: 79.0,
            read_error_rate: 0.0,
            samples: 100,
        };
        let (mut m, _) = propose(&g, &p, &obs, 1000).unwrap();
        m.requested_authority = Authority::Constitutional;
        assert!(matches!(
            m.admissible(&g, 1000),
            Err(AdmissionError::AuthorityExpansion { .. })
        ));
    }

    #[test]
    fn every_proposal_is_reversible_and_expires() {
        let (g, p) = base();
        let obs = Observation {
            die_temp_c: 79.0,
            read_error_rate: 0.0,
            samples: 100,
        };
        let (m, _) = propose(&g, &p, &obs, 1000).unwrap();
        assert_eq!(m.rollback_target.as_deref(), Some(g.hash.as_str()));
        assert_eq!(m.expires_at, Some(1000 + 86_400));
    }

    /// An expired mutation must stop being admissible on its own.
    #[test]
    fn an_expired_proposal_is_refused() {
        let (g, p) = base();
        let obs = Observation {
            die_temp_c: 79.0,
            read_error_rate: 0.0,
            samples: 100,
        };
        let (m, _) = propose(&g, &p, &obs, 1000).unwrap();
        assert!(m.admissible(&g, 1000).is_ok());
        assert!(
            m.admissible(&g, 1000 + 86_401).is_err(),
            "expiry not enforced"
        );
    }

    /// Raising the sample rate while already hot may not claim to preserve the
    /// thermal invariant — that claim is what the gate checks against.
    #[test]
    fn speeding_up_while_hot_does_not_claim_thermal_safety() {
        let (g, mut p) = base();
        p.poll_interval_ms = 5000;
        let obs = Observation {
            die_temp_c: 82.0,
            read_error_rate: 0.0,
            samples: 100,
        };
        if let Some((m, next)) = propose(&g, &p, &obs, 1000) {
            if next.poll_interval_ms < p.poll_interval_ms {
                assert!(
                    !m.preserved_invariants[0].holds,
                    "claimed thermal safety while speeding up on a hot box"
                );
            }
        }
    }
}
