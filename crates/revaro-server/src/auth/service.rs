//! The credential, session and profile settings service.
//!
//! Everything here is a straight port of Go's `auth.Service`. The only
//! structural changes are the typed [`AuthError`] and the fact that each Argon2
//! derivation is awaited through a process-global semaphore while it runs on a
//! blocking thread.

use std::fmt;
use std::sync::{Arc, LazyLock};

use revaro_core::Timestamp;
use rusqlite::{Connection, OptionalExtension as _, params};
use tokio::sync::Semaphore;

use crate::db::{Database, DbError};
use crate::ids::new_id;

use super::AuthError;
use super::limiter::LoginLimiter;
use super::password::{DEFAULT_PARAMS, Params, hash_password, verify_password};
use super::session::{SESSION_LIFETIME_MILLIS, new_token, token_hash};
use super::totp::{TOTP_CONFIG_KEY, TOTP_PENDING_KEY, TOTP_RECOVERY_KEY};

/// Settings key holding the administrator login name.
pub const USERNAME_KEY: &str = "admin_username";
/// Settings key holding the Argon2id password hash.
pub const PASSWORD_HASH_KEY: &str = "admin_password_hash";
/// Settings key holding the `avatar_mime` marker.
pub const AVATAR_MIME_KEY: &str = "avatar_mime";

/// Login verifications allowed to run at once, mirroring Go's two-slot channel.
pub const MAX_LOGIN_CONCURRENCY: usize = 2;

/// A process-global pool of two Argon2id slots.
///
/// Argon2 deliberately allocates tens of megabytes per call. Without this bound
/// a burst of concurrent logins can exhaust the process, which is exactly the
/// failure Go's `kdfSlots` channel prevented.
static KDF_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(MAX_LOGIN_CONCURRENCY));

/// Run `operation` on a blocking thread holding one KDF slot.
///
/// The permit is released when the blocking closure returns, so a panic in the
/// derivation cannot leak a slot.
pub(super) async fn with_kdf<T, F>(operation: F) -> Result<T, AuthError>
where
    F: FnOnce() -> Result<T, AuthError> + Send + 'static,
    T: Send + 'static,
{
    let permit = KDF_SLOTS
        .acquire()
        .await
        .map_err(|_| AuthError::Internal("password derivation pool is closed".to_owned()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|error| AuthError::Internal(error.to_string()))?
}

/// An injectable source of wall-clock time.
///
/// The Go service had a `Now func() time.Time` field for the same reason: the
/// TOTP lifecycle test advances time by 30-second steps instead of sleeping.
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> Timestamp + Send + Sync>);

impl Clock {
    /// The system clock.
    #[must_use]
    pub fn system() -> Self {
        Self(Arc::new(Timestamp::now))
    }

    /// A clock frozen at `value`.
    #[must_use]
    pub fn fixed(value: Timestamp) -> Self {
        Self(Arc::new(move || value))
    }

    /// A clock backed by an arbitrary closure.
    #[must_use]
    pub fn from_fn<F>(source: F) -> Self
    where
        F: Fn() -> Timestamp + Send + Sync + 'static,
    {
        Self(Arc::new(source))
    }

    /// The current instant.
    #[must_use]
    pub fn now(&self) -> Timestamp {
        (self.0)()
    }
}

impl fmt::Debug for Clock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Clock")
    }
}

/// What [`AuthService::initialize`] and [`AuthService::reset_credentials`]
/// report about the administrator account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitialCredentials {
    /// A new account was created by this call.
    pub created: bool,
    /// The password was generated rather than supplied.
    pub generated: bool,
    /// The login name that is now in effect.
    pub username: String,
    /// The generated password, present only when one was generated.
    pub password: String,
}

/// The administrator credential, session and second-factor service.
#[derive(Clone)]
pub struct AuthService {
    pub(super) db: Database,
    pub(super) params: Params,
    pub(super) clock: Clock,
    limiter: LoginLimiter,
}

impl fmt::Debug for AuthService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthService")
            .field("params", &self.params)
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl AuthService {
    /// Build a service over `db` with the product defaults and the system clock.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self {
            db,
            params: DEFAULT_PARAMS,
            clock: Clock::system(),
            limiter: LoginLimiter::new(),
        }
    }

    /// Override the Argon2id parameters.
    #[must_use]
    pub fn with_params(mut self, params: Params) -> Self {
        self.params = params;
        self
    }

    /// Override the clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self.limiter = LoginLimiter::with_clock(self.clock.clone());
        self
    }

    /// Override the login limiter (used by tests to inject a clock).
    #[must_use]
    pub fn with_limiter(mut self, limiter: LoginLimiter) -> Self {
        self.limiter = limiter;
        self
    }

    /// The login attempt limiter shared by the HTTP handlers.
    #[must_use]
    pub fn limiter(&self) -> &LoginLimiter {
        &self.limiter
    }

    /// The current instant according to the configured clock.
    #[must_use]
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    /// The effective Argon2id parameters, substituting the defaults when the
    /// configured set is the zero value (Go's `params()` helper).
    pub(super) fn effective_params(&self) -> Params {
        if self.params.memory == 0 {
            DEFAULT_PARAMS
        } else {
            self.params
        }
    }

    /// Create the administrator account when none exists yet.
    ///
    /// Returns `created = false` (and no password) when the database already has
    /// a password hash, which makes this safe to call on every start.
    ///
    /// # Errors
    /// Returns [`AuthError::Invalid`] for an out-of-range name or password, and
    /// database errors unchanged.
    pub async fn initialize(
        &self,
        username: &str,
        password: &str,
    ) -> Result<InitialCredentials, AuthError> {
        if self.read_setting(PASSWORD_HASH_KEY).await?.is_some() {
            return Ok(InitialCredentials::default());
        }
        let username = if username.is_empty() {
            "admin".to_owned()
        } else {
            username.to_owned()
        };
        let generated = password.is_empty();
        let password = if generated {
            random_text()
        } else {
            password.to_owned()
        };
        if username.len() > 128 || password.len() < 12 || password.len() > 1024 {
            return Err(AuthError::Invalid(
                "administrator username/password length is invalid (password minimum is 12 characters)"
                    .to_owned(),
            ));
        }
        let hash = self.hash_password(&password).await?;
        let now = self.clock.now().to_rfc3339();
        let username_for_return = username.clone();
        let password_for_return = password.clone();
        self.db
            .call(move |connection| {
                let transaction = connection.transaction()?;
                for (key, value) in [
                    (USERNAME_KEY, username.as_str()),
                    (PASSWORD_HASH_KEY, hash.as_str()),
                ] {
                    transaction.execute(
                        "INSERT INTO settings(key,value,updated_at) VALUES(?1,?2,?3)",
                        params![key, value, now],
                    )?;
                }
                transaction.commit()?;
                Ok(())
            })
            .await?;
        Ok(InitialCredentials {
            created: true,
            generated,
            username: username_for_return,
            password: if generated {
                password_for_return
            } else {
                String::new()
            },
        })
    }

    /// Verify credentials and mint a session.
    ///
    /// # Errors
    /// Returns [`AuthError::InvalidCredentials`], [`AuthError::TotpRequired`]
    /// or [`AuthError::InvalidSecondFactor`] for the expected failures.
    pub async fn login(
        &self,
        username: &str,
        password: &str,
        second_factor: &str,
    ) -> Result<(String, Timestamp), AuthError> {
        self.verify_credentials(username, password).await?;
        let status = self.totp_status().await?;
        if status.enabled {
            if second_factor.trim().is_empty() {
                return Err(AuthError::TotpRequired);
            }
            self.consume_second_factor(password, second_factor, self.clock.now())
                .await?;
        }
        let token = new_token();
        let now = self.clock.now();
        let expires =
            Timestamp::from_unix_millis(now.unix_millis().saturating_add(SESSION_LIFETIME_MILLIS));
        let id = new_id();
        let stored_hash = token_hash(&token);
        self.db
            .call(move |connection| {
                connection
                    .execute(
                        "INSERT INTO sessions(id, token_hash, created_at, expires_at) \
                         VALUES(?1,?2,?3,?4)",
                        params![id, stored_hash, now.to_rfc3339(), expires.to_rfc3339()],
                    )
                    .map_err(DbError::Query)
            })
            .await?;
        Ok((token, expires))
    }

    /// Replace the administrator name and password, revoking every session.
    ///
    /// The stored TOTP secret is re-encrypted under the new password so a
    /// password change never locks the second factor out.
    ///
    /// # Errors
    /// Returns [`AuthError::Invalid`] for bad lengths and
    /// [`AuthError::InvalidCredentials`] when the current password is wrong.
    pub async fn change_credentials(
        &self,
        current_username: &str,
        current_password: &str,
        new_username: &str,
        new_password: &str,
    ) -> Result<(), AuthError> {
        if new_username.is_empty()
            || new_username.len() > 128
            || new_password.len() < 12
            || new_password.len() > 1024
        {
            return Err(AuthError::Invalid(
                "administrator username/password length is invalid (password minimum is 12 characters)"
                    .to_owned(),
            ));
        }
        self.verify_credentials(current_username, current_password)
            .await?;

        let reencrypted = match self.read_setting(TOTP_CONFIG_KEY).await? {
            Some(raw) => {
                let encrypted = super::totp::decode_encrypted(&raw)?;
                let secret = self
                    .decrypt_secret(current_password.to_owned(), encrypted)
                    .await?;
                Some(self.encrypt_secret(new_password.to_owned(), secret).await?)
            }
            None => None,
        };

        let new_hash = self.hash_password(new_password).await?;
        let now = self.clock.now().to_rfc3339();
        let new_username = new_username.to_owned();
        self.db
            .call(move |connection| {
                let transaction = connection.transaction()?;
                transaction.execute(
                    "UPDATE settings SET value=?1,updated_at=?2 WHERE key='admin_username'",
                    params![new_username, now],
                )?;
                transaction.execute(
                    "UPDATE settings SET value=?1,updated_at=?2 WHERE key='admin_password_hash'",
                    params![new_hash, now],
                )?;
                if let Some(encrypted) = reencrypted {
                    let raw = serde_json::to_string(&encrypted)
                        .map_err(|error| DbError::Worker(error.to_string()))?;
                    transaction.execute(
                        "UPDATE settings SET value=?1,updated_at=?2 WHERE key=?3",
                        params![raw, now, TOTP_CONFIG_KEY],
                    )?;
                }
                transaction.execute(
                    "DELETE FROM settings WHERE key=?1",
                    params![TOTP_PENDING_KEY],
                )?;
                transaction.execute("DELETE FROM sessions", [])?;
                transaction.commit()?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Rename the administrator without touching the password or sessions.
    ///
    /// This is intentional in the original product: authentication is enforced
    /// by the HTTP handler before the call, and existing sessions keep working
    /// under the new name.
    ///
    /// # Errors
    /// Returns [`AuthError::Invalid`] for a bad name and
    /// [`AuthError::Internal`] when no administrator row exists.
    pub async fn change_username(&self, new_username: &str) -> Result<(), AuthError> {
        let new_username = new_username.trim().to_owned();
        if new_username.is_empty() || new_username.len() > 128 {
            return Err(AuthError::Invalid(
                "administrator username length is invalid".to_owned(),
            ));
        }
        let now = self.clock.now().to_rfc3339();
        let updated = self
            .db
            .call(move |connection| {
                connection
                    .execute(
                        "UPDATE settings SET value=?1,updated_at=?2 WHERE key='admin_username'",
                        params![new_username, now],
                    )
                    .map_err(DbError::Query)
            })
            .await?;
        if updated != 1 {
            return Err(AuthError::Internal(
                "administrator username is not initialized".to_owned(),
            ));
        }
        Ok(())
    }

    /// Generate a fresh password and clear every session and TOTP setting.
    ///
    /// # Errors
    /// Returns [`AuthError::Invalid`] for an over-long name.
    pub async fn reset_credentials(&self, username: &str) -> Result<InitialCredentials, AuthError> {
        let username = if username.is_empty() {
            "admin".to_owned()
        } else {
            username.to_owned()
        };
        if username.len() > 128 {
            return Err(AuthError::Invalid(
                "administrator username length is invalid".to_owned(),
            ));
        }
        let password = random_text();
        let hash = self.hash_password(&password).await?;
        let now = self.clock.now().to_rfc3339();
        let (username_for_return, password_for_return) = (username.clone(), password.clone());
        self.db
            .call(move |connection| {
                let transaction = connection.transaction()?;
                for (key, value) in [
                    (USERNAME_KEY, username.as_str()),
                    (PASSWORD_HASH_KEY, hash.as_str()),
                ] {
                    transaction.execute(
                        "INSERT INTO settings(key,value,updated_at) VALUES(?1,?2,?3) \
                         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
                        params![key, value, now],
                    )?;
                }
                transaction.execute(
                    "DELETE FROM settings WHERE key IN (?1,?2,?3,?4)",
                    params![
                        TOTP_CONFIG_KEY,
                        TOTP_RECOVERY_KEY,
                        super::totp::TOTP_LAST_STEP_KEY,
                        TOTP_PENDING_KEY
                    ],
                )?;
                transaction.execute("DELETE FROM sessions", [])?;
                transaction.commit()?;
                Ok(())
            })
            .await?;
        Ok(InitialCredentials {
            created: true,
            generated: true,
            username: username_for_return,
            password: password_for_return,
        })
    }

    /// Resolve a session token to the current administrator name.
    ///
    /// An expired row is deleted as it is encountered, matching Go.
    ///
    /// # Errors
    /// Returns [`AuthError::InvalidSession`] or [`AuthError::ExpiredSession`].
    pub async fn authenticate(&self, token: &str) -> Result<String, AuthError> {
        if token.is_empty() {
            return Err(AuthError::InvalidSession);
        }
        let stored_hash = token_hash(token);
        let row = self
            .db
            .call(move |connection| {
                connection
                    .query_row(
                        "SELECT sess.expires_at, admin.value FROM sessions sess \
                         JOIN settings admin ON admin.key='admin_username' \
                         WHERE sess.token_hash=?1",
                        params![stored_hash],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()
                    .map_err(DbError::Query)
            })
            .await?;
        let Some((expiry, username)) = row else {
            return Err(AuthError::InvalidSession);
        };
        let Ok(expiry) = Timestamp::parse(&expiry) else {
            let _ = self.logout(token).await;
            return Err(AuthError::ExpiredSession);
        };
        if expiry.is_expired_at(self.clock.now()) {
            let _ = self.logout(token).await;
            return Err(AuthError::ExpiredSession);
        }
        Ok(username)
    }

    /// Delete one session. Used by the logout endpoint.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn logout(&self, token: &str) -> Result<(), AuthError> {
        if token.is_empty() {
            return Ok(());
        }
        let stored_hash = token_hash(token);
        self.db
            .call(move |connection| {
                connection
                    .execute(
                        "DELETE FROM sessions WHERE token_hash=?1",
                        params![stored_hash],
                    )
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Delete every expired session.
    ///
    /// The maintenance runtime invokes this as the 15-minute cleanup job used
    /// by the Go server.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn cleanup_expired_sessions(&self) -> Result<usize, AuthError> {
        let now = self.clock.now().to_rfc3339();
        let removed = self
            .db
            .call(move |connection| {
                connection
                    .execute("DELETE FROM sessions WHERE expires_at <= ?1", params![now])
                    .map_err(DbError::Query)
            })
            .await?;
        Ok(removed)
    }

    /// Verify a username/password pair against the stored hash.
    ///
    /// The password is always verified (even when the name is wrong) so the
    /// timing does not reveal which half failed.
    pub(super) async fn verify_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), AuthError> {
        let row = self
            .db
            .call(|connection| {
                connection
                    .query_row(
                        "SELECT MAX(CASE WHEN key='admin_username' THEN value END), \
                                MAX(CASE WHEN key='admin_password_hash' THEN value END) \
                         FROM settings WHERE key IN ('admin_username','admin_password_hash')",
                        [],
                        |row| {
                            Ok((
                                row.get::<_, Option<String>>(0)?,
                                row.get::<_, Option<String>>(1)?,
                            ))
                        },
                    )
                    .map_err(DbError::Query)
            })
            .await?;
        let (Some(saved_user), Some(saved_hash)) = row else {
            return Err(AuthError::InvalidCredentials);
        };
        let password = password.to_owned();
        let valid = with_kdf(move || verify_password(&password, &saved_hash)).await?;
        let name_matches = bool::from(subtle::ConstantTimeEq::ct_eq(
            username.as_bytes(),
            saved_user.as_bytes(),
        ));
        if name_matches && valid {
            Ok(())
        } else {
            Err(AuthError::InvalidCredentials)
        }
    }

    /// Hash a password inside a KDF slot.
    pub(super) async fn hash_password(&self, password: &str) -> Result<String, AuthError> {
        let params = self.effective_params();
        let password = password.to_owned();
        with_kdf(move || hash_password(&password, params)).await
    }

    /// Read one `settings` value, or `None` when the key is absent.
    pub(super) async fn read_setting(&self, key: &str) -> Result<Option<String>, AuthError> {
        let key = key.to_owned();
        let value = self
            .db
            .call(move |connection| read_setting(connection, &key))
            .await?;
        Ok(value)
    }

    /// Insert or replace one `settings` value.
    pub(super) async fn write_setting(&self, key: &str, value: &str) -> Result<(), AuthError> {
        let key = key.to_owned();
        let value = value.to_owned();
        let now = self.clock.now().to_rfc3339();
        self.db
            .call(move |connection| upsert_setting(connection, &key, &value, &now))
            .await?;
        Ok(())
    }

    /// Whether an avatar is stored.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn has_avatar(&self) -> Result<bool, AuthError> {
        Ok(self
            .read_setting(AVATAR_MIME_KEY)
            .await?
            .is_some_and(|value| !value.is_empty()))
    }

    /// The stored avatar MIME type, or `None` when no avatar exists.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn avatar_mime(&self) -> Result<Option<String>, AuthError> {
        self.read_setting(AVATAR_MIME_KEY).await
    }

    /// Record the avatar MIME type after the bytes have been stored.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn set_avatar_mime(&self, mime: &str) -> Result<(), AuthError> {
        self.write_setting(AVATAR_MIME_KEY, mime).await
    }

    /// Remove the avatar marker row.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn clear_avatar_mime(&self) -> Result<(), AuthError> {
        self.db
            .call(|connection| {
                connection
                    .execute("DELETE FROM settings WHERE key='avatar_mime'", [])
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}

/// Read one `settings` value on an open connection.
pub(super) fn read_setting(connection: &Connection, key: &str) -> Result<Option<String>, DbError> {
    connection
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(DbError::Query)
}

/// Insert or replace one `settings` value on an open connection.
pub(super) fn upsert_setting(
    connection: &Connection,
    key: &str,
    value: &str,
    now: &str,
) -> Result<(), DbError> {
    connection
        .execute(
            "INSERT INTO settings(key,value,updated_at) VALUES(?1,?2,?3) \
             ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
            params![key, value, now],
        )
        .map_err(DbError::Query)?;
    Ok(())
}

/// A generated password, mirroring Go's `crypto/rand.Text()`.
///
/// `rand.Text()` returns 26 base32 characters (128 bits, no padding); the
/// generated administrator password inherits that shape.
pub(super) fn random_text() -> String {
    super::base32::encode(&super::random_bytes(16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_passwords_are_base32_and_long_enough() {
        let password = random_text();
        assert_eq!(password.len(), 26);
        assert!(password.len() >= 12);
        assert!(
            password
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || (b'2'..=b'7').contains(&byte)),
            "{password}"
        );
        assert_ne!(password, random_text());
    }
}
