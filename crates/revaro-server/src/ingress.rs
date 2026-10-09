//! Revaro's public entry point: HTTP-01, canonical HTTPS redirects and one
//! renewable certificate resolver shared by TCP TLS and standard QUIC.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Response, StatusCode, Uri},
    routing::get_service,
};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use rustls_acme::{AccountCache, AcmeConfig, AcmeState, CertCache, UseChallenge};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcmeSettings {
    pub domain: String,
    pub contact: Vec<String>,
    pub directory: String,
    pub cache_dir: PathBuf,
    /// An additional trusted CA for private ACME services; verification stays on.
    pub ca_file: Option<PathBuf>,
}

impl AcmeSettings {
    pub(crate) fn load(
        lookup: &dyn Fn(&str) -> Option<String>,
        data_dir: &Path,
    ) -> Result<Option<Self>, String> {
        let Some(domain) = lookup("APP_DOMAIN").filter(|v| !v.trim().is_empty()) else {
            return Ok(None);
        };
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        if domain.len() > 253
            || !domain.contains('.')
            || domain.parse::<std::net::IpAddr>().is_ok()
            || domain.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err("APP_DOMAIN must be a DNS hostname (use punycode for international domains), without scheme, port or wildcard".into());
        }
        let staging = match lookup("ACME_STAGING").as_deref().unwrap_or("false") {
            "true" | "1" => true,
            "false" | "0" | "" => false,
            _ => return Err("ACME_STAGING must be true or false".into()),
        };
        let directory = lookup("ACME_DIRECTORY_URL")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| {
                if staging {
                    "https://acme-staging-v02.api.letsencrypt.org/directory".into()
                } else {
                    "https://acme-v02.api.letsencrypt.org/directory".into()
                }
            });
        let parsed = url::Url::parse(&directory).map_err(|_| "invalid ACME_DIRECTORY_URL")?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(
                "ACME_DIRECTORY_URL must be an HTTPS URL without credentials or fragment".into(),
            );
        }
        let contact = lookup("ACME_EMAIL")
            .filter(|v| !v.trim().is_empty())
            .map(|email| -> Result<Vec<String>, String> {
                let email = email.trim().strip_prefix("mailto:").unwrap_or(email.trim());
                if !email.contains('@')
                    || email
                        .bytes()
                        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
                {
                    return Err("ACME_EMAIL must be an email address".into());
                }
                Ok(vec![format!("mailto:{email}")])
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Some(Self {
            domain,
            contact,
            directory,
            cache_dir: lookup("ACME_CACHE_DIR")
                .filter(|v| !v.trim().is_empty())
                .map_or_else(|| data_dir.join("acme"), PathBuf::from),
            ca_file: lookup("ACME_CA_FILE")
                .filter(|v| !v.trim().is_empty())
                .map(PathBuf::from),
        }))
    }

    pub async fn state(&self) -> io::Result<AcmeState<io::Error>> {
        let cache = PrivateCache::open(self.cache_dir.clone()).await?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = AcmeConfig::new_with_provider([self.domain.clone()], provider.clone());
        if let Some(path) = &self.ca_file {
            let pem = tokio::fs::read(path).await?;
            let mut roots = rustls::RootCertStore::empty();
            for cert in CertificateDer::pem_slice_iter(&pem) {
                roots
                    .add(cert.map_err(io::Error::other)?)
                    .map_err(io::Error::other)?;
            }
            if roots.is_empty() {
                return Err(io::Error::other("ACME_CA_FILE contains no certificates"));
            }
            config = config.client_tls_config(Arc::new(
                rustls::ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .map_err(io::Error::other)?
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            ));
        }
        Ok(config
            .directory(&self.directory)
            .contact(self.contact.clone())
            .challenge_type(UseChallenge::Http01)
            .cache(cache)
            .state())
    }
}

pub fn http_router(state: &AcmeState<io::Error>, origin: &str) -> Router {
    let origin = origin.to_owned();
    Router::new()
        .route(
            "/.well-known/acme-challenge/{token}",
            get_service(state.http01_challenge_tower_service()),
        )
        .fallback(move |uri: Uri| {
            let origin = origin.clone();
            async move {
                // Concatenation preserves the path/query without treating //host
                // as a new authority. Host and forwarded headers are never trusted.
                let path = uri.path_and_query().map_or("/", |v| v.as_str());
                Response::builder()
                    .status(StatusCode::PERMANENT_REDIRECT)
                    .header(http::header::LOCATION, format!("{origin}{path}"))
                    .header(http::header::CACHE_CONTROL, "no-store")
                    .body(Body::empty())
                    .expect("validated HTTPS origin and request URI")
            }
        })
}

/// Account keys and certificate/private-key bundles survive restarts without
/// exposing secrets through default umask or truncating the last good copy.
#[derive(Clone, Debug)]
pub struct PrivateCache {
    directory: PathBuf,
}

impl PrivateCache {
    pub async fn open(directory: PathBuf) -> io::Result<Self> {
        tokio::fs::create_dir_all(&directory).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).await?;
        }
        let cache = Self { directory };
        // Fail startup on an unwritable cache instead of issuing ephemeral certs.
        let probe = cache
            .directory
            .join(format!(".probe-{}", uuid::Uuid::new_v4()));
        cache.atomic_write(&probe, b"").await?;
        tokio::fs::remove_file(probe).await?;
        Ok(cache)
    }

    fn path(&self, kind: &str, names: &[String], directory_url: &str) -> PathBuf {
        let mut hash = Sha256::new();
        for name in names {
            hash.update(name.as_bytes());
            hash.update([0]);
        }
        hash.update(directory_url.as_bytes());
        self.directory
            .join(format!("{kind}-{}", hex::encode(hash.finalize())))
    }

    async fn load(&self, path: PathBuf) -> io::Result<Option<Vec<u8>>> {
        match tokio::fs::read(path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn atomic_write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let temp = self
            .directory
            .join(format!(".pending-{}", uuid::Uuid::new_v4()));
        let result = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temp).await?;
            file.write_all(bytes).await?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&temp, path).await?;
            #[cfg(unix)]
            tokio::fs::File::open(&self.directory)
                .await?
                .sync_all()
                .await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        result
    }
}

#[async_trait]
impl CertCache for PrivateCache {
    type EC = io::Error;
    async fn load_cert(
        &self,
        domains: &[String],
        directory_url: &str,
    ) -> io::Result<Option<Vec<u8>>> {
        self.load(self.path("cert", domains, directory_url)).await
    }
    async fn store_cert(
        &self,
        domains: &[String],
        directory_url: &str,
        cert: &[u8],
    ) -> io::Result<()> {
        self.atomic_write(&self.path("cert", domains, directory_url), cert)
            .await
    }
}

#[async_trait]
impl AccountCache for PrivateCache {
    type EA = io::Error;
    async fn load_account(
        &self,
        contact: &[String],
        directory_url: &str,
    ) -> io::Result<Option<Vec<u8>>> {
        self.load(self.path("account", contact, directory_url))
            .await
    }
    async fn store_account(
        &self,
        contact: &[String],
        directory_url: &str,
        account: &[u8],
    ) -> io::Result<()> {
        self.atomic_write(&self.path("account", contact, directory_url), account)
            .await
    }
}
