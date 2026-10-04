//! History entries, overlay dismissal and route replacement.

use super::*;

pub(super) fn push_browser_history() {
    if let Some(window) = web_sys::window()
        && let Ok(history) = window.history()
    {
        let _ = history.push_state_with_url(&JsValue::NULL, "", None);
    }
}

pub(super) fn request_overlay_close(
    nav_actions: RwSignal<Vec<NavAction>>,
    history_suppressed: RwSignal<bool>,
) -> bool {
    if history_suppressed.get_untracked()
        || !nav_actions
            .get_untracked()
            .last()
            .is_some_and(|action| matches!(action, NavAction::Overlay))
    {
        return false;
    }
    if let Some(window) = web_sys::window()
        && let Ok(history) = window.history()
    {
        let _ = history.back();
        return true;
    }
    false
}

pub(super) fn replace_reader_url(id: &str) {
    if let Some(window) = web_sys::window()
        && let Ok(history) = window.history()
    {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&format!("/read/{id}")));
    }
}

pub(super) fn replace_folder_url(id: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let url = folder_url(id, ROOT_ID);
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
    }
}
