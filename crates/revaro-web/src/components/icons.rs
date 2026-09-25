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

/// Lucide-style edit mark used by the account username action.
pub fn edit() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="m4 16-.8 4 4-.8L18.5 7.9l-3.2-3.2L4 16Z"></path>
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

/// Lucide `x` — cancel a background task.
pub fn close_square() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M18 6 6 18"></path>
            <path d="m6 6 12 12"></path>
        </svg>
    }
}

/// Lucide `rotate-ccw` — retry a failed background task.
pub fn rotate_ccw() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"></path>
            <path d="M3 3v5h5"></path>
        </svg>
    }
}

/// Lucide `music-2` — the audio-player fallback artwork.
pub fn music_2() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="8" cy="18" r="4"></circle>
            <path d="M12 18V2l7 4"></path>
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

/// Lucide `ellipsis` — an overflow menu.
pub fn more_horizontal() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="12" cy="12" r="1"></circle>
            <circle cx="19" cy="12" r="1"></circle>
            <circle cx="5" cy="12" r="1"></circle>
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
            <line x1="21" x2="16.65" y1="21" y2="16.65"></line>
            <line x1="11" x2="11" y1="8" y2="14"></line>
            <line x1="8" x2="14" y1="11" y2="11"></line>
        </svg>
    }
}

/// Lucide `zoom-out` — reduce an image.
pub fn zoom_out() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="11" cy="11" r="8"></circle>
            <line x1="21" x2="16.65" y1="21" y2="16.65"></line>
            <line x1="8" x2="14" y1="11" y2="11"></line>
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
            <path d="M2 7v10"></path>
            <path d="M6 5v14"></path>
            <rect width="12" height="18" x="10" y="3" rx="2"></rect>
        </svg>
    }
}

/// Lucide `download` — save the original file.
pub fn download() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 15V3"></path>
            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
            <path d="m7 10 5 5 5-5"></path>
        </svg>
    }
}

/// Lucide `move` — move an item to another directory.
pub fn move_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 2v20"></path>
            <path d="m15 19-3 3-3-3"></path>
            <path d="m19 9 3 3-3 3"></path>
            <path d="M2 12h20"></path>
            <path d="m5 9-3 3 3 3"></path>
            <path d="m9 5 3-3 3 3"></path>
        </svg>
    }
}

/// Lucide `copy` — duplicate an item.
pub fn copy() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect width="14" height="14" x="8" y="8" rx="2" ry="2"></rect>
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
            <path d="M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z"></path>
        </svg>
    }
}

/// Lucide `pause` — pause playback.
pub fn pause() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect x="14" y="3" width="5" height="18" rx="1"></rect>
            <rect x="5" y="3" width="5" height="18" rx="1"></rect>
        </svg>
    }
}

/// Lucide `rotate-cw` — seek forward.
pub fn rotate_cw() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8"></path>
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
            <path d="M12 4v16"></path>
            <path d="M4 7V5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v2"></path>
            <path d="M9 20h6"></path>
        </svg>
    }
}

/// Switch the reading paper between light and dark.
pub fn sun_moon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 2v2"></path>
            <path d="M14.837 16.385a6 6 0 1 1-7.223-7.222c.624-.147.97.66.715 1.248a4 4 0 0 0 5.26 5.259c.589-.255 1.396.09 1.248.715"></path>
            <path d="M16 12a4 4 0 0 0-4-4"></path>
            <path d="m19 5-1.256 1.256"></path>
            <path d="M20 12h2"></path>
        </svg>
    }
}

/// Lucide `volume-2` — audible playback.
pub fn volume_2() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z"></path>
            <path d="M16 9a5 5 0 0 1 0 6"></path>
            <path d="M19.364 18.364a9 9 0 0 0 0-12.728"></path>
        </svg>
    }
}

/// Lucide `volume-1` — quiet playback.
pub fn volume_1() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z"></path>
            <path d="M16 9a5 5 0 0 1 0 6"></path>
        </svg>
    }
}

/// Lucide `volume-x` — muted playback.
pub fn volume_x() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M11 4.702a.7.7 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.7.7 0 0 0 11 19.298z"></path>
            <path d="m16.5 14.5 5-5"></path>
            <path d="m16.5 9.5 5 5"></path>
        </svg>
    }
}

/// Lucide `skip-back` — previous chapter.
pub fn skip_back() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M17.971 4.285A2 2 0 0 1 21 6v12a2 2 0 0 1-3.029 1.715l-9.997-5.998a2 2 0 0 1-.003-3.432z"></path>
            <path d="M3 20V4"></path>
        </svg>
    }
}

/// Lucide `skip-forward` — next chapter.
pub fn skip_forward() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M21 4v16"></path>
            <path d="M6.029 4.285A2 2 0 0 0 3 6v12a2 2 0 0 0 3.029 1.715l9.997-5.998a2 2 0 0 0 .003-3.432z"></path>
        </svg>
    }
}

/// Lucide `settings-2` — playback settings.
pub fn settings_2() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M14 17H5"></path>
            <path d="M19 7h-9"></path>
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
            <path d="M3 5V19A9 3 0 0 0 21 19V5"></path>
            <path d="M3 12A9 3 0 0 0 21 12"></path>
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
