//! Object-store primitives crossing the boundary between the server and its
//! storage/media engines.
//!
//! The local store is addressed by opaque string keys (see [`crate::keys`]) and
//! reports just enough metadata for the HTTP layer: a size and an entity tag.
//! These types are also the JSON shapes used by the multipart upload API, so
//! they are shared rather than duplicated.

use serde::{Deserialize, Serialize};

use crate::time::Timestamp;

/// Size and validator of a stored object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectInfo {
    /// Object size in bytes.
    pub size: i64,
    /// Entity tag, derived from size and modification time for local objects.
    pub etag: String,
}

/// One part referenced when completing a multipart upload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedPart {
    /// 1-based part number.
    pub part_number: i32,
    /// Entity tag the server returned when the part was stored.
    pub etag: String,
}

/// A stored object discovered by a prefix scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectRef {
    /// Object key.
    pub key: String,
    /// Object size in bytes.
    pub size: i64,
    /// Last modification time.
    pub last_modified: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_info_serializes_with_the_stored_field_names() {
        let info = ObjectInfo {
            size: 7,
            etag: "abc".into(),
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::json!({"size": 7, "etag": "abc"})
        );
    }

    #[test]
    fn completed_parts_match_the_completion_request_body() {
        let part = CompletedPart {
            part_number: 3,
            etag: "etag-3".into(),
        };
        assert_eq!(
            serde_json::to_value(&part).unwrap(),
            serde_json::json!({"part_number": 3, "etag": "etag-3"})
        );
    }
}
