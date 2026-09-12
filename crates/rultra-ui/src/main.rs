//! `rultra-ui` — the box's management console.
//!
//! A small Rust backend over the same crates the CLI uses, serving a
//! single-page UI. Everything the console shows comes from the real device
//! catalog and the real witness chain — there is no separate source of truth,
//! and nothing here can report a device as working that the catalog does not.
#![forbid(unsafe_code)]

mod api;
mod state;

use axum::{
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::var("RULTRA_UI_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(17880);

    let app = Router::new()
        .route("/", get(api::index))
        .route("/api/summary", get(api::summary))
        .route("/api/devices", get(api::devices))
        .route("/api/telemetry", get(api::telemetry))
        .route("/api/policy", get(api::policy))
        .route("/api/chain", get(api::chain))
        .route("/api/cycle", post(api::cycle))
        .route("/api/matrix", post(api::matrix))
        .route("/api/lcd", post(api::lcd))
        .layer(CorsLayer::permissive());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("rultra-ui listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
