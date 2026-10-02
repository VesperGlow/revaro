//! Durable delivery of generated administrator credentials.

use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::AsyncWriteExt as _;

use super::service::{PASSWORD_HASH_KEY, random_text};
use super::{AuthError, AuthService, InitialCredentials};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    username: String,
    password: String,
}

impl AuthService {
    /// Deliver a reset password before committing the reset, without putting secrets in logs.
    pub async fn reset_with_credentials_file(
        &self,
        username: &str,
        path: &Path,
    ) -> Result<(), AuthError> {
        let credentials = Credentials {
            username: if username.is_empty() {
                "admin"
            } else {
                username
            }
            .to_owned(),
            password: random_text(),
        };
        if credentials.username.len() > 128 {
            return Err(AuthError::Invalid(
                "administrator username is too long".to_owned(),
            ));
        }
        let bytes = serde_json::to_vec_pretty(&credentials)
            .map_err(|_| AuthError::Internal("credential encoding failed".to_owned()))?;
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(path).await.map_err(file_error)?;
        file.write_all(&bytes).await.map_err(file_error)?;
        file.sync_all().await.map_err(file_error)?;
        if let Some(parent) = path.parent() {
            tokio::fs::File::open(parent)
                .await
                .map_err(file_error)?
                .sync_all()
                .await
                .map_err(file_error)?;
        }
        self.reset_to(&credentials.username, &credentials.password)
            .await?;
        Ok(())
    }

    /// Stage generated credentials durably before committing their password hash.
    /// A restart after staging reuses the same file, so the administrator can
    /// always retrieve the password actually committed to the database.
    pub async fn initialize_with_credentials_file(
        &self,
        username: &str,
        password: &str,
        path: &Path,
    ) -> Result<InitialCredentials, AuthError> {
        if self.read_setting(PASSWORD_HASH_KEY).await?.is_some() {
            return Ok(InitialCredentials::default());
        }
        if !password.is_empty() {
            return self.initialize(username, password).await;
        }
        let staged = match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) if metadata.is_file() && metadata.len() <= 4096 => {
                let bytes = tokio::fs::read(path).await.map_err(file_error)?;
                let credentials: Credentials = serde_json::from_slice(&bytes).map_err(|_| {
                    AuthError::Invalid("initial credentials file is invalid".to_owned())
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                        .await
                        .map_err(file_error)?;
                }
                credentials
            }
            Ok(_) => {
                return Err(AuthError::Invalid(
                    "initial credentials path must be a regular file".to_owned(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let credentials = Credentials {
                    username: if username.is_empty() {
                        "admin"
                    } else {
                        username
                    }
                    .to_owned(),
                    password: random_text(),
                };
                if credentials.username.len() > 128 {
                    return Err(AuthError::Invalid(
                        "administrator username is too long".to_owned(),
                    ));
                }
                let bytes = serde_json::to_vec_pretty(&credentials).map_err(|_| {
                    AuthError::Internal("could not encode initial credentials".to_owned())
                })?;
                let mut options = tokio::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                options.mode(0o600);
                let mut file = options.open(path).await.map_err(file_error)?;
                file.write_all(&bytes).await.map_err(file_error)?;
                file.sync_all().await.map_err(file_error)?;
                if let Some(parent) = path.parent() {
                    tokio::fs::File::open(parent)
                        .await
                        .map_err(file_error)?
                        .sync_all()
                        .await
                        .map_err(file_error)?;
                }
                credentials
            }
            Err(error) => return Err(file_error(error)),
        };
        if staged.username.is_empty() || staged.username.len() > 128 || staged.password.is_empty() {
            return Err(AuthError::Invalid(
                "initial credentials file is invalid".to_owned(),
            ));
        }
        let mut result = self.initialize(&staged.username, &staged.password).await?;
        result.generated = result.created;
        // Startup needs the delivery path and username, never the plaintext.
        result.password.clear();
        Ok(result)
    }
}

fn file_error(error: std::io::Error) -> AuthError {
    AuthError::Internal(format!(
        "could not secure initial credentials file: {error}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[tokio::test]
    async fn generated_credentials_survive_restart_and_are_private() {
        let root = std::env::temp_dir().join(format!("revaro-bootstrap-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let path = root.join("initial-admin-credentials");
        let auth = AuthService::new(Database::open_in_memory().unwrap());
        let result = auth
            .initialize_with_credentials_file("admin", "", &path)
            .await
            .unwrap();
        assert!(result.created && result.generated && result.password.is_empty());
        let bytes = tokio::fs::read(&path).await.unwrap();
        let credentials: Credentials = serde_json::from_slice(&bytes).unwrap();
        assert!(
            auth.login(&credentials.username, &credentials.password, "")
                .await
                .is_ok()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                tokio::fs::metadata(&path)
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(
            !auth
                .initialize_with_credentials_file("admin", "", &path)
                .await
                .unwrap()
                .created
        );
        assert_eq!(tokio::fs::read(&path).await.unwrap(), bytes);
        // Simulate a crash after the file was written, before the DB transaction.
        let recovered = AuthService::new(Database::open_in_memory().unwrap());
        recovered
            .initialize_with_credentials_file("admin", "", &path)
            .await
            .unwrap();
        assert!(
            recovered
                .login(&credentials.username, &credentials.password, "")
                .await
                .is_ok()
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn delivery_failure_does_not_create_an_inaccessible_account() {
        let auth = AuthService::new(Database::open_in_memory().unwrap());
        let path = std::env::temp_dir()
            .join(format!("revaro-missing-{}", uuid::Uuid::new_v4()))
            .join("credentials");
        assert!(
            auth.initialize_with_credentials_file("admin", "", &path)
                .await
                .is_err()
        );
        assert!(
            auth.read_setting(PASSWORD_HASH_KEY)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            auth.initialize_with_credentials_file("admin", "known-safe-password", &path)
                .await
                .unwrap()
                .created
        );
    }
}
