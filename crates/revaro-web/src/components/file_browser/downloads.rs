//! Original-file and prepared ZIP downloads through the shared transport.

use super::*;

pub(in crate::components) fn download_file(file: &File) {
    start_download(&format!("/api/files/{}/download", file.id));
}

pub(super) fn start_download(path: &str) {
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
    // Chromium bypasses Service Workers for the HTML `download` attribute.
    // Use ordinary navigation: the shared worker streams a response carrying
    // Content-Disposition: attachment and the server's safe filename.
    let _ = anchor.set_attribute("hidden", "");
    if body.append_child(&anchor).is_ok()
        && let Ok(anchor) = anchor.dyn_into::<web_sys::HtmlElement>()
    {
        anchor.click();
        anchor.remove();
    }
}
