//! Shared contracts for the personal content library.
use serde::{Deserialize, Serialize};

use crate::{Timestamp, model::File};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryItem {
    pub file: File,
    pub kind: String,
    pub favorite: bool,
    pub last_opened: Option<Timestamp>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub reading_progress: Option<f64>,
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub series_index: Option<f64>,
    /// Nonzero only for an aggregated series card. Members reuse ordinary file actions.
    #[serde(default)]
    pub series_files: Vec<File>,
}

impl LibraryItem {
    pub fn selectable_files(&self) -> Vec<File> {
        if self.series_files.is_empty() {
            vec![self.file.clone()]
        } else {
            self.series_files.clone()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibraryListing {
    pub items: Vec<LibraryItem>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub item_count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemUpdate {
    pub favorite: Option<bool>,
    #[serde(default)]
    pub opened: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCollection {
    pub name: String,
    pub kind: String,
}
