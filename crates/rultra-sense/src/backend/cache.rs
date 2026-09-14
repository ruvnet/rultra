//! A caching wrapper around any [`Backend`].
//!
//! # Why this exists
//!
//! Physical reads are expensive in wall-clock terms and the cost is fixed by
//! the parts, not the code: the BH1750 needs a 180ms one-shot conversion, and
//! an HC-SR04 read is a median of five bursts with the datasheet's 60ms
//! settling between them. Measured on this board, one pass over the sensors
//! costs about 460ms.
//!
//! The console polls every three seconds, the MCP server reads on demand, and
//! the cycle samples in windows. Without a cache each of those independently
//! fires the transducer, so a second viewer doubles the physical work for no
//! extra information — and an ultrasonic transducer driven continuously is
//! wear, not just latency.
//!
//! # The honest part
//!
//! A cached reading keeps the `at` of the moment it was **measured**, never the
//! moment it was served. A consumer comparing `at` to now sees the true age.
//! Restamping it would make a stale reading indistinguishable from a fresh one,
//! which is the same failure as presenting an unvalidated reading as a
//! measurement — the project's whole objection, one layer down.

use crate::{Backend, DeviceId, Presence, Reading};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a reading stays fresh.
///
/// Chosen against the physics, not preference: one pass over the sensors costs
/// ~460ms, so a window shorter than that guarantees every request pays full
/// price and the cache does nothing. 1s is comfortably longer while staying
/// well inside the 3s console poll, so the displayed value is never more than
/// one poll behind the world.
pub const DEFAULT_TTL: Duration = Duration::from_millis(1000);

/// Wraps a backend and serves recent readings from memory.
pub struct CachingBackend<B: Backend> {
    inner: B,
    ttl: Duration,
    readings: HashMap<DeviceId, (Instant, Reading)>,
    presence: Option<(Instant, Vec<Presence>)>,
    hits: u64,
    misses: u64,
}

impl<B: Backend> CachingBackend<B> {
    /// Wrap with the default TTL.
    pub fn new(inner: B) -> Self {
        Self::with_ttl(inner, DEFAULT_TTL)
    }

    /// Wrap with an explicit TTL. A zero TTL disables caching entirely, which
    /// is the right setting for a measurement window that must sample the
    /// world rather than its own memory.
    pub fn with_ttl(inner: B, ttl: Duration) -> Self {
        Self {
            inner,
            ttl,
            readings: HashMap::new(),
            presence: None,
            hits: 0,
            misses: 0,
        }
    }

    /// Cache hits and misses since construction, for diagnostics.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    fn fresh<T>(&self, slot: &Option<(Instant, T)>) -> bool {
        slot.as_ref()
            .map(|(t, _)| t.elapsed() < self.ttl)
            .unwrap_or(false)
    }
}

impl<B: Backend> Backend for CachingBackend<B> {
    fn probe(&mut self) -> anyhow::Result<Vec<Presence>> {
        if self.fresh(&self.presence) {
            self.hits += 1;
            // Safe: `fresh` just confirmed it is populated.
            return Ok(self
                .presence
                .as_ref()
                .expect("fresh implies present")
                .1
                .clone());
        }
        self.misses += 1;
        let p = self.inner.probe()?;
        self.presence = Some((Instant::now(), p.clone()));
        Ok(p)
    }

    fn read(&mut self, id: DeviceId) -> anyhow::Result<Reading> {
        if let Some((t, r)) = self.readings.get(&id) {
            if t.elapsed() < self.ttl {
                self.hits += 1;
                // `at` is preserved from the original measurement on purpose.
                return Ok(r.clone());
            }
        }
        self.misses += 1;
        // A failed read is NOT cached: a sensor that has just come back would
        // otherwise stay "broken" for a full TTL, and an error is not an
        // observation worth remembering.
        let r = self.inner.read(id)?;
        self.readings.insert(id, (Instant::now(), r.clone()));
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::MockBackend;
    use crate::{Value, Verification};

    /// A backend that counts physical reads, so the test measures avoided work
    /// rather than elapsed time.
    struct Counting {
        inner: MockBackend,
        reads: std::rc::Rc<std::cell::Cell<u32>>,
    }
    impl Backend for Counting {
        fn probe(&mut self) -> anyhow::Result<Vec<Presence>> {
            self.inner.probe()
        }
        fn read(&mut self, id: DeviceId) -> anyhow::Result<Reading> {
            self.reads.set(self.reads.get() + 1);
            self.inner.read(id)
        }
    }
    fn counting() -> (Counting, std::rc::Rc<std::cell::Cell<u32>>) {
        let c = std::rc::Rc::new(std::cell::Cell::new(0));
        (
            Counting {
                inner: MockBackend::crowpi(),
                reads: c.clone(),
            },
            c,
        )
    }

    #[test]
    fn repeated_reads_hit_the_hardware_once() {
        let (b, n) = counting();
        let mut c = CachingBackend::new(b);
        for _ in 0..10 {
            c.read(DeviceId::Light).unwrap();
        }
        assert_eq!(n.get(), 1, "ten reads should cost one physical read");
        assert_eq!(c.stats(), (9, 1));
    }

    #[test]
    fn different_devices_are_cached_independently() {
        let (b, n) = counting();
        let mut c = CachingBackend::new(b);
        c.read(DeviceId::Light).unwrap();
        c.read(DeviceId::CpuTemp).unwrap();
        c.read(DeviceId::Light).unwrap();
        assert_eq!(n.get(), 2, "one read each, then a hit");
    }

    #[test]
    fn an_expired_entry_is_refetched() {
        let (b, n) = counting();
        let mut c = CachingBackend::with_ttl(b, Duration::from_millis(1));
        c.read(DeviceId::Light).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        c.read(DeviceId::Light).unwrap();
        assert_eq!(n.get(), 2);
    }

    /// The honesty property: a cached reading must not claim to be newer than
    /// the measurement it came from.
    #[test]
    fn a_cached_reading_keeps_its_original_timestamp() {
        let (b, _) = counting();
        let mut c = CachingBackend::new(b);
        let first = c.read(DeviceId::Light).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let second = c.read(DeviceId::Light).unwrap();
        assert_eq!(first.at, second.at, "serving must not restamp the reading");
        assert_eq!(first.value, second.value);
    }

    /// A zero TTL is the right setting for a measurement window, which must
    /// sample the world rather than its own memory.
    #[test]
    fn a_zero_ttl_disables_caching() {
        let (b, n) = counting();
        let mut c = CachingBackend::with_ttl(b, Duration::ZERO);
        for _ in 0..4 {
            c.read(DeviceId::Light).unwrap();
        }
        assert_eq!(n.get(), 4);
    }

    /// A sensor that has just recovered must not stay "broken" for a full TTL.
    #[test]
    fn failures_are_not_cached() {
        let (b, n) = counting();
        let mut c = CachingBackend::new(b);
        assert!(
            c.read(DeviceId::Matrix).is_err(),
            "actuator is not readable"
        );
        assert!(c.read(DeviceId::Matrix).is_err());
        assert_eq!(n.get(), 2, "an error must be retried, not remembered");
    }

    #[test]
    fn probe_is_cached_too() {
        let mut c = CachingBackend::new(MockBackend::crowpi());
        let a = c.probe().unwrap();
        let b = c.probe().unwrap();
        assert_eq!(a.len(), b.len());
        assert_eq!(c.stats().0, 1, "second probe should be a hit");
    }

    #[test]
    fn caching_preserves_verification() {
        let mut c = CachingBackend::new(MockBackend::crowpi());
        let r = c.read(DeviceId::Light).unwrap();
        assert_eq!(r.verification, Verification::Working);
        assert!(matches!(r.value, Value::Scalar { .. }));
    }
}
