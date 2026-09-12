//! The TOTP second-factor state machine, ported from Go's `internal/auth/totp.go`.
//!
//! The stored formats are part of the compatibility surface and are reproduced
//! exactly:
//!
//! * `admin_totp_config` — Go's `encryptedSecret` JSON: `version`, `salt`,
//!   `nonce`, `ciphertext` (all base64 **RawStd**, unpadded), plus the KDF
//!   parameters `memory`, `iterations`, `parallelism`. The secret itself is
//!   AES-256-GCM encrypted under an Argon2id key derived from the current
//!   password, with `revaro-totp-v1` as the additional authenticated data.
//! * `admin_totp_recovery_codes` — a JSON array of
//!   `base64 RawURL(sha256("revaro-recovery-v1:" + normalised))`.
//! * `admin_totp_last_step` — the last accepted time step, as decimal text.
//! * `admin_totp_pending` — Go's `pendingTOTP` JSON, valid for ten minutes.
//!
//! ## Recovery codes are unsalted hashes
//!
//! This is inherited, not endorsed: a recovery code carries only 80 bits of
//! entropy and is stored as a bare SHA-256 so that a database reader can test
//! guesses offline. The format is preserved because codes already stored must
//! keep working; hardening it would require a rehash on next successful login.
//!
//! ## TOTP itself
//!
//! `issuer=revaro`, 30-second period, six digits, SHA-1, 20-byte secret, and a
//! ±1 step acceptance window. Replay protection rejects any accepted step that
//! is not strictly greater than `admin_totp_last_step`.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac as _};
use revaro_core::Timestamp;
use revaro_core::api::auth::TotpStatus;
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};
use sha1::Sha1;

use crate::db::DbError;

use super::AuthError;
use super::base32;
use super::service::{AuthService, read_setting, upsert_setting, with_kdf};

/// Settings key holding the encrypted shared secret.
pub(super) const TOTP_CONFIG_KEY: &str = "admin_totp_config";
/// Settings key holding the recovery-code hashes.
pub(super) const TOTP_RECOVERY_KEY: &str = "admin_totp_recovery_codes";
/// Settings key holding the last accepted time step.
pub(super) const TOTP_LAST_STEP_KEY: &str = "admin_totp_last_step";
/// Settings key holding an in-progress setup.
pub(super) const TOTP_PENDING_KEY: &str = "admin_totp_pending";

/// Issuer advertised in the `otpauth://` URI.
pub const TOTP_ISSUER: &str = "revaro";
/// Time step in seconds.
const TOTP_PERIOD_SECONDS: i64 = 30;
/// Number of digits.
const TOTP_DIGITS: u32 = 6;
/// Shared secret size in bytes.
const TOTP_SECRET_BYTES: usize = 20;
/// How long a pending setup stays valid.
const SETUP_LIFETIME_MILLIS: i64 = 10 * 60 * 1000;
/// Number of recovery codes generated at once.
const RECOVERY_COUNT: usize = 10;
/// Bytes of entropy behind one recovery code.
const RECOVERY_BYTES: usize = 10;
/// Additional authenticated data for the secret envelope.
const SECRET_AAD: &[u8] = b"revaro-totp-v1";
/// Prefix mixed into a recovery-code hash.
const RECOVERY_PREFIX: &str = "revaro-recovery-v1:";
/// Largest KDF memory cost accepted inside a stored envelope (1 GiB in KiB).
const MAX_SECRET_MEMORY: u32 = 1024 * 1024;
/// Largest KDF time cost accepted inside a stored envelope.
const MAX_SECRET_ITERATIONS: u32 = 10;
/// Largest KDF parallelism accepted inside a stored envelope.
const MAX_SECRET_PARALLELISM: u8 = 16;

/// A freshly generated shared secret and its provisioning URI.
///
/// The QR code is added at the HTTP boundary, which is the only layer that
/// knows about PNG data URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpSetup {
    /// Base32 shared secret.
    pub secret: String,
    /// `otpauth://` provisioning URI.
    pub uri: String,
}

/// Go's `encryptedSecret` JSON envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedSecret {
    /// Envelope version; only `1` is understood.
    pub version: i32,
    /// RawStd base64 salt.
    pub salt: String,
    /// RawStd base64 AES-GCM nonce.
    pub nonce: String,
    /// RawStd base64 ciphertext and tag.
    pub ciphertext: String,
    /// Argon2id memory cost in KiB.
    pub memory: u32,
    /// Argon2id time cost.
    pub iterations: u32,
    /// Argon2id lanes.
    pub parallelism: u8,
}

/// Go's `pendingTOTP` JSON envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingTotp {
    secret: EncryptedSecret,
    uri: String,
    expires_at: String,
}

/// Decode the stored config JSON, mapping every failure to [`AuthError::Corrupt`].
pub(super) fn decode_encrypted(raw: &str) -> Result<EncryptedSecret, AuthError> {
    serde_json::from_str(raw)
        .map_err(|_| AuthError::Corrupt("stored TOTP configuration is invalid".to_owned()))
}

/// Encrypt `secret` with a key derived from `password`.
///
/// # Errors
/// Returns [`AuthError::Corrupt`] for an out-of-range parameter set or a
/// primitive failure.
pub(super) fn encrypt_secret(
    password: &str,
    secret: &str,
    params: super::Params,
) -> Result<EncryptedSecret, AuthError> {
    let salt = super::random_bytes(16);
    let key = super::password::derive_raw(
        password,
        &salt,
        params.memory,
        params.iterations,
        params.parallelism,
        32,
    )?;
    let nonce = super::random_bytes(12);
    let ciphertext = seal(&key, &nonce, secret.as_bytes())?;
    Ok(EncryptedSecret {
        version: 1,
        salt: STANDARD_NO_PAD.encode(&salt),
        nonce: STANDARD_NO_PAD.encode(&nonce),
        ciphertext: STANDARD_NO_PAD.encode(&ciphertext),
        memory: params.memory,
        iterations: params.iterations,
        parallelism: params.parallelism,
    })
}

/// Decrypt a stored envelope.
///
/// # Errors
/// Returns [`AuthError::Corrupt`] for an unsupported version, out-of-range KDF
/// parameters, malformed base64 or a failed authentication tag.
pub(super) fn decrypt_secret(
    password: &str,
    encrypted: &EncryptedSecret,
) -> Result<String, AuthError> {
    if encrypted.version != 1 {
        return Err(AuthError::Corrupt(
            "unsupported encrypted secret version".to_owned(),
        ));
    }
    if encrypted.memory == 0
        || encrypted.memory > MAX_SECRET_MEMORY
        || encrypted.iterations == 0
        || encrypted.iterations > MAX_SECRET_ITERATIONS
        || encrypted.parallelism == 0
        || encrypted.parallelism > MAX_SECRET_PARALLELISM
    {
        return Err(AuthError::Corrupt(
            "invalid encrypted secret KDF parameters".to_owned(),
        ));
    }
    let salt = STANDARD_NO_PAD
        .decode(&encrypted.salt)
        .map_err(|_| AuthError::Corrupt("invalid encrypted secret salt".to_owned()))?;
    if salt.len() != 16 {
        return Err(AuthError::Corrupt(
            "invalid encrypted secret salt".to_owned(),
        ));
    }
    let nonce = STANDARD_NO_PAD
        .decode(&encrypted.nonce)
        .map_err(|_| AuthError::Corrupt("invalid encrypted secret nonce".to_owned()))?;
    if nonce.len() != 12 {
        return Err(AuthError::Corrupt(
            "invalid encrypted secret nonce size".to_owned(),
        ));
    }
    let ciphertext = STANDARD_NO_PAD
        .decode(&encrypted.ciphertext)
        .map_err(|_| AuthError::Corrupt("invalid encrypted secret ciphertext".to_owned()))?;
    let key = super::password::derive_raw(
        password,
        &salt,
        encrypted.memory,
        encrypted.iterations,
        encrypted.parallelism,
        32,
    )?;
    let plaintext = open(&key, &nonce, &ciphertext)?;
    String::from_utf8(plaintext)
        .map_err(|_| AuthError::Corrupt("could not decrypt TOTP secret".to_owned()))
}

/// AES-256-GCM seal with the fixed product AAD.
fn seal(key: &[u8], nonce: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, AuthError> {
    use aes_gcm::aead::{Aead as _, Nonce, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit as _};

    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|error| AuthError::Internal(error.to_string()))?;
    cipher
        .encrypt(
            Nonce::<Aes256Gcm>::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad: SECRET_AAD,
            },
        )
        .map_err(|_| AuthError::Corrupt("could not encrypt TOTP secret".to_owned()))
}

/// AES-256-GCM open with the fixed product AAD.
fn open(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, AuthError> {
    use aes_gcm::aead::{Aead as _, Nonce, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit as _};

    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|error| AuthError::Internal(error.to_string()))?;
    cipher
        .decrypt(
            Nonce::<Aes256Gcm>::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: SECRET_AAD,
            },
        )
        .map_err(|_| AuthError::Corrupt("could not decrypt TOTP secret".to_owned()))
}

/// Build the provisioning URI in the exact field order Go's `url.Values.Encode`
/// produces (query keys sorted, path `issuer:account`).
#[must_use]
pub(super) fn provisioning_uri(issuer: &str, account: &str, secret: &str) -> String {
    format!(
        "otpauth://totp/{}:{}?algorithm=SHA1&digits={TOTP_DIGITS}&issuer={}&period={TOTP_PERIOD_SECONDS}&secret={secret}",
        escape_path(issuer),
        escape_path(account),
        escape_query(issuer),
    )
}

/// Percent-encode a URI path segment, leaving the unreserved set and `:`.
fn escape_path(value: &str) -> String {
    percent_encode(value, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':')
    })
}

/// Percent-encode a query value, leaving only the unreserved set.
fn escape_query(value: &str) -> String {
    percent_encode(value, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
    })
}

fn percent_encode(value: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if keep(byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Render `uri` as a PNG QR code wrapped in a data URL.
///
/// # Errors
/// Returns [`AuthError::Internal`] when the QR code cannot be encoded.
pub(super) fn qr_png_data_url(uri: &str) -> Result<String, AuthError> {
    use base64::engine::general_purpose::STANDARD;

    let code = qrcode::QrCode::new(uri.as_bytes())
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    let rendered = code
        .render::<image::Luma<u8>>()
        .min_dimensions(256, 256)
        .build();
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(rendered)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    Ok(format!("data:image/png;base64,{}", STANDARD.encode(&png)))
}

/// Compute the six-digit code for a counter, matching `pquerna/otp`.
///
/// # Errors
/// Returns [`AuthError::Corrupt`] when the secret is not valid base32.
pub(super) fn generate_code(secret: &str, counter: u64) -> Result<String, AuthError> {
    let key = base32::decode(secret)?;
    let mut mac = Hmac::<Sha1>::new_from_slice(&key)
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[19] & 0x0f);
    let binary = u32::from_be_bytes([
        digest[offset],
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]) & 0x7fff_ffff;
    Ok(format!("{:06}", binary % 10u32.pow(TOTP_DIGITS)))
}

/// Check `code` against the ±1 step window around `now`.
///
/// Returns the accepted step so the caller can record it for replay protection.
/// A non-six-digit code is not an error, just not a match.
///
/// # Errors
/// Returns [`AuthError::Corrupt`] when the stored secret is not valid base32.
pub(super) fn validate_totp(
    code: &str,
    secret: &str,
    now: Timestamp,
) -> Result<(u64, bool), AuthError> {
    if !is_six_digits(code) {
        return Ok((0, false));
    }
    let current = now.unix_seconds() / TOTP_PERIOD_SECONDS;
    for offset in [0i64, -1, 1] {
        let counter = current + offset;
        if counter < 0 {
            continue;
        }
        let candidate = generate_code(secret, counter as u64)?;
        if bool::from(subtle::ConstantTimeEq::ct_eq(
            candidate.as_bytes(),
            code.as_bytes(),
        )) {
            return Ok((counter as u64, true));
        }
    }
    Ok((0, false))
}

/// True when `code` is exactly six ASCII digits.
#[must_use]
pub(super) fn is_six_digits(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit())
}

/// Generate ten recovery codes and their stored hashes.
#[must_use]
pub(super) fn generate_recovery_codes() -> (Vec<String>, Vec<String>) {
    let mut codes = Vec::with_capacity(RECOVERY_COUNT);
    let mut hashes = Vec::with_capacity(RECOVERY_COUNT);
    for _ in 0..RECOVERY_COUNT {
        let raw = base32::encode(&super::random_bytes(RECOVERY_BYTES));
        let code = format!(
            "{}-{}-{}-{}",
            &raw[0..4],
            &raw[4..8],
            &raw[8..12],
            &raw[12..16]
        );
        hashes.push(recovery_code_hash(&raw));
        codes.push(code);
    }
    (codes, hashes)
}

/// Uppercase and strip separators, matching Go's `normalizeRecoveryCode`.
#[must_use]
pub(super) fn normalize_recovery_code(code: &str) -> String {
    code.trim()
        .chars()
        .filter(|character| *character != '-' && *character != ' ')
        .flat_map(char::to_uppercase)
        .collect()
}

/// The stored hash of a recovery code.
#[must_use]
pub(super) fn recovery_code_hash(code: &str) -> String {
    let material = format!("{RECOVERY_PREFIX}{}", normalize_recovery_code(code));
    URL_SAFE_NO_PAD.encode(revaro_core::hash::sha256(material.as_bytes()))
}

impl AuthService {
    /// Read the current second-factor status.
    ///
    /// # Errors
    /// Returns [`AuthError::Corrupt`] when the stored JSON is unusable.
    pub async fn totp_status(&self) -> Result<TotpStatus, AuthError> {
        let Some(raw) = self.read_setting(TOTP_CONFIG_KEY).await? else {
            return Ok(TotpStatus::default());
        };
        let encrypted = decode_encrypted(&raw)?;
        if encrypted.version != 1 {
            return Err(AuthError::Corrupt(
                "stored TOTP configuration is invalid".to_owned(),
            ));
        }
        let mut status = TotpStatus {
            enabled: true,
            recovery_codes: 0,
        };
        if let Some(raw) = self.read_setting(TOTP_RECOVERY_KEY).await? {
            let hashes: Vec<String> = serde_json::from_str(&raw)
                .map_err(|_| AuthError::Corrupt("stored recovery codes are invalid".to_owned()))?;
            status.recovery_codes = hashes.len() as i64;
        }
        Ok(status)
    }

    /// Begin enrolling a new authenticator.
    ///
    /// # Errors
    /// Returns [`AuthError::InvalidCredentials`] for a wrong password and
    /// [`AuthError::TotpAlreadyEnabled`] when TOTP is already on.
    pub async fn begin_totp_setup(
        &self,
        username: &str,
        password: &str,
    ) -> Result<TotpSetup, AuthError> {
        self.verify_credentials(username, password).await?;
        let status = self.totp_status().await?;
        if status.enabled {
            return Err(AuthError::TotpAlreadyEnabled);
        }
        let secret = base32::encode(&super::random_bytes(TOTP_SECRET_BYTES));
        let uri = provisioning_uri(TOTP_ISSUER, username, &secret);
        let encrypted = self
            .encrypt_secret(password.to_owned(), secret.clone())
            .await?;
        let pending = PendingTotp {
            secret: encrypted,
            uri: uri.clone(),
            expires_at: Timestamp::from_unix_millis(
                self.now().unix_millis() + SETUP_LIFETIME_MILLIS,
            )
            .to_rfc3339(),
        };
        let raw = serde_json::to_string(&pending)
            .map_err(|error| AuthError::Internal(error.to_string()))?;
        self.write_setting(TOTP_PENDING_KEY, &raw).await?;
        Ok(TotpSetup { secret, uri })
    }

    /// Confirm a pending setup with the first authenticator code.
    ///
    /// # Errors
    /// Returns the `totpProblem` set: wrong password, already enabled, expired
    /// setup, or an invalid code.
    pub async fn confirm_totp_setup(
        &self,
        username: &str,
        password: &str,
        code: &str,
        session_token: &str,
    ) -> Result<Vec<String>, AuthError> {
        self.verify_credentials(username, password).await?;
        let status = self.totp_status().await?;
        if status.enabled {
            return Err(AuthError::TotpAlreadyEnabled);
        }
        let Some(raw) = self.read_setting(TOTP_PENDING_KEY).await? else {
            return Err(AuthError::TotpSetupExpired);
        };
        let pending: PendingTotp = serde_json::from_str(&raw)
            .map_err(|_| AuthError::Corrupt("stored pending TOTP setup is invalid".to_owned()))?;
        let now = self.now();
        let still_valid = Timestamp::parse(&pending.expires_at)
            .map(|expires| !expires.is_expired_at(now))
            .unwrap_or(false);
        if !still_valid {
            let _ = self.delete_setting(TOTP_PENDING_KEY).await;
            return Err(AuthError::TotpSetupExpired);
        }
        let secret = self
            .decrypt_secret(password.to_owned(), pending.secret.clone())
            .await
            .map_err(|_| AuthError::InvalidCredentials)?;
        let (step, accepted) = validate_totp(code, &secret, now)?;
        if !accepted {
            return Err(AuthError::InvalidSecondFactor);
        }
        let (codes, hashes) = generate_recovery_codes();
        let config_raw = serde_json::to_string(&pending.secret)
            .map_err(|error| AuthError::Internal(error.to_string()))?;
        let hashes_raw = serde_json::to_string(&hashes)
            .map_err(|error| AuthError::Internal(error.to_string()))?;
        let now_text = now.to_rfc3339();
        let session_token = session_token.to_owned();
        self.db
            .call(move |connection| {
                let transaction = connection.transaction()?;
                for (key, value) in [
                    (TOTP_CONFIG_KEY, config_raw.as_str()),
                    (TOTP_RECOVERY_KEY, hashes_raw.as_str()),
                    (TOTP_LAST_STEP_KEY, step.to_string().as_str()),
                ] {
                    upsert_setting(&transaction, key, value, &now_text)?;
                }
                transaction.execute(
                    "DELETE FROM settings WHERE key=?1",
                    params![TOTP_PENDING_KEY],
                )?;
                revoke_other_sessions(&transaction, &session_token)?;
                transaction.commit()?;
                Ok(())
            })
            .await?;
        Ok(codes)
    }

    /// Replace the recovery codes after re-verifying the second factor.
    ///
    /// # Errors
    /// Returns [`AuthError::InvalidCredentials`], [`AuthError::TotpNotEnabled`]
    /// or [`AuthError::InvalidSecondFactor`].
    pub async fn regenerate_recovery_codes(
        &self,
        username: &str,
        password: &str,
        code: &str,
    ) -> Result<Vec<String>, AuthError> {
        self.verify_credentials(username, password).await?;
        self.consume_second_factor(password, code, self.now())
            .await?;
        let (codes, hashes) = generate_recovery_codes();
        let raw = serde_json::to_string(&hashes)
            .map_err(|error| AuthError::Internal(error.to_string()))?;
        self.write_setting(TOTP_RECOVERY_KEY, &raw).await?;
        Ok(codes)
    }

    /// Turn the second factor off.
    ///
    /// # Errors
    /// Returns [`AuthError::InvalidCredentials`], [`AuthError::TotpNotEnabled`]
    /// or [`AuthError::InvalidSecondFactor`].
    pub async fn disable_totp(
        &self,
        username: &str,
        password: &str,
        code: &str,
        session_token: &str,
    ) -> Result<(), AuthError> {
        self.verify_credentials(username, password).await?;
        let status = self.totp_status().await?;
        if !status.enabled {
            return Err(AuthError::TotpNotEnabled);
        }
        self.consume_second_factor(password, code, self.now())
            .await?;
        let session_token = session_token.to_owned();
        self.db
            .call(move |connection| {
                let transaction = connection.transaction()?;
                transaction.execute(
                    "DELETE FROM settings WHERE key IN (?1,?2,?3,?4)",
                    params![
                        TOTP_CONFIG_KEY,
                        TOTP_RECOVERY_KEY,
                        TOTP_LAST_STEP_KEY,
                        TOTP_PENDING_KEY
                    ],
                )?;
                revoke_other_sessions(&transaction, &session_token)?;
                transaction.commit()?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Accept either an authenticator code or a recovery code.
    pub(super) async fn consume_second_factor(
        &self,
        password: &str,
        code: &str,
        now: Timestamp,
    ) -> Result<(), AuthError> {
        let Some(raw) = self.read_setting(TOTP_CONFIG_KEY).await? else {
            return Err(AuthError::TotpNotEnabled);
        };
        let encrypted = decode_encrypted(&raw)?;
        let secret = self
            .decrypt_secret(password.to_owned(), encrypted)
            .await
            .map_err(|_| AuthError::InvalidCredentials)?;
        let trimmed = code.trim();
        if is_six_digits(trimmed) {
            let (step, accepted) = validate_totp(trimmed, &secret, now)?;
            if !accepted {
                return Err(AuthError::InvalidSecondFactor);
            }
            return self.consume_totp_step(step).await;
        }
        self.consume_recovery_code(trimmed).await
    }

    /// Record an accepted time step, rejecting replays.
    ///
    /// The check and the write share one transaction; the closure reports
    /// "replayed" as `false` rather than an error so the two protocol outcomes
    /// stay distinguishable from a real database failure.
    async fn consume_totp_step(&self, step: u64) -> Result<(), AuthError> {
        let now = self.now().to_rfc3339();
        let accepted = self
            .db
            .call(move |connection| {
                let transaction = connection.transaction()?;
                if let Some(raw) = read_setting(&transaction, TOTP_LAST_STEP_KEY)? {
                    let last = raw.parse::<u64>().map_err(|_| {
                        DbError::Worker("stored TOTP replay state is invalid".to_owned())
                    })?;
                    if step <= last {
                        return Ok(false);
                    }
                }
                upsert_setting(&transaction, TOTP_LAST_STEP_KEY, &step.to_string(), &now)?;
                transaction.commit()?;
                Ok(true)
            })
            .await?;
        if accepted {
            Ok(())
        } else {
            Err(AuthError::InvalidSecondFactor)
        }
    }

    /// Consume one recovery code.
    async fn consume_recovery_code(&self, code: &str) -> Result<(), AuthError> {
        let normalized = normalize_recovery_code(code);
        if normalized.len() != 16 {
            return Err(AuthError::InvalidSecondFactor);
        }
        let want = recovery_code_hash(&normalized);
        let now = self.now().to_rfc3339();
        let consumed = self
            .db
            .call(move |connection| {
                let transaction = connection.transaction()?;
                let Some(raw) = read_setting(&transaction, TOTP_RECOVERY_KEY)? else {
                    return Ok(false);
                };
                let mut hashes: Vec<String> = serde_json::from_str(&raw)
                    .map_err(|_| DbError::Worker("stored recovery codes are invalid".to_owned()))?;
                let mut found = None;
                for (index, hash) in hashes.iter().enumerate() {
                    if bool::from(subtle::ConstantTimeEq::ct_eq(
                        hash.as_bytes(),
                        want.as_bytes(),
                    )) {
                        // No early break: Go compared every hash and kept the
                        // last match, which keeps the loop constant-time.
                        found = Some(index);
                    }
                }
                let Some(index) = found else {
                    return Ok(false);
                };
                hashes.remove(index);
                let next = serde_json::to_string(&hashes)
                    .map_err(|error| DbError::Worker(error.to_string()))?;
                transaction.execute(
                    "UPDATE settings SET value=?1,updated_at=?2 WHERE key=?3",
                    params![next, now, TOTP_RECOVERY_KEY],
                )?;
                transaction.commit()?;
                Ok(true)
            })
            .await?;
        if consumed {
            Ok(())
        } else {
            Err(AuthError::InvalidSecondFactor)
        }
    }

    /// Encrypt a secret under a password in a KDF slot.
    pub(super) async fn encrypt_secret(
        &self,
        password: String,
        secret: String,
    ) -> Result<EncryptedSecret, AuthError> {
        let params = self.effective_params();
        with_kdf(move || encrypt_secret(&password, &secret, params)).await
    }

    /// Decrypt a secret under a password in a KDF slot.
    pub(super) async fn decrypt_secret(
        &self,
        password: String,
        encrypted: EncryptedSecret,
    ) -> Result<String, AuthError> {
        with_kdf(move || decrypt_secret(&password, &encrypted)).await
    }

    /// Delete one settings key, ignoring absence.
    async fn delete_setting(&self, key: &str) -> Result<(), AuthError> {
        let key = key.to_owned();
        self.db
            .call(move |connection| {
                connection
                    .execute("DELETE FROM settings WHERE key=?1", params![key])
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}

/// Delete every session except the one making the request.
///
/// Go required a non-empty current token; without one this would silently log
/// the caller out, so an empty token is refused.
fn revoke_other_sessions(
    transaction: &Transaction<'_>,
    session_token: &str,
) -> Result<(), DbError> {
    if session_token.is_empty() {
        return Err(DbError::Worker("missing current session".to_owned()));
    }
    transaction
        .execute(
            "DELETE FROM sessions WHERE token_hash<>?1",
            params![super::token_hash(session_token)],
        )
        .map_err(DbError::Query)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238's SHA-1 secret: the ASCII bytes of `12345678901234567890`.
    const RFC_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn code_generation_matches_rfc_6238_sha1_vectors() {
        // The RFC publishes eight-digit codes; the product uses six, so the
        // expected values are the low six digits of each vector.
        let cases: &[(u64, i64, &str)] = &[
            (59, 1, "287082"),
            (1_111_111_109, 37_037_036, "081804"),
            (1_111_111_111, 37_037_037, "050471"),
            (1_234_567_890, 41_152_263, "005924"),
            (2_000_000_000, 66_666_666, "279037"),
            (20_000_000_000, 666_666_666, "353130"),
        ];
        for (seconds, counter, expected) in cases {
            assert_eq!(
                generate_code(RFC_SECRET, *counter as u64).unwrap(),
                *expected,
                "counter {counter} (T={seconds})"
            );
        }
    }

    #[test]
    fn validation_accepts_the_plus_minus_one_step_window() {
        let now = Timestamp::parse("2026-08-22T01:00:00Z").unwrap();
        let current = now.unix_seconds() / TOTP_PERIOD_SECONDS;
        for offset in [-1i64, 0, 1] {
            let code = generate_code(RFC_SECRET, (current + offset) as u64).unwrap();
            let (step, accepted) = validate_totp(&code, RFC_SECRET, now).unwrap();
            assert!(accepted, "offset {offset} must be accepted");
            assert_eq!(step as i64, current + offset);
        }
        for offset in [-2i64, 2, 10] {
            let code = generate_code(RFC_SECRET, (current + offset) as u64).unwrap();
            let (_, accepted) = validate_totp(&code, RFC_SECRET, now).unwrap();
            assert!(!accepted, "offset {offset} must be rejected");
        }
        // A non-numeric or wrong-length code is simply not a match.
        for code in ["", "12345", "1234567", "abcdef", " 12345"] {
            let (_, accepted) = validate_totp(code, RFC_SECRET, now).unwrap();
            assert!(!accepted, "{code:?}");
        }
    }

    #[test]
    fn provisioning_uri_matches_the_go_field_order() {
        let uri = provisioning_uri("revaro", "admin", RFC_SECRET);
        assert_eq!(
            uri,
            "otpauth://totp/revaro:admin?algorithm=SHA1&digits=6&issuer=revaro&period=30&secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
        );
    }

    #[test]
    fn recovery_codes_have_the_documented_shape_and_hash() {
        let (codes, hashes) = generate_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_COUNT);
        assert_eq!(hashes.len(), RECOVERY_COUNT);
        for code in &codes {
            assert_eq!(code.len(), 19, "{code}");
            let parts: Vec<&str> = code.split('-').collect();
            assert_eq!(parts.len(), 4);
            // Each group is four base32 alphabet characters. A 4-character group
            // is 20 bits and is *not* independently decodable, so validity is
            // asserted over the whole code below.
            assert!(
                parts.iter().all(|part| part.len() == 4
                    && part.chars().all(|character| character.is_ascii_uppercase()
                        || ('2'..='7').contains(&character))),
                "{code}"
            );
        }
        // Sixteen base32 characters carry the ten random bytes.
        let joined = codes[0].replace('-', "");
        assert_eq!(joined.len(), 16);
        assert_eq!(
            base32::decode(&joined)
                .expect("the joined code is valid base32")
                .len(),
            10
        );
        // Normalisation makes the hash independent of case and separators.
        let normalized = normalize_recovery_code(&codes[0]);
        assert_eq!(normalized.len(), 16);
        assert_eq!(recovery_code_hash(&codes[0]), hashes[0]);
        assert_eq!(recovery_code_hash(&normalized), hashes[0]);
        assert_eq!(
            recovery_code_hash(&normalized.to_lowercase().replace('-', " ")),
            hashes[0]
        );
    }

    #[test]
    fn recovery_code_hashes_match_the_go_material() {
        // Python: base64url(sha256("revaro-recovery-v1:ABCDEFGHIJKLMNOP")).
        assert_eq!(
            recovery_code_hash("ABCD-EFGH-IJKL-MNOP"),
            "fx5XifE7dPVc3n5S_aiTgTzUe3R9Y8MZNQA86qUaaZE"
        );
    }

    #[test]
    fn the_secret_envelope_round_trips_with_go_field_names() {
        let params = super::super::Params {
            memory: 8 * 1024,
            iterations: 1,
            parallelism: 1,
            salt_length: 16,
            key_length: 32,
        };
        let encrypted = encrypt_secret("a-secure-test-password", RFC_SECRET, params).unwrap();
        assert_eq!(encrypted.version, 1);
        assert_eq!(encrypted.memory, 8 * 1024);
        assert_eq!(encrypted.iterations, 1);
        assert_eq!(encrypted.parallelism, 1);
        let json: serde_json::Value = serde_json::to_value(&encrypted).unwrap();
        for key in [
            "version",
            "salt",
            "nonce",
            "ciphertext",
            "memory",
            "iterations",
            "parallelism",
        ] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
        assert_eq!(json.as_object().unwrap().len(), 7);
        // No padding anywhere: this is Go's RawStdEncoding.
        assert!(!encrypted.salt.contains('='));
        assert!(!encrypted.nonce.contains('='));
        assert!(!encrypted.ciphertext.contains('='));
        assert_eq!(
            decrypt_secret("a-secure-test-password", &encrypted).unwrap(),
            RFC_SECRET
        );
        // The wrong password fails the GCM tag.
        assert!(decrypt_secret("wrong-password", &encrypted).is_err());
    }

    #[test]
    fn a_tampered_envelope_is_rejected() {
        let params = super::super::Params {
            memory: 1024,
            iterations: 1,
            parallelism: 1,
            salt_length: 16,
            key_length: 32,
        };
        let mut encrypted = encrypt_secret("pw", RFC_SECRET, params).unwrap();
        assert!(decrypt_secret("pw", &encrypted).is_ok());
        encrypted.version = 2;
        assert!(decrypt_secret("pw", &encrypted).is_err());
        encrypted.version = 1;
        encrypted.memory = 0;
        assert!(decrypt_secret("pw", &encrypted).is_err());
        encrypted.memory = 1024;
        encrypted.parallelism = 17;
        assert!(decrypt_secret("pw", &encrypted).is_err());
        encrypted.parallelism = 1;
        encrypted.ciphertext.push('A');
        assert!(decrypt_secret("pw", &encrypted).is_err());
    }

    #[test]
    fn the_qr_code_is_a_png_data_url() {
        let uri = provisioning_uri("revaro", "admin", RFC_SECRET);
        let data_url = qr_png_data_url(&uri).unwrap();
        let encoded = data_url
            .strip_prefix("data:image/png;base64,")
            .expect("PNG data URL");
        let png = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}
