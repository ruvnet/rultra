//! HTTP handlers. Every response is derived from the real catalog, the real
//! buses, and the real witness chain on disk.

use crate::state;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use rultra_sense::{device, DeviceId, DeviceKind, Verification};
use rultra_witness::Chain;
use serde_json::{json, Value as J};

/// three.js, served from the binary.
///
/// Vendored rather than CDN-loaded: this is an appliance that may have no
/// internet, and a screensaver that fails offline is not a screensaver. The
/// console never loads it at startup — the client fetches it only when the
/// screensaver actually activates, so an idle feature costs nothing until it
/// is used.
pub async fn vendor_three() -> impl IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/javascript"),
            // Immutable: the file only changes when the binary does.
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=31536000, immutable",
            ),
        ],
        include_str!("../ui/vendor/three.min.js"),
    )
}

pub async fn index() -> Html<&'static str> {
    // One definition, in the module that also structurally validates it — so
    // what ships is exactly what the tests checked.
    Html(crate::asset::INDEX)
}

fn verification_str(v: Verification) -> &'static str {
    match v {
        Verification::Working => "working",
        Verification::Unvalidated => "unvalidated",
        Verification::AcksButSilent => "acks_but_silent",
        Verification::Untested => "untested",
    }
}

/// Headline numbers for the overview.
pub async fn summary() -> Json<J> {
    let policy = state::applier().load();
    // One trip to the sensors for everything this endpoint needs, rather than
    // four separate locks and four chances to interleave with another request.
    // From the snapshot: no lock on the bus, no waiting on physics.
    let snap = state::latest();
    let responding = snap.presence.iter().filter(|p| p.responding).count();
    let temp = state::snap_scalar(&snap, DeviceId::CpuTemp);
    let lux = state::snap_scalar(&snap, DeviceId::Light);
    let range_cm = state::snap_scalar(&snap, DeviceId::Range);
    let working = device::CATALOG
        .iter()
        .filter(|d| d.verification == Verification::Working)
        .count();

    // The fused band rather than raw centimetres: the range finder is
    // Unvalidated, so a band is what it can honestly support (ADR-0006).
    let room = rultra_spatial::RoomState::fuse(
        range_cm.map(|cm| cm / 100.0),
        lux,
        device::lookup(DeviceId::Range)
            .map(|d| d.verification)
            .unwrap_or(Verification::Untested),
        device::lookup(DeviceId::Light)
            .map(|d| d.verification)
            .unwrap_or(Verification::Untested),
        None,
    );
    let steering = rultra_spatial::Steering::from(&room);
    let ceiling = rultra_evolve::policy::THERMAL_CEILING_C;

    // Chain depth is a liveness signal: a console that claims the loop is
    // running should be able to point at entries proving it.
    let chain_len = std::fs::read_to_string(state::state_dir().join("witness.jsonl"))
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);

    Json(json!({
        "die_temp_c": temp,
        "thermal_ceiling_c": ceiling,
        "thermally_stressed": temp.map(|t| t >= ceiling).unwrap_or(false),
        "lux": lux,
        "poll_interval_ms": policy.poll_interval_ms,
        "policy_hash": policy.content_hash(),
        "devices_total": device::CATALOG.len(),
        "devices_responding": responding,
        "devices_working": working,
        "witness_entries": chain_len,
        "swept_at": snap.swept_at,
        "proximity": room.proximity,
        "light_band": room.light,
        "range_cm": range_cm,
        "steering": {
            "intensity": steering.intensity,
            "luminance": steering.luminance,
            "calm": steering.calm,
            "billable": steering.billable,
        },
    }))
}

/// The catalog joined with a live probe.
pub async fn devices() -> Json<J> {
    let presence = state::latest().presence;
    let items: Vec<J> = device::CATALOG
        .iter()
        .map(|d| {
            let p = presence.iter().find(|p| p.device == d.id);
            json!({
                "id": format!("{:?}", d.id).to_lowercase(),
                "part": d.part,
                "kind": match d.kind { DeviceKind::Sensor => "sensor", DeviceKind::Actuator => "actuator" },
                "bus": serde_json::to_value(&d.bus).unwrap_or(J::Null),
                "verification": verification_str(d.verification),
                "evidence": d.evidence,
                "responding": p.map(|p| p.responding).unwrap_or(false),
                "detail": p.map(|p| p.detail.clone()).unwrap_or_default(),
            })
        })
        .collect();
    Json(json!({ "devices": items }))
}

/// One reading from every sensor. The UI polls this; a websocket would be
/// more elegant but a poll survives a reconnect without extra machinery, which
/// matters more on a box that may be power-cycled.
pub async fn telemetry() -> Json<J> {
    let snap = state::latest();
    let readings: Vec<J> = {
        device::CATALOG
            .iter()
            .filter(|d| d.kind == DeviceKind::Sensor)
            .filter_map(|d| {
                let r = snap.readings.get(&d.id)?.clone();
                Some(json!({
                    "id": format!("{:?}", d.id).to_lowercase(),
                    "value": r.value,
                    // The measurement time, which may predate this response
                    // when the reading came from cache. Compare it to `at`
                    // below to see the true age.
                    "at": r.at,
                    "verification": verification_str(r.verification),
                }))
            })
            .collect()
    };
    Json(json!({ "readings": readings, "at": rultra_sense::now() }))
}

/// Is the box actually running cycles on its own, and what did it last decide?
///
/// "Self-optimizing" is only true if something runs the loop unattended. This
/// endpoint reports the real systemd timer state rather than assuming it, and
/// says plainly when no schedule is installed.
pub async fn schedule() -> Json<J> {
    let active = std::process::Command::new("systemctl")
        .args(["is-active", "rultra-cycle.timer"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    // `systemctl list-timers` is the only place the *next* elapse is exposed.
    let next = std::process::Command::new("systemctl")
        .args([
            "list-timers",
            "rultra-cycle.timer",
            "--no-pager",
            "--no-legend",
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    // Last decision, read from the chain rather than a separate status file —
    // one source of truth, and it cannot drift from the signed record.
    let mut last_decision = None;
    let mut last_reason = None;
    if let Ok(text) = std::fs::read_to_string(state::state_dir().join("witness.jsonl")) {
        if let Ok(entries) = Chain::parse_jsonl(&text) {
            // Walk backwards collecting both, and stop only when both are
            // found. Breaking on the decision alone always returned a null
            // reason, because `gated` is written BEFORE promoted/rolled_back
            // and so is reached later in a reverse scan.
            for e in entries.iter().rev() {
                let v = serde_json::to_value(&e.event).unwrap_or(J::Null);
                match v.get("event").and_then(|x| x.as_str()) {
                    Some(k @ ("promoted" | "rolled_back")) if last_decision.is_none() => {
                        last_decision = Some(k.to_string());
                    }
                    Some("gated") if last_reason.is_none() => {
                        last_reason = v.get("reason").and_then(|x| x.as_str()).map(str::to_string);
                    }
                    _ => {}
                }
                if last_decision.is_some() && last_reason.is_some() {
                    break;
                }
            }
        }
    }

    Json(json!({
        "timer_active": active == "active",
        "timer_state": active,
        "next_elapse": next,
        "last_decision": last_decision,
        "last_gate_reason": last_reason,
    }))
}

/// Which ruvnet tools are on this box, and what they report.
///
/// Probed, never declared. A tools page that lists what was *installed* drifts
/// the moment something is removed or stops; this asks each one.
pub async fn tools() -> Json<J> {
    let probe = |bin: &str, args: &[&str]| -> Option<String> {
        let out = std::process::Command::new(bin).args(args).output().ok()?;
        let t = String::from_utf8_lossy(&out.stdout);
        t.lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
    };
    // ruview's sensing server, which is the interesting one: it is a real
    // running service with its own honesty posture.
    let ruview_health = std::process::Command::new("curl")
        .args(["-s", "--max-time", "3", "http://127.0.0.1:3000/health"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<J>(&o.stdout).ok());

    Json(json!({
        "tools": [
            { "id": "ruview", "role": "WiFi-DensePose sensing server and the claim-check honesty guardrail",
              "present": std::path::Path::new("/usr/bin/ruview").exists(),
              "endpoint": "http://127.0.0.1:3000",
              "health": ruview_health,
              // Surfaced deliberately: ruview labels its own data source, and
              // a consumer that hides that label is worse than one that never
              // had it.
              "source_note": "ruview reports its own data source; `simulated` means it is not live radio" },
            { "id": "ruvector", "role": "vector memory, HNSW, RVF containers",
              // Resolved through the global npm root: `npx --no-install` does
              // not find globally installed packages, so probing that way
              // under-reported a tool that is present. A tools page that
              // under-reports is as wrong as one that over-reports.
              "present": probe("npm", &["root", "-g"])
                  .map(|root| std::path::Path::new(&root).join("@ruvector/cli").exists())
                  .unwrap_or(false) },
            { "id": "ruflo", "role": "swarm orchestration, memory, hooks",
              "present": std::path::Path::new("/usr/local/bin/ruflo").exists() },
            { "id": "ruv-swarm", "role": "multi-agent coordination",
              "present": std::path::Path::new("/usr/bin/ruv-swarm").exists() },
            { "id": "agentdb", "role": "agent memory with vector embeddings",
              "present": std::path::Path::new("/usr/bin/agentdb").exists() },
            { "id": "rultra-mcp", "role": "this box's sensors as MCP tools and ruv:// resources",
              "present": std::path::Path::new("/usr/local/bin/rultra-mcp").exists() },
        ]
    }))
}

/// Lint this box's own accuracy claims with `ruview claim-check`.
///
/// The device catalog's evidence strings are accuracy claims about hardware,
/// and this project has spent its whole life insisting those be honest. Running
/// someone else's linter over them is the only way to find out whether that
/// discipline actually holds, rather than whether it *feels* like it holds.
pub async fn claimcheck() -> Json<J> {
    let mut results = Vec::new();
    let mut failed = 0usize;
    for d in device::CATALOG {
        let out = std::process::Command::new("ruview")
            .args(["claim-check", "--text", d.evidence])
            .output();
        let verdict = match out {
            Ok(o) => serde_json::from_slice::<J>(&o.stdout).unwrap_or_else(
                |_| json!({ "ok": null, "summary": "claim-check produced no JSON" }),
            ),
            Err(e) => json!({ "ok": null, "summary": format!("ruview unavailable: {e}") }),
        };
        if verdict.get("ok") == Some(&J::Bool(false)) {
            failed += 1;
        }
        results.push(json!({
            "device": format!("{:?}", d.id).to_lowercase(),
            "verification": verification_str(d.verification),
            "verdict": verdict,
        }));
    }
    Json(json!({ "checked": results.len(), "flagged": failed, "results": results }))
}

pub async fn policy() -> Json<J> {
    let p = state::applier().load();
    Json(json!({
        "poll_interval_ms": p.poll_interval_ms,
        "content_hash": p.content_hash(),
        "min_poll_ms": rultra_evolve::policy::MIN_POLL_MS,
        "max_poll_ms": rultra_evolve::policy::MAX_POLL_MS,
        "thermal_ceiling_c": rultra_evolve::policy::THERMAL_CEILING_C,
        "thermal_comfort_c": rultra_evolve::policy::THERMAL_COMFORT_C,
    }))
}

/// The witness chain, newest last, with its verification status.
pub async fn chain() -> Json<J> {
    let path = state::state_dir().join("witness.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Json(json!({ "entries": [], "verified": false, "reason": "no chain on disk yet" }));
    };
    match Chain::parse_jsonl(&text) {
        Ok(entries) => {
            let items: Vec<J> = entries
                .iter()
                .map(|e| json!({ "seq": e.seq, "at": e.at, "event": e.event, "hash": e.hash }))
                .collect();
            Json(json!({ "entries": items, "count": items.len(), "verified": true }))
        }
        // A malformed chain is reported, never silently skipped — hiding it
        // would conceal exactly the tampering the chain exists to detect.
        Err(e) => Json(json!({ "entries": [], "verified": false, "reason": e.to_string() })),
    }
}

/// Run one governed cycle by invoking the `rultra` binary, so the console and
/// the CLI execute identical logic rather than two drifting copies.
pub async fn cycle() -> impl IntoResponse {
    let out = std::process::Command::new("rultra")
        .arg("cycle")
        .arg("8")
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let v: J = serde_json::from_slice(&o.stdout)
                .unwrap_or(json!({"raw": String::from_utf8_lossy(&o.stdout)}));
            (StatusCode::OK, Json(v))
        }
        Ok(o) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": String::from_utf8_lossy(&o.stderr) })),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": format!("rultra binary not available: {e}") })),
        ),
    }
}

// Fields are read only inside the hardware-gated branch.
#[cfg_attr(not(all(target_os = "linux", feature = "hardware")), allow(dead_code))]
#[derive(serde::Deserialize)]
pub struct MatrixReq {
    pub pattern: Option<String>,
    pub text: Option<String>,
}

pub async fn matrix(Json(req): Json<MatrixReq>) -> impl IntoResponse {
    #[cfg(all(target_os = "linux", feature = "hardware"))]
    {
        use rultra_sense::backend::linux::LinuxBackend;
        const HEART: [u8; 8] = [0x00, 0x66, 0xff, 0xff, 0xff, 0x7e, 0x3c, 0x18];
        let r = match req.pattern.as_deref().unwrap_or("heart") {
            "heart" => LinuxBackend::matrix_draw(&HEART),
            "clear" => LinuxBackend::matrix_draw(&[0; 8]),
            "scroll" => LinuxBackend::matrix_scroll(req.text.as_deref().unwrap_or("RULTRA"), 60),
            other => Err(anyhow::anyhow!("unknown pattern: {other}")),
        };
        return match r {
            Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            ),
        };
    }
    #[cfg(not(all(target_os = "linux", feature = "hardware")))]
    {
        let _ = req;
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({ "error": "built without the hardware feature" })),
        )
    }
}

#[cfg_attr(not(all(target_os = "linux", feature = "hardware")), allow(dead_code))]
#[derive(serde::Deserialize)]
pub struct LcdReq {
    pub line1: Option<String>,
    pub line2: Option<String>,
    pub backlight: Option<bool>,
}

pub async fn lcd(Json(req): Json<LcdReq>) -> impl IntoResponse {
    #[cfg(all(target_os = "linux", feature = "hardware"))]
    {
        use rultra_sense::backend::linux::LinuxBackend;
        let Ok(b) = LinuxBackend::open() else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "no i2c bus" })),
            );
        };
        let r = match req.backlight {
            Some(on) => b.lcd_backlight(on),
            None => b.lcd_write(
                req.line1.as_deref().unwrap_or(""),
                req.line2.as_deref().unwrap_or(""),
            ),
        };
        return match r {
            Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            ),
        };
    }
    #[cfg(not(all(target_os = "linux", feature = "hardware")))]
    {
        let _ = req;
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({ "error": "built without the hardware feature" })),
        )
    }
}

/// The runnable examples this box offers.
pub async fn examples() -> Json<J> {
    Json(json!({
        "examples": crate::toolkit::EXAMPLES.iter().map(|e| json!({
            "id": e.id, "title": e.title, "blurb": e.blurb, "needs": e.needs,
        })).collect::<Vec<_>>()
    }))
}

#[derive(serde::Deserialize)]
pub struct RunArgs {
    pub id: String,
}

pub async fn run_example(Json(a): Json<RunArgs>) -> Json<J> {
    Json(crate::toolkit::run_example(&a.id).await)
}

pub async fn selftest() -> Json<J> {
    Json(crate::toolkit::selftest().await)
}

pub async fn interpret() -> Json<J> {
    Json(crate::toolkit::interpret().await)
}

pub async fn set_policy(
    Json(p): Json<crate::toolkit::PolicyPatch>,
) -> Result<Json<J>, (StatusCode, Json<J>)> {
    crate::toolkit::set_policy(p)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))))
}
