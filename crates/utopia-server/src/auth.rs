//! Authentication: argon2 password hashing + JWT (HttpOnly cookie, Bearer also accepted).
//! The cookie itself is a session cookie; expiry is controlled by the JWT's exp (7 days).

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use utopia_core::models::User;
use utopia_core::AppError;
use uuid::Uuid;

use crate::error::ApiErr;
use crate::state::AppState;

pub const COOKIE_NAME: &str = "utopia_token";
const TOKEN_TTL_DAYS: i64 = 7;

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: Uuid,
    exp: i64,
}

pub fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Other(anyhow::anyhow!("Password hashing failed: {e}")))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

pub fn issue_token(state: &AppState, user_id: Uuid) -> Result<String, AppError> {
    let claims = Claims {
        sub: user_id,
        exp: (Utc::now() + chrono::Duration::days(TOKEN_TTL_DAYS)).timestamp(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(state.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Other(anyhow::anyhow!("Token issuance failed: {e}")))
}

/// Whether TLS is running out in front. Reverse proxies all set `X-Forwarded-Proto`; with no
/// such header (local direct connection, dev environment) we treat it as plaintext, do not set
/// Secure, and login keeps working.
///
/// No "trusted proxies" list is needed: forging this header only turns the attacker's own cookie
/// into a Secure one, which is stricter rather than looser, so there is nothing to gain from it.
/// `UTOPIA_COOKIE_SECURE=true` can force it on, as a fallback for proxies that do not send the
/// header.
pub fn behind_tls(headers: &axum::http::HeaderMap, forced: bool) -> bool {
    forced
        || headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            // Through multiple proxies this header is a comma-separated chain, and the
            // leftmost entry is the original hop
            .and_then(|v| v.split(',').next())
            .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"))
}

/// The session cookie. `secure` is decided by [`behind_tls`] -- under HTTPS we set Secure, and
/// then the browser will no longer send it out over a plaintext link.
pub fn auth_cookie(token: String, secure: bool) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, token))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .build()
}

/// The deletion instruction used for logout. **The attributes must match those used when it was
/// issued**: the browser matches on name + domain + path to work out which entry to delete, and
/// if you give only the name then path degrades to the current request path
/// (`/api/v1/auth`), which does not match the `/` used at issue time -- the cookie stays in the
/// browser and the human thinks they logged themselves out.
pub fn clear_auth_cookie(secure: bool) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .build()
}

pub(crate) fn decode_user_id(state: &AppState, token: &str) -> Result<Uuid, AppError> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| AppError::Unauthorized)?;
    Ok(data.claims.sub)
}

/// Extractor for a logged-in user: the `utopia_token` cookie, or `Authorization: Bearer`.
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiErr;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get(COOKIE_NAME)
            .map(|c| c.value().to_string())
            .or_else(|| {
                parts
                    .headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .map(|v| v.to_string())
            })
            .ok_or(AppError::Unauthorized)?;

        let user_id = decode_user_id(state, &token)?;
        let user = utopia_store::accounts::find_user_by_id(&state.pool, user_id)
            .await?
            .ok_or(AppError::Unauthorized)?;
        Ok(AuthUser(user))
    }
}

/// Generate one JWT signing secret: 32 bytes from a CSPRNG, hex-encoded into 64 characters.
///
/// The length is 32 bytes because HS256's HMAC block is exactly 32 bytes -- anything longer gets
/// hashed down first and does not add strength. hex rather than base64: this value shows up in
/// logs, in environment variables and in operators' copy-paste, and carrying no +/= saves a whole
/// class of escaping problems.
pub fn generate_jwt_secret() -> String {
    use argon2::password_hash::rand_core::RngCore;
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims_in(days: i64) -> Claims {
        Claims {
            sub: Uuid::now_v7(),
            exp: (Utc::now() + chrono::Duration::days(days)).timestamp(),
        }
    }

    /// Guardrail for JWT validation: the default configuration must accept HS256 tokens we
    /// issued ourselves, and reject both a swapped key and an expired token.
    /// Added during the jsonwebtoken 9 → 10 upgrade (CVE-2026-25537: the type confusion in
    /// <10.3.0 can bypass authorization) -- a library's default validation semantics are the part
    /// the compiler cannot see, so this is what catches the regression.
    #[test]
    fn default_validation_accepts_own_token_and_rejects_the_rest() {
        let secret = b"test-secret-not-a-real-key";
        let claims = claims_in(7);
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret),
        )
        .expect("issuing must succeed");

        let decoded = decode::<Claims>(
            &token,
            &DecodingKey::from_secret(secret),
            &Validation::default(),
        )
        .expect("own token must verify under default validation");
        assert_eq!(decoded.claims.sub, claims.sub, "sub must survive the trip");

        assert!(
            decode::<Claims>(
                &token,
                &DecodingKey::from_secret(b"a-different-secret"),
                &Validation::default(),
            )
            .is_err(),
            "a token signed with another key must not verify"
        );

        let expired = encode(
            &Header::default(),
            &claims_in(-1),
            &EncodingKey::from_secret(secret),
        )
        .expect("issuing must succeed");
        assert!(
            decode::<Claims>(
                &expired,
                &DecodingKey::from_secret(secret),
                &Validation::default(),
            )
            .is_err(),
            "exp must be enforced by default"
        );
    }

    fn headers_with(proto: Option<&str>) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        if let Some(p) = proto {
            h.insert("x-forwarded-proto", p.parse().unwrap());
        }
        h
    }

    /// The Secure decision must be true only when TLS is confirmed to be in the path: get it
    /// wrong in the true direction and users of a plaintext deployment have the browser throw the
    /// cookie away right after login, with the symptom "clicked log in and came back to the login
    /// page", and no error reported.
    #[test]
    fn secure_only_when_tls_is_actually_in_front() {
        // No proxy: local direct connection, cargo run -- must not set Secure, or you cannot
        // log in over HTTP
        assert!(!behind_tls(&headers_with(None), false));
        assert!(!behind_tls(&headers_with(Some("http")), false));

        assert!(behind_tls(&headers_with(Some("https")), false));
        // The header casing is up to the proxy, so it cannot be assumed
        assert!(behind_tls(&headers_with(Some("HTTPS")), false));

        // With multiple proxies this header is a comma-separated chain and the leftmost entry
        // is the original hop -- taking the wrong end judges "user goes HTTPS to the edge, edge
        // goes HTTP back to the origin" as plaintext
        assert!(behind_tls(&headers_with(Some("https, http")), false));
        assert!(!behind_tls(&headers_with(Some("http, https")), false));

        // Forced on by configuration: a fallback for proxies that do not send the header
        assert!(behind_tls(&headers_with(None), true));
    }
}
