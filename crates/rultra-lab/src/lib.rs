//! `ruv://lab/*` — one addressable fabric over every sensor and actuator on the
//! box, so that adding hardware adds a discoverable capability rather than
//! another bespoke demo.
//!
//! Three ideas hold it together:
//!
//! 1. [`uri`] gives every capability one address that carries its plane, so a
//!    policy can default-deny all actuation without enumerating devices.
//! 2. [`observe`] refuses to record readings as simultaneous when they are not.
//!    Bad ground truth is silent and expensive; a rejected sample is neither.
//! 3. [`policy`] stands between agents and hardware, with exact grants, rate
//!    limits, an emergency stop, and a reason attached to every refusal.

pub mod observe;
pub mod policy;
pub mod uri;

pub use observe::{MonoNanos, Observation, Reading, SkewError, Transport, Value};
pub use policy::{Decision, DenyReason, Grant, Policy};
pub use uri::{LabUri, Plane, UriError};
