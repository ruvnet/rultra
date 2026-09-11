//! One audit trail for the whole loop.
//!
//! This is seam 4 from ADR-0003. Both `ruvector` and `autogenous` sign
//! ed25519 content-addressed receipts, but they are **separate chains that do
//! not reference each other**. Neither can answer the only question an operator
//! actually asks: *why is this box configured the way it is right now?*
//!
//! A [`Chain`] answers it, by recording the causal sequence — observed, then
//! proposed, then gated, then promoted or rolled back — with each entry linked
//! to its predecessor by hash and signed. Entries may carry the receipt hashes
//! emitted by the other two systems, so this chain *cross-references* them
//! rather than replacing either.
//!
//! # Threat model
//!
//! The chain protects against silent rewriting of history: any edit, deletion
//! or reordering breaks either a hash link or a signature. It does **not**
//! protect against an attacker with the signing key, and it does not stop a
//! truncation of the tail — a reader must compare against an externally
//! anchored head to detect that. Both limits are deliberate and stated rather
//! than papered over.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What happened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A measurement window closed.
    Observed {
        /// Mean die temperature over the window.
        die_temp_c: f64,
        /// Fraction of reads that failed.
        read_error_rate: f64,
        /// How many samples the window covered.
        samples: u32,
    },
    /// A mutation was proposed from an observation.
    Proposed {
        /// The mutation's id.
        mutation_id: String,
        /// Genome hash it descends from.
        parent_genome_hash: String,
        /// Genome hash it would produce.
        candidate_hash: String,
    },
    /// The gate ran. Records the decision *and* why.
    Gated {
        /// The mutation judged.
        mutation_id: String,
        /// Did it clear the hard AND-gate and beat its parent?
        passed: bool,
        /// Human-readable reason, present for refusals especially.
        reason: String,
    },
    /// A candidate was promoted into service.
    Promoted {
        /// The mutation promoted.
        mutation_id: String,
        /// Genome hash now in force.
        genome_hash: String,
    },
    /// A promotion was reversed and the reversal confirmed.
    RolledBack {
        /// The mutation reversed.
        mutation_id: String,
        /// Genome hash restored.
        restored_hash: String,
        /// Was the restoration verified by reading it back?
        verified: bool,
    },
}

/// One link in the chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Position, from zero.
    pub seq: u64,
    /// Hash of the previous entry; all-zero for the first.
    pub prev_hash: String,
    /// Unix epoch seconds.
    pub at: u64,
    /// What happened.
    pub event: Event,
    /// Receipt hash from autogenous's own witness chain, when one exists.
    pub autogenous_receipt: Option<String>,
    /// Receipt hash from ruvector's cognitive-container chain, when one exists.
    pub ruvector_receipt: Option<String>,
    /// This entry's content hash.
    pub hash: String,
    /// Detached ed25519 signature over `hash`, hex-encoded.
    pub signature: String,
}

impl Entry {
    /// Recompute the content hash from the fields it covers.
    ///
    /// Deliberately excludes `hash` and `signature`: a hash cannot cover
    /// itself, and including the signature would make verification circular.
    fn compute_hash(
        seq: u64,
        prev_hash: &str,
        at: u64,
        event: &Event,
        autogenous_receipt: &Option<String>,
        ruvector_receipt: &Option<String>,
    ) -> String {
        let payload = serde_json::json!({
            "seq": seq,
            "prev_hash": prev_hash,
            "at": at,
            "event": event,
            "autogenous_receipt": autogenous_receipt,
            "ruvector_receipt": ruvector_receipt,
        });
        let mut h = Sha256::new();
        h.update(
            serde_json::to_string(&payload)
                .unwrap_or_default()
                .as_bytes(),
        );
        format!("{:x}", h.finalize())
    }
}

/// The genesis `prev_hash`.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// An append-only signed chain.
pub struct Chain {
    key: SigningKey,
    entries: Vec<Entry>,
}

impl Chain {
    /// Start an empty chain with a signing key.
    pub fn new(key: SigningKey) -> Self {
        Self {
            key,
            entries: Vec::new(),
        }
    }

    /// The public key entries are signed with.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    /// Every entry, oldest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Hash of the newest entry, or [`GENESIS`] when empty.
    pub fn head(&self) -> String {
        self.entries
            .last()
            .map(|e| e.hash.clone())
            .unwrap_or_else(|| GENESIS.to_string())
    }

    /// Append an event.
    pub fn append(
        &mut self,
        at: u64,
        event: Event,
        autogenous_receipt: Option<String>,
        ruvector_receipt: Option<String>,
    ) -> &Entry {
        let seq = self.entries.len() as u64;
        let prev_hash = self.head();
        let hash = Entry::compute_hash(
            seq,
            &prev_hash,
            at,
            &event,
            &autogenous_receipt,
            &ruvector_receipt,
        );
        let signature = hex(&self.key.sign(hash.as_bytes()).to_bytes());
        self.entries.push(Entry {
            seq,
            prev_hash,
            at,
            event,
            autogenous_receipt,
            ruvector_receipt,
            hash,
            signature,
        });
        self.entries.last().expect("just pushed")
    }

    /// Verify the whole chain: hashes, links, ordering and signatures.
    pub fn verify(&self, key: &VerifyingKey) -> Result<(), ChainError> {
        let mut expected_prev = GENESIS.to_string();
        for (i, e) in self.entries.iter().enumerate() {
            if e.seq != i as u64 {
                return Err(ChainError::OutOfOrder { at: i as u64 });
            }
            if e.prev_hash != expected_prev {
                return Err(ChainError::BrokenLink { at: e.seq });
            }
            let recomputed = Entry::compute_hash(
                e.seq,
                &e.prev_hash,
                e.at,
                &e.event,
                &e.autogenous_receipt,
                &e.ruvector_receipt,
            );
            if recomputed != e.hash {
                return Err(ChainError::Tampered { at: e.seq });
            }
            let sig_bytes = unhex(&e.signature).ok_or(ChainError::BadSignature { at: e.seq })?;
            let sig = Signature::from_slice(&sig_bytes)
                .map_err(|_| ChainError::BadSignature { at: e.seq })?;
            key.verify(e.hash.as_bytes(), &sig)
                .map_err(|_| ChainError::BadSignature { at: e.seq })?;
            expected_prev = e.hash.clone();
        }
        Ok(())
    }

    /// Serialize to JSON Lines, one entry per line.
    pub fn to_jsonl(&self) -> String {
        self.entries
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Why a chain failed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainError {
    /// An entry's `prev_hash` does not match its predecessor.
    BrokenLink {
        /// Sequence number of the offending entry.
        at: u64,
    },
    /// An entry's contents do not match its recorded hash.
    Tampered {
        /// Sequence number.
        at: u64,
    },
    /// A signature is missing, malformed, or not by the expected key.
    BadSignature {
        /// Sequence number.
        at: u64,
    },
    /// Sequence numbers are not consecutive from zero.
    OutOfOrder {
        /// Position where ordering broke.
        at: u64,
    },
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrokenLink { at } => write!(f, "broken hash link at entry {at}"),
            Self::Tampered { at } => write!(f, "contents do not match hash at entry {at}"),
            Self::BadSignature { at } => write!(f, "invalid signature at entry {at}"),
            Self::OutOfOrder { at } => write!(f, "sequence out of order at entry {at}"),
        }
    }
}

impl std::error::Error for ChainError {}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic key so tests do not depend on entropy.
    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn populated() -> Chain {
        let mut c = Chain::new(key());
        c.append(
            100,
            Event::Observed {
                die_temp_c: 79.0,
                read_error_rate: 0.0,
                samples: 100,
            },
            None,
            Some("rvf-receipt-abc".into()),
        );
        c.append(
            101,
            Event::Proposed {
                mutation_id: "poll-2000-101".into(),
                parent_genome_hash: "aaa".into(),
                candidate_hash: "bbb".into(),
            },
            None,
            None,
        );
        c.append(
            102,
            Event::Gated {
                mutation_id: "poll-2000-101".into(),
                passed: true,
                reason: "cleared hard gates; CI lower bound above zero".into(),
            },
            Some("autogenous-receipt-xyz".into()),
            None,
        );
        c
    }

    #[test]
    fn a_fresh_chain_verifies() {
        let c = populated();
        assert_eq!(c.verify(&c.verifying_key()), Ok(()));
        assert_eq!(c.entries().len(), 3);
    }

    #[test]
    fn the_first_entry_links_to_genesis() {
        let c = populated();
        assert_eq!(c.entries()[0].prev_hash, GENESIS);
    }

    /// The central property: editing recorded history must be detectable.
    #[test]
    fn editing_an_event_is_detected() {
        let mut c = populated();
        c.entries[1].event = Event::Proposed {
            mutation_id: "poll-9999-101".into(),
            parent_genome_hash: "aaa".into(),
            candidate_hash: "bbb".into(),
        };
        assert_eq!(
            c.verify(&c.verifying_key()),
            Err(ChainError::Tampered { at: 1 })
        );
    }

    /// An attacker without the signing key can recompute a hash but cannot
    /// re-sign it, so forgery is caught at the signature before the chain
    /// structure is even consulted.
    #[test]
    fn rewriting_an_entry_without_the_key_fails_at_the_signature() {
        let mut c = populated();
        let e = &mut c.entries[1];
        e.event = Event::Gated {
            mutation_id: "forged".into(),
            passed: true,
            reason: "forged".into(),
        };
        e.hash = Entry::compute_hash(
            e.seq,
            &e.prev_hash,
            e.at,
            &e.event,
            &e.autogenous_receipt,
            &e.ruvector_receipt,
        );
        assert_eq!(
            c.verify(&c.verifying_key()),
            Err(ChainError::BadSignature { at: 1 })
        );
    }

    /// And if the attacker *does* hold the signing key — the case the threat
    /// model admits is not defended — the hash chain is still the second line:
    /// altering one entry invalidates every link after it, so a forger must
    /// rewrite the entire tail rather than one inconvenient record.
    #[test]
    fn even_with_the_signing_key_one_rewritten_entry_breaks_the_next_link() {
        let mut c = populated();
        let k = key();
        let e = &mut c.entries[1];
        e.event = Event::Gated {
            mutation_id: "forged".into(),
            passed: true,
            reason: "forged".into(),
        };
        e.hash = Entry::compute_hash(
            e.seq,
            &e.prev_hash,
            e.at,
            &e.event,
            &e.autogenous_receipt,
            &e.ruvector_receipt,
        );
        e.signature = hex(&k.sign(e.hash.as_bytes()).to_bytes());
        assert_eq!(
            c.verify(&c.verifying_key()),
            Err(ChainError::BrokenLink { at: 2 })
        );
    }

    #[test]
    fn deleting_an_entry_is_detected() {
        let mut c = populated();
        c.entries.remove(1);
        assert!(c.verify(&c.verifying_key()).is_err());
    }

    #[test]
    fn a_signature_from_another_key_is_rejected() {
        let c = populated();
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
        assert_eq!(c.verify(&other), Err(ChainError::BadSignature { at: 0 }));
    }

    #[test]
    fn external_receipts_are_carried_and_hashed() {
        let c = populated();
        assert_eq!(
            c.entries()[0].ruvector_receipt.as_deref(),
            Some("rvf-receipt-abc")
        );
        assert_eq!(
            c.entries()[2].autogenous_receipt.as_deref(),
            Some("autogenous-receipt-xyz")
        );
        // Changing a receipt reference must invalidate the entry, or the
        // cross-reference would be decorative rather than attested.
        let mut t = populated();
        t.entries[0].ruvector_receipt = Some("rvf-receipt-forged".into());
        assert_eq!(
            t.verify(&t.verifying_key()),
            Err(ChainError::Tampered { at: 0 })
        );
    }

    #[test]
    fn jsonl_round_trips_every_entry() {
        let c = populated();
        let jsonl = c.to_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 3);
        for l in lines {
            let e: Entry = serde_json::from_str(l).expect("valid json");
            assert!(!e.hash.is_empty());
        }
    }
}
