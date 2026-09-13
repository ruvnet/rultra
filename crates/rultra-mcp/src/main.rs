//! `rultra-mcp` — the box's sensors and capabilities over MCP.
//!
//! Exposes what the CLI and console already do, as tools an agent can call and
//! `ruv://` resources it can read, following the URI convention used by the
//! ruvnet federation gateway.
//!
//! # stdout is the protocol
//!
//! This is a stdio MCP server: stdout carries JSON-RPC frames and nothing else.
//! A stray `println!` anywhere in the process corrupts the stream and the
//! client disconnects with a parse error that points nowhere near the cause.
//! Diagnostics go to stderr, always.
#![forbid(unsafe_code)]

mod server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    eprintln!("rultra-mcp: starting stdio server");
    server::run().await
}
