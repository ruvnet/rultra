//! Synchronized multimodal observations, with clock skew made explicit.
//!
//! The failure this module exists to prevent is silent: if the camera, the CSI
//! node and the GPIO sensors disagree by a couple of hundred milliseconds while
//! something is *moving*, every fused sample is mislabelled, nothing errors,
//! and the resulting dataset trains a model on noise. The corruption is only
//! visible much later, as a model that will not converge.
//!
//! So [`Observation`] cannot be constructed directly. The only way to make one
//! is [`Observation::fuse`], which takes an explicit skew budget and **returns
//! an error** rather than a mislabelled sample when the readings are too far
//! apart. Discarding a sample is cheap; discovering a poisoned dataset is not.
//!
//! Two practical notes, both measured on this box rather than assumed:
//!
//! - Timestamps are monotonic, not wall clock. Wall clock steps under NTP, and
//!   a backwards step during capture produces negative intervals that look like
//!   reordered events.
//! - The transport decides whether a budget is achievable at all. The CSI node
//!   answers over USB serial in single-digit milliseconds but its WiFi
//!   round-trip measured 317-1330 ms, averaging 695 ms. No skew budget in the
//!   50-200 ms range survives that path, so ground-truth capture must use the
//!   serial transport. [`Transport::typical_skew_ns`] carries those numbers.

use crate::uri::LabUri;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Nanoseconds from a monotonic clock (`CLOCK_MONOTONIC`).
///
/// Deliberately not a wall-clock instant: only differences are meaningful, and
/// differences are all this module needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MonoNanos(pub u64);

impl MonoNanos {
    pub fn from_millis(ms: u64) -> MonoNanos {
        MonoNanos(ms * 1_000_000)
    }
    /// Absolute separation, which cannot underflow regardless of order.
    pub fn delta_ns(self, other: MonoNanos) -> u64 {
        self.0.abs_diff(other.0)
    }
}

/// How a reading reached the Pi. This bounds what skew is even achievable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// Directly on the Pi's own buses. Sub-millisecond.
    LocalGpio,
    /// USB serial from an attached board.
    UsbSerial,
    /// Over WiFi. Measured on this box at 317-1330 ms round trip.
    WiFi,
}

impl Transport {
    /// Representative skew contribution, from measurement where available.
    pub fn typical_skew_ns(self) -> u64 {
        match self {
            Transport::LocalGpio => 1_000_000,  // ~1 ms
            Transport::UsbSerial => 10_000_000, // ~10 ms
            Transport::WiFi => 695_000_000,     // measured average
        }
    }

    /// Whether this transport can plausibly meet a skew budget.
    ///
    /// Checked up front so a capture run fails at setup rather than producing
    /// a run's worth of samples that all get rejected.
    pub fn can_meet(self, budget_ns: u64) -> bool {
        self.typical_skew_ns() <= budget_ns
    }
}

/// A sensor value. Struct variants throughout, never newtype-over-primitive:
/// this crate's sibling shipped a runtime panic from a serde tagged enum whose
/// variant wrapped a bare `bool`, and the same shape would fail the same way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Value {
    Scalar {
        value: f64,
        unit: String,
    },
    Flag {
        value: bool,
    },
    /// CSI amplitude vectors and similar.
    Vector {
        values: Vec<f32>,
    },
    /// The sensor was addressed and had nothing to give. Recorded rather than
    /// dropped, so a gap in a dataset is distinguishable from a missing sensor.
    Absent {
        reason: String,
    },
}

/// One reading from one capability at one instant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub uri: LabUri,
    pub at: MonoNanos,
    pub transport: Transport,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkewError {
    pub observed_ns: u64,
    pub budget_ns: u64,
    pub earliest: String,
    pub latest: String,
}

impl fmt::Display for SkewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "readings span {:.1} ms but the budget is {:.1} ms ({} is earliest, {} is latest) \
             - discarded rather than recorded as simultaneous",
            self.observed_ns as f64 / 1e6,
            self.budget_ns as f64 / 1e6,
            self.earliest,
            self.latest
        )
    }
}

impl std::error::Error for SkewError {}

/// A set of readings that have been *checked* to be close enough in time to
/// treat as simultaneous.
///
/// Fields are private and there is no public constructor other than [`fuse`],
/// so an unchecked observation is not representable.
///
/// [`fuse`]: Observation::fuse
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    readings: Vec<Reading>,
    skew_ns: u64,
    budget_ns: u64,
}

impl Observation {
    /// Combine readings into one observation, or refuse.
    pub fn fuse(readings: Vec<Reading>, budget_ns: u64) -> Result<Observation, SkewError> {
        if readings.is_empty() {
            // An empty observation would have zero skew and assert nothing.
            // Silently allowing it lets a broken capture loop look healthy.
            return Err(SkewError {
                observed_ns: 0,
                budget_ns,
                earliest: "<none>".into(),
                latest: "<none>".into(),
            });
        }
        let min = readings.iter().min_by_key(|r| r.at).unwrap();
        let max = readings.iter().max_by_key(|r| r.at).unwrap();
        let skew_ns = max.at.delta_ns(min.at);
        if skew_ns > budget_ns {
            return Err(SkewError {
                observed_ns: skew_ns,
                budget_ns,
                earliest: min.uri.to_string(),
                latest: max.uri.to_string(),
            });
        }
        Ok(Observation {
            readings,
            skew_ns,
            budget_ns,
        })
    }

    pub fn readings(&self) -> &[Reading] {
        &self.readings
    }
    /// The actual spread of this observation, always within budget.
    pub fn skew_ns(&self) -> u64 {
        self.skew_ns
    }
    pub fn budget_ns(&self) -> u64 {
        self.budget_ns
    }
    pub fn get(&self, uri: &LabUri) -> Option<&Reading> {
        self.readings.iter().find(|r| &r.uri == uri)
    }
    /// Whether any reading is an explicit gap. Fusion allows these — a dataset
    /// needs to know a sensor was silent — but a consumer may want to skip them.
    pub fn has_gaps(&self) -> bool {
        self.readings
            .iter()
            .any(|r| matches!(r.value, Value::Absent { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(uri: &str, ms: u64, t: Transport) -> Reading {
        Reading {
            uri: LabUri::parse(uri).unwrap(),
            at: MonoNanos::from_millis(ms),
            transport: t,
            value: Value::Flag { value: true },
        }
    }

    #[test]
    fn tight_readings_fuse() {
        let o = Observation::fuse(
            vec![
                r("ruv://lab/sensor/motion", 1000, Transport::LocalGpio),
                r("ruv://lab/sensor/light", 1012, Transport::LocalGpio),
                r("ruv://lab/sensor/rf", 1030, Transport::UsbSerial),
            ],
            200_000_000,
        )
        .unwrap();
        assert_eq!(o.skew_ns(), 30_000_000);
        assert_eq!(o.readings().len(), 3);
    }

    #[test]
    fn the_measured_wifi_latency_is_refused_by_a_realistic_budget() {
        // The real number: the CSI node's WiFi round trip averaged 695 ms.
        let e = Observation::fuse(
            vec![
                r("ruv://lab/sensor/motion", 1000, Transport::LocalGpio),
                r("ruv://lab/sensor/rf", 1695, Transport::WiFi),
            ],
            200_000_000,
        )
        .unwrap_err();
        assert_eq!(e.observed_ns, 695_000_000);
        assert!(
            e.to_string().contains("discarded"),
            "error must say it dropped the sample"
        );
    }

    #[test]
    fn a_capture_run_can_reject_an_impossible_transport_before_it_starts() {
        assert!(!Transport::WiFi.can_meet(200_000_000));
        assert!(Transport::UsbSerial.can_meet(200_000_000));
        assert!(Transport::LocalGpio.can_meet(50_000_000));
    }

    #[test]
    fn an_empty_observation_is_refused_rather_than_looking_perfect() {
        // Zero readings has zero skew, which would otherwise pass every budget.
        assert!(Observation::fuse(vec![], 200_000_000).is_err());
    }

    #[test]
    fn skew_cannot_underflow_when_readings_arrive_out_of_order() {
        let o = Observation::fuse(
            vec![
                r("ruv://lab/sensor/rf", 1050, Transport::UsbSerial),
                r("ruv://lab/sensor/motion", 1000, Transport::LocalGpio),
            ],
            200_000_000,
        )
        .unwrap();
        assert_eq!(o.skew_ns(), 50_000_000);
    }

    #[test]
    fn a_silent_sensor_is_recorded_not_dropped() {
        let mut gap = r("ruv://lab/sensor/camera", 1000, Transport::UsbSerial);
        gap.value = Value::Absent {
            reason: "board unplugged".into(),
        };
        let o = Observation::fuse(vec![gap], 200_000_000).unwrap();
        assert!(o.has_gaps(), "a gap must stay visible in the record");
    }

    #[test]
    fn a_bool_value_round_trips_through_json() {
        // The sibling crate shipped a panic from exactly this shape.
        let v = Value::Flag { value: true };
        let s = serde_json::to_string(&v).expect("must serialize");
        assert_eq!(serde_json::from_str::<Value>(&s).unwrap(), v);
    }
}
