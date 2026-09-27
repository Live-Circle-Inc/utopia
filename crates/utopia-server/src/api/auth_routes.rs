use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::{User, Workspace};
use utopia_core::AppError;

use crate::auth::{self, AuthUser};
use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct RegisterReq {
    pub email: String,
    pub password: String,
    pub display_name: String,
    pub org_name: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub email: String,
    pub password: String,
}

fn validate_register(req: &RegisterReq) -> Result<(), AppError> {
    if !req.email.contains('@') || req.email.len() > 254 {
        return Err(AppError::invalid("bad_email", "Invalid email address"));
    }
    if req.password.chars().count() < 8 {
        return Err(AppError::invalid(
            "password_too_short",
            "Password must be at least 8 characters",
        ));
    }
    if req.display_name.trim().is_empty() || req.display_name.chars().count() > 64 {
        return Err(AppError::invalid(
            "bad_display_name",
            "Display name must be 1-64 characters",
        ));
    }
    Ok(())
}

pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<RegisterReq>,
) -> ApiResult<(CookieJar, Json<serde_json::Value>)> {
    validate_register(&req)?;
    let hash = auth::hash_password(&req.password)?;
    let org_name = req
        .org_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // Registration switch: the in-database setting (toggleable from /admin) wins; before the
    // database is built (first-user bootstrap) it always lets you through
    let open = utopia_store::access::open_registration(&state.pool)
        .await
        .unwrap_or(state.open_registration);
    let (user, workspace): (User, Workspace) = utopia_store::accounts::register(
        &state.pool,
        req.email.trim(),
        &hash,
        req.display_name.trim(),
        org_name,
        open,
    )
    .await?;

    let token = auth::issue_token(&state, user.id)?;
    let secure = auth::behind_tls(&headers, state.cookie_secure);
    let jar = jar.add(auth::auth_cookie(token.clone(), secure));
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "auth.register",
        "user",
        Some(user.id),
        json!({ "email": user.email, "is_admin": user.is_admin }),
    )
    .await;
    Ok((
        jar,
        Json(json!({ "user": user, "workspace": workspace, "token": token })),
    ))
}

/// Leave a trace of a failed login: there is no account to attribute it to, so actor is NULL
/// and the attempted email goes into detail. Tell "the email does not exist" apart from "the
/// password is wrong" -- a flood of the former from one IP is email enumeration, a flood of the
/// latter concentrated on a single account is credential stuffing, and the two attacks have
/// different shapes. The ledger is readable by admins only, so there is no way to use this to
/// probe whether an account is registered.
async fn record_login_failure(state: &AppState, email: &str, reason: &str) {
    let _ = utopia_store::audit::record_opt(
        &state.pool,
        None,
        None,
        "auth.login_failed",
        "user",
        None,
        json!({ "email": email, "reason": reason }),
    )
    .await;
}

pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<LoginReq>,
) -> ApiResult<(CookieJar, Json<serde_json::Value>)> {
    let email = req.email.trim();
    let Some(user) = utopia_store::accounts::find_user_by_email(&state.pool, email).await? else {
        record_login_failure(&state, email, "unknown_email").await;
        return Err(AppError::Unauthorized.into());
    };
    if !auth::verify_password(&req.password, &user.password_hash) {
        record_login_failure(&state, email, "bad_password").await;
        return Err(AppError::Unauthorized.into());
    }
    let token = auth::issue_token(&state, user.id)?;
    let secure = auth::behind_tls(&headers, state.cookie_secure);
    let jar = jar.add(auth::auth_cookie(token.clone(), secure));
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "auth.login",
        "user",
        Some(user.id),
        json!({}),
    )
    .await;
    Ok((jar, Json(json!({ "user": user, "token": token }))))
}

/// Logging out does not require a valid session -- an expired cookie must still be clearable,
/// otherwise the frontend is stuck on a dead session. So this decodes the token itself to get
/// the identity; if it will not decode, it just clears the cookie and records nothing.
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> (CookieJar, Json<serde_json::Value>) {
    if let Some(user_id) = jar
        .get(auth::COOKIE_NAME)
        .and_then(|c| auth::decode_user_id(&state, c.value()).ok())
    {
        let _ = utopia_store::audit::record(
            &state.pool,
            None,
            user_id,
            "auth.logout",
            "user",
            Some(user_id),
            json!({}),
        )
        .await;
    }
    let secure = auth::behind_tls(&headers, state.cookie_secure);
    let jar = jar.remove(auth::clear_auth_cookie(secure));
    (jar, Json(json!({ "ok": true })))
}

pub async fn me(AuthUser(user): AuthUser) -> Json<User> {
    Json(user)
}

#[derive(Deserialize)]
pub struct UpdateMeReq {
    pub display_name: String,
}

/// Profile: change the display name.
pub async fn update_me(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<UpdateMeReq>,
) -> ApiResult<Json<User>> {
    let name = req.display_name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(
            AppError::invalid("bad_display_name", "Display name must be 1-64 characters").into(),
        );
    }
    let updated = utopia_store::accounts::update_display_name(&state.pool, user.id, name).await?;
    Ok(Json(updated))
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    pub current_password: String,
    pub new_password: String,
}

/// Change password: verify the old one → hash the new one. A wrong old password is kept
/// distinct from "not logged in", so the frontend can give an accurate message.
pub async fn change_password(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<ChangePasswordReq>,
) -> ApiResult<Json<serde_json::Value>> {
    if !auth::verify_password(&req.current_password, &user.password_hash) {
        return Err(AppError::invalid("wrong_password", "Current password is incorrect").into());
    }
    if req.new_password.chars().count() < 8 {
        return Err(AppError::invalid(
            "password_too_short",
            "Password must be at least 8 characters",
        )
        .into());
    }
    let hash = auth::hash_password(&req.new_password)?;
    utopia_store::accounts::update_password(&state.pool, user.id, &hash).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "auth.password_changed",
        "user",
        Some(user.id),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}
