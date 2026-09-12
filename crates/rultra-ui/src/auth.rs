//! Bind address and token authentication.
//!
//! The console can run a governed cycle and drive hardware, so it is a control
//! surface, not a dashboard. Two rules follow from that:
//!
//! 1. **Loopback by default.** Reaching it from another machine should be a
//!    deliberate act (an SSH tunnel, or an explicit bind), never the default.
//! 2. **A non-loopback bind requires a token.** Widening the listener without
//!    authentication is refused at startup rather than warned about, because a
//!    warning in a log nobody reads is not a control.

use std::net::{IpAddr, SocketAddr};

/// Why the server refused to start.
#[derive(Debug, PartialEq, Eq)]
pub enum BindError {
    /// A non-loopback bind was requested with no token configured.
    ExposedWithoutToken(IpAddr),
    /// The address could not be parsed.
    BadAddress(String),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExposedWithoutToken(ip) => write!(
                f,
                "refusing to bind {ip} without authentication: this console can run \
                 cycles and drive hardware. Set RULTRA_UI_TOKEN, or bind loopback \
                 and reach it over an SSH tunnel."
            ),
            Self::BadAddress(s) => write!(f, "could not parse bind address {s:?}"),
        }
    }
}

impl std::error::Error for BindError {}

/// Resolve the listen address, refusing an unauthenticated public bind.
pub fn resolve_bind(host: &str, port: u16, token: Option<&str>) -> Result<SocketAddr, BindError> {
    let ip: IpAddr = host
        .parse()
        .map_err(|_| BindError::BadAddress(host.to_string()))?;
    if !ip.is_loopback() && token.is_none() {
        return Err(BindError::ExposedWithoutToken(ip));
    }
    Ok(SocketAddr::new(ip, port))
}

/// Compare two secrets without leaking their relationship through timing.
///
/// Length is compared first and separately, which does leak length — that is
/// accepted and standard; the value is what must not leak byte by byte.
pub fn secret_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Extract a presented token from an `Authorization: Bearer` header, falling
/// back to an `X-Rultra-Token` header.
///
/// Deliberately does **not** read a query parameter: a token in a URL lands in
/// server logs, browser history and `Referer` headers.
pub fn presented<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Option<&'a str> {
    if let Some(v) = get("authorization") {
        if let Some(rest) = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
        {
            return Some(rest.trim());
        }
    }
    get("x-rultra-token").map(str::trim)
}

/// Is this request allowed?
pub fn authorized<'a>(configured: Option<&str>, get: impl Fn(&str) -> Option<&'a str>) -> bool {
    match configured {
        // No token configured is only reachable on loopback — resolve_bind
        // guarantees it — so an unauthenticated local request is intended.
        None => true,
        Some(want) => presented(get)
            .map(|got| secret_eq(got, want))
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<&'static str> {
        None
    }

    #[test]
    fn loopback_needs_no_token() {
        assert!(resolve_bind("127.0.0.1", 17880, None).is_ok());
        assert!(resolve_bind("::1", 17880, None).is_ok());
    }

    /// The rule that matters: widening the listener without a token is refused
    /// at startup, not warned about.
    #[test]
    fn a_public_bind_without_a_token_is_refused() {
        let e = resolve_bind("0.0.0.0", 17880, None).unwrap_err();
        assert!(matches!(e, BindError::ExposedWithoutToken(_)));
        assert!(
            e.to_string().contains("RULTRA_UI_TOKEN"),
            "must say how to fix it"
        );
    }

    #[test]
    fn a_public_bind_with_a_token_is_allowed() {
        assert!(resolve_bind("0.0.0.0", 17880, Some("s3cret")).is_ok());
    }

    #[test]
    fn a_bad_address_is_rejected() {
        assert_eq!(
            resolve_bind("not-an-ip", 1, None).unwrap_err(),
            BindError::BadAddress("not-an-ip".into())
        );
    }

    #[test]
    fn secret_eq_matches_only_identical_values() {
        assert!(secret_eq("abc", "abc"));
        assert!(!secret_eq("abc", "abd"));
        assert!(!secret_eq("abc", "abcd"));
        assert!(!secret_eq("", "x"));
        assert!(secret_eq("", ""));
    }

    #[test]
    fn bearer_token_is_extracted_case_insensitively() {
        assert_eq!(
            presented(|k| (k == "authorization").then_some("Bearer tok")),
            Some("tok")
        );
        assert_eq!(
            presented(|k| (k == "authorization").then_some("bearer tok")),
            Some("tok")
        );
    }

    #[test]
    fn the_fallback_header_works() {
        assert_eq!(
            presented(|k| (k == "x-rultra-token").then_some("tok")),
            Some("tok")
        );
    }

    /// A token in a query string leaks into logs and history, so it must not
    /// be an accepted way to authenticate.
    #[test]
    fn a_query_parameter_is_not_a_credential() {
        assert_eq!(presented(|k| (k == "token").then_some("tok")), None);
    }

    #[test]
    fn without_a_configured_token_everything_is_allowed() {
        assert!(authorized(None, none));
    }

    #[test]
    fn with_a_token_the_right_one_passes_and_others_do_not() {
        assert!(authorized(Some("tok"), |k| (k == "authorization")
            .then_some("Bearer tok")));
        assert!(!authorized(Some("tok"), |k| (k == "authorization")
            .then_some("Bearer nope")));
        assert!(!authorized(Some("tok"), none));
    }

    #[test]
    fn a_malformed_authorization_header_does_not_pass() {
        assert!(!authorized(Some("tok"), |k| (k == "authorization").then_some("tok")));
    }
}
