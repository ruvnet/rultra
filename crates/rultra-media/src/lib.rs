//! Turning a room into a media request, without breaking anyone's contract.
//!
//! # The constraint that shapes this crate
//!
//! Cognitum Media's job and dispatch schemas are **closed**
//! (`additionalProperties: false`), and the repo ships a fixture named
//! `job.invalid-extra-field.json` whose only defect is one unversioned field.
//! That is deliberate governance: a producer cannot smuggle private data into a
//! shared contract.
//!
//! So rultra does not attach a steering blob to a job. The room state is
//! expressed through the fields the contract already has — **which profile to
//! run, and which deliverables to ask for** — and anything richer requires a
//! *versioned* contract change, proposed upstream rather than bolted on here.
//!
//! # What this crate does not do
//!
//! It builds requests; it does not send them. Dispatch needs an
//! `authorization_id`, a `quote_digest` and a `budget_ceiling_micros` issued by
//! the control plane, and Lyria admission additionally requires a verified
//! Google service-account on an explicit allowlist. Those are authorization
//! decisions, not code.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use rultra_spatial::{Light, Proximity, RoomState, Steering};
use serde::{Deserialize, Serialize};

/// Contract version this crate emits. Must match the vendored schemas.
pub const CONTRACT_VERSION: &str = "0.1.0";

/// Who the job belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    /// Tenant.
    pub tenant_id: String,
    /// Owner within the tenant.
    pub owner_id: String,
}

/// A job request in the shape the contract requires.
///
/// Field-for-field the upstream `Job` definition, in declaration order, with no
/// additions. A test validates instances against the vendored schema, and a
/// second test asserts that adding a field makes them invalid — so this type
/// cannot drift into being "the contract plus our bits".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRequest {
    /// Contract version.
    pub contract_version: String,
    /// Job identity.
    pub job_id: String,
    /// Owner.
    pub owner: Owner,
    /// Which service profile to run.
    pub profile_id: String,
    /// Profile version.
    pub profile_version: u32,
    /// Deduplication key.
    pub idempotency_key: String,
    /// Lifecycle state; a new request is always `requested`.
    pub state: String,
    /// Digest of the quote this job is billed against.
    pub quote_digest: String,
    /// Authorization permitting the spend.
    pub authorization_id: String,
    /// What to produce.
    pub requested_deliverables: Vec<String>,
    /// Always empty on request; the control plane fills it.
    pub artifacts: Vec<serde_json::Value>,
    /// Creation time, unix seconds.
    pub created_at_unix: u64,
    /// Update time, unix seconds.
    pub updated_at_unix: u64,
}

/// What the room suggests producing.
///
/// The mapping is intentionally coarse. A room sensed through one uncalibrated
/// range finder and one light sensor can justify choosing between a handful of
/// profiles; it cannot justify a continuous parameter space, and pretending
/// otherwise would dress up noise as intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Nobody present and dark: produce nothing.
    Dormant,
    /// Present and calm: ambient audio.
    Ambient,
    /// Present and active: audio with visuals.
    Active,
}

impl Intent {
    /// Decide from a room state.
    pub fn from_room(room: &RoomState) -> Self {
        match (room.proximity, room.light) {
            // No usable range sensor is no evidence of presence, so it must
            // never reach Active — that is the branch that asks a provider for
            // video as well as audio. Falling back to the light term alone is
            // the conservative reading, not the neutral one.
            (None, Light::Dark) => Intent::Dormant,
            (None, _) => Intent::Ambient,
            (Some(Proximity::Empty), Light::Dark) => Intent::Dormant,
            (Some(Proximity::Empty), _) => Intent::Ambient,
            (Some(Proximity::Close | Proximity::Near), _) if room.stillness < 1.0 => Intent::Active,
            _ => Intent::Ambient,
        }
    }

    /// The service profile this intent runs, and what it asks for.
    ///
    /// Profile ids are taken from the upstream fixtures rather than invented;
    /// `music.production` producing `audio.master` is the shipped example.
    pub fn profile(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            Intent::Dormant => None,
            Intent::Ambient => Some(("music.production", &["audio.master"])),
            Intent::Active => Some(("music.production", &["audio.master", "video.preview"])),
        }
    }
}

/// Everything the control plane must issue before a job can be dispatched.
///
/// Taken as input rather than fabricated: a quote digest this crate invented
/// would be a fiction, and a budget ceiling it chose would be spending someone
/// else's money.
#[derive(Debug, Clone)]
pub struct Grant {
    /// Authorization permitting the spend.
    pub authorization_id: String,
    /// Digest of the quote.
    pub quote_digest: String,
}

/// Why a room could not become a job.
#[derive(Debug, PartialEq, Eq)]
pub enum PlanError {
    /// The room does not warrant producing anything.
    Dormant,
    /// The state is not trustworthy enough to bill against.
    NotBillable,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dormant => write!(f, "room is dormant; nothing to produce"),
            Self::NotBillable => write!(
                f,
                "room state is not verified well enough to spend against \
                 (see ADR-0006): refusing to bill for a reading nobody has checked"
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// Plan a job from a room state.
///
/// Refuses on an unbillable state. That refusal is the point: the range finder
/// is `Unvalidated`, and authorizing spend on it would turn an uncalibrated
/// number into a charge.
pub fn plan(
    room: &RoomState,
    owner: Owner,
    grant: &Grant,
    job_id: impl Into<String>,
    idempotency_key: impl Into<String>,
    now_unix: u64,
) -> Result<JobRequest, PlanError> {
    if !room.fit_to_spend_on() {
        return Err(PlanError::NotBillable);
    }
    let Some((profile_id, deliverables)) = Intent::from_room(room).profile() else {
        return Err(PlanError::Dormant);
    };
    Ok(JobRequest {
        contract_version: CONTRACT_VERSION.to_string(),
        job_id: job_id.into(),
        owner,
        profile_id: profile_id.to_string(),
        profile_version: 1,
        idempotency_key: idempotency_key.into(),
        state: "requested".to_string(),
        quote_digest: grant.quote_digest.clone(),
        authorization_id: grant.authorization_id.clone(),
        requested_deliverables: deliverables.iter().map(|s| s.to_string()).collect(),
        artifacts: Vec::new(),
        created_at_unix: now_unix,
        updated_at_unix: now_unix,
    })
}

/// The steering a consumer may apply *outside* the contract, e.g. as provider
/// parameters on a profile that accepts them.
///
/// Returned separately, never merged into [`JobRequest`], because merging it
/// would produce exactly the extra field the contract rejects.
pub fn steering_for(room: &RoomState) -> Steering {
    Steering::from(room)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rultra_sense::Verification;
    use serde_json::json;

    /// The upstream `Job` definition, resolvable on its own.
    ///
    /// `Job` lives inside `common.schema.json`, so its internal `#/$defs/...`
    /// references resolve once the whole file travels with it.
    fn job_schema() -> serde_json::Value {
        let mut common: serde_json::Value =
            serde_json::from_str(include_str!("../contracts/common.schema.json")).unwrap();
        let obj = common.as_object_mut().unwrap();
        obj.insert("$ref".into(), json!("#/$defs/Job"));
        obj.remove("$id");
        common
    }

    fn validate(doc: &serde_json::Value) -> Result<(), String> {
        let schema = job_schema();
        let compiled = jsonschema::validator_for(&schema).map_err(|e| e.to_string())?;
        if compiled.is_valid(doc) {
            Ok(())
        } else {
            Err(compiled
                .iter_errors(doc)
                .map(|e| format!("{e}"))
                .collect::<Vec<_>>()
                .join("; "))
        }
    }

    fn room(p: Proximity, l: Light, still: f64, v: Verification) -> RoomState {
        RoomState {
            proximity: Some(p),
            light: l,
            range_m: None,
            lux: None,
            stillness: still,
            verification: v,
        }
    }

    fn grant() -> Grant {
        Grant {
            authorization_id: "auth-001".into(),
            quote_digest: "sha256:quote".into(),
        }
    }

    fn owner() -> Owner {
        Owner {
            tenant_id: "tenant-a".into(),
            owner_id: "owner-a".into(),
        }
    }

    fn planned() -> JobRequest {
        plan(
            &room(Proximity::Near, Light::Lit, 0.5, Verification::Working),
            owner(),
            &grant(),
            "job-00000001",
            "idem-001",
            1_800_000_000,
        )
        .expect("should plan")
    }

    /// The whole point: what we emit is valid against the upstream contract,
    /// checked with the same validator family upstream uses.
    #[test]
    fn a_planned_job_validates_against_the_upstream_schema() {
        let doc = serde_json::to_value(planned()).unwrap();
        validate(&doc).expect("planned job must satisfy the contract");
    }

    /// Proves the contract is genuinely closed, so the test above is not
    /// passing vacuously against a permissive schema.
    #[test]
    fn one_extra_field_makes_it_invalid() {
        let mut doc = serde_json::to_value(planned()).unwrap();
        doc.as_object_mut()
            .unwrap()
            .insert("unversioned_breaking_field".into(), json!(true));
        assert!(
            validate(&doc).is_err(),
            "the contract must reject unversioned fields — if this passes, \
             additionalProperties is no longer false upstream and our \
             assumptions need rechecking"
        );
    }

    /// Field-for-field agreement with the shipped gold fixture.
    #[test]
    fn the_planned_shape_matches_the_upstream_fixture() {
        let fixture = json!({
            "contract_version": "0.1.0",
            "job_id": "job-00000001",
            "owner": { "tenant_id": "tenant-a", "owner_id": "owner-a" },
            "profile_id": "music.production",
            "profile_version": 1,
            "idempotency_key": "idem-001",
            "state": "requested",
            "quote_digest": "sha256:quote",
            "authorization_id": "auth-001",
            "requested_deliverables": ["audio.master"],
            "artifacts": [],
            "created_at_unix": 1800000000,
            "updated_at_unix": 1800000000
        });
        let ours = serde_json::to_value(
            plan(
                &room(Proximity::Empty, Light::Lit, 1.0, Verification::Working),
                owner(),
                &grant(),
                "job-00000001",
                "idem-001",
                1_800_000_000,
            )
            .unwrap(),
        )
        .unwrap();
        let (a, b) = (fixture.as_object().unwrap(), ours.as_object().unwrap());
        let mut mine: Vec<_> = b.keys().collect();
        let mut theirs: Vec<_> = a.keys().collect();
        mine.sort();
        theirs.sort();
        assert_eq!(mine, theirs, "field set diverged from the fixture");
        assert_eq!(
            fixture, ours,
            "an ambient room should reproduce the fixture exactly"
        );
    }

    /// ADR-0006 enforced at the point it costs money.
    #[test]
    fn an_unvalidated_room_refuses_to_become_a_billable_job() {
        let e = plan(
            &room(Proximity::Near, Light::Lit, 0.5, Verification::Unvalidated),
            owner(),
            &grant(),
            "j",
            "i",
            0,
        )
        .unwrap_err();
        assert_eq!(e, PlanError::NotBillable);
        assert!(e.to_string().contains("nobody has checked"));
    }

    #[test]
    fn a_dark_empty_room_produces_nothing() {
        assert_eq!(
            plan(
                &room(Proximity::Empty, Light::Dark, 1.0, Verification::Working),
                owner(),
                &grant(),
                "j",
                "i",
                0
            )
            .unwrap_err(),
            PlanError::Dormant
        );
    }

    #[test]
    fn an_active_room_also_asks_for_visuals() {
        let j = planned();
        assert!(j
            .requested_deliverables
            .contains(&"video.preview".to_string()));
        assert!(j
            .requested_deliverables
            .contains(&"audio.master".to_string()));
    }

    /// Steering must never be merged into the job, or it becomes the extra
    /// field the contract rejects.
    #[test]
    fn steering_travels_separately_from_the_job() {
        let r = room(Proximity::Close, Light::Bright, 0.5, Verification::Working);
        let s = steering_for(&r);
        assert_eq!(s.intensity, 1.0);
        let doc = serde_json::to_value(planned()).unwrap();
        for k in ["steering", "intensity", "luminance", "calm"] {
            assert!(
                !doc.as_object().unwrap().contains_key(k),
                "{k} leaked into the job"
            );
        }
    }

    #[test]
    fn a_request_never_carries_artifacts() {
        assert!(
            planned().artifacts.is_empty(),
            "the control plane fills these"
        );
        assert_eq!(planned().state, "requested");
    }
}
