//! Shared helpers for reading the box's real state.

use rultra_evolve::policy::Applier;
use rultra_sense::{backend::cache::CachingBackend, Backend, DeviceId, Value};
use std::sync::{Mutex, OnceLock};

/// Where the cycle keeps its policy and chain. Mirrors the `rultra` binary so
/// the console and the CLI cannot disagree about what is in force.
pub fn state_dir() -> std::path::PathBuf {
    std::env::var("RULTRA_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/rultra"))
}

pub fn applier() -> Applier {
    Applier::new(state_dir().join("policy.json"))
}

fn open_backend() -> Box<dyn Backend + Send> {
    #[cfg(all(target_os = "linux", feature = "hardware"))]
    {
        if let Ok(b) = rultra_sense::backend::linux::LinuxBackend::open() {
            return Box::new(CachingBackend::new(b));
        }
    }
    Box::new(CachingBackend::new(
        rultra_sense::backend::mock::MockBackend::crowpi(),
    ))
}

/// One backend for the whole process, wrapped in a cache.
///
/// Process-lifetime rather than per-request, which is the entire point: a cache
/// rebuilt for each request caches nothing, and two viewers would each fire the
/// ultrasonic transducer independently for identical information.
fn shared() -> &'static Mutex<Box<dyn Backend + Send>> {
    static B: OnceLock<Mutex<Box<dyn Backend + Send>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(open_backend()))
}

/// The most recent successful reading of every sensor, plus the last probe.
///
/// Handlers read THIS, never the hardware. That is the whole design: a request
/// that touches a sensor waits on physics, and with one lock in front of the
/// bus a background sweep holding it for ~400ms puts every concurrent request
/// behind that sweep. Measured at the console's real 3s cadence the mean was
/// 263ms — better than the original 458ms, but still latency a dashboard should
/// never pay.
///
/// Separating them makes the two costs independent: sampling happens at a fixed
/// cadence on one blocking thread, and serving is a read lock over a HashMap.
#[derive(Default, Clone)]
pub struct Snapshot {
    /// Latest reading per device, carrying its original measurement time.
    pub readings: std::collections::HashMap<DeviceId, rultra_sense::Reading>,
    /// Latest probe result.
    pub presence: Vec<rultra_sense::Presence>,
    /// When the last sweep completed, unix seconds. Zero before the first.
    pub swept_at: u64,
}

fn snapshot() -> &'static std::sync::RwLock<Snapshot> {
    static S: OnceLock<std::sync::RwLock<Snapshot>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// The latest snapshot. Cheap: a read lock and a clone of small maps.
pub fn latest() -> Snapshot {
    snapshot().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A scalar from the snapshot.
pub fn snap_scalar(s: &Snapshot, id: DeviceId) -> Option<f64> {
    match s.readings.get(&id)?.value {
        Value::Scalar { n, .. } => Some(n),
        _ => None,
    }
}

/// Run a closure against the shared backend on a blocking thread.
///
/// `spawn_blocking` because an uncached pass over the sensors costs ~460ms of
/// genuine waiting — the BH1750's conversion time and the ultrasonic's settling
/// between bursts. Doing that inside an async handler would park a tokio worker
/// for half a second and stall every other request on the runtime, which on a
/// four-worker Pi is most of them.
pub async fn with_backend<T, F>(f: F) -> T
where
    T: Send + 'static,
    F: FnOnce(&mut dyn Backend) -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut guard = shared().lock().unwrap_or_else(|e| e.into_inner());
        f(guard.as_mut())
    })
    .await
    .expect("sensor task panicked")
}

/// Keep the cache warm in the background.
///
/// Without this the cache only helps when readers overlap: the console polls
/// every 3s against a 1s TTL, so a single viewer misses every time and pays the
/// full ~460ms. A refresher inverts the relationship — physical sampling
/// happens at a fixed cadence no matter how many readers there are, so cost is
/// bounded by the box rather than by demand, and every request is served from
/// memory in about a millisecond.
///
/// The cadence is deliberately just under the TTL so an entry is replaced
/// slightly before it expires, leaving no window where a reader arrives to find
/// nothing fresh.
pub fn spawn_refresher() {
    let period = std::time::Duration::from_millis(800);
    tokio::spawn(async move {
        loop {
            // Build the new snapshot OFF the shared lock, then swap it in.
            // Readers are blocked only for the swap, not for the I/O.
            let swept = with_backend(|b| {
                let mut readings = std::collections::HashMap::new();
                for d in rultra_sense::device::CATALOG
                    .iter()
                    // Sensors only: reading an actuator would drive it, which
                    // is exactly the bug the DeviceKind split exists to stop.
                    .filter(|d| d.kind == rultra_sense::DeviceKind::Sensor)
                {
                    if let Ok(r) = b.read(d.id) {
                        readings.insert(d.id, r);
                    }
                }
                Snapshot {
                    readings,
                    presence: b.probe().unwrap_or_default(),
                    swept_at: rultra_sense::now(),
                }
            })
            .await;

            // A sweep that read nothing must not blank a good snapshot: a
            // transient bus error would otherwise empty the console.
            if !swept.readings.is_empty() || !swept.presence.is_empty() {
                *snapshot().write().unwrap_or_else(|e| e.into_inner()) = swept;
            }
            tokio::time::sleep(period).await;
        }
    });
}
