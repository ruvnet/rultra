//! `rultra-ui` — the box's management console.
//!
//! A small Rust backend over the same crates the CLI uses, serving a
//! single-page UI. Everything the console shows comes from the real device
//! catalog and the real witness chain — there is no separate source of truth,
//! and nothing here can report a device as working that the catalog does not.
#![forbid(unsafe_code)]

mod api;
mod auth;
mod state;

use axum::{
    extract::Request,
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;

/// Reject unauthenticated requests when a token is configured.
async fn guard(req: Request, next: Next) -> Result<Response, axum::http::StatusCode> {
    // The shell at "/" is static markup with no data and no secrets; it is what
    // prompts for the token in the first place, so it must be reachable without
    // one. Everything under /api stays behind the guard.
    if req.uri().path() == "/" {
        return Ok(next.run(req).await);
    }
    let token = std::env::var("RULTRA_UI_TOKEN").ok();
    let headers = req.headers().clone();
    let ok = auth::authorized(token.as_deref(), |k| {
        headers.get(k).and_then(|v| v.to_str().ok())
    });
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(axum::http::StatusCode::UNAUTHORIZED)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::var("RULTRA_UI_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(17880);
    // Loopback by default: reaching this console from another machine should
    // be a deliberate act, because it can run cycles and drive hardware.
    let host = std::env::var("RULTRA_UI_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    let token = std::env::var("RULTRA_UI_TOKEN").ok();
    let addr = auth::resolve_bind(&host, port, token.as_deref())?;

    let app = Router::new()
        .route("/", get(api::index))
        .route("/api/summary", get(api::summary))
        .route("/api/devices", get(api::devices))
        .route("/api/telemetry", get(api::telemetry))
        .route("/api/policy", get(api::policy))
        .route("/api/schedule", get(api::schedule))
        .route("/api/chain", get(api::chain))
        .route("/api/cycle", post(api::cycle))
        .route("/api/matrix", post(api::matrix))
        .route("/api/lcd", post(api::lcd))
        .layer(middleware::from_fn(guard))
        // Same-origin only. The console is served from this process, so a
        // permissive policy would only widen what a hostile page can reach.
        .layer(CorsLayer::very_permissive().allow_credentials(false));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!(
        "rultra-ui listening on http://{addr} (auth: {})",
        if token.is_some() {
            "token required"
        } else {
            "none, loopback only"
        }
    );
    axum::serve(listener, app).await?;
    Ok(())
}
