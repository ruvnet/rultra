//! The gate between agents and hardware.
//!
//! The rule is that an agent never holds raw GPIO. It holds addresses, and
//! every use of an address passes through [`Policy::permit`], which is
//! default-deny for the actuator plane.
//!
//! This is not theoretical caution. Earlier on this box a probe of GPIO18 used
//! a timed hold that expired and left the line floating, and the buzzer
//! screamed until the pin was explicitly driven low. Nothing in that sequence
//! was malicious or even unusual — it was a diagnostic that ended slightly
//! differently than intended. A relay failing the same way is a different
//! magnitude of problem, which is why relays are denied unless named.
//!
//! Three properties are deliberate:
//!
//! - **Grants are exact, never prefix matches.** A grant for
//!   `ruv://lab/actuator/relay/1` confers nothing on `relay/2`. Prefix matching
//!   would silently widen authority whenever a new instance appeared.
//! - **Every decision carries a reason**, so the event log says why something
//!   was refused rather than only that it was.
//! - **Emergency stop outranks every grant**, and does not disable sensing.
//!   Observing is exactly what is wanted during a stop.

use crate::observe::MonoNanos;
use crate::uri::{LabUri, Plane};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Why an action was allowed or refused. Carried into the audit log verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny { reason: DenyReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// The actuator plane is default-deny and this address was never granted.
    NotGranted,
    /// Used again before its minimum interval elapsed.
    RateLimited { retry_after_ns: u64 },
    /// Emergency stop is engaged.
    EmergencyStop,
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DenyReason::NotGranted => write!(f, "no grant for this address"),
            DenyReason::RateLimited { retry_after_ns } => {
                write!(
                    f,
                    "rate limited, retry in {:.0} ms",
                    *retry_after_ns as f64 / 1e6
                )
            }
            DenyReason::EmergencyStop => write!(f, "emergency stop engaged"),
        }
    }
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}

/// One grant: permission to use one exact address, at a bounded rate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub uri: LabUri,
    /// Minimum interval between uses. Zero means unlimited.
    pub min_interval_ns: u64,
}

/// The capability layer. Holds grants and the last-use clock.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    grants: HashMap<LabUri, Grant>,
    last_use: HashMap<LabUri, MonoNanos>,
    estop: bool,
}

impl Policy {
    pub fn new() -> Policy {
        Policy::default()
    }

    /// Grant one exact address. Returns self for chaining at setup.
    pub fn grant(mut self, uri: LabUri, min_interval_ns: u64) -> Policy {
        self.grants.insert(
            uri.clone(),
            Grant {
                uri,
                min_interval_ns,
            },
        );
        self
    }

    /// Engage or release emergency stop.
    pub fn set_emergency_stop(&mut self, engaged: bool) {
        self.estop = engaged;
    }

    pub fn emergency_stopped(&self) -> bool {
        self.estop
    }

    /// Decide whether `uri` may be used at `now`, recording the use if allowed.
    ///
    /// Takes `&mut self` because rate limiting is stateful: a check that did
    /// not record the use would let a caller pass the same check repeatedly.
    pub fn permit(&mut self, uri: &LabUri, now: MonoNanos) -> Decision {
        // Sensors: always readable, including during an emergency stop.
        if uri.plane.safe_by_default() {
            return Decision::Allow;
        }
        if self.estop {
            return Decision::Deny {
                reason: DenyReason::EmergencyStop,
            };
        }
        let Some(grant) = self.grants.get(uri) else {
            return Decision::Deny {
                reason: DenyReason::NotGranted,
            };
        };
        if grant.min_interval_ns > 0 {
            if let Some(last) = self.last_use.get(uri) {
                let since = now.delta_ns(*last);
                if since < grant.min_interval_ns {
                    return Decision::Deny {
                        reason: DenyReason::RateLimited {
                            retry_after_ns: grant.min_interval_ns - since,
                        },
                    };
                }
            }
        }
        self.last_use.insert(uri.clone(), now);
        Decision::Allow
    }

    /// Addresses currently granted, for display and audit.
    pub fn granted(&self) -> Vec<&LabUri> {
        let mut v: Vec<&LabUri> = self.grants.keys().collect();
        v.sort_by_key(|u| u.to_string());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> LabUri {
        LabUri::parse(s).unwrap()
    }

    #[test]
    fn actuators_are_denied_until_named() {
        let mut p = Policy::new();
        let d = p.permit(&u("ruv://lab/actuator/relay/1"), MonoNanos(0));
        assert_eq!(
            d,
            Decision::Deny {
                reason: DenyReason::NotGranted
            }
        );
    }

    #[test]
    fn sensors_need_no_grant() {
        let mut p = Policy::new();
        assert!(p
            .permit(&u("ruv://lab/sensor/light"), MonoNanos(0))
            .is_allowed());
    }

    #[test]
    fn a_grant_for_one_relay_does_not_confer_another() {
        // Prefix matching here would hand over every relay the moment a second
        // one was wired up.
        let mut p = Policy::new().grant(u("ruv://lab/actuator/relay/1"), 0);
        assert!(p
            .permit(&u("ruv://lab/actuator/relay/1"), MonoNanos(0))
            .is_allowed());
        assert!(!p
            .permit(&u("ruv://lab/actuator/relay/2"), MonoNanos(0))
            .is_allowed());
        // Nor does it confer the un-instanced address.
        assert!(!p
            .permit(&u("ruv://lab/actuator/relay"), MonoNanos(0))
            .is_allowed());
    }

    #[test]
    fn the_buzzer_cannot_be_retriggered_faster_than_its_limit() {
        // The incident this guards against: a stuck emitter.
        let buzzer = u("ruv://lab/actuator/buzzer");
        let mut p = Policy::new().grant(buzzer.clone(), 1_000_000_000);
        assert!(p.permit(&buzzer, MonoNanos::from_millis(0)).is_allowed());
        match p.permit(&buzzer, MonoNanos::from_millis(400)) {
            Decision::Deny {
                reason: DenyReason::RateLimited { retry_after_ns },
            } => {
                assert_eq!(retry_after_ns, 600_000_000);
            }
            other => panic!("expected rate limit, got {other:?}"),
        }
        assert!(p.permit(&buzzer, MonoNanos::from_millis(1000)).is_allowed());
    }

    #[test]
    fn emergency_stop_outranks_a_grant_but_leaves_sensing_alive() {
        let relay = u("ruv://lab/actuator/relay/1");
        let mut p = Policy::new().grant(relay.clone(), 0);
        p.set_emergency_stop(true);
        assert_eq!(
            p.permit(&relay, MonoNanos(0)),
            Decision::Deny {
                reason: DenyReason::EmergencyStop
            }
        );
        // Observing during a stop is the whole point.
        assert!(p
            .permit(&u("ruv://lab/sensor/motion"), MonoNanos(0))
            .is_allowed());
    }

    #[test]
    fn a_denial_explains_itself_for_the_audit_log() {
        let mut p = Policy::new();
        let Decision::Deny { reason } = p.permit(&u("ruv://lab/actuator/buzzer"), MonoNanos(0))
        else {
            panic!("should deny");
        };
        assert!(reason.to_string().contains("no grant"));
    }

    #[test]
    fn checking_permission_records_the_use() {
        // A non-recording check would make the rate limit decorative.
        let b = u("ruv://lab/actuator/buzzer");
        let mut p = Policy::new().grant(b.clone(), 500_000_000);
        assert!(p.permit(&b, MonoNanos::from_millis(0)).is_allowed());
        assert!(!p.permit(&b, MonoNanos::from_millis(1)).is_allowed());
    }
}
