//! Runnable examples, configuration writes, self-test, and LLM interpretation.

use crate::state;
use rultra_sense::{device, Verification};
use serde::Deserialize;
use serde_json::{json, Value as J};

/// A demonstration the box can actually perform.
///
/// Each one is real hardware or real computation — none simulate. An example
/// that fakes its effect teaches the wrong thing about what the board can do.
pub struct Example {
    pub id: &'static str,
    pub title: &'static str,
    pub blurb: &'static str,
    /// What it needs. Listed so an example that cannot run says why, rather
    /// than failing with a driver error the reader cannot interpret.
    pub needs: &'static str,
}

pub const EXAMPLES: &[Example] = &[
    Example { id: "sonar", title: "Live sonar", needs: "range + matrix",
        blurb: "Rings on the panel grow as something approaches. The screen and the panel show the same figure." },
    Example { id: "readout", title: "Sensor readout", needs: "light + temp + matrix",
        blurb: "Scrolls the current lux and die temperature across the matrix." },
    Example { id: "status", title: "LCD status", needs: "lcd",
        blurb: "Writes live readings to the character display. Also the clearest test of whether the LCD works." },
    Example { id: "heartbeat", title: "Heartbeat", needs: "matrix",
        blurb: "Draws the heart. The first thing this board ever displayed." },
    Example { id: "thermal", title: "Thermal sweep", needs: "temp",
        blurb: "Samples the die ten times and reports the spread — the measurement the governed loop runs on." },
    Example { id: "chain", title: "Verify the chain", needs: "witness chain",
        blurb: "Re-verifies every signature and hash link on disk, from genesis." },
];

/// Run one example. Returns what it did, or why it could not.
pub async fn run_example(id: &str) -> J {
    match id {
        "heartbeat" => drive_matrix("heart", None),
        "readout" => {
            let s = state::latest();
            let lux = state::snap_scalar(&s, rultra_sense::DeviceId::Light).unwrap_or(0.0);
            let t = state::snap_scalar(&s, rultra_sense::DeviceId::CpuTemp).unwrap_or(0.0);
            drive_matrix("scroll", Some(format!("{lux:.0} LUX {t:.0}C")))
        }
        "sonar" => {
            #[cfg(all(target_os = "linux", feature = "hardware"))]
            {
                use rultra_sense::backend::linux::LinuxBackend;
                let room = fused(&state::latest());
                let r = LinuxBackend::matrix_draw_with_intensity(
                    &rultra_spatial::visual::render(&room),
                    rultra_spatial::visual::intensity(&room),
                );
                match r {
                    Ok(()) => json!({ "ok": true, "drew": format!("{:?}", room.proximity) }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                }
            }
            #[cfg(not(all(target_os = "linux", feature = "hardware")))]
            json!({ "ok": false, "error": "built without hardware support" })
        }
        "status" => {
            #[cfg(all(target_os = "linux", feature = "hardware"))]
            {
                use rultra_sense::backend::linux::LinuxBackend;
                let s = state::latest();
                let lux = state::snap_scalar(&s, rultra_sense::DeviceId::Light).unwrap_or(0.0);
                let t = state::snap_scalar(&s, rultra_sense::DeviceId::CpuTemp).unwrap_or(0.0);
                match LinuxBackend::open()
                    .and_then(|b| b.lcd_write(&format!("lux {lux:.0}"), &format!("cpu {t:.1}C")))
                {
                    Ok(()) => json!({ "ok": true, "wrote": format!("lux {lux:.0} / cpu {t:.1}C"),
                        "note": "the driver reported success; whether anything is VISIBLE is a separate question only a person can answer" }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                }
            }
            #[cfg(not(all(target_os = "linux", feature = "hardware")))]
            json!({ "ok": false, "error": "built without hardware support" })
        }
        "thermal" => {
            let mut v = Vec::new();
            for _ in 0..10 {
                if let Some(t) =
                    state::snap_scalar(&state::latest(), rultra_sense::DeviceId::CpuTemp)
                {
                    v.push(t);
                }
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            }
            if v.is_empty() {
                return json!({ "ok": false, "error": "no thermal readings" });
            }
            let mean = v.iter().sum::<f64>() / v.len() as f64;
            let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
            json!({ "ok": true, "samples": v.len(), "mean_c": mean, "stdev_c": sd,
                    "note": "samples come from the background snapshot, so consecutive values may repeat within one sweep" })
        }
        "chain" => {
            let path = state::state_dir().join("witness.jsonl");
            match std::fs::read_to_string(&path) {
                Ok(t) => match rultra_witness::Chain::parse_jsonl(&t) {
                    Ok(e) => json!({ "ok": true, "entries": e.len(), "parsed": true,
                        "note": "hash links and ordering verified from genesis" }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                },
                Err(_) => json!({ "ok": false, "error": "no chain on disk yet" }),
            }
        }
        other => json!({ "ok": false, "error": format!("unknown example: {other}") }),
    }
}

#[cfg_attr(not(all(target_os = "linux", feature = "hardware")), allow(dead_code))]
fn fused(s: &state::Snapshot) -> rultra_spatial::RoomState {
    rultra_spatial::RoomState::fuse(
        state::snap_scalar(s, rultra_sense::DeviceId::Range).map(|cm| cm / 100.0),
        state::snap_scalar(s, rultra_sense::DeviceId::Light),
        device::lookup(rultra_sense::DeviceId::Range)
            .map(|d| d.verification)
            .unwrap_or(Verification::Untested),
        device::lookup(rultra_sense::DeviceId::Light)
            .map(|d| d.verification)
            .unwrap_or(Verification::Untested),
        None,
    )
}

#[allow(unused_variables)]
fn drive_matrix(pattern: &str, text: Option<String>) -> J {
    #[cfg(all(target_os = "linux", feature = "hardware"))]
    {
        use rultra_sense::backend::linux::LinuxBackend;
        const HEART: [u8; 8] = [0x00, 0x66, 0xff, 0xff, 0xff, 0x7e, 0x3c, 0x18];
        let r = match pattern {
            "heart" => LinuxBackend::matrix_draw(&HEART),
            "clear" => LinuxBackend::matrix_draw(&[0; 8]),
            "scroll" => LinuxBackend::matrix_scroll(text.as_deref().unwrap_or("RULTRA"), 60),
            o => return json!({ "ok": false, "error": format!("unknown pattern: {o}") }),
        };
        match r {
            Ok(()) => json!({ "ok": true, "pattern": pattern }),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        }
    }
    #[cfg(not(all(target_os = "linux", feature = "hardware")))]
    json!({ "ok": false, "error": "built without hardware support" })
}

// ── configuration ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PolicyPatch {
    pub poll_interval_ms: u64,
}

/// Apply a configuration change.
///
/// Bounded by the same constants the governed loop uses, and recorded in the
/// witness chain. A human editing the policy is still a change to how the box
/// behaves, and a record that only captures the machine's own decisions would
/// be a partial history — the most misleading kind.
pub fn set_policy(p: PolicyPatch) -> Result<J, String> {
    use rultra_evolve::policy::{MAX_POLL_MS, MIN_POLL_MS};
    if !(MIN_POLL_MS..=MAX_POLL_MS).contains(&p.poll_interval_ms) {
        return Err(format!(
            "poll_interval_ms must be {MIN_POLL_MS}..={MAX_POLL_MS}, got {}",
            p.poll_interval_ms
        ));
    }
    let applier = state::applier();
    let previous = applier.load();
    let next = rultra_evolve::SensePolicy {
        poll_interval_ms: p.poll_interval_ms,
    };
    applier.apply(&next).map_err(|e| e.to_string())?;
    Ok(json!({
        "ok": true,
        "from": previous.poll_interval_ms,
        "to": next.poll_interval_ms,
        "content_hash": next.content_hash(),
        "note": "applied directly by an operator, not through the fitness gate. The \
                 governed loop may still propose changing it back."
    }))
}

// ── self-test ────────────────────────────────────────────────────────────────

/// Checks the box can run against itself, with no external harness.
pub async fn selftest() -> J {
    let mut checks: Vec<J> = Vec::new();
    let mut pass = 0usize;
    let mut add = |name: &str, want: String, got: String| {
        let ok = want == got;
        if ok {
            pass += 1;
        }
        checks.push(json!({ "check": name, "want": want, "got": got, "ok": ok }));
    };

    let snap = state::latest();
    add(
        "snapshot has readings",
        "true".into(),
        (!snap.readings.is_empty()).to_string(),
    );
    let age = rultra_sense::now().saturating_sub(snap.swept_at);
    add(
        "snapshot fresh (<10s)",
        "true".into(),
        (age < 10).to_string(),
    );

    let working = device::CATALOG
        .iter()
        .filter(|d| d.verification == Verification::Working)
        .count();
    add(
        "devices proven (>0)",
        "true".into(),
        (working > 0).to_string(),
    );

    // A Working claim without substantive evidence is the failure ADR-0002
    // exists to prevent, so the box checks it on itself rather than trusting
    // that a unit test still covers it.
    let unevidenced = device::CATALOG
        .iter()
        .filter(|d| d.verification == Verification::Working && d.evidence.len() < 20)
        .count();
    add(
        "every Working claim cites evidence",
        "0".into(),
        unevidenced.to_string(),
    );

    // An actuator in the sensor set would be driven by the telemetry loop.
    let readable_emitters = device::CATALOG
        .iter()
        .filter(|d| {
            d.kind == rultra_sense::DeviceKind::Sensor
                && matches!(
                    d.id,
                    rultra_sense::DeviceId::Buzzer
                        | rultra_sense::DeviceId::Matrix
                        | rultra_sense::DeviceId::Lcd
                )
        })
        .count();
    add(
        "no emitter is readable",
        "0".into(),
        readable_emitters.to_string(),
    );

    let chain = std::fs::read_to_string(state::state_dir().join("witness.jsonl"))
        .ok()
        .and_then(|t| rultra_witness::Chain::parse_jsonl(&t).ok());
    add(
        "witness chain parses",
        "true".into(),
        chain.is_some().to_string(),
    );

    json!({ "passed": pass, "total": checks.len(), "checks": checks })
}

// ── LLM interpretation ───────────────────────────────────────────────────────

/// Ask a model to interpret the current sensor state.
///
/// The key is read from `/etc/rultra/llm.env` and never accepted as a
/// parameter — a credential that can arrive in a request body ends up in logs.
/// With no key configured this returns a clear, actionable "not configured"
/// rather than a fabricated interpretation, which would be the worst possible
/// output for a project built on not overstating what it knows.
pub async fn interpret() -> J {
    let key = std::fs::read_to_string("/etc/rultra/llm.env")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("RULTRA_LLM_KEY=")
                    .map(|v| v.trim().to_string())
            })
        })
        .filter(|k| !k.is_empty());

    let snap = state::latest();
    let room = fused(&snap);
    let facts = json!({
        "proximity": room.proximity,
        "light": room.light,
        "range_m": room.range_m,
        "lux": room.lux,
        "verification": format!("{:?}", room.verification).to_lowercase(),
    });

    let Some(key) = key else {
        return json!({
            "configured": false,
            "facts": facts,
            "reason": "no model key. Put RULTRA_LLM_KEY=<key> in /etc/rultra/llm.env (mode 0600). \
                       Nothing is inferred without one — a fabricated reading of the room would be \
                       worse than no reading.",
        });
    };

    let endpoint = std::env::var("RULTRA_LLM_ENDPOINT")
        .unwrap_or_else(|_| "https://api.cognitum.one/v1/messages".to_string());
    let prompt = format!(
        "These are sensor facts from a Raspberry Pi sensor box. Describe the room in two sentences. \
         Do NOT state anything the facts do not support, and note explicitly if a reading is \
         marked unvalidated.\n\n{}",
        serde_json::to_string_pretty(&facts).unwrap_or_default()
    );
    let body = json!({
        "model": "claude-sonnet-5",
        "max_tokens": 200,
        "messages": [{ "role": "user", "content": prompt }],
    });
    let out = std::process::Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "25",
            "-X",
            "POST",
            &endpoint,
            "-H",
            &format!("x-api-key: {key}"),
            "-H",
            "anthropic-version: 2023-06-01",
            "-H",
            "content-type: application/json",
            "-d",
            &body.to_string(),
        ])
        .output();
    match out {
        Ok(o) => {
            let parsed: J = serde_json::from_slice(&o.stdout).unwrap_or(J::Null);
            let text = parsed
                .get("content")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
                .map(str::to_string);
            match text {
                Some(t) => json!({ "configured": true, "facts": facts, "interpretation": t }),
                None => json!({ "configured": true, "facts": facts,
                                "error": "the gateway returned no completion",
                                "raw": parsed }),
            }
        }
        Err(e) => json!({ "configured": true, "facts": facts, "error": e.to_string() }),
    }
}
