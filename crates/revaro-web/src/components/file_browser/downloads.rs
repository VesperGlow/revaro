//! Original-file and prepared ZIP downloads.

use super::*;

pub(in crate::components) fn download_file(file: &File) {
    start_download_with_name(
        &format!("/api/files/{}/download", file.id),
        Some(&file.name),
    );
}

pub(super) fn start_download(path: &str) {
    start_download_with_name(path, None);
}

pub(super) fn start_download_with_name(path: &str, name: Option<&str>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(body) = document.body() else {
        return;
    };
    let Ok(anchor) = document.create_element("a") else {
        return;
    };
    let _ = anchor.set_attribute("href", path);
    if let Some(name) = name {
        let _ = anchor.set_attribute("download", name);
    }
    let _ = anchor.set_attribute("hidden", "");
    if body.append_child(&anchor).is_ok()
        && let Ok(anchor) = anchor.dyn_into::<web_sys::HtmlElement>()
    {
        anchor.click();
        anchor.remove();
    }
}
