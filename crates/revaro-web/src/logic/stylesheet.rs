//! The load-bearing stylesheet cascade, pinned by a test.
//!
//! `static/styles.css` is the single entry point `index.html` links. It is an
//! ordered list of `@import`s recovered from the Vue entry point
//! (`main.ts` → `style.css` → the seven `styles/*.css` parts, then
//! `account.css`, `ui.css` and the remaining parts, with `video-player.css`
//! injected last by `VideoPlayer.vue`'s global `<style src>`).
//!
//! The order is not cosmetic. Roughly 200 rules are last-wins overrides, and the
//! clearest example is `.app-shell`'s `grid-template-columns`, declared as
//! `230px 1fr` by `shell.css` and `1fr` by `uploads.css`; the full-width
//! desktop layout only holds while `uploads.css` wins. A future edit that
//! reorders the imports would change layouts with no visible error, so the
//! sequence is asserted below against [`CASCADE`] + [`COMPONENT_SHEETS`].

/// The manifest text, embedded so the order test runs without touching the
/// filesystem (and so it compiles for wasm like the rest of `logic`).
pub const MANIFEST: &str = include_str!("../../static/styles.css");

/// The fourteen global stylesheets, in the exact cascade order Vue produced.
///
/// This is the list a reorder must not violate; the purpose strings document
/// why each sheet exists and therefore why its position matters.
pub const CASCADE: [Stylesheet; 14] = [
    Stylesheet::new(
        "styles/shell.css",
        "reset, splash, login, .app-shell grid, topbar, cards, modals, toast",
    ),
    Stylesheet::new(
        "styles/browser.css",
        "brand, connection pulse, storage bar, modal-backdrop states",
    ),
    Stylesheet::new(
        "styles/uploads.css",
        "file tiles and type icons, video thumb, create/upload menus",
    ),
    Stylesheet::new("styles/dialogs.css", "chapter-equalizer audio animation"),
    Stylesheet::new(
        "styles/media.css",
        "preview modal, command bar, filmstrip, image stage, audio player",
    ),
    Stylesheet::new(
        "styles/responsive.css",
        "viewport/intrinsic-size guards and breakpoint re-theming",
    ),
    Stylesheet::new(
        "styles/account.css",
        "account modal, avatar, password, TOTP and recovery panels",
    ),
    Stylesheet::new(
        "styles/ui.css",
        "authoritative design tokens and shared dialog/account primitives",
    ),
    Stylesheet::new("styles/selection-toolbar.css", "selection action toolbar"),
    Stylesheet::new("styles/share-dialog.css", "share dialog"),
    Stylesheet::new(
        "styles/document-editor.css",
        "editor chrome and markdown preview typography",
    ),
    Stylesheet::new(
        "styles/reader-flow.css",
        "reader typography, viewport, pager and chunk layout",
    ),
    Stylesheet::new(
        "styles/reader-chrome.css",
        "reader bar, footer, zones, TOC drawer, font popover",
    ),
    Stylesheet::new(
        "styles/video-player.css",
        "video shell, subtitles, controls and range styling",
    ),
];

/// The ported `<style scoped>` component blocks, loaded after every global
/// sheet.
///
/// Vue emitted these with an extra `[data-v-*]` attribute, so they outranked the
/// global sheets at equal specificity; loading them last and anchoring their
/// generic class names under each component root reproduces that. They are
/// listed separately because they are not part of the original 15-file cascade.
pub const COMPONENT_SHEETS: [Stylesheet; 9] = [
    Stylesheet::new(
        "styles/components/app-topbar.css",
        "AppTopbar scoped block; :deep() resolved",
    ),
    Stylesheet::new(
        "styles/components/task-center.css",
        "TaskCenter scoped block",
    ),
    Stylesheet::new(
        "styles/components/system-status.css",
        "SystemStatus scoped block",
    ),
    Stylesheet::new(
        "styles/components/service-card.css",
        "ServiceCard scoped block; :deep() resolved",
    ),
    Stylesheet::new(
        "styles/components/status-badge.css",
        "StatusBadge scoped block",
    ),
    Stylesheet::new(
        "styles/components/directory-flyout.css",
        "DirectoryPicker transition as a class toggle",
    ),
    Stylesheet::new(
        "styles/components/directory-picker.css",
        "DirectoryPicker and transfer dialog layout",
    ),
    Stylesheet::new(
        "styles/components/file-browser-header.css",
        "FileBrowserHeader breadcrumbs and responsive path styling",
    ),
    Stylesheet::new(
        "styles/components/full-bleed-progress.css",
        "FullBleedProgress track, buffer, chapter markers and native range overlay",
    ),
];

/// One stylesheet with a short note about what it owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stylesheet {
    /// Path relative to `static/`, as written in the manifest's `@import`.
    pub file: &'static str,
    /// What the sheet is responsible for; the reason its position matters.
    pub purpose: &'static str,
}

impl Stylesheet {
    /// Build a section entry.
    #[must_use]
    pub const fn new(file: &'static str, purpose: &'static str) -> Self {
        Self { file, purpose }
    }
}

/// Parse the `@import "path";` targets out of [`MANIFEST`], in file order.
///
/// Deliberately a tiny line scanner instead of a CSS parser: the manifest is
/// hand-written and only ever contains comments, `@import`s and blanks, so this
/// keeps the guard dependency-free and readable.
#[must_use]
pub fn imported_paths() -> Vec<String> {
    MANIFEST
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("@import")?.trim();
            let rest = rest.strip_prefix('"')?;
            let (path, _) = rest.split_once('"')?;
            Some(path.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full expected import list: the fourteen globals, then the component
    /// sheets, concatenated in order.
    fn expected_paths() -> Vec<&'static str> {
        CASCADE
            .iter()
            .chain(COMPONENT_SHEETS.iter())
            .map(|sheet| sheet.file)
            .collect()
    }

    #[test]
    fn the_global_cascade_keeps_its_fourteen_entries() {
        // The recovered Vue cascade had 15 entries; the media-library sheet
        // (and later its file-list successor) was removed with the category
        // sidebar and the list view. `style.css` only held the `@import`s and
        // became this manifest.
        assert_eq!(CASCADE.len(), 14);
    }

    #[test]
    fn the_manifest_imports_every_sheet_in_the_documented_order() {
        assert_eq!(imported_paths(), expected_paths());
    }

    #[test]
    fn uploads_css_loads_after_shell_so_the_browser_is_full_width() {
        // The specific trap the audit calls out: both declare
        // `.app-shell { grid-template-columns }`. With the category sidebar
        // removed, uploads.css's single-column value must win.
        let paths = imported_paths();
        let index = |needle: &str| {
            paths
                .iter()
                .position(|path| path == needle)
                .unwrap_or_else(|| panic!("{needle} missing from static/styles.css"))
        };
        assert!(index("styles/shell.css") < index("styles/uploads.css"));
    }

    #[test]
    fn ui_css_loads_after_responsive_css_so_its_tokens_win() {
        // Both used to define the same 37 tokens; ui.css's `--shadow-dialog:
        // #17212b33` wins purely because it loads later, and the duplicate in
        // responsive.css was deleted. The order still has to hold.
        let paths = imported_paths();
        let responsive = paths
            .iter()
            .position(|path| path == "styles/responsive.css")
            .expect("responsive.css imported");
        let ui = paths
            .iter()
            .position(|path| path == "styles/ui.css")
            .expect("ui.css imported");
        assert!(responsive < ui);
    }

    #[test]
    fn every_imported_stylesheet_exists_on_disk() {
        // Native-only in practice (the test target is the host), and it catches
        // the common mistake of renaming a file without editing the manifest.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static");
        for path in imported_paths() {
            let full = root.join(&path);
            assert!(
                full.is_file(),
                "manifest imports missing {}",
                full.display()
            );
        }
    }
}
