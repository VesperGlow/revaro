//! Inline SVG icons.
//!
//! The Vue app pulled these from `@lucide/vue`. The Leptos client has no icon
//! crate in the workspace, and every icon it needs is a handful of static SVG
//! paths, so they are written out here rather than adding a dependency. The
//! geometry is lucide's 24×24 stroke set; the markup keeps `aria-hidden="true"`
//! on every icon because the surrounding buttons carry the accessible names, and
//! the ported CSS sizes them through descendant selectors such as
//! `.category-icon svg`.
//!
//! Each helper returns `impl IntoView` rather than being a component: there is
//! no state to own and no props to thread.

#![allow(dead_code)]

use leptos::prelude::*;

/// Lucide `activity` — the task-centre trigger.
pub fn activity() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2"></path>
        </svg>
    }
}

/// Lucide `settings` — the mobile account menu.
pub fn settings() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915"></path>
            <circle cx="12" cy="12" r="3"></circle>
        </svg>
    }
}

/// Lucide `trash-2` — the trash entry in the top bar and the sidebar footer.
pub fn trash() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M10 11v6"></path>
            <path d="M14 11v6"></path>
            <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"></path>
            <path d="M3 6h18"></path>
            <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
        </svg>
    }
}

/// Lucide `square-x` — cancel a background task.
pub fn close_square() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="18" height="18" x="3" y="3" rx="2"></rect>
            <path d="m9 9 6 6"></path>
            <path d="m15 9-6 6"></path>
        </svg>
    }
}

/// Lucide `key-round` — supply an archive password.
pub fn key_round() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="m21 2-2 2m-7.61 7.61a5.5 5.5 0 1 1-7.778 7.778 5.5 5.5 0 0 1 7.777-7.777Zm0 0L15.5 7.5m0 0 3 3L22 7l-3-3m-3.5 3.5L19 4"></path>
        </svg>
    }
}

/// Lucide `rotate-ccw` — retry a failed background task.
pub fn rotate_ccw() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M3 12a9 9 0 1 0 3-6.7L3 8"></path>
            <path d="M3 3v5h5"></path>
        </svg>
    }
}

/// Lucide `book-open` — the 书架 category.
pub fn book_open() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 5v16"></path>
            <path d="M20.001 19A2 2 0 0022 17V5a2 2 0 00-1.999-2L16 3.002A5 5 0 0012 5a5 5 0 00-4-2H4a2 2 0 00-2 2v12a2 2 0 001.999 2H8a5 5 0 014 2 5 5 0 014-2z"></path>
        </svg>
    }
}

/// Lucide `image` — the 图片 category.
pub fn image() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="18" height="18" x="3" y="3" rx="2" ry="2"></rect>
            <circle cx="9" cy="9" r="2"></circle>
            <path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"></path>
        </svg>
    }
}

/// Lucide `film` — the 视频 category.
pub fn film() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="18" height="18" x="3" y="3" rx="2"></rect>
            <path d="M7 3v18"></path>
            <path d="M3 7.5h4"></path>
            <path d="M3 12h18"></path>
            <path d="M3 16.5h4"></path>
            <path d="M17 3v18"></path>
            <path d="M17 7.5h4"></path>
            <path d="M17 16.5h4"></path>
        </svg>
    }
}

/// Lucide `music` — the 音乐 category.
pub fn music() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M9 18V5l12-2v13"></path>
            <circle cx="6" cy="18" r="3"></circle>
            <circle cx="18" cy="16" r="3"></circle>
        </svg>
    }
}

/// Lucide `folder-closed` — the 文件 category.
pub fn folder_closed() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"></path>
            <path d="M2 10h20"></path>
        </svg>
    }
}

/// Lucide `file-text` — a generic text document.
pub fn file_text() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"></path>
            <path d="M14 2v6h6"></path>
            <path d="M8 13h8"></path>
            <path d="M8 17h8"></path>
            <path d="M8 9h2"></path>
        </svg>
    }
}

/// Lucide `file` — a fallback for files without a specialised type.
pub fn file() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"></path>
            <path d="M14 2v6h6"></path>
        </svg>
    }
}

/// Lucide `chevron-left` — the closed mobile drawer handle.
pub fn chevron_left() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="m15 18-6-6 6-6"></path>
        </svg>
    }
}

/// Lucide `chevron-right` — the open mobile drawer handle and the path accordion.
pub fn chevron_right() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="m9 18 6-6-6-6"></path>
        </svg>
    }
}

/// Lucide `chevron-right` — the separator used by the file-browser path.
pub fn breadcrumb_separator() -> impl IntoView {
    view! {
        <svg class="breadcrumb-separator" viewBox="0 0 24 24" width="15" height="15" fill="none"
            stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"
            aria-hidden="true">
            <path d="m9 18 6-6-6-6"></path>
        </svg>
    }
}

/// Lucide `panel-left-open` — expand the collapsed sidebar rail.
pub fn panel_left_open() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="18" height="18" x="3" y="3" rx="2"></rect>
            <path d="M9 3v18"></path>
            <path d="m14 9 3 3-3 3"></path>
        </svg>
    }
}

/// Lucide `panel-left-close` — collapse the sidebar to its icon rail.
pub fn panel_left_close() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="18" height="18" x="3" y="3" rx="2"></rect>
            <path d="M9 3v18"></path>
            <path d="m16 15-3-3 3-3"></path>
        </svg>
    }
}

/// Lucide `ellipsis` — an overflow menu.
pub fn more_horizontal() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="5" cy="12" r="1"></circle>
            <circle cx="12" cy="12" r="1"></circle>
            <circle cx="19" cy="12" r="1"></circle>
        </svg>
    }
}

/// Lucide `x` — close a viewer or sheet.
pub fn x() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M18 6 6 18"></path>
            <path d="m6 6 12 12"></path>
        </svg>
    }
}

/// Lucide `zoom-in` — enlarge an image.
pub fn zoom_in() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="11" cy="11" r="8"></circle>
            <path d="m21 21-4.3-4.3"></path>
            <path d="M11 8v6"></path>
            <path d="M8 11h6"></path>
        </svg>
    }
}

/// Lucide `zoom-out` — reduce an image.
pub fn zoom_out() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="11" cy="11" r="8"></circle>
            <path d="m21 21-4.3-4.3"></path>
            <path d="M8 11h6"></path>
        </svg>
    }
}

/// Lucide `scan` — fit an image to the stage.
pub fn scan() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M3 7V5a2 2 0 0 1 2-2h2"></path>
            <path d="M17 3h2a2 2 0 0 1 2 2v2"></path>
            <path d="M21 17v2a2 2 0 0 1-2 2h-2"></path>
            <path d="M7 21H5a2 2 0 0 1-2-2v-2"></path>
        </svg>
    }
}

/// Lucide `gallery-horizontal-end` — toggle the image filmstrip.
pub fn gallery_horizontal_end() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M2 3v18"></path>
            <rect width="12" height="18" x="6" y="3" rx="2"></rect>
            <path d="M22 15V9"></path>
        </svg>
    }
}

/// Lucide `download` — save the original file.
pub fn download() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 3v12"></path>
            <path d="m7 10 5 5 5-5"></path>
            <path d="M5 21h14"></path>
        </svg>
    }
}

/// Lucide `move` — move an item to another directory.
pub fn move_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M5 9 2 12l3 3"></path>
            <path d="m9 5 3-3 3 3"></path>
            <path d="m15 19-3 3-3-3"></path>
            <path d="m19 9 3 3-3 3"></path>
            <path d="M2 12h20"></path>
            <path d="M12 2v20"></path>
        </svg>
    }
}

/// Lucide `copy` — duplicate an item.
pub fn copy() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="14" height="14" x="8" y="8" rx="2"></rect>
            <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"></path>
        </svg>
    }
}

/// Lucide `info` — file metadata.
pub fn info() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="12" cy="12" r="10"></circle>
            <path d="M12 16v-4"></path>
            <path d="M12 8h.01"></path>
        </svg>
    }
}

/// Lucide `circle-alert` — the neutral action-dialog marker.
pub fn circle_alert() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="12" cy="12" r="10"></circle>
            <line x1="12" x2="12" y1="8" y2="12"></line>
            <line x1="12" x2="12.01" y1="16" y2="16"></line>
        </svg>
    }
}

/// Lucide `play` — start playback.
pub fn play() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="m6 3 14 9-14 9Z"></path>
        </svg>
    }
}

/// Lucide `pause` — pause playback.
pub fn pause() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="4" height="16" x="6" y="4" rx="1"></rect>
            <rect width="4" height="16" x="14" y="4" rx="1"></rect>
        </svg>
    }
}

/// Lucide `rotate-cw` — seek forward.
pub fn rotate_cw() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M21 12a9 9 0 1 1-3-6.7L21 8"></path>
            <path d="M21 3v5h-5"></path>
        </svg>
    }
}

/// Lucide `list` — show chapters.
pub fn list() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M3 5h.01"></path>
            <path d="M3 12h.01"></path>
            <path d="M3 19h.01"></path>
            <path d="M8 5h13"></path>
            <path d="M8 12h13"></path>
            <path d="M8 19h13"></path>
        </svg>
    }
}

/// Lucide `file-plus-2` — create a text document.
pub fn file_plus() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M11.35 22H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.706.706l3.588 3.588A2.4 2.4 0 0 1 20 8v5.35"></path>
            <path d="M14 2v5a1 1 0 0 0 1 1h5"></path>
            <path d="M14 19h6"></path>
            <path d="M17 16v6"></path>
        </svg>
    }
}

/// Lucide `folder-plus` — create a directory.
pub fn folder_plus() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 10v6"></path>
            <path d="M9 13h6"></path>
            <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"></path>
        </svg>
    }
}

/// Lucide `folder-up` — upload a directory.
pub fn folder_up() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"></path>
            <path d="M12 10v6"></path>
            <path d="m9 13 3-3 3 3"></path>
        </svg>
    }
}

/// Lucide `chevron-down` — native disclosure menu indicator.
pub fn chevron_down() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="m6 9 6 6 6-6"></path>
        </svg>
    }
}

/// Typography settings.
pub fn type_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M4 7V4h16v3"></path>
            <path d="M9 20h6"></path>
            <path d="M12 4v16"></path>
        </svg>
    }
}

/// Switch the reading paper between light and dark.
pub fn sun_moon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"></path>
            <path d="M19 3v4"></path>
            <path d="M21 5h-4"></path>
        </svg>
    }
}

/// Lucide `volume-2` — audible playback.
pub fn volume_2() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"></polygon>
            <path d="M19.07 4.93a10 10 0 0 1 0 14.14"></path>
            <path d="M15.54 8.46a5 5 0 0 1 0 7.07"></path>
        </svg>
    }
}

/// Lucide `volume-1` — quiet playback.
pub fn volume_1() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"></polygon>
            <path d="M15.54 8.46a5 5 0 0 1 0 7.07"></path>
        </svg>
    }
}

/// Lucide `volume-x` — muted playback.
pub fn volume_x() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"></polygon>
            <line x1="23" y1="9" x2="17" y2="15"></line>
            <line x1="17" y1="9" x2="23" y2="15"></line>
        </svg>
    }
}

/// Lucide `skip-back` — previous chapter.
pub fn skip_back() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polygon points="19 20 9 12 19 4 19 20"></polygon>
            <line x1="5" y1="19" x2="5" y2="5"></line>
        </svg>
    }
}

/// Lucide `skip-forward` — next chapter.
pub fn skip_forward() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polygon points="5 4 15 12 5 20 5 4"></polygon>
            <line x1="19" y1="5" x2="19" y2="19"></line>
        </svg>
    }
}

/// Lucide `captions` — subtitle settings.
pub fn captions() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="20" height="14" x="2" y="5" rx="2"></rect>
            <path d="M7 15h4"></path>
            <path d="M13 15h4"></path>
            <path d="M7 11h2"></path>
            <path d="M13 11h2"></path>
        </svg>
    }
}

/// Lucide `settings-2` — playback settings.
pub fn settings_2() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M20 7h-9"></path>
            <path d="M14 17H5"></path>
            <circle cx="17" cy="17" r="3"></circle>
            <circle cx="7" cy="7" r="3"></circle>
        </svg>
    }
}

/// Lucide `maximize` — enter full screen.
pub fn maximize() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M8 3H5a2 2 0 0 0-2 2v3"></path>
            <path d="M21 8V5a2 2 0 0 0-2-2h-3"></path>
            <path d="M3 16v3a2 2 0 0 0 2 2h3"></path>
            <path d="M16 21h3a2 2 0 0 0 2-2v-3"></path>
        </svg>
    }
}

/// Lucide `minimize` — exit full screen.
pub fn minimize() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M8 3v3a2 2 0 0 1-2 2H3"></path>
            <path d="M21 8h-3a2 2 0 0 1-2-2V3"></path>
            <path d="M3 16h3a2 2 0 0 1 2 2v3"></path>
            <path d="M16 21v-3a2 2 0 0 1 2-2h3"></path>
        </svg>
    }
}

/// Lucide `database` — the database service status card.
pub fn database() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <ellipse cx="12" cy="5" rx="9" ry="3"></ellipse>
            <path d="M3 5v14c0 1.657 4.03 3 9 3s9-1.343 9-3V5"></path>
            <path d="M3 12c0 1.657 4.03 3 9 3s9-1.343 9-3"></path>
        </svg>
    }
}

/// Lucide `cloud` — the storage service status card.
pub fn cloud() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z"></path>
        </svg>
    }
}

/// Lucide `hard-drive` — the cache service status card.
pub fn hard_drive() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M10 16h.01"></path>
            <path d="M2.212 11.577a2 2 0 0 0-.212.896V18a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-5.527a2 2 0 0 0-.212-.896L18.55 5.11A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"></path>
            <path d="M21.946 12.013H2.054"></path>
            <path d="M6 16h.01"></path>
        </svg>
    }
}

/// Lucide `layout-grid` — a grid view switch.
pub fn layout_grid() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="7" height="7" x="3" y="3" rx="1"></rect>
            <rect width="7" height="7" x="14" y="3" rx="1"></rect>
            <rect width="7" height="7" x="14" y="14" rx="1"></rect>
            <rect width="7" height="7" x="3" y="14" rx="1"></rect>
        </svg>
    }
}

/// Lucide `images` — the album view switch.
pub fn images() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="m2 16 4.5-4.5a2.12 2.12 0 0 1 3 0L14 16"></path>
            <path d="m14 14 1.5-1.5a2.12 2.12 0 0 1 3 0L22 16"></path>
            <path d="M4 19h16a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2Z"></path>
            <circle cx="8.5" cy="8.5" r="1.5"></circle>
        </svg>
    }
}

/// Lucide `refresh-cw` — reload a library view.
pub fn refresh_cw() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"></path>
            <path d="M21 3v5h-5"></path>
            <path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"></path>
            <path d="M8 16H3v5"></path>
        </svg>
    }
}

/// Lucide `upload` — an upload action.
pub fn upload() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 3v12"></path>
            <path d="m17 8-5-5-5 5"></path>
            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
        </svg>
    }
}
