//! Administrator authentication, ported from Go's `internal/auth` package.
//!
//! This module owns three things and nothing about HTTP:
//!
//! * **Credentials** — Argon2id password hashing in Go's exact encoded format,
//!   so hashes already in the SQLite `settings` table keep verifying.
//! * **Sessions** — opaque tokens whose SHA-256 is the only thing persisted,
//!   with a fixed lifetime and no rotation.
//! * **TOTP** — the second-factor state machine, including the password-derived
//!   encrypted secret envelope, recovery codes and replay protection.
//!
//! ## Deliberate differences from the Go implementation
//!
//! * There is no background cleanup goroutine. [`AuthService::cleanup_expired_sessions`]
//!   is exposed instead, and the server's timer should call it every 15 minutes
//!   (the Go server registered `auth.Cleanup` with that interval).
//! * Errors are a typed [`AuthError`] rather than Go sentinel values compared
//!   with `errors.Is`; the HTTP boundary maps them in one place.
//! * Every Argon2id derivation is serialised through a process-global semaphore
//!   of two, mirroring Go's `kdfSlots`, and runs on a blocking thread so a
//!   64 MiB password hash never stalls the async runtime.

mod base32;
pub mod extract;
mod limiter;
mod password;
mod service;
mod session;
mod totp;

pub use extract::AuthUser;
pub use limiter::{LoginLimiter, RETRY_AFTER_BLOCKED, RETRY_AFTER_BUSY};
pub use password::{DEFAULT_PARAMS, Params, hash_password, verify_password};
pub use revaro_core::api::auth::TotpStatus;
pub use service::{AuthService, Clock, InitialCredentials, MAX_LOGIN_CONCURRENCY};
pub use session::{SESSION_COOKIE, SESSION_LIFETIME_MILLIS, token_hash};
pub use totp::{TOTP_ISSUER, TotpSetup};

/// Render a provisioning URI as a PNG QR data URL.
///
/// Exposed for the HTTP layer, which is the only place that knows about data
/// URLs; the TOTP state machine itself stays free of presentation concerns.
///
/// # Errors
/// Returns [`AuthError::Internal`] when the QR code cannot be encoded.
pub fn qr_code_data_url(uri: &str) -> Result<String, AuthError> {
    totp::qr_png_data_url(uri)
}

use crate::db::DbError;

/// Fill a fresh buffer from the operating system CSPRNG.
///
/// Every secret this module mints — salts, session tokens, TOTP secrets, nonces
/// and recovery-code entropy — comes from here.
fn random_bytes(length: usize) -> Vec<u8> {
    use rand::RngCore as _;

    let mut buffer = vec![0u8; length];
    rand::rng().fill_bytes(&mut buffer);
    buffer
}

/// Every way an authentication operation can fail.
///
/// The variants are the Go package's sentinel errors plus the validation
/// failures it returned with `errors.New`; the HTTP layer is responsible for
/// turning them into status codes and messages.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The username or password did not match the stored administrator.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// A second factor is required but none was supplied.
    #[error("two-factor authentication required")]
    TotpRequired,
    /// The authenticator or recovery code was rejected (including replay).
    #[error("invalid two-factor code")]
    InvalidSecondFactor,
    /// TOTP is already enabled.
    #[error("two-factor authentication already enabled")]
    TotpAlreadyEnabled,
    /// TOTP is not enabled, so the operation is meaningless.
    #[error("two-factor authentication is not enabled")]
    TotpNotEnabled,
    /// The pending TOTP setup is missing or older than ten minutes.
    #[error("two-factor setup expired")]
    TotpSetupExpired,
    /// The session token is absent from the database.
    #[error("invalid session")]
    InvalidSession,
    /// The session exists but its deadline has passed (the row is removed).
    #[error("expired session")]
    ExpiredSession,
    /// A caller-supplied value failed a length or shape check.
    #[error("{0}")]
    Invalid(String),
    /// The password hash, encrypted secret or stored JSON is unusable.
    #[error("{0}")]
    Corrupt(String),
    /// The database call failed.
    #[error(transparent)]
    Database(#[from] DbError),
    /// A blocking worker failed or a primitive rejected its inputs.
    #[error("{0}")]
    Internal(String),
}
