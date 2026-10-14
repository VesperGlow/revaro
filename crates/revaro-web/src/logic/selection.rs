//! Availability of selection controls; mutation callbacks stay in the workspace.

use revaro_core::{
    classify,
    model::{File, FileKind},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAction {
    Directory,
    Editor,
    Image,
    Media,
}

impl OpenAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Directory => "打开",
            Self::Editor => "编辑文本",
            Self::Image => "预览",
            Self::Media => "播放",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MobileActions {
    pub read: bool,
    pub open_other: Option<OpenAction>,
    pub copy: bool,
    pub rename: bool,
    pub share: bool,
}

pub fn mobile_actions(files: &[File], file_tools: bool, trash: bool) -> MobileActions {
    if files.is_empty() || trash {
        return MobileActions::default();
    }
    let single = (files.len() == 1).then(|| &files[0]);
    let read = single.is_some_and(classify::is_book);
    let open_other = single.filter(|_| file_tools).and_then(|file| {
        if file.kind == FileKind::Directory {
            Some(OpenAction::Directory)
        } else if classify::is_editable(file) {
            Some(OpenAction::Editor)
        } else if read {
            None
        } else if classify::is_image(file) {
            Some(OpenAction::Image)
        } else if classify::is_audio(file) || classify::is_video(file) {
            Some(OpenAction::Media)
        } else {
            None
        }
    });
    MobileActions {
        read,
        open_other,
        copy: file_tools,
        rename: file_tools && single.is_some(),
        share: file_tools && single.is_some_and(|file| file.kind == FileKind::File),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FavoriteActions {
    pub favorite: bool,
    pub unfavorite: bool,
}

pub fn favorite_actions(states: impl IntoIterator<Item = bool>) -> FavoriteActions {
    let mut actions = FavoriteActions::default();
    for favorite in states {
        actions.favorite |= !favorite;
        actions.unfavorite |= favorite;
    }
    actions
}

pub fn favorite_actions_for_selection(
    selected_ids: &[String],
    states: &[revaro_core::library::SelectionItemState],
) -> Option<FavoriteActions> {
    let states = states
        .iter()
        .map(|state| (state.file_id.as_str(), state.favorite))
        .collect::<std::collections::HashMap<_, _>>();
    let values = selected_ids
        .iter()
        .map(|id| states.get(id.as_str()).copied())
        .collect::<Option<Vec<_>>>()?;
    Some(favorite_actions(values))
}

#[derive(Debug, Clone, Copy)]
pub struct MenuAnchor {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct MenuPlacement {
    pub left: f64,
    pub top: f64,
    pub available_height: f64,
}

/// Keep the selection menu inside the viewport, including after rotation or
/// content growth. Natural content height avoids staying in a cramped space
/// merely because the previous layout already capped the menu's height.
pub fn mobile_menu_placement(
    viewport: [f64; 2],
    anchor: MenuAnchor,
    panel: [f64; 2],
) -> MenuPlacement {
    let [width, height] = viewport;
    let margin = 8.0;
    let gap = 8.0;
    let below = (height - (anchor.top + anchor.height).max(margin) - gap - margin).max(0.0);
    let above = (anchor.top.min(height - margin) - gap - margin).max(0.0);
    let desired_height = panel[1].min(360.0).min((height - 2.0 * margin).max(0.0));
    let opens_up = desired_height > below && above > below;
    let available_height = (if opens_up { above } else { below }).min(360.0);
    let visible_height = desired_height.min(available_height);
    let left = (anchor.left + (anchor.width - panel[0]) / 2.0)
        .clamp(margin, (width - margin - panel[0]).max(margin));
    let top = if opens_up {
        anchor.top - gap - visible_height
    } else {
        anchor.top + anchor.height + gap
    };
    MenuPlacement {
        left,
        top: top.clamp(margin, (height - margin - visible_height).max(margin)),
        available_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use revaro_core::model::FileStatus;

    #[test]
    fn mobile_menu_fits_viewport_edges_short_screens_and_offscreen_anchors() {
        for width in [320.0, 390.0, 850.0] {
            for height in [200.0, 260.0, 900.0] {
                for top in [-50.0, 80.0, height - 20.0, height + 100.0] {
                    let anchor = MenuAnchor {
                        left: width - 60.0,
                        top,
                        width: 44.0,
                        height: 44.0,
                    };
                    let placement = mobile_menu_placement([width, height], anchor, [240.0, 500.0]);
                    assert!(placement.left >= 8.0 && placement.left + 240.0 <= width - 8.0);
                    assert!(placement.top >= 8.0);
                    assert!(placement.top + placement.available_height <= height - 8.0);
                }
            }
        }
    }

    #[test]
    fn growing_menus_flip_up_instead_of_reusing_the_old_height_cap() {
        let anchor = MenuAnchor {
            left: 250.0,
            top: 120.0,
            width: 44.0,
            height: 44.0,
        };
        let placement = mobile_menu_placement([320.0, 260.0], anchor, [240.0, 352.0]);
        assert_eq!(placement.top, 8.0);
        assert_eq!(placement.available_height, 104.0);
    }

    #[test]
    fn favorite_state_requires_every_selected_file_and_ignores_other_files() {
        use revaro_core::library::SelectionItemState;
        let states = vec![
            SelectionItemState {
                file_id: "selected".into(),
                favorite: true,
            },
            SelectionItemState {
                file_id: "other".into(),
                favorite: false,
            },
        ];
        assert_eq!(
            favorite_actions_for_selection(&["selected".into()], &states),
            Some(FavoriteActions {
                favorite: false,
                unfavorite: true
            })
        );
        assert_eq!(
            favorite_actions_for_selection(&["selected".into(), "missing".into()], &states),
            None
        );
    }

    fn file(name: &str) -> File {
        File {
            name: name.into(),
            kind: FileKind::File,
            status: FileStatus::Ready,
            ..File::default()
        }
    }

    #[test]
    fn single_books_keep_reading_distinct_from_text_editing() {
        for name in ["book.epub", "book.txt"] {
            let selected = [file(name)];
            let actions = mobile_actions(&selected, true, false);
            assert!(actions.read && actions.rename && actions.share && actions.copy);
            assert_eq!(
                actions.open_other,
                if name.ends_with(".txt") {
                    Some(OpenAction::Editor)
                } else {
                    None
                }
            );
            assert!(mobile_actions(&selected, false, false).read);
        }
    }

    #[test]
    fn multiple_selections_do_not_expose_single_item_actions() {
        let actions = mobile_actions(&[file("a.epub"), file("b.epub")], true, false);
        assert!(actions.copy);
        assert!(!actions.read && !actions.rename && !actions.share);
        assert_eq!(actions.open_other, None);
    }

    #[test]
    fn directories_documents_media_and_trash_use_the_existing_eligibility() {
        let mut directory = file("folder");
        directory.kind = FileKind::Directory;
        let actions = mobile_actions(&[directory], true, false);
        assert_eq!(actions.open_other, Some(OpenAction::Directory));
        assert!(actions.rename && !actions.share && !actions.read);
        for (name, open) in [
            ("notes.md", OpenAction::Editor),
            ("photo.png", OpenAction::Image),
            ("song.mp3", OpenAction::Media),
        ] {
            assert_eq!(
                mobile_actions(&[file(name)], true, false).open_other,
                Some(open)
            );
        }
        assert_eq!(
            mobile_actions(&[file("a.epub")], true, true),
            MobileActions::default()
        );
        assert_eq!(mobile_actions(&[], true, false), MobileActions::default());
    }

    #[test]
    fn favorite_choices_match_all_none_and_mixed_selection_states() {
        assert_eq!(favorite_actions([]), FavoriteActions::default());
        assert_eq!(
            favorite_actions([true, true]),
            FavoriteActions {
                favorite: false,
                unfavorite: true
            }
        );
        assert_eq!(
            favorite_actions([false, false]),
            FavoriteActions {
                favorite: true,
                unfavorite: false
            }
        );
        assert_eq!(
            favorite_actions([true, false]),
            FavoriteActions {
                favorite: true,
                unfavorite: true
            }
        );
    }
}
