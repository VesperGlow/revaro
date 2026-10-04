//! Versioned browser resource URLs shared by file tiles and media previews.

use revaro_core::model::File;

pub(super) fn thumbnail_url(file: &File) -> String {
    format!(
        "/api/files/{}/thumbnail?v={}",
        file.id,
        js_sys::encode_uri_component(&file.etag)
            .as_string()
            .unwrap_or_default()
    )
}
