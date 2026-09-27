//! Request-origin capture: stash the client IP and User-Agent in a task-local for the audit
//! writes to read back.
//!
//! It goes through a task-local instead of being threaded through as parameters because
//! `audit::record` has twenty-odd call sites scattered across the individual business
//! handlers; rewriting every signature for two fields that have nothing to do with the
//! business would only make every call site carry something that is none of its concern.

use axum::extract::ConnectInfo;
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use std::net::SocketAddr;
use utopia_store::audit::{ClientContext, CLIENT};

/// The original client address as forwarded by a reverse proxy. Takes the first segment of
/// X-Forwarded-For (the hop nearest the client), then X-Real-IP, and if neither is present,
/// the real TCP peer address.
///
/// Both of those headers can be forged by the client and should not be trusted in a
/// direct-connect deployment; but on a direct connection they do not exist in the first
/// place, so falling back to the TCP peer address is the true value. When deployed behind a
/// proxy, the proxy should overwrite them rather than append to them.
fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<String> {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = xff
            .split(',')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(first.to_string());
        }
    }
    if let Some(real) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let real = real.trim();
        if !real.is_empty() {
            return Some(real.to_string());
        }
    }
    peer.map(|a| a.ip().to_string())
}

/// The User-Agent is truncated to 256 bytes: it is entirely client-controlled, and one
/// request header should not be allowed to blow the ledger wide open.
fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut end = s.len().min(256);
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            s[..end].to_string()
        })
}

pub async fn capture(req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0);
    let ctx = ClientContext {
        ip: client_ip(req.headers(), peer),
        user_agent: user_agent(req.headers()),
    };
    CLIENT.scope(ctx, next.run(req)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn xff_wins_and_takes_the_client_hop() {
        let h = headers(&[("x-forwarded-for", "203.0.113.7, 10.0.0.1, 10.0.0.2")]);
        let peer = Some("10.0.0.9:5000".parse().unwrap());
        assert_eq!(client_ip(&h, peer).as_deref(), Some("203.0.113.7"));
    }

    #[test]
    fn falls_back_to_real_ip_then_peer() {
        let h = headers(&[("x-real-ip", "198.51.100.4")]);
        assert_eq!(client_ip(&h, None).as_deref(), Some("198.51.100.4"));

        let peer = Some("192.0.2.5:443".parse().unwrap());
        assert_eq!(
            client_ip(&HeaderMap::new(), peer).as_deref(),
            Some("192.0.2.5")
        );
        assert_eq!(client_ip(&HeaderMap::new(), None), None);
    }

    #[test]
    fn blank_headers_do_not_shadow_the_peer() {
        let h = headers(&[("x-forwarded-for", "  "), ("x-real-ip", "")]);
        let peer = Some("192.0.2.5:443".parse().unwrap());
        assert_eq!(client_ip(&h, peer).as_deref(), Some("192.0.2.5"));
    }

    #[test]
    fn user_agent_is_capped_at_256_bytes() {
        let long = "Mozilla/5.0 ".repeat(50); // 600 bytes, way over the cap
        let h = headers(&[("user-agent", long.as_str())]);
        let got = user_agent(&h).unwrap();
        assert_eq!(got.len(), 256);
        assert!(
            long.starts_with(&got),
            "after truncation it is still a prefix of the original"
        );
    }

    /// HeaderValue::to_str only accepts visible ASCII, so a non-ASCII UA cannot be read out
    /// -- recording nothing is better than recording a stretch of mojibake. It is also why
    /// truncating by bytes above is safe: whatever we can read out is guaranteed to be ASCII.
    #[test]
    fn non_ascii_user_agent_is_dropped_rather_than_mangled() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::USER_AGENT,
            axum::http::HeaderValue::from_bytes("浏览器".as_bytes()).unwrap(),
        );
        assert_eq!(user_agent(&h), None);
    }
}
