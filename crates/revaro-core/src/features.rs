//! Shared payloads for browsing, share management and document recovery.
use crate::{
    Timestamp,
    model::{File, ShareStatus},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareRequest {
    /// None creates a permanent link; otherwise seconds from issuance (1..=365 days).
    pub expires_in_seconds: Option<i64>,
    /// Optional URL suffix after `/s/`; an empty value creates a random link.
    pub custom_slug: Option<String>,
}

/// Validate a share suffix, excluding reserved application route names.
pub fn valid_share_slug(slug: &str) -> bool {
    (1..=64).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && !matches!(
            slug.to_ascii_lowercase().as_str(),
            "api"
                | "s"
                | "f"
                | "read"
                | "files"
                | "trash"
                | "library"
                | "music"
                | "gallery"
                | "videos"
                | "healthz"
                | "readyz"
                | "login"
                | "logout"
                | "account"
                | "settings"
                | "assets"
                | "static"
                | "pkg"
        )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareEntry {
    pub file_id: String,
    pub name: String,
    #[serde(flatten)]
    pub status: ShareStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Listing {
    pub items: Vec<File>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
    pub total_bytes: i64,
    pub file_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentVersion {
    pub id: String,
    pub size: i64,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreVersionRequest {
    pub etag: String,
}
