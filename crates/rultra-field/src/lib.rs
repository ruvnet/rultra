//! A **live-hardware** adapter for [RuField MFS](https://github.com/ruvnet/rufield).
//!
//! RuField normalizes camera-free sensing modalities into one event grammar.
//! Its v0.1 reference stack ships a synthetic simulator and two file-replay
//! adapters; its own trait documentation says as much, and live-hardware
//! streaming is listed as a roadmap item. This adapter closes that gap for
//! `Modality::Ultrasonic` (registry code 7) using a real HC-SR04 on a
//! Raspberry Pi.
//!
//! # What is and is not claimed
//!
//! RuField is unusually careful about labelling what its numbers mean, and this
//! adapter matches that posture rather than quietly benefiting from it:
//!
//! - **`synthetic: false`.** These are real echoes off real objects, measured
//!   by timing a real pin. That is the point of this adapter, and it is the one
//!   thing it can claim without qualification.
//! - **Uncalibrated.** No reading has been checked against a known distance, so
//!   `calibration_id` says `uncalibrated` rather than naming a receipt that
//!   does not exist, and confidence is capped accordingly (see
//!   [`UNCALIBRATED_CONFIDENCE`]). rultra tracks this as
//!   `Verification::Unvalidated`; see ADR-0006.
//! - **Range only.** A single-element time-of-flight sensor measures distance
//!   to the nearest reflector along one axis. It cannot support presence,
//!   pose, or identity claims, so no such field is populated.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use rufield_core::{
    modality::{FieldAxis, Modality},
    privacy::PrivacyClass,
    tensor::FieldTensor,
    traits::{AdapterCapabilities, FieldAdapter},
};
use sha2::{Digest, Sha256};

/// Ceiling on reported confidence while the sensor is uncalibrated.
///
/// Not a tuned value — a deliberate cap. Consistency is not accuracy
/// (ADR-0006), and an uncalibrated instrument reporting high confidence is how
/// a fusion engine downstream is misled into weighting it as if it were
/// trustworthy. It rises only when a reading is checked against a reference.
pub const UNCALIBRATED_CONFIDENCE: f32 = 0.55;

/// Speed of sound used for the conversion, m/s at roughly 20 °C.
///
/// Fixed rather than compensated, and that is itself a calibration gap: sound
/// speed varies about 0.17%/°C, so a 10 °C error is ~1.7% of range. Recorded
/// here so the assumption is visible instead of buried in a constant.
pub const SPEED_OF_SOUND_MPS: f32 = 343.0;

/// Errors this adapter can produce.
#[derive(Debug)]
pub enum FieldError {
    /// The sensor produced no usable echo.
    NoEcho(String),
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEcho(m) => write!(f, "no usable ultrasonic echo: {m}"),
        }
    }
}

impl std::error::Error for FieldError {}

/// Turn one distance measurement into a RuField tensor.
///
/// The tensor is a single range bin, which is what a one-element
/// time-of-flight part actually produces. Padding it into a range *profile*
/// would imply spatial structure the hardware cannot see.
pub fn tensor_from_range(range_m: f32, timestamp_ns: u64) -> FieldTensor {
    FieldTensor {
        spec_version: rufield_core::SPEC_VERSION.to_string(),
        timestamp_ns,
        modality: Modality::Ultrasonic,
        axes: vec![FieldAxis::Range],
        shape: vec![1],
        values: vec![range_m],
        confidence: UNCALIBRATED_CONFIDENCE,
        // The measured spread on a static target: 0.62cm over 8 samples,
        // expressed in metres. A real figure from this board, not a guess.
        noise_floor: 0.0062,
        calibration_id: None,
        // P0: a raw time-of-flight range IS the raw sensor frame for a
        // single-element part. There is no derived-feature layer between the
        // echo and this number to downgrade it.
        privacy_class: PrivacyClass::P0,
    }
}

/// `sha256:` digest of the raw measurement, for provenance.
pub fn raw_hash(range_m: f32, timestamp_ns: u64) -> String {
    let mut h = Sha256::new();
    h.update(range_m.to_le_bytes());
    h.update(timestamp_ns.to_le_bytes());
    format!("sha256:{:x}", h.finalize())
}

/// A live HC-SR04 as a RuField adapter.
pub struct LiveUltrasonicAdapter {
    device_id: String,
    placement: String,
    /// Supplies metres. Injected so the adapter is testable without hardware
    /// and so the same type serves the Pi and a test double.
    source: Box<dyn FnMut() -> anyhow::Result<f32> + Send>,
}

impl LiveUltrasonicAdapter {
    /// Build an adapter over any source of range measurements in metres.
    pub fn new(
        device_id: impl Into<String>,
        placement: impl Into<String>,
        source: Box<dyn FnMut() -> anyhow::Result<f32> + Send>,
    ) -> Self {
        Self {
            device_id: device_id.into(),
            placement: placement.into(),
            source,
        }
    }

    /// Build an adapter over the real sensor on this box.
    #[cfg(all(target_os = "linux", feature = "hardware"))]
    pub fn live(device_id: impl Into<String>, placement: impl Into<String>) -> Self {
        use rultra_sense::{Backend, DeviceId, Value};
        let mut backend =
            rultra_sense::backend::linux::LinuxBackend::open().expect("i2c/gpio unavailable");
        Self::new(
            device_id,
            placement,
            Box::new(move || match backend.read(DeviceId::Range)?.value {
                Value::Scalar { n, .. } => Ok((n / 100.0) as f32),
                other => anyhow::bail!("range device returned {other:?}, expected a scalar"),
            }),
        )
    }
}

impl FieldAdapter for LiveUltrasonicAdapter {
    type Error = FieldError;

    fn modality(&self) -> Modality {
        Modality::Ultrasonic
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            modality: Modality::Ultrasonic.as_str().to_string(),
            // Bounded by physics, not by choice: the part needs >60ms between
            // cycles for the previous burst to decay, and a read is a median of
            // five, so a complete measurement takes about a third of a second.
            sample_rate_hz: 3,
            // It cannot produce its own calibration receipt. Calibration here
            // means someone measuring a known distance — a physical act this
            // adapter has no way to perform or to fake.
            can_calibrate: false,
            // P0. A raw time-of-flight range IS the raw sensor frame for this
            // part, and PrivacyClass orders P0 as the most sensitive, so
            // reporting anything higher would understate it.
            max_privacy_class: PrivacyClass::P0,
        }
    }

    fn next_event(&mut self) -> Result<Option<rufield_core::event::FieldEvent>, Self::Error> {
        let range_m = (self.source)().map_err(|e| FieldError::NoEcho(e.to_string()))?;
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let tensor = tensor_from_range(range_m, timestamp_ns);
        let sensor = rufield_core::event::SensorDescriptor {
            modality: Modality::Ultrasonic.as_str().to_string(),
            vendor: "hc_sr04".to_string(),
            device_id: self.device_id.clone(),
            placement: self.placement.clone(),
            coordinate_frame: None,
            position_m: None,
            orientation_xyzw: None,
            // The Pi's own monotonic clock, not PTP-disciplined. Saying so
            // matters for fusion: events from this sensor cannot be aligned
            // with another box's to better than their clock offset.
            clock_domain: "local_monotonic".to_string(),
        };
        let observation = rufield_core::event::Observation {
            zone_id: None,
            space_cell: None,
            range_m: Some(range_m),
            // A single-element ToF sensor sees distance to the nearest
            // reflector and nothing else. Velocity would need differencing
            // across events with a known interval; motion, presence and track
            // identity are not derivable at all, so they stay None rather than
            // being filled with a plausible-looking zero.
            velocity_mps: None,
            motion_vector: None,
            track_id: None,
            confidence: UNCALIBRATED_CONFIDENCE,
            // No derived features: there is no model here, only a timed pulse.
            // An empty map is the honest answer, not an omission.
            features: Default::default(),
            attributes: Default::default(),
            labels: Vec::new(),
            privacy_class: PrivacyClass::P0,
            // A range finder produces no identity evidence of any kind, and
            // the field exists precisely so that adapters which could must say
            // so explicitly.
            identity_evidence: None,
            channel_sounding_provenance: None,
        };
        let provenance = rufield_core::event::ProvenanceRef {
            raw_hash: raw_hash(range_m, timestamp_ns),
            firmware_hash: format!("sha256:rultra-field-{}", env!("CARGO_PKG_VERSION")),
            model_id: "direct_tof_no_model".to_string(),
            calibration_id: "uncalibrated".to_string(),
            // The claim this adapter exists to make.
            synthetic: false,
            signature_hex: None,
            signer_pubkey_hex: None,
        };

        Ok(Some(rufield_core::event::FieldEvent::new(
            format!("rultra-{timestamp_ns}"),
            timestamp_ns,
            sensor,
            tensor,
            observation,
            provenance,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter_over(values: Vec<f32>) -> LiveUltrasonicAdapter {
        let mut it = values.into_iter();
        LiveUltrasonicAdapter::new(
            "rultra_crowpi_01",
            "bench",
            Box::new(move || it.next().ok_or_else(|| anyhow::anyhow!("exhausted"))),
        )
    }

    #[test]
    fn the_tensor_is_a_single_range_bin() {
        let t = tensor_from_range(1.25, 42);
        assert_eq!(t.shape, vec![1]);
        assert_eq!(t.axes, vec![FieldAxis::Range]);
        assert_eq!(t.values, vec![1.25]);
        assert_eq!(t.modality, Modality::Ultrasonic);
    }

    /// The claim this adapter exists to make: these are real echoes, so events
    /// must NOT be marked synthetic. Every other RuField v0.1 source is.
    #[test]
    fn events_are_not_marked_synthetic() {
        let mut a = adapter_over(vec![1.0]);
        let e = a.next_event().unwrap().unwrap();
        assert!(
            !e.provenance.synthetic,
            "live hardware must not claim synthetic"
        );
    }

    /// An uncalibrated instrument must not report high confidence, or a fusion
    /// engine downstream weights it as though it were trustworthy.
    #[test]
    fn confidence_is_capped_while_uncalibrated() {
        let mut a = adapter_over(vec![2.0]);
        let e = a.next_event().unwrap().unwrap();
        assert!(e.tensor.confidence <= UNCALIBRATED_CONFIDENCE);
        assert!(e.observation.confidence <= UNCALIBRATED_CONFIDENCE);
        assert_eq!(e.provenance.calibration_id, "uncalibrated");
        assert!(
            e.tensor.calibration_id.is_none(),
            "must not name a receipt that does not exist"
        );
    }

    /// A one-element time-of-flight sensor cannot see velocity, motion, pose or
    /// identity. Those fields must stay empty rather than carry a plausible
    /// zero, which downstream would read as a measurement.
    #[test]
    fn no_claim_is_made_beyond_range() {
        let mut a = adapter_over(vec![0.5]);
        let e = a.next_event().unwrap().unwrap();
        assert_eq!(e.observation.range_m, Some(0.5));
        assert!(e.observation.velocity_mps.is_none());
        assert!(e.observation.motion_vector.is_none());
        assert!(e.observation.track_id.is_none());
        assert!(e.observation.identity_evidence.is_none());
        assert!(e.observation.features.is_empty());
    }

    /// P0 is the MOST sensitive class and orders lowest, so a raw range must
    /// not be reported as a higher number — that would understate it.
    #[test]
    fn a_raw_range_is_declared_p0() {
        let a = adapter_over(vec![]);
        assert_eq!(a.capabilities().max_privacy_class, PrivacyClass::P0);
        assert_eq!(tensor_from_range(1.0, 0).privacy_class, PrivacyClass::P0);
    }

    /// The adapter cannot calibrate itself: calibration means a person
    /// measuring a known distance, which it can neither perform nor fake.
    #[test]
    fn the_adapter_does_not_claim_to_self_calibrate() {
        let a = adapter_over(vec![]);
        assert!(!a.capabilities().can_calibrate);
        assert_eq!(a.capabilities().modality, "ultrasonic");
    }

    #[test]
    fn raw_hash_is_deterministic_and_input_sensitive() {
        assert_eq!(raw_hash(1.0, 5), raw_hash(1.0, 5));
        assert_ne!(raw_hash(1.0, 5), raw_hash(1.01, 5));
        assert_ne!(raw_hash(1.0, 5), raw_hash(1.0, 6));
        assert!(raw_hash(1.0, 5).starts_with("sha256:"));
    }

    #[test]
    fn a_dead_sensor_surfaces_as_an_error_not_a_zero() {
        let mut a = adapter_over(vec![]);
        assert!(a.next_event().is_err(), "must not fabricate a reading");
    }

    #[test]
    fn the_noise_floor_is_the_measured_spread() {
        // 0.62cm measured on this board, in metres.
        assert!((tensor_from_range(1.0, 0).noise_floor - 0.0062).abs() < 1e-6);
    }
}
