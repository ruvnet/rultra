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
//! 3. **Watching and driving are separate capabilities.** A second, independent
//!    read-only token grants telemetry and the witness chain but carries no
//!    control authority. Giving someone a dashboard should not hand them the
//!    ability to run cycles and drive hardware.
//!
//! The capability split follows the admission model in `cognitum-one/cognitum-media`
//! (ADR-0004, admitted streaming audio): *independent* capabilities rather than
//! one scoped down, because a derived listener token is one bug away from being
//! a control token.

use std::net::{IpAddr, SocketAddr};

/// What a caller is allowed to do.
///
/// Ordered: `Control` strictly exceeds `Listen`. Deliberately NOT a bitfield —
/// a capability set invites "listen plus a bit of control", which is how a
/// read-only credential quietly acquires authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// May read telemetry, devices and the witness chain. Nothing else.
    Listen,
    /// May additionally run cycles and drive hardware.
    Control,
}

/// Why the server refused to start.
#[derive(Debug, PartialEq, Eq)]
pub enum BindError {
    /// A non-loopback bind was requested with no token configured.
    ExposedWithoutToken(IpAddr),
    /// A read-only token was configured without a control token, which would
    /// leave writes unauthenticated while implying separation exists.
    ReadTokenWithoutControlToken,
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
            Self::ReadTokenWithoutControlToken => write!(
                f,
                "RULTRA_UI_READ_TOKEN is set but RULTRA_UI_TOKEN is not: that would \
                 leave cycles and hardware control unauthenticated while implying a \
                 read-only tier exists. Set both, or neither."
            ),
            Self::BadAddress(s) => write!(f, "could not parse bind address {s:?}"),
        }
    }
}

impl std::error::Error for BindError {}

/// Resolve the listen address, refusing an unauthenticated public bind and an
/// incoherent token configuration.
pub fn resolve_bind(
    host: &str,
    port: u16,
    control: Option<&str>,
    read: Option<&str>,
) -> Result<SocketAddr, BindError> {
    let ip: IpAddr = host
        .parse()
        .map_err(|_| BindError::BadAddress(host.to_string()))?;
    if read.is_some() && control.is_none() {
        return Err(BindError::ReadTokenWithoutControlToken);
    }
    if !ip.is_loopback() && control.is_none() {
        return Err(BindError::ExposedWithoutToken(ip));
    }
    Ok(SocketAddr::new(ip, port))
}

/// What capability a request needs.
///
/// Anything that changes the world needs `Control`. Reading does not. The split
/// is by effect, not by convenience: a GET that ran a cycle would be a GET that
/// needs Control, and the right fix would be to stop it being a GET.
pub fn required_for(method: &str, path: &str) -> Capability {
    if method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD") {
        Capability::Listen
    } else {
        let _ = path;
        Capability::Control
    }
}

/// Does a request from the loopback interface get read-only access without a
/// token?
///
/// **Off by default, and deliberately opt-in.** The desktop app on the box
/// needs to render telemetry without handling a credential, but enabling that
/// silently would change the security posture of every deployment that merely
/// upgraded. The appliance turns it on explicitly; the library never assumes it.
///
/// It grants `Listen` only. Control still requires the token, because "already
/// on the box" is a reason to let someone watch, not a reason to let them drive
/// the hardware. Peer addresses come from the accepted socket, so a remote
/// client cannot claim to be loopback.
pub fn local_listen_enabled(env: Option<&str>) -> bool {
    matches!(env, Some("1") | Some("true") | Some("yes"))
}

/// Which capability the presented credential carries, if any.
///
/// The control token is checked first so that a deployment which accidentally
/// sets both variables to the same value grants control rather than silently
/// downgrading the operator to read-only.
pub fn capability<'a>(
    control: Option<&str>,
    read: Option<&str>,
    peer_loopback: bool,
    local_listen: bool,
    get: impl Fn(&str) -> Option<&'a str>,
) -> Option<Capability> {
    // No control token is only reachable on loopback — resolve_bind guarantees
    // it — so a local request is intended and fully authorized.
    let Some(control) = control else {
        return Some(Capability::Control);
    };
    // A presented credential is judged on its own merits first, so a local
    // caller holding the control token still gets Control rather than being
    // capped at the loopback grant.
    if let Some(presented) = presented(get) {
        if secret_eq(presented, control) {
            return Some(Capability::Control);
        }
        if let Some(read) = read {
            if secret_eq(presented, read) {
                return Some(Capability::Listen);
            }
        }
        // A wrong token is a wrong token even from loopback: falling through to
        // the local grant would make a bad credential indistinguishable from
        // none, and hide a misconfigured client.
        return None;
    }
    if peer_loopback && local_listen {
        return Some(Capability::Listen);
    }
    None
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
#[allow(clippy::too_many_arguments)]
pub fn authorized<'a>(
    control: Option<&str>,
    read: Option<&str>,
    peer_loopback: bool,
    local_listen: bool,
    method: &str,
    path: &str,
    get: impl Fn(&str) -> Option<&'a str>,
) -> bool {
    match capability(control, read, peer_loopback, local_listen, get) {
        Some(held) => held >= required_for(method, path),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calls `authorized` with the loopback grant OFF, which is the default and
    /// what almost every test wants to assert against.
    fn ok<'a>(
        c: Option<&str>,
        r: Option<&str>,
        m: &str,
        path: &str,
        g: impl Fn(&str) -> Option<&'a str>,
    ) -> bool {
        authorized(c, r, false, false, m, path, g)
    }

    fn none(_: &str) -> Option<&'static str> {
        None
    }
    fn bearer(tok: &'static str) -> impl Fn(&str) -> Option<&'static str> {
        move |k: &str| {
            (k == "authorization")
                .then_some(Box::leak(format!("Bearer {tok}").into_boxed_str()) as &'static str)
        }
    }

    #[test]
    fn loopback_needs_no_token() {
        assert!(resolve_bind("127.0.0.1", 17880, None, None).is_ok());
        assert!(resolve_bind("::1", 17880, None, None).is_ok());
    }

    /// The rule that matters: widening the listener without a token is refused
    /// at startup, not warned about.
    #[test]
    fn a_public_bind_without_a_token_is_refused() {
        let e = resolve_bind("0.0.0.0", 17880, None, None).unwrap_err();
        assert!(matches!(e, BindError::ExposedWithoutToken(_)));
        assert!(
            e.to_string().contains("RULTRA_UI_TOKEN"),
            "must say how to fix it"
        );
    }

    #[test]
    fn a_public_bind_with_a_token_is_allowed() {
        assert!(resolve_bind("0.0.0.0", 17880, Some("s3cret"), None).is_ok());
    }

    /// A read token with no control token implies a separation that does not
    /// exist: writes would be unauthenticated. Refuse rather than mislead.
    #[test]
    fn a_read_token_without_a_control_token_is_refused() {
        assert_eq!(
            resolve_bind("127.0.0.1", 17880, None, Some("ro")).unwrap_err(),
            BindError::ReadTokenWithoutControlToken
        );
    }

    #[test]
    fn a_bad_address_is_rejected() {
        assert_eq!(
            resolve_bind("not-an-ip", 1, None, None).unwrap_err(),
            BindError::BadAddress("not-an-ip".into())
        );
    }

    #[test]
    fn secret_eq_matches_only_identical_values() {
        assert!(secret_eq("abc", "abc"));
        assert!(!secret_eq("abc", "abd"));
        assert!(!secret_eq("abc", "abcd"));
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

    /// A token in a query string leaks into logs, history and Referer headers.
    #[test]
    fn a_query_parameter_is_not_a_credential() {
        assert_eq!(presented(|k| (k == "token").then_some("tok")), None);
    }

    #[test]
    fn reads_need_listen_and_writes_need_control() {
        assert_eq!(required_for("GET", "/api/summary"), Capability::Listen);
        assert_eq!(required_for("HEAD", "/api/summary"), Capability::Listen);
        assert_eq!(required_for("POST", "/api/cycle"), Capability::Control);
        assert_eq!(required_for("DELETE", "/api/anything"), Capability::Control);
    }

    #[test]
    fn control_strictly_exceeds_listen() {
        assert!(Capability::Control > Capability::Listen);
    }

    #[test]
    fn without_a_control_token_everything_is_allowed() {
        assert!(ok(None, None, "POST", "/api/cycle", none));
    }

    #[test]
    fn the_control_token_may_read_and_write() {
        assert!(ok(Some("c"), Some("r"), "GET", "/api/summary", bearer("c")));
        assert!(ok(Some("c"), Some("r"), "POST", "/api/cycle", bearer("c")));
    }

    /// The point of the split: a listener can watch the box and cannot drive it.
    #[test]
    fn the_read_token_may_read_but_never_write() {
        assert!(ok(Some("c"), Some("r"), "GET", "/api/summary", bearer("r")));
        assert!(!ok(Some("c"), Some("r"), "POST", "/api/cycle", bearer("r")));
        assert!(!ok(
            Some("c"),
            Some("r"),
            "POST",
            "/api/matrix",
            bearer("r")
        ));
    }

    #[test]
    fn an_unknown_token_gets_nothing() {
        assert!(!ok(
            Some("c"),
            Some("r"),
            "GET",
            "/api/summary",
            bearer("nope")
        ));
        assert!(!ok(Some("c"), Some("r"), "POST", "/api/cycle", none));
    }

    /// If a deployment sets both variables to the same value, the operator must
    /// keep control rather than be silently downgraded to read-only.
    #[test]
    fn identical_tokens_resolve_to_control_not_listen() {
        assert_eq!(
            capability(Some("same"), Some("same"), false, false, bearer("same")),
            Some(Capability::Control)
        );
    }

    #[test]
    fn a_malformed_authorization_header_does_not_pass() {
        assert!(!ok(Some("c"), None, "GET", "/api/s", |k| {
            (k == "authorization").then_some("c")
        }));
    }

    #[test]
    fn the_loopback_grant_is_off_unless_explicitly_enabled() {
        assert!(!local_listen_enabled(None));
        assert!(!local_listen_enabled(Some("0")));
        assert!(!local_listen_enabled(Some("")));
        assert!(local_listen_enabled(Some("1")));
        assert!(local_listen_enabled(Some("true")));
    }

    /// The desktop app's case: a local window renders telemetry with no
    /// credential at all.
    #[test]
    fn loopback_reads_without_a_token_when_enabled() {
        assert!(authorized(
            Some("c"),
            None,
            true,
            true,
            "GET",
            "/api/summary",
            none
        ));
    }

    /// "Already on the box" is a reason to let someone watch, not to let them
    /// drive the hardware.
    #[test]
    fn loopback_can_never_write_without_a_token() {
        assert!(!authorized(
            Some("c"),
            None,
            true,
            true,
            "POST",
            "/api/cycle",
            none
        ));
        assert!(!authorized(
            Some("c"),
            None,
            true,
            true,
            "POST",
            "/api/matrix",
            none
        ));
    }

    #[test]
    fn a_remote_peer_gets_nothing_even_when_the_grant_is_on() {
        assert!(!authorized(
            Some("c"),
            None,
            false,
            true,
            "GET",
            "/api/summary",
            none
        ));
    }

    #[test]
    fn loopback_without_the_grant_still_needs_a_token() {
        assert!(!authorized(
            Some("c"),
            None,
            true,
            false,
            "GET",
            "/api/summary",
            none
        ));
    }

    /// A local caller holding the control token keeps Control; the loopback
    /// grant must not cap them at read-only.
    #[test]
    fn the_loopback_grant_never_downgrades_a_real_credential() {
        assert_eq!(
            capability(Some("c"), None, true, true, bearer("c")),
            Some(Capability::Control)
        );
        assert!(authorized(
            Some("c"),
            None,
            true,
            true,
            "POST",
            "/api/cycle",
            bearer("c")
        ));
    }

    /// A wrong token is a wrong token even from loopback. Falling through to
    /// the grant would make a bad credential indistinguishable from none and
    /// hide a misconfigured client.
    #[test]
    fn a_wrong_token_from_loopback_is_refused_not_downgraded() {
        assert_eq!(
            capability(Some("c"), None, true, true, bearer("wrong")),
            None
        );
        assert!(!authorized(
            Some("c"),
            None,
            true,
            true,
            "GET",
            "/api/s",
            bearer("wrong")
        ));
    }
}
