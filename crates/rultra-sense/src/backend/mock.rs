//! A deterministic in-memory backend.
//!
//! Exists so the unified surface is testable without a CrowPi attached. It is
//! deterministic rather than random: a test that fails must fail reproducibly.

use crate::{device, now, Backend, DeviceId, Presence, Reading, Value, Verification};
use std::collections::HashMap;

/// Scripted backend. Devices not explicitly present are reported absent.
#[derive(Debug, Default)]
pub struct MockBackend {
    present: HashMap<DeviceId, Value>,
    /// Incremented on every read, so successive reads differ predictably.
    tick: u64,
}

impl MockBackend {
    /// An empty board: nothing responds.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A board mirroring what this project has actually verified on hardware.
    pub fn crowpi() -> Self {
        let mut b = Self::default();
        b.attach(
            DeviceId::Light,
            Value::Scalar {
                n: 58.3,
                unit: "lux".into(),
            },
        );
        b.attach(
            DeviceId::CpuTemp,
            Value::Scalar {
                n: 47.2,
                unit: "celsius".into(),
            },
        );
        b.attach(
            DeviceId::Range,
            Value::Scalar {
                n: 42.0,
                unit: "centimetre".into(),
            },
        );
        b.attach(DeviceId::Buttons, Value::Bool { on: false });
        b.attach(DeviceId::Tilt, Value::Bool { on: false });
        b
    }

    /// Make a device respond with `v`.
    pub fn attach(&mut self, id: DeviceId, v: Value) -> &mut Self {
        self.present.insert(id, v);
        self
    }
}

impl Backend for MockBackend {
    fn probe(&mut self) -> anyhow::Result<Vec<Presence>> {
        Ok(device::CATALOG
            .iter()
            .map(|d| {
                let responding = self.present.contains_key(&d.id);
                Presence {
                    device: d.id,
                    responding,
                    verification: d.verification,
                    detail: if responding {
                        format!("mock: {} responding", d.part)
                    } else {
                        format!("mock: {} absent", d.part)
                    },
                }
            })
            .collect())
    }

    fn read(&mut self, id: DeviceId) -> anyhow::Result<Reading> {
        let v = self
            .present
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{id:?} is not attached to this mock backend"))?;
        self.tick += 1;
        // Nudge scalars so consecutive reads are distinguishable but bounded.
        let value = match v {
            Value::Scalar { n, unit } => Value::Scalar {
                n: n + (self.tick % 3) as f64 * 0.1,
                unit,
            },
            other => other,
        };
        Ok(Reading {
            device: id,
            at: now(),
            value,
            verification: device::lookup(id)
                .map(|d| d.verification)
                .unwrap_or(Verification::Untested),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant must survive a JSON round-trip. A tagged enum accepts
    /// shapes at compile time that it cannot serialize at runtime, so this is
    /// checked rather than assumed.
    #[test]
    fn every_value_variant_round_trips_through_json() {
        for v in [
            Value::Scalar {
                n: 1.5,
                unit: "lux".into(),
            },
            Value::Bool { on: true },
            Value::Count { n: 7 },
        ] {
            let s = serde_json::to_string(&v).expect("must serialize");
            let back: Value = serde_json::from_str(&s).expect("must deserialize");
            assert_eq!(v, back);
        }
    }

    #[test]
    fn empty_board_reports_everything_absent() {
        let mut b = MockBackend::empty();
        let p = b.probe().unwrap();
        assert_eq!(p.len(), device::CATALOG.len());
        assert!(p.iter().all(|x| !x.responding));
    }

    #[test]
    fn reading_an_absent_device_errors_rather_than_fabricating() {
        let mut b = MockBackend::empty();
        assert!(b.read(DeviceId::Light).is_err());
    }

    #[test]
    fn crowpi_profile_reports_the_verified_devices() {
        let mut b = MockBackend::crowpi();
        let p = b.probe().unwrap();
        let up: Vec<_> = p
            .iter()
            .filter(|x| x.responding)
            .map(|x| x.device)
            .collect();
        assert!(up.contains(&DeviceId::Light));
        assert!(up.contains(&DeviceId::CpuTemp));
    }

    /// Probing must not launder an unverified device into a verified reading.
    #[test]
    fn probe_preserves_catalog_verification() {
        let mut b = MockBackend::crowpi();
        for p in b.probe().unwrap() {
            let cat = device::lookup(p.device).unwrap();
            assert_eq!(p.verification, cat.verification);
        }
    }

    #[test]
    fn successive_reads_differ_but_stay_bounded() {
        let mut b = MockBackend::crowpi();
        let a = b.read(DeviceId::Light).unwrap();
        let c = b.read(DeviceId::Light).unwrap();
        match (a.value, c.value) {
            (Value::Scalar { n: x, .. }, Value::Scalar { n: y, .. }) => {
                assert!((x - y).abs() < 1.0, "mock drifted too far: {x} vs {y}");
            }
            _ => panic!("expected scalars"),
        }
    }
}
