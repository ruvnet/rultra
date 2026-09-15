//! Rotating status pages on the 16x2 character LCD.
//!
//! # Why this reads from the snapshot
//!
//! The panel is refreshed from [`crate::state::latest`], never by sampling the
//! bus itself. A display that triggered its own sensor pass would make the
//! physical load a function of the refresh rate, and would drive the ultrasonic
//! every few seconds for the sake of showing a number.
//!
//! # Why an actuator write goes through a policy
//!
//! The LCD is an actuator, and this crate already learned once that an emitter
//! left in an undefined state is a problem you hear rather than see. The
//! refresh therefore holds an explicit grant for `ruv://lab/actuator/display`
//! and asks [`rultra_lab::Policy`] before every write. The grant's rate limit —
//! not a bare `sleep` — is what bounds how often the panel is touched, so a
//! future caller that refreshes from somewhere else inherits the same bound.
//!
//! # Honesty on a 16-character line
//!
//! A display has no room for provenance, which makes it the easiest place to
//! launder a bad reading into something authoritative-looking. Values from
//! sensors that are not trustworthy render as `--`. The range finder is
//! currently `Faulty` — it measures a floating pin — so it shows `--` rather
//! than a plausible distance.

use rultra_lab::{LabUri, MonoNanos, Policy};

/// Character columns on the panel. Anything longer is truncated by the
/// controller silently, so the renderer must do it deliberately instead.
pub const COLS: usize = 16;

/// How many distinct pages the rotation cycles through.
pub const PAGES: usize = 4;

/// Default seconds per page, and the range a caller may choose from.
pub const DEFAULT_SECS: u64 = 15;
pub const MIN_SECS: u64 = 5;
pub const MAX_SECS: u64 = 120;

// Compile-time, so an edit that makes the default unreachable or the floor
// fast enough to flicker fails the build rather than a test run.
const _: () = assert!(MIN_SECS <= DEFAULT_SECS && DEFAULT_SECS <= MAX_SECS);
const _: () = assert!(MIN_SECS >= 5);

/// The numbers a page may show. Every optional field means "no trustworthy
/// value", which is rendered as `--` rather than guessed at.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub temp_c: Option<f64>,
    pub lux: Option<f64>,
    pub light_band: Option<&'static str>,
    /// `None` when the range finder is Faulty, which it currently is.
    pub range_cm: Option<f64>,
    pub devices_working: usize,
    pub devices_total: usize,
    pub witness_entries: usize,
    pub uptime_s: u64,
}

/// Pad or truncate to exactly the panel width.
fn fit(s: &str) -> String {
    let mut out: String = s.chars().take(COLS).collect();
    while out.chars().count() < COLS {
        out.push(' ');
    }
    out
}

/// Right-align a value in a labelled row, so digits do not jitter between
/// refreshes as the number of significant figures changes.
fn row(label: &str, value: &str) -> String {
    let label: String = label.chars().take(COLS).collect();
    let room = COLS.saturating_sub(label.chars().count());
    let value: String = value
        .chars()
        .rev()
        .take(room)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let pad = room - value.chars().count();
    fit(&format!("{label}{}{value}", " ".repeat(pad)))
}

fn num(v: Option<f64>, dp: usize, unit: &str) -> String {
    match v {
        Some(x) => format!("{x:.dp$}{unit}"),
        // The one string that must never be a number.
        None => "--".to_string(),
    }
}

fn uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Render one page as two exactly-`COLS`-wide lines.
pub fn render(s: &Stats, page: usize) -> (String, String) {
    match page % PAGES {
        0 => (
            row("rultra", &num(s.temp_c, 1, "C")),
            row("up", &uptime(s.uptime_s)),
        ),
        1 => (
            row("light", &num(s.lux, 0, "lx")),
            row("band", s.light_band.unwrap_or("--")),
        ),
        2 => (
            row(
                "devices",
                &format!("{}/{}", s.devices_working, s.devices_total),
            ),
            row("witness", &s.witness_entries.to_string()),
        ),
        _ => (
            row("range", &num(s.range_cm, 1, "cm")),
            // Says why, in the space available. A blank second line would read
            // as "nothing to report" rather than "this sensor is condemned".
            fit(if s.range_cm.is_none() {
                "sensor faulty"
            } else {
                ""
            }),
        ),
    }
}

/// Seconds per page, from `RULTRA_LCD_SECS`, clamped to a sane range.
pub fn interval_secs() -> u64 {
    std::env::var("RULTRA_LCD_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECS)
        .clamp(MIN_SECS, MAX_SECS)
}

/// The address this refresher drives.
pub fn display_uri() -> LabUri {
    LabUri::actuator("display", None)
}

/// A policy granting exactly this one actuator at the refresh rate.
///
/// Built here rather than passed in so the grant and the cadence cannot drift
/// apart: the rate limit *is* the refresh interval.
pub fn refresh_policy(secs: u64) -> Policy {
    Policy::new().grant(display_uri(), secs.saturating_mul(1_000_000_000))
}

/// Read the host's uptime in seconds.
pub fn host_uptime_s() -> u64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split('.').next().and_then(|n| n.parse().ok()))
        .unwrap_or(0)
}

/// Build the display stats from the current snapshot.
///
/// Applies the same trust rule the API does — a `Faulty` sensor contributes
/// nothing — so the panel and `/api/summary` can never disagree about whether
/// a reading is real.
pub fn stats_from_snapshot() -> Stats {
    use rultra_sense::{device, DeviceId, Verification};
    let snap = crate::state::latest();
    let verification = |id| {
        device::lookup(id)
            .map(|d| d.verification)
            .unwrap_or(Verification::Untested)
    };
    let lux = rultra_spatial::usable(
        crate::state::snap_scalar(&snap, DeviceId::Light),
        verification(DeviceId::Light),
    );
    Stats {
        temp_c: rultra_spatial::usable(
            crate::state::snap_scalar(&snap, DeviceId::CpuTemp),
            verification(DeviceId::CpuTemp),
        ),
        lux,
        light_band: lux.map(|l| match rultra_spatial::Light::from_lux(l) {
            rultra_spatial::Light::Dark => "dark",
            rultra_spatial::Light::Dim => "dim",
            rultra_spatial::Light::Lit => "lit",
            rultra_spatial::Light::Bright => "bright",
        }),
        range_cm: rultra_spatial::usable(
            crate::state::snap_scalar(&snap, DeviceId::Range),
            verification(DeviceId::Range),
        ),
        devices_working: device::CATALOG
            .iter()
            .filter(|d| d.verification == Verification::Working)
            .count(),
        devices_total: device::CATALOG.len(),
        witness_entries: std::fs::read_to_string(crate::state::state_dir().join("witness.jsonl"))
            .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0),
        uptime_s: host_uptime_s(),
    }
}

/// Start the rotation. A no-op unless `RULTRA_LCD` is set to something other
/// than `off`, because the panel is `AcksButSilent` on this box and a refresher
/// nobody asked for would write to hardware every few seconds for no observable
/// benefit.
pub fn spawn() {
    if std::env::var("RULTRA_LCD")
        .unwrap_or_default()
        .eq_ignore_ascii_case("off")
    {
        return;
    }
    let secs = interval_secs();
    tokio::spawn(async move {
        let mut policy = refresh_policy(secs);
        let uri = display_uri();
        let start = std::time::Instant::now();
        let mut page = 0usize;
        loop {
            let now = MonoNanos(start.elapsed().as_nanos() as u64);
            if policy.permit(&uri, now).is_allowed() {
                let (l1, l2) = render(&stats_from_snapshot(), page);
                write_panel(l1, l2).await;
                page = page.wrapping_add(1);
            }
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
    });
}

#[cfg(all(target_os = "linux", feature = "hardware"))]
async fn write_panel(l1: String, l2: String) {
    // Off the async runtime: the HD44780 wake-up sequence sleeps ~60ms, which
    // would park a tokio worker on a four-core Pi.
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(b) = rultra_sense::backend::linux::LinuxBackend::open() {
            let _ = b.lcd_write(&l1, &l2);
        }
    })
    .await;
}

#[cfg(not(all(target_os = "linux", feature = "hardware")))]
async fn write_panel(_l1: String, _l2: String) {}
#[cfg(test)]
mod tests {
    use super::*;
    use rultra_lab::{Decision, DenyReason};

    fn full() -> Stats {
        Stats {
            temp_c: Some(72.15),
            lux: Some(231.6),
            light_band: Some("lit"),
            range_cm: None, // Faulty, as on the real box
            devices_working: 5,
            devices_total: 9,
            witness_entries: 257,
            uptime_s: 245_893,
        }
    }

    #[test]
    fn every_page_fits_the_panel_exactly() {
        for p in 0..PAGES {
            let (a, b) = render(&full(), p);
            assert_eq!(a.chars().count(), COLS, "page {p} line 1: {a:?}");
            assert_eq!(b.chars().count(), COLS, "page {p} line 2: {b:?}");
        }
    }

    #[test]
    fn a_long_value_is_truncated_rather_than_overflowing() {
        let s = Stats {
            witness_entries: 999_999_999_999,
            ..full()
        };
        let (a, b) = render(&s, 2);
        assert_eq!(a.chars().count(), COLS);
        assert_eq!(b.chars().count(), COLS);
    }

    #[test]
    fn a_condemned_sensor_shows_dashes_not_a_plausible_number() {
        // The whole point: 16 characters have no room for provenance, so an
        // untrustworthy value must not be displayed as if it were measured.
        let (l1, l2) = render(&full(), 3);
        assert!(l1.contains("--"), "range must render as dashes: {l1:?}");
        assert!(!l1.chars().any(|c| c.is_ascii_digit()), "no digits: {l1:?}");
        assert!(l2.contains("faulty"), "second line must say why: {l2:?}");
    }

    #[test]
    fn a_trustworthy_sensor_shows_its_value() {
        let s = Stats {
            range_cm: Some(120.0),
            ..full()
        };
        let (l1, l2) = render(&s, 3);
        assert!(l1.contains("120.0cm"), "{l1:?}");
        assert!(l2.trim().is_empty(), "no fault note when there is no fault");
    }

    #[test]
    fn missing_values_never_render_as_zero() {
        // Zero is a measurement. Absent is not.
        let s = Stats {
            temp_c: None,
            lux: None,
            light_band: None,
            ..Default::default()
        };
        let (a, _) = render(&s, 0);
        assert!(a.contains("--") && !a.contains('0'), "{a:?}");
        let (c, d) = render(&s, 1);
        assert!(c.contains("--"), "{c:?}");
        assert!(d.contains("--"), "{d:?}");
    }

    #[test]
    fn the_rotation_visits_every_page_then_repeats() {
        let seen: Vec<_> = (0..PAGES * 2).map(|i| render(&full(), i)).collect();
        assert_eq!(seen[0], seen[PAGES], "rotation must wrap");
        let distinct: std::collections::HashSet<_> = seen[..PAGES].iter().collect();
        assert_eq!(distinct.len(), PAGES, "pages must differ from each other");
    }

    #[test]
    fn uptime_reads_naturally_at_each_scale() {
        assert_eq!(uptime(245_893), "2d 20h 18m");
        assert_eq!(uptime(3_700), "1h 1m");
        assert_eq!(uptime(90), "1m");
    }

    #[test]
    fn the_grant_rate_limit_is_the_refresh_interval() {
        // If these could drift apart, the policy would stop bounding anything.
        let mut p = refresh_policy(15);
        let u = display_uri();
        assert!(p.permit(&u, MonoNanos::from_millis(0)).is_allowed());
        assert!(!p.permit(&u, MonoNanos::from_millis(14_999)).is_allowed());
        assert!(p.permit(&u, MonoNanos::from_millis(15_000)).is_allowed());
    }

    #[test]
    fn nothing_but_the_display_is_granted() {
        let mut p = refresh_policy(15);
        for other in ["ruv://lab/actuator/relay/1", "ruv://lab/actuator/buzzer"] {
            let u = LabUri::parse(other).unwrap();
            assert_eq!(
                p.permit(&u, MonoNanos(0)),
                Decision::Deny {
                    reason: DenyReason::NotGranted
                },
                "{other} must not be reachable from a display refresher"
            );
        }
    }
}
