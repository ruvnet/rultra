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

pub async fn index() -> Html<&'static str> {
    Html(include_str!("../ui/index.html"))
}

fn verification_str(v: Verification) -> &'static str {
    match v {
        Verification::Working => "working",
        Verification::AcksButSilent => "acks_but_silent",
        Verification::Untested => "untested",
    }
}

/// Headline numbers for the overview.
pub async fn summary() -> Json<J> {
    let mut b = state::backend();
    let policy = state::applier().load();
    let presence = b.probe().unwrap_or_default();
    let responding = presence.iter().filter(|p| p.responding).count();
    let working = device::CATALOG
        .iter()
        .filter(|d| d.verification == Verification::Working)
        .count();

    let temp = state::scalar(b.as_mut(), DeviceId::CpuTemp);
    let lux = state::scalar(b.as_mut(), DeviceId::Light);
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
    }))
}

/// The catalog joined with a live probe.
pub async fn devices() -> Json<J> {
    let mut b = state::backend();
    let presence = b.probe().unwrap_or_default();
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
    let mut b = state::backend();
    let readings: Vec<J> = device::CATALOG
        .iter()
        .filter(|d| d.kind == DeviceKind::Sensor)
        .filter_map(|d| {
            let r = b.read(d.id).ok()?;
            Some(json!({
                "id": format!("{:?}", d.id).to_lowercase(),
                "value": r.value,
                "at": r.at,
                "verification": verification_str(r.verification),
            }))
        })
        .collect();
    Json(json!({ "readings": readings, "at": rultra_sense::now() }))
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
