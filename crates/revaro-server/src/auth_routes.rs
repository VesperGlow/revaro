//! Authentication, second-factor and profile HTTP endpoints.
//!
//! Ported from Go's `internal/server/server_auth.go`. Two things are preserved
//! exactly because the browser client depends on them:
//!
//! * the request and response bodies, which are the shared DTOs in
//!   [`revaro_core::api::auth`];
//! * the error mapping, including the `totp_required` and
//!   `invalid_second_factor` codes and the `410 Gone` for an expired setup.
//!
//! The origin guard already rejects cross-origin writes before these handlers
//! run; nothing here re-implements it. The session cookie is the
//! `SameSite=Lax` half of the same defence.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{FromRequest, FromRequestParts, Request, State};
use axum::response::{IntoResponse as _, Response};
use axum::{Json, Router, routing};
use axum_extra::extract::cookie::{Cookie, SameSite};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::StatusCode;
use http::header::{self, HeaderValue};
use http::request::Parts;
use revaro_core::api::auth::{
    AvatarRequest, ChangePasswordRequest, ChangeUsernameRequest, LoginRequest, PasswordCodeRequest,
    PasswordRequest, TotpRecovery, TotpSetup,
};
use revaro_core::keys::AVATAR_KEY;
use revaro_core::model::Profile;
use revaro_core::{ApiError, ErrorCode, Timestamp};
use time::{Duration as TimeDuration, OffsetDateTime};

use crate::auth::{AuthError, AuthUser, SESSION_COOKIE, SESSION_LIFETIME_MILLIS};
use crate::config::Config;
use crate::error::{JsonStatus, invalid_json};
use crate::state::AppState;

/// Largest accepted avatar, matching Go's `maxAvatarBytes` (`2 << 20`).
pub const MAX_AVATAR_BYTES: usize = 2 << 20;

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginInput {
    username: Option<String>,
    password: Option<String>,
    second_factor: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordInput {
    current_password: Option<String>,
    password: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UsernameInput {
    username: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentPasswordInput {
    current_password: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordCodeInput {
    current_password: Option<String>,
    code: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AvatarInput {
    data_url: Option<String>,
}

/// The authenticated HTTP surface, mounted under `/api` by [`crate::router`].
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/login", routing::post(login))
        .route("/auth/logout", routing::post(logout))
        .route("/auth/me", routing::get(me))
        .route("/auth/password", routing::patch(change_password))
        .route("/auth/totp", routing::get(totp_status).delete(disable_totp))
        .route("/auth/totp/setup", routing::post(begin_totp_setup))
        .route("/auth/totp/enable", routing::post(enable_totp))
        .route(
            "/auth/totp/recovery-codes",
            routing::post(regenerate_totp_recovery_codes),
        )
        .route("/profile/username", routing::patch(change_username))
        .route(
            "/profile/avatar",
            routing::get(get_avatar)
                .put(update_avatar)
                .delete(delete_avatar),
        )
}

/// A JSON body extractor that collapses every rejection into the historical
/// opaque `400 invalid JSON request`.
///
/// Go's `decodeJSON` set `DisallowUnknownFields` and reported nothing about the
/// parse failure; the DTOs already deny unknown fields, so this only rewrites
/// the message.
pub struct JsonBody<T>(pub T);

impl<T, S> FromRequest<S> for JsonBody<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(_) => Err(invalid_json()),
        }
    }
}

/// The client address used for login rate limiting.
///
/// Derived exactly like Go's `clientIP`: the peer address wins unless it sits
/// inside `TRUSTED_PROXIES`, in which case `X-Forwarded-For` is walked
/// right-to-left and the first untrusted address is the client. Spoofed values
/// farther left are ignored because the chain is only believed from a trusted
/// peer.
pub struct ClientIp(pub String);

impl FromRequestParts<Arc<AppState>> for ClientIp {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let peer = axum::extract::ConnectInfo::<SocketAddr>::from_request_parts(parts, state)
            .await
            .ok()
            .map(|connect| connect.0);
        Ok(Self(client_ip(parts, &state.config, peer)))
    }
}

/// Resolve the client address from the peer and any forwarded chain.
fn client_ip(parts: &Parts, config: &Config, peer: Option<SocketAddr>) -> String {
    let host = peer.map_or_else(String::new, |address| address.ip().to_string());
    let Ok(peer_address) = host.parse::<IpAddr>() else {
        return host;
    };
    let peer_address = unmap(peer_address);
    if !trusted(config, peer_address) {
        return host;
    }
    if let Some(header) = parts
        .headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    {
        for candidate in header.split(',').rev() {
            let Ok(address) = candidate.trim().parse::<IpAddr>() else {
                continue;
            };
            let address = unmap(address);
            if !trusted(config, address) {
                return address.to_string();
            }
        }
    }
    peer_address.to_string()
}

fn trusted(config: &Config, address: IpAddr) -> bool {
    config
        .trusted_proxies
        .iter()
        .any(|prefix| prefix.contains(address))
}

fn unmap(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(value) => match value.to_ipv4_mapped() {
            Some(mapped) => IpAddr::V4(mapped),
            None => address,
        },
        IpAddr::V4(_) => address,
    }
}

/// `POST /api/auth/login`
async fn login(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    request: Request,
) -> Result<Response, ApiError> {
    let limiter = state.auth.limiter();
    if !limiter.allow(&ip) {
        return Ok(problem_with_retry(
            StatusCode::TOO_MANY_REQUESTS,
            "too many login attempts; try again later",
            crate::auth::RETRY_AFTER_BLOCKED,
        ));
    }
    let JsonBody(input) = JsonBody::<Option<LoginInput>>::from_request(request, &state).await?;
    let input = input.unwrap_or_default();
    let body = LoginRequest {
        username: input.username.unwrap_or_default(),
        password: input.password.unwrap_or_default(),
        second_factor: input.second_factor.unwrap_or_default(),
    };
    if body.username.len() > 128 || body.password.len() > 1024 || body.second_factor.len() > 128 {
        limiter.fail(&ip);
        return Err(ApiError::unauthorized("invalid credentials"));
    }
    let Some(permit) = limiter.acquire() else {
        return Ok(problem_with_retry(
            StatusCode::TOO_MANY_REQUESTS,
            "login verification is busy; try again shortly",
            crate::auth::RETRY_AFTER_BUSY,
        ));
    };
    let outcome = state
        .auth
        .login(&body.username, &body.password, &body.second_factor)
        .await;
    drop(permit);
    match outcome {
        Ok((token, expires)) => {
            limiter.success(&ip);
            let profile = Profile {
                username: body.username,
                has_avatar: state.auth.has_avatar().await.unwrap_or(false),
            };
            tracing::info!(user = %profile.username, "user logged in");
            let response = Json(profile).into_response();
            Ok(with_cookie(
                response,
                session_cookie(&state.config, &token, expires),
            ))
        }
        Err(AuthError::TotpRequired) => Err(ApiError::unauthorized(
            "enter your authenticator or recovery code",
        )
        .with_code(ErrorCode::TOTP_REQUIRED)),
        Err(AuthError::InvalidSecondFactor) => {
            limiter.fail(&ip);
            Err(
                ApiError::unauthorized("the authenticator or recovery code is incorrect")
                    .with_code(ErrorCode::INVALID_SECOND_FACTOR),
            )
        }
        Err(AuthError::InvalidCredentials) => {
            limiter.fail(&ip);
            Err(ApiError::unauthorized("invalid credentials"))
        }
        Err(error) => {
            tracing::error!(%error, "login failed");
            Err(ApiError::internal("could not verify login"))
        }
    }
}

/// `POST /api/auth/logout`
async fn logout(State(state): State<Arc<AppState>>, user: AuthUser) -> Result<Response, ApiError> {
    if let Err(error) = state.auth.logout(&user.token).await {
        tracing::error!(%error, "logout failed");
    }
    Ok(with_cookie(
        StatusCode::NO_CONTENT.into_response(),
        clear_session_cookie(&state.config),
    ))
}

/// `GET /api/auth/me`
async fn me(State(state): State<Arc<AppState>>, user: AuthUser) -> Result<Json<Profile>, ApiError> {
    Ok(Json(Profile {
        username: user.username,
        has_avatar: state.auth.has_avatar().await.unwrap_or(false),
    }))
}

/// `PATCH /api/auth/password`
async fn change_password(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    request: Request,
) -> Result<Response, ApiError> {
    let JsonBody(input) = JsonBody::<Option<PasswordInput>>::from_request(request, &state).await?;
    let input = input.unwrap_or_default();
    let body = ChangePasswordRequest {
        current_password: input.current_password.unwrap_or_default(),
        password: input.password.unwrap_or_default(),
    };
    if body.password.len() < 12 || body.password.len() > 1024 || body.current_password.len() > 1024
    {
        return Err(ApiError::bad_request(
            "password must be between 12 and 1024 characters",
        ));
    }
    match state
        .auth
        .change_credentials(
            &user.username,
            &body.current_password,
            &user.username,
            &body.password,
        )
        .await
    {
        Ok(()) => {
            tracing::info!(user = %user.username, "administrator password changed");
            Ok(with_cookie(
                StatusCode::NO_CONTENT.into_response(),
                clear_session_cookie(&state.config),
            ))
        }
        Err(AuthError::InvalidCredentials) => {
            Err(ApiError::unauthorized("current password is incorrect"))
        }
        Err(error) => {
            tracing::error!(%error, "password change failed");
            Err(ApiError::internal("could not update password"))
        }
    }
}

/// `PATCH /api/profile/username`
async fn change_username(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    request: Request,
) -> Result<StatusCode, ApiError> {
    let JsonBody(input) = JsonBody::<Option<UsernameInput>>::from_request(request, &state).await?;
    let body = ChangeUsernameRequest {
        username: input.unwrap_or_default().username.unwrap_or_default(),
    };
    let username = body.username.trim();
    if username.is_empty() || username.len() > 128 {
        return Err(ApiError::bad_request(
            "username must be between 1 and 128 characters",
        ));
    }
    match state.auth.change_username(username).await {
        Ok(()) => {
            tracing::info!(previous_user = %user.username, user = %username, "administrator username changed");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(error) => {
            tracing::error!(%error, "username change failed");
            Err(ApiError::internal("could not update username"))
        }
    }
}

/// `GET /api/auth/totp`
async fn totp_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<revaro_core::api::auth::TotpStatus>, ApiError> {
    match state.auth.totp_status().await {
        Ok(status) => Ok(Json(status)),
        Err(error) => {
            tracing::error!(%error, "TOTP status read failed");
            Err(ApiError::internal("could not read two-factor settings"))
        }
    }
}

/// `POST /api/auth/totp/setup`
async fn begin_totp_setup(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    request: Request,
) -> Result<JsonStatus<TotpSetup>, ApiError> {
    let JsonBody(input) =
        JsonBody::<Option<CurrentPasswordInput>>::from_request(request, &state).await?;
    let body = PasswordRequest {
        current_password: input
            .unwrap_or_default()
            .current_password
            .unwrap_or_default(),
    };
    if body.current_password.is_empty() || body.current_password.len() > 1024 {
        return Err(ApiError::bad_request("current password is required"));
    }
    let setup = match state
        .auth
        .begin_totp_setup(&user.username, &body.current_password)
        .await
    {
        Ok(setup) => setup,
        Err(error) => return Err(totp_problem(error)),
    };
    let qr_data_url = match crate::auth::qr_code_data_url(&setup.uri) {
        Ok(data_url) => data_url,
        Err(error) => {
            tracing::error!(%error, "TOTP QR encoding failed");
            return Err(ApiError::internal("could not create authenticator QR code"));
        }
    };
    Ok(JsonStatus(
        StatusCode::CREATED,
        TotpSetup {
            secret: setup.secret,
            uri: setup.uri,
            qr_data_url,
        },
    ))
}

/// `POST /api/auth/totp/enable`
async fn enable_totp(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    request: Request,
) -> Result<Json<TotpRecovery>, ApiError> {
    let JsonBody(input) =
        JsonBody::<Option<PasswordCodeInput>>::from_request(request, &state).await?;
    let input = input.unwrap_or_default();
    let body = PasswordCodeRequest {
        current_password: input.current_password.unwrap_or_default(),
        code: input.code.unwrap_or_default(),
    };
    if !valid_totp_confirmation(&body) {
        return Err(ApiError::bad_request(
            "current password and verification code are required",
        ));
    }
    match state
        .auth
        .confirm_totp_setup(
            &user.username,
            &body.current_password,
            &body.code,
            &user.token,
        )
        .await
    {
        Ok(codes) => {
            tracing::info!(user = %user.username, "TOTP two-factor authentication enabled");
            Ok(Json(TotpRecovery {
                enabled: true,
                recovery_codes: codes,
            }))
        }
        Err(error) => Err(totp_problem(error)),
    }
}

/// `POST /api/auth/totp/recovery-codes`
async fn regenerate_totp_recovery_codes(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    request: Request,
) -> Result<Json<TotpRecovery>, ApiError> {
    let JsonBody(input) =
        JsonBody::<Option<PasswordCodeInput>>::from_request(request, &state).await?;
    let input = input.unwrap_or_default();
    let body = PasswordCodeRequest {
        current_password: input.current_password.unwrap_or_default(),
        code: input.code.unwrap_or_default(),
    };
    if !valid_totp_confirmation(&body) {
        return Err(ApiError::bad_request(
            "current password and verification code are required",
        ));
    }
    match state
        .auth
        .regenerate_recovery_codes(&user.username, &body.current_password, &body.code)
        .await
    {
        Ok(codes) => {
            tracing::info!(user = %user.username, "TOTP recovery codes regenerated");
            Ok(Json(TotpRecovery {
                enabled: true,
                recovery_codes: codes,
            }))
        }
        Err(error) => Err(totp_problem(error)),
    }
}

/// `DELETE /api/auth/totp`
async fn disable_totp(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    request: Request,
) -> Result<StatusCode, ApiError> {
    let JsonBody(input) =
        JsonBody::<Option<PasswordCodeInput>>::from_request(request, &state).await?;
    let input = input.unwrap_or_default();
    let body = PasswordCodeRequest {
        current_password: input.current_password.unwrap_or_default(),
        code: input.code.unwrap_or_default(),
    };
    if !valid_totp_confirmation(&body) {
        return Err(ApiError::bad_request(
            "current password and verification code are required",
        ));
    }
    match state
        .auth
        .disable_totp(
            &user.username,
            &body.current_password,
            &body.code,
            &user.token,
        )
        .await
    {
        Ok(()) => {
            tracing::info!(user = %user.username, "TOTP two-factor authentication disabled");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(error) => Err(totp_problem(error)),
    }
}

/// `GET /api/profile/avatar`
async fn get_avatar(State(state): State<Arc<AppState>>) -> Result<Response, ApiError> {
    let mime = match state.auth.avatar_mime().await {
        Ok(Some(mime)) => mime,
        Ok(None) => return Err(ApiError::not_found("avatar not found")),
        Err(error) => {
            tracing::error!(%error, "avatar metadata read failed");
            return Err(ApiError::internal("database error"));
        }
    };
    let data = match state.store.read(AVATAR_KEY, MAX_AVATAR_BYTES).await {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "avatar read failed");
            return Err(ApiError::new(502, "could not read avatar"));
        }
    };
    let length = data.len();
    let mut response = Response::new(axum::body::Body::from(data));
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&mime) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    Ok(response)
}

/// `PUT /api/profile/avatar`
async fn update_avatar(
    State(state): State<Arc<AppState>>,
    request: Request,
) -> Result<StatusCode, ApiError> {
    let JsonBody(input) = JsonBody::<Option<AvatarInput>>::from_request(request, &state).await?;
    let body = AvatarRequest {
        data_url: input.unwrap_or_default().data_url.unwrap_or_default(),
    };
    let Some(comma) = body.data_url.find(',') else {
        return Err(ApiError::bad_request("avatar must be a data URL"));
    };
    if !body.data_url.starts_with("data:image/") {
        return Err(ApiError::bad_request("avatar must be a data URL"));
    }
    let Ok(data) = STANDARD.decode(&body.data_url[comma + 1..]) else {
        return Err(ApiError::bad_request("avatar data is invalid"));
    };
    if data.is_empty() {
        return Err(ApiError::bad_request("avatar data is invalid"));
    }
    if data.len() > MAX_AVATAR_BYTES {
        return Err(ApiError::payload_too_large("avatar must not exceed 2 MiB"));
    }
    let Some(mime) = sniff_image(&data) else {
        return Err(ApiError::unsupported_media_type(
            "avatar must be JPEG, PNG, GIF, or WebP",
        ));
    };
    if let Err(error) = state.store.put(AVATAR_KEY, &data).await {
        tracing::error!(%error, "avatar write failed");
        return Err(ApiError::new(502, "could not save avatar"));
    }
    if let Err(error) = state.auth.set_avatar_mime(mime).await {
        tracing::error!(%error, "avatar metadata write failed");
        return Err(ApiError::internal("could not save avatar"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/profile/avatar`
async fn delete_avatar(State(state): State<Arc<AppState>>) -> Result<StatusCode, ApiError> {
    if let Err(error) = state.store.delete(AVATAR_KEY).await {
        tracing::error!(%error, "avatar delete failed");
        return Err(ApiError::new(502, "could not delete avatar"));
    }
    if let Err(error) = state.auth.clear_avatar_mime().await {
        tracing::error!(%error, "avatar metadata delete failed");
        return Err(ApiError::internal("could not delete avatar"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The `totpProblem` mapping from Go, variant for variant.
fn totp_problem(error: AuthError) -> ApiError {
    match error {
        AuthError::InvalidCredentials => ApiError::unauthorized("current password is incorrect"),
        AuthError::InvalidSecondFactor => {
            ApiError::unauthorized("the authenticator or recovery code is incorrect")
        }
        AuthError::TotpAlreadyEnabled => {
            ApiError::conflict("two-factor authentication is already enabled")
        }
        AuthError::TotpNotEnabled => ApiError::conflict("two-factor authentication is not enabled"),
        AuthError::TotpSetupExpired => ApiError::new(410, "two-factor setup expired; start again"),
        other => {
            tracing::error!(error = %other, "TOTP operation failed");
            ApiError::internal("could not update two-factor settings")
        }
    }
}

fn valid_totp_confirmation(body: &PasswordCodeRequest) -> bool {
    !body.current_password.is_empty()
        && body.current_password.len() <= 1024
        && !body.code.trim().is_empty()
        && body.code.len() <= 128
}

/// Build the session cookie: `Path=/`, `HttpOnly`, `SameSite=Lax`, `Secure`
/// from configuration, `Expires` and `MaxAge`, and deliberately no `Domain`.
fn session_cookie(config: &Config, token: &str, expires: Timestamp) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token.to_owned()))
        .path("/")
        .http_only(true)
        .secure(config.cookie_secure)
        .same_site(SameSite::Lax)
        .expires(offset_datetime(expires))
        .max_age(TimeDuration::seconds(SESSION_LIFETIME_MILLIS / 1000))
        .build()
}

/// Build the clearing cookie: empty value, `MaxAge=-1`, expiry in 1971.
fn clear_session_cookie(config: &Config) -> Cookie<'static> {
    let epoch_plus_one =
        OffsetDateTime::from_unix_timestamp(1).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    Cookie::build((SESSION_COOKIE, String::new()))
        .path("/")
        .http_only(true)
        .secure(config.cookie_secure)
        .same_site(SameSite::Lax)
        .expires(epoch_plus_one)
        .max_age(TimeDuration::seconds(-1))
        .build()
}

fn with_cookie(mut response: Response, cookie: Cookie<'static>) -> Response {
    if let Ok(value) = HeaderValue::from_str(&cookie.to_string()) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

fn offset_datetime(timestamp: Timestamp) -> OffsetDateTime {
    let value = timestamp.as_datetime();
    let nanos =
        i128::from(value.timestamp()) * 1_000_000_000 + i128::from(value.timestamp_subsec_nanos());
    OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// A problem response carrying a `Retry-After` header.
fn problem_with_retry(status: StatusCode, message: &str, retry_after: &str) -> Response {
    let mut response = ApiError::new(status.as_u16(), message).into_response();
    if let Ok(value) = HeaderValue::from_str(retry_after) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// Identify the four accepted avatar formats the way Go's
/// `http.DetectContentType` does for these signatures.
fn sniff_image(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}
