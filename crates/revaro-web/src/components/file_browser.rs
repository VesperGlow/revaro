//! The authenticated file browser and its basic lifecycle actions.
//!
//! This slice owns folder navigation, breadcrumbs, the grid/list choice,
//! selection, file and folder mutations, and the trash view. Media previews
//! are mounted in the same authenticated shell so their session and gallery
//! state stay attached to the listing.

use std::collections::HashSet;

use futures_channel::oneshot;
use futures_util::{StreamExt, stream};
use leptos::prelude::*;
use revaro_core::api::auth::Session;
use revaro_core::api::files::{
    Children, CopyFileRequest, CreateDirectoryRequest, CreateDocumentRequest, FileDetail,
    PatchFileRequest, UpdateDocumentRequest,
};
use revaro_core::classify;
use revaro_core::classify::LibraryKind;
use revaro_core::ids::ROOT_ID;
use revaro_core::model::{File, FileKind, FileStatus, LibraryCounts, LibraryItem};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

use crate::api;
use crate::browser;
use crate::logic::feedback::{Feedback, FeedbackKind};
use crate::logic::format::{format_date, format_size};
use crate::logic::routing::{folder_id, folder_url, library_route, library_url};

use super::account::AccountSettings;
use super::dialogs::{ActionDialog, RenameDialog};
use super::editor::{DocumentEditor, EditorMode};
use super::file_browser_header::FileBrowserHeader;
use super::library::{self, LibraryView};
use super::media::MediaPreview;
use super::reader::ReaderView;
use super::selection_toolbar::SelectionToolbar;
use super::share::ShareDialog;
use super::sidebar::{AppSidebar, LibraryTrees};
use super::tasks::TaskController;
use super::topbar::AppTopbar;
use super::transfer::{TransferDialog, TransferMode};
use super::uploads::{UploadController, UploadRefresh, UploadSurface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Grid,
    List,
}

const FILE_VIEW_MODE_KEY: &str = "revaro:library:media:file";

#[derive(Debug, Clone, PartialEq, Eq)]
enum DialogState {
    CreateFolder,
    DiscardEditor,
    Rename { id: String },
    Delete,
    ExtractArchive { name: String },
    RegenerateShare,
    RevokeShare,
    Purge,
    EmptyTrash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NavAction {
    /// The folder to restore when the browser moves one level back.
    Folder { id: String },
    /// The section and folder context to restore after a category switch.
    Section {
        section: LibraryKind,
        folder_id: String,
    },
    /// A modal/disclosure surface that should be closed by the browser back
    /// button before leaving the current page.
    Overlay,
}

#[derive(Debug, Clone)]
struct TransferRequest {
    mode: TransferMode,
    targets: Vec<File>,
}

/// A folder load optionally used as an awaitable refresh by another shell
/// operation. Ordinary navigation does not need the completion sender, but
/// folder uploads must preserve the reference ordering: refresh first, then
/// show the success feedback.
struct FolderLoadRequest {
    id: String,
    completion: Option<oneshot::Sender<()>>,
}

fn finish_folder_load(completion: &mut Option<oneshot::Sender<()>>) {
    if let Some(sender) = completion.take() {
        let _ = sender.send(());
    }
}

/// The authenticated file browser.
#[component]
pub fn FileBrowser(
    session: Session,
    on_logout: Callback<()>,
    on_username_changed: Callback<String>,
    on_password_changed: Callback<String>,
) -> impl IntoView {
    let current_id = RwSignal::new(ROOT_ID.to_owned());
    let current = RwSignal::new(None::<File>);
    let breadcrumbs = RwSignal::new(Vec::<File>::new());
    let items = RwSignal::new(Vec::<File>::new());
    let total_bytes = RwSignal::new(0_i64);
    let file_count = RwSignal::new(0_i64);
    let loading = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let trash_mode = RwSignal::new(false);
    let view_mode = RwSignal::new(
        match browser::local_storage_get(FILE_VIEW_MODE_KEY).as_deref() {
            Some("list") => ViewMode::List,
            _ => ViewMode::Grid,
        },
    );
    {
        let view_mode = view_mode;
        Effect::new(move |_| {
            browser::local_storage_set(
                FILE_VIEW_MODE_KEY,
                if view_mode.get() == ViewMode::List {
                    "list"
                } else {
                    "grid"
                },
            );
        });
    }
    let section = RwSignal::new(LibraryKind::File);
    let sidebar_collapsed = RwSignal::new(
        crate::browser::local_storage_get("revaro:sidebar:collapsed").as_deref() == Some("1"),
    );
    let sidebar_mobile_open = RwSignal::new(false);
    let library_folder_id = RwSignal::new(None::<String>);
    let library_items = RwSignal::new(Vec::<LibraryItem>::new());
    let library_items_by_type =
        RwSignal::new(std::collections::HashMap::<LibraryKind, Vec<LibraryItem>>::new());
    let library_counts = RwSignal::new(LibraryCounts::default());
    let library_trees = RwSignal::new(LibraryTrees::new());
    let library_loading = RwSignal::new(false);
    let library_error = RwSignal::new(String::new());
    let library_filter_label = RwSignal::new("全部位置".to_owned());
    let library_sequence = RwSignal::new(0_u64);
    let library_loaded = RwSignal::new(false);
    let tree_token = RwSignal::new(0_u64);
    let request_sequence = RwSignal::new(0_u64);
    let selected_ids = RwSignal::new(HashSet::<String>::new());
    let dialog = RwSignal::new(None::<DialogState>);
    let dialog_value = RwSignal::new(String::new());
    let dialog_busy = RwSignal::new(false);
    let dialog_error = RwSignal::new(String::new());
    let feedback = RwSignal::new(None::<Feedback>);
    let feedback_timer = RwSignal::new(None::<i32>);
    let notify = {
        let feedback = feedback;
        let feedback_timer = feedback_timer;
        Callback::new(move |notification: Feedback| {
            if let Some(timer) = feedback_timer.get_untracked() {
                feedback_timer.set(None);
                if let Some(window) = web_sys::window() {
                    window.clear_timeout_with_handle(timer);
                }
            }
            if notification.message.is_empty() {
                feedback.set(None);
                return;
            }
            feedback.set(Some(notification));
            let Some(window) = web_sys::window() else {
                return;
            };
            let feedback_for_timer = feedback;
            let timer_for_callback = feedback_timer;
            let callback = Closure::once_into_js(move || {
                feedback_for_timer.set(None);
                timer_for_callback.set(None);
            });
            if let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                3_600,
            ) {
                feedback_timer.set(Some(timer));
            }
        })
    };
    let media_file = RwSignal::new(None::<File>);
    let preview_items = RwSignal::new(Vec::<File>::new());
    let archive_target = RwSignal::new(None::<File>);
    let share_file = RwSignal::new(None::<File>);
    let share_active = RwSignal::new(false);
    let share_url = RwSignal::new(String::new());
    let share_created_at = RwSignal::new(String::new());
    let share_busy = RwSignal::new(false);
    let share_error = RwSignal::new(String::new());
    let share_copied = RwSignal::new(false);
    let reader_file = RwSignal::new(None::<File>);
    let editor_open = RwSignal::new(false);
    let editor_is_new = RwSignal::new(false);
    let editor_readonly = RwSignal::new(false);
    let editor_file_id = RwSignal::new(String::new());
    let editor_name = RwSignal::new(String::new());
    let editor_original_name = RwSignal::new(String::new());
    let editor_content = RwSignal::new(String::new());
    let editor_original = RwSignal::new(String::new());
    let editor_etag = RwSignal::new(String::new());
    let editor_mode = RwSignal::new(EditorMode::Edit);
    let editor_busy = RwSignal::new(false);
    let editor_error = RwSignal::new(String::new());
    let editor_dirty = RwSignal::new(false);
    let editor_sequence = RwSignal::new(0_u64);
    {
        let editor_name = editor_name;
        let editor_original_name = editor_original_name;
        let editor_content = editor_content;
        let editor_original = editor_original;
        let editor_dirty = editor_dirty;
        Effect::new(move |_| {
            let dirty = editor_name.get() != editor_original_name.get()
                || editor_content.get() != editor_original.get();
            if editor_dirty.get_untracked() != dirty {
                editor_dirty.set(dirty);
            }
        });
    }
    let transfer_open = RwSignal::new(false);
    let transfer_targets = RwSignal::new(Vec::<File>::new());
    let transfer_mode = RwSignal::new(TransferMode::Move);
    let transfer_target_id = RwSignal::new(ROOT_ID.to_owned());
    let transfer_busy = RwSignal::new(false);
    let transfer_error = RwSignal::new(String::new());
    let account_open = RwSignal::new(false);
    let nav_actions = RwSignal::new(Vec::<NavAction>::new());
    let history_suppressed = RwSignal::new(false);
    let initial_route_pending = RwSignal::new(false);
    let fallback_to_root = RwSignal::new(false);
    // A document save resolves only after the reference controller's folder
    // refresh resolves. The optional parent id lets that refresh deliver the
    // success toast (or its error) without making every folder load awaitable.
    let pending_editor_refresh = RwSignal::new(None::<String>);

    let load_folder_request = {
        let current_id = current_id;
        let current = current;
        let breadcrumbs = breadcrumbs;
        let items = items;
        let total_bytes = total_bytes;
        let file_count = file_count;
        let loading = loading;
        let error = error;
        let trash_mode = trash_mode;
        let request_sequence = request_sequence;
        let selected_ids = selected_ids;
        let notify = notify.clone();
        let preview_items = preview_items;
        let tree_token = tree_token;
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        let initial_route_pending = initial_route_pending;
        let fallback_to_root = fallback_to_root;
        let pending_editor_refresh = pending_editor_refresh;
        let editor_error = editor_error;
        let on_logout = on_logout.clone();
        Callback::new(move |request: FolderLoadRequest| {
            let FolderLoadRequest {
                id: requested_id,
                completion,
            } = request;
            let sequence = request_sequence.get_untracked().wrapping_add(1);
            request_sequence.set(sequence);
            loading.set(true);
            error.set(String::new());
            if pending_editor_refresh
                .get_untracked()
                .is_some_and(|expected_id| expected_id != requested_id)
            {
                pending_editor_refresh.set(None);
            }
            let suppress_history = history_suppressed.get_untracked();
            let initial_request = initial_route_pending.get_untracked();
            initial_route_pending.set(false);
            let logout = on_logout.clone();

            let mut completion = completion;
            leptos::task::spawn_local(async move {
                let result = async {
                    let detail = api::fetch_file(&requested_id).await?;
                    let children = api::fetch_children(&requested_id).await?;
                    Ok::<(FileDetail, Children), api::RequestError>((detail, children))
                }
                .await;

                if request_sequence.get_untracked() != sequence {
                    finish_folder_load(&mut completion);
                    return;
                }

                match result {
                    Ok((detail, children)) => {
                        let changed_folder = requested_id != current_id.get_untracked();
                        if !suppress_history && changed_folder {
                            nav_actions.update(|actions| {
                                actions.push(NavAction::Folder {
                                    id: current_id.get_untracked(),
                                });
                            });
                            push_browser_history();
                        }
                        current_id.set(requested_id.clone());
                        current.set(Some(detail.file));
                        breadcrumbs.set(detail.breadcrumbs);
                        preview_items.set(children.items.clone());
                        items.set(children.items);
                        total_bytes.set(children.total_bytes);
                        file_count.set(children.file_count);
                        selected_ids.set(HashSet::new());
                        trash_mode.set(false);
                        tree_token.update(|token| *token = token.wrapping_add(1));
                        replace_folder_url(&requested_id);
                        loading.set(false);
                        if pending_editor_refresh.get_untracked().as_deref()
                            == Some(requested_id.as_str())
                        {
                            pending_editor_refresh.set(None);
                            notify.run(Feedback::success("文档已保存"));
                        }
                    }
                    Err(request_error) if request_error.is_unauthorized() => {
                        loading.set(false);
                        logout.run(());
                    }
                    Err(request_error) => {
                        loading.set(false);
                        let editor_refresh_failed =
                            pending_editor_refresh.get_untracked().as_deref()
                                == Some(requested_id.as_str());
                        if editor_refresh_failed {
                            pending_editor_refresh.set(None);
                            editor_error.set(request_error.message);
                        } else if initial_request && requested_id != ROOT_ID {
                            // The reference startup route treats an invalid
                            // `/f/{id}` bookmark as a stale URL: it returns to
                            // the root and loads that folder without leaving a
                            // transient error screen behind. Ordinary in-app
                            // navigation still reports its error below.
                            replace_folder_url(ROOT_ID);
                            fallback_to_root.set(true);
                        } else {
                            notify.run(Feedback::error(request_error.message));
                        }
                    }
                }
                finish_folder_load(&mut completion);
            });
        })
    };

    let load_folder = {
        let load_folder_request = load_folder_request.clone();
        Callback::new(move |id: String| {
            load_folder_request.run(FolderLoadRequest {
                id,
                completion: None,
            });
        })
    };

    let fallback_loader = load_folder.clone();
    Effect::new(move |_| {
        if fallback_to_root.get() {
            fallback_to_root.set(false);
            fallback_loader.run(ROOT_ID.to_owned());
        }
    });

    let load_trash = {
        let current = current;
        let breadcrumbs = breadcrumbs;
        let items = items;
        let total_bytes = total_bytes;
        let file_count = file_count;
        let loading = loading;
        let error = error;
        let trash_mode = trash_mode;
        let request_sequence = request_sequence;
        let selected_ids = selected_ids;
        let notify = notify.clone();
        let section = section;
        let library_folder_id = library_folder_id;
        let preview_items = preview_items;
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            let sequence = request_sequence.get_untracked().wrapping_add(1);
            request_sequence.set(sequence);
            loading.set(true);
            error.set(String::new());
            let logout = on_logout.clone();

            leptos::task::spawn_local(async move {
                match api::fetch_trash().await {
                    Ok(trash) if request_sequence.get_untracked() == sequence => {
                        current.set(None);
                        breadcrumbs.set(Vec::new());
                        preview_items.set(trash.items.clone());
                        items.set(trash.items);
                        total_bytes.set(trash.total_bytes);
                        file_count.set(trash.file_count);
                        selected_ids.set(HashSet::new());
                        trash_mode.set(true);
                        section.set(LibraryKind::File);
                        library_folder_id.set(None);
                        loading.set(false);
                    }
                    Err(request_error)
                        if request_sequence.get_untracked() == sequence
                            && request_error.is_unauthorized() =>
                    {
                        loading.set(false);
                        logout.run(());
                    }
                    Err(request_error) if request_sequence.get_untracked() == sequence => {
                        loading.set(false);
                        notify.run(Feedback::error(request_error.message));
                    }
                    Ok(_) | Err(_) => {}
                }
            });
        })
    };

    let apply_library_view = {
        let library_items = library_items;
        let library_items_by_type = library_items_by_type;
        let library_folder_id = library_folder_id;
        let preview_items = preview_items;
        let library_filter_label = library_filter_label;
        Callback::new(move |kind: LibraryKind| {
            let selected_folder = library_folder_id.get_untracked();
            let all = library_items_by_type
                .get_untracked()
                .get(&kind)
                .cloned()
                .unwrap_or_default();
            library_filter_label.set(library::filter_label(&all, selected_folder.as_deref()));
            let filtered = library::filter_folder(&all, selected_folder.as_deref());
            preview_items.set(filtered.iter().map(|item| item.file.clone()).collect());
            library_items.set(filtered);
        })
    };

    let load_library = {
        let section = section;
        let library_items = library_items;
        let library_items_by_type = library_items_by_type;
        let library_counts = library_counts;
        let library_trees = library_trees;
        let library_loading = library_loading;
        let library_error = library_error;
        let library_folder_id = library_folder_id;
        let preview_items = preview_items;
        let library_filter_label = library_filter_label;
        let library_sequence = library_sequence;
        let library_loaded = library_loaded;
        let apply_library_view = apply_library_view.clone();
        let on_logout = on_logout.clone();
        Callback::new(move |(kind, force): (LibraryKind, bool)| {
            if kind == LibraryKind::File {
                return;
            }
            if library_loaded.get_untracked() && !force {
                apply_library_view.run(kind);
                return;
            }
            let sequence = library_sequence.get_untracked().wrapping_add(1);
            library_sequence.set(sequence);
            section.set(kind);
            library_loading.set(true);
            library_error.set(String::new());
            let logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                match api::fetch_library_all().await {
                    Ok(response) if library_sequence.get_untracked() == sequence => {
                        let mut by_type = std::collections::HashMap::new();
                        by_type.insert(LibraryKind::Book, response.items.book);
                        by_type.insert(LibraryKind::Image, response.items.image);
                        by_type.insert(LibraryKind::Video, response.items.video);
                        by_type.insert(LibraryKind::Audio, response.items.audio);
                        let mut trees = LibraryTrees::new();
                        for kind in [
                            LibraryKind::Book,
                            LibraryKind::Image,
                            LibraryKind::Video,
                            LibraryKind::Audio,
                        ] {
                            if let Some(items) = by_type.get(&kind)
                                && !items.is_empty()
                            {
                                trees.insert(kind, library::build_folder_tree(items));
                            }
                        }
                        library_counts.set(response.counts);
                        library_items_by_type.set(by_type.clone());
                        library_trees.set(trees);
                        library_loaded.set(true);
                        apply_library_view.run(kind);
                        library_loading.set(false);

                        // The reference fills in audio durations that the
                        // library projection does not yet know in the
                        // background. Keep its three-request bound and make
                        // each completed probe update the visible list.
                        let missing_audio = by_type
                            .get(&LibraryKind::Audio)
                            .into_iter()
                            .flat_map(|items| items.iter())
                            .filter(|item| item.duration_ms <= 0)
                            .map(|item| item.file.id.clone())
                            .collect::<Vec<_>>();
                        if !missing_audio.is_empty() {
                            let library_items_by_type = library_items_by_type;
                            let library_items = library_items;
                            let preview_items = preview_items;
                            let library_filter_label = library_filter_label;
                            let library_folder_id = library_folder_id;
                            let section = section;
                            let library_sequence = library_sequence;
                            leptos::task::spawn_local(async move {
                                let _ = stream::iter(missing_audio)
                                    .map(|id| async move {
                                        let duration = api::fetch_audio_media(&id)
                                            .await
                                            .ok()
                                            .filter(|media| {
                                                media.duration.is_finite() && media.duration > 0.0
                                            })
                                            .map(|media| (media.duration * 1000.0).round() as i64);
                                        (id, duration)
                                    })
                                    .buffer_unordered(3)
                                    .for_each(|(id, duration)| {
                                        let library_items_by_type = library_items_by_type;
                                        let library_items = library_items;
                                        let preview_items = preview_items;
                                        let library_filter_label = library_filter_label;
                                        let library_folder_id = library_folder_id;
                                        let section = section;
                                        let library_sequence = library_sequence;
                                        async move {
                                            let Some(duration) = duration else {
                                                return;
                                            };
                                            if library_sequence.get_untracked() != sequence {
                                                return;
                                            }
                                            library_items_by_type.update(|buckets| {
                                                if let Some(items) =
                                                    buckets.get_mut(&LibraryKind::Audio)
                                                    && let Some(item) = items
                                                        .iter_mut()
                                                        .find(|item| item.file.id == id)
                                                {
                                                    item.duration_ms = duration;
                                                }
                                            });
                                            if section.get_untracked() == LibraryKind::Audio {
                                                let all = library_items_by_type
                                                    .get_untracked()
                                                    .get(&LibraryKind::Audio)
                                                    .cloned()
                                                    .unwrap_or_default();
                                                let folder = library_folder_id.get_untracked();
                                                let filtered =
                                                    library::filter_folder(&all, folder.as_deref());
                                                library_filter_label.set(library::filter_label(
                                                    &all,
                                                    folder.as_deref(),
                                                ));
                                                preview_items.set(
                                                    filtered
                                                        .iter()
                                                        .map(|item| item.file.clone())
                                                        .collect(),
                                                );
                                                library_items.set(filtered);
                                            }
                                        }
                                    })
                                    .await;
                            });
                        }
                    }
                    Err(request_error)
                        if library_sequence.get_untracked() == sequence
                            && request_error.is_unauthorized() =>
                    {
                        library_loading.set(false);
                        logout.run(());
                    }
                    Err(request_error) if library_sequence.get_untracked() == sequence => {
                        library_loading.set(false);
                        library_error.set(request_error.message);
                    }
                    Ok(_) | Err(_) => {}
                }
            });
        })
    };

    let force_load_library = {
        let load_library = load_library.clone();
        Callback::new(move |kind: LibraryKind| {
            load_library.run((kind, true));
        })
    };

    let file_input = NodeRef::<leptos::html::Input>::new();
    let folder_input = NodeRef::<leptos::html::Input>::new();
    let upload_refresh = {
        let current_id = current_id;
        let trash_mode = trash_mode;
        let load_folder_request = load_folder_request.clone();
        let section = section;
        let force_load_library = force_load_library.clone();
        Callback::new(move |request: UploadRefresh| {
            let current_file_folder = !trash_mode.get_untracked()
                && section.get_untracked() == LibraryKind::File
                && current_id.get_untracked() == request.parent_id;
            if current_file_folder {
                load_folder_request.run(FolderLoadRequest {
                    id: request.parent_id,
                    completion: request.completion,
                });
                return;
            }
            if !trash_mode.get_untracked() && section.get_untracked() != LibraryKind::File {
                force_load_library.run(section.get_untracked());
            }
            request.finish();
        })
    };
    let upload_feedback = notify.clone();
    let uploads = UploadController::new(
        current_id,
        current,
        trash_mode,
        file_input,
        folder_input,
        upload_refresh,
        upload_feedback,
        on_logout.clone(),
    );

    let task_refresh = {
        let current_id = current_id;
        let trash_mode = trash_mode;
        let load_folder = load_folder.clone();
        Callback::new(move |(): ()| {
            if !trash_mode.get_untracked() {
                load_folder.run(current_id.get_untracked());
            }
        })
    };
    let mut task_center = TaskController::new(on_logout.clone(), task_refresh, upload_feedback);
    let uploads_for_task_cancel =
        leptos::__reexports::send_wrapper::SendWrapper::new(uploads.clone());
    let uploads_for_task_retry = uploads_for_task_cancel.clone();
    task_center.set_upload_actions(
        Callback::new(move |upload_id: String| {
            uploads_for_task_cancel.cancel_by_upload_id(upload_id);
        }),
        Callback::new(move |upload_id: String| {
            uploads_for_task_retry.retry_by_upload_id(upload_id)
        }),
    );
    let task_center_cleanup =
        leptos::__reexports::send_wrapper::SendWrapper::new(task_center.clone());
    on_cleanup(move || task_center_cleanup.dispose());
    let push_overlay = {
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        let media_file = media_file;
        let reader_file = reader_file;
        let editor_open = editor_open;
        let transfer_open = transfer_open;
        let share_file = share_file;
        let account_open = account_open;
        Callback::new(move |(): ()| {
            if history_suppressed.get_untracked() {
                return;
            }
            let already_open = media_file.get_untracked().is_some()
                || reader_file.get_untracked().is_some()
                || editor_open.get_untracked()
                || transfer_open.get_untracked()
                || share_file.get_untracked().is_some()
                || account_open.get_untracked();
            if !already_open {
                nav_actions.update(|actions| actions.push(NavAction::Overlay));
                push_browser_history();
            }
        })
    };

    let show_extract = {
        let archive_target = archive_target;
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        Callback::new(move |file: File| {
            if !classify::is_archive(&file) {
                return;
            }
            let name = file.name.clone();
            archive_target.set(Some(file));
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::ExtractArchive { name }));
        })
    };
    let show_share = {
        let share_file = share_file;
        let share_active = share_active;
        let share_url = share_url;
        let share_created_at = share_created_at;
        let share_busy = share_busy;
        let share_error = share_error;
        let share_copied = share_copied;
        let on_logout = on_logout.clone();
        let push_overlay = push_overlay.clone();
        Callback::new(move |file: File| {
            if file.kind != FileKind::File {
                return;
            }
            push_overlay.run(());
            let id = file.id.clone();
            share_file.set(Some(file));
            share_active.set(false);
            share_url.set(String::new());
            share_created_at.set(String::new());
            share_error.set(String::new());
            share_copied.set(false);
            share_busy.set(true);
            let on_logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                match api::fetch_share(&id).await {
                    Ok(status) => {
                        share_active.set(status.active);
                        share_url.set(status.url.unwrap_or_default());
                        share_created_at.set(
                            status
                                .created_at
                                .map(|value| value.to_rfc3339())
                                .unwrap_or_default(),
                        );
                    }
                    Err(error) if error.is_unauthorized() => on_logout.run(()),
                    Err(error) => share_error.set(error.message),
                }
                share_busy.set(false);
            });
        })
    };
    let create_share_request = {
        let share_file = share_file;
        let share_active = share_active;
        let share_url = share_url;
        let share_created_at = share_created_at;
        let share_busy = share_busy;
        let share_error = share_error;
        let share_copied = share_copied;
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let on_logout = on_logout.clone();
        Callback::new(move |replace: bool| {
            if replace {
                dialog_value.set(String::new());
                dialog_error.set(String::new());
                dialog.set(Some(DialogState::RegenerateShare));
                return;
            }
            let Some(file) = share_file.get_untracked() else {
                return;
            };
            share_busy.set(true);
            share_error.set(String::new());
            share_copied.set(false);
            let id = file.id;
            let on_logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                match api::create_share(&id).await {
                    Ok(status) => {
                        share_active.set(status.active);
                        share_url.set(status.url.unwrap_or_default());
                        share_created_at.set(
                            status
                                .created_at
                                .map(|value| value.to_rfc3339())
                                .unwrap_or_default(),
                        );
                    }
                    Err(error) if error.is_unauthorized() => on_logout.run(()),
                    Err(error) => share_error.set(error.message),
                }
                share_busy.set(false);
            });
        })
    };
    let revoke_share_request = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        Callback::new(move |(): ()| {
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::RevokeShare));
        })
    };
    let copy_share = {
        let share_url = share_url;
        let share_copied = share_copied;
        let share_error = share_error;
        let notify = notify.clone();
        Callback::new(move |(): ()| {
            let value = share_url.get_untracked();
            if value.is_empty() {
                return;
            }
            let Some(clipboard) = web_sys::window().map(|window| window.navigator().clipboard())
            else {
                share_error.set("复制失败，请手动选择链接复制".to_owned());
                return;
            };
            let promise = clipboard.write_text(&value);
            leptos::task::spawn_local(async move {
                if wasm_bindgen_futures::JsFuture::from(promise).await.is_ok() {
                    share_copied.set(true);
                    notify.run(Feedback::success("分享链接已复制"));
                } else {
                    share_error.set("复制失败，请手动选择链接复制".to_owned());
                }
            });
        })
    };
    let close_share = {
        let share_file = share_file;
        let share_error = share_error;
        let share_copied = share_copied;
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        Callback::new(move |(): ()| {
            if request_overlay_close(nav_actions, history_suppressed) {
                return;
            }
            share_file.set(None);
            share_error.set(String::new());
            share_copied.set(false);
        })
    };
    let uploads_for_cleanup = leptos::__reexports::send_wrapper::SendWrapper::new(uploads.clone());
    on_cleanup(move || uploads_for_cleanup.dispose());

    let clear_selection = {
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| selected_ids.set(HashSet::new()))
    };
    let clear_selection_from_blank = {
        let selected_ids = selected_ids;
        Callback::new(move |event: web_sys::MouseEvent| {
            if selected_ids.get_untracked().is_empty() {
                return;
            }
            let Some(target) = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            else {
                return;
            };
            if target
                .closest("button,a,input,textarea,select,[role=\"toolbar\"],.file-card,.file-row")
                .ok()
                .flatten()
                .is_some()
            {
                return;
            }
            selected_ids.set(HashSet::new());
        })
    };
    let toggle_selection = {
        let selected_ids = selected_ids;
        Callback::new(move |item: File| {
            selected_ids.update(|selected| {
                if !selected.insert(item.id.clone()) {
                    selected.remove(&item.id);
                }
            });
        })
    };
    let select_all = {
        let items = items;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            let entries = items.get_untracked();
            if selected_ids.get_untracked().len() == entries.len() {
                selected_ids.set(HashSet::new());
            } else {
                selected_ids.set(entries.into_iter().map(|item| item.id).collect());
            }
        })
    };

    let show_create_folder = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        Callback::new(move |(): ()| {
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::CreateFolder));
        })
    };
    let show_rename = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let items = items;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            let Some(item) = items
                .get_untracked()
                .into_iter()
                .find(|item| selected_ids.get_untracked().contains(&item.id))
            else {
                return;
            };
            dialog_value.set(item.name);
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::Rename { id: item.id }));
        })
    };
    let show_delete = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            if selected_ids.get_untracked().is_empty() {
                return;
            }
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::Delete));
        })
    };
    let restore_selected = {
        let selected_ids = selected_ids;
        let items = items;
        let load_trash = load_trash.clone();
        let notify = notify.clone();
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            let targets = items
                .get_untracked()
                .into_iter()
                .filter(|item| selected_ids.get_untracked().contains(&item.id))
                .collect::<Vec<_>>();
            if targets.is_empty() {
                return;
            }
            let logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                for item in targets {
                    match api::restore_trash(&item.id).await {
                        Ok(()) => {}
                        Err(error) if error.is_unauthorized() => {
                            logout.run(());
                            return;
                        }
                        Err(error) => {
                            notify
                                .run(Feedback::error(format!("{}：{}", item.name, error.message)));
                            return;
                        }
                    }
                }
                selected_ids.set(HashSet::new());
                load_trash.run(());
                notify.run(Feedback::success("所选项目已恢复"));
            });
        })
    };
    let show_purge = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            if selected_ids.get_untracked().is_empty() {
                return;
            }
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::Purge));
        })
    };
    let show_empty_trash = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let items = items;
        let trash_mode = trash_mode;
        Callback::new(move |(): ()| {
            if !trash_mode.get_untracked() || items.get_untracked().is_empty() {
                return;
            }
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::EmptyTrash));
        })
    };

    let submit_dialog = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_busy = dialog_busy;
        let dialog_error = dialog_error;
        let selected_ids = selected_ids;
        let items = items;
        let current_id = current_id;
        let trash_mode = trash_mode;
        let notify = notify.clone();
        let load_folder = load_folder.clone();
        let load_trash = load_trash.clone();
        let on_logout = on_logout.clone();
        let editor_open = editor_open;
        let archive_target = archive_target;
        let share_file = share_file;
        let share_active = share_active;
        let share_url = share_url;
        let share_created_at = share_created_at;
        let share_busy = share_busy;
        let share_error = share_error;
        let share_copied = share_copied;
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        let task_center = leptos::__reexports::send_wrapper::SendWrapper::new(task_center.clone());
        Callback::new(move |value: String| {
            let Some(state) = dialog.get_untracked() else {
                return;
            };
            if dialog_busy.get_untracked() {
                return;
            }
            dialog_busy.set(true);
            dialog_error.set(String::new());
            let discard_editor = matches!(&state, DialogState::DiscardEditor);
            let extract_archive = matches!(&state, DialogState::ExtractArchive { .. });
            let rename_action = matches!(&state, DialogState::Rename { .. });
            let share_action = matches!(
                &state,
                DialogState::RegenerateShare | DialogState::RevokeShare
            );
            let selected = selected_ids.get_untracked();
            let ids: Vec<String> = items
                .get_untracked()
                .into_iter()
                .filter(|item| selected.contains(&item.id))
                .map(|item| item.id)
                .collect();
            let parent_id = current_id.get_untracked();
            let refresh_parent_id = parent_id.clone();
            let in_trash = trash_mode.get_untracked();
            let refresh_folder = load_folder.clone();
            let refresh_trash = load_trash.clone();
            let logout = on_logout.clone();
            let task_center = task_center.clone();

            // The reference `confirmDialog`/`promptDialog` resolves and
            // removes AppDialog synchronously. The mutation continues after
            // the confirmation surface is gone; only RenameDialog stays
            // mounted while its save request is in flight.
            if !rename_action {
                dialog.set(None);
                dialog_value.set(String::new());
                dialog_error.set(String::new());
            }

            leptos::task::spawn_local(async move {
                let result: Result<String, api::RequestError> = async {
                    match state {
                        DialogState::CreateFolder => {
                            let name = value.trim().to_owned();
                            if name.is_empty() {
                                Err(api::RequestError {
                                    status: 0,
                                    code: None,
                                    message: "文件夹名称不能为空".to_owned(),
                                })
                            } else {
                                api::create_directory(&CreateDirectoryRequest { parent_id, name })
                                    .await
                                    .map(|_| "文件夹已创建".to_owned())
                            }
                        }
                        DialogState::DiscardEditor => Ok(String::new()),
                        DialogState::ExtractArchive { .. } => {
                            let Some(file) = archive_target.get_untracked() else {
                                return Err(api::RequestError {
                                    status: 0,
                                    code: None,
                                    message: "没有可解压的文件".to_owned(),
                                });
                            };
                            api::extract_archive(&file.id).await?;
                            Ok(format!("「{}」已加入解压队列", file.name))
                        }
                        DialogState::RegenerateShare => {
                            let Some(file) = share_file.get_untracked() else {
                                return Err(api::RequestError {
                                    status: 0,
                                    code: None,
                                    message: "分享文件已关闭".to_owned(),
                                });
                            };
                            share_busy.set(true);
                            let result = api::create_share(&file.id).await;
                            match result {
                                Ok(status) => {
                                    share_active.set(status.active);
                                    share_url.set(status.url.unwrap_or_default());
                                    share_created_at.set(
                                        status
                                            .created_at
                                            .map(|value| value.to_rfc3339())
                                            .unwrap_or_default(),
                                    );
                                    share_copied.set(false);
                                    share_error.set(String::new());
                                    Ok("分享链接已重新生成".to_owned())
                                }
                                Err(error) => Err(error),
                            }
                        }
                        DialogState::RevokeShare => {
                            let Some(file) = share_file.get_untracked() else {
                                return Err(api::RequestError {
                                    status: 0,
                                    code: None,
                                    message: "分享文件已关闭".to_owned(),
                                });
                            };
                            share_busy.set(true);
                            api::revoke_share(&file.id).await?;
                            share_active.set(false);
                            share_url.set(String::new());
                            share_created_at.set(String::new());
                            share_copied.set(false);
                            Ok("分享已停止".to_owned())
                        }
                        DialogState::Rename { id } => {
                            let name = value.trim().to_owned();
                            if name.is_empty() {
                                Err(api::RequestError {
                                    status: 0,
                                    code: None,
                                    message: "名称不能为空".to_owned(),
                                })
                            } else {
                                api::patch_file(
                                    &id,
                                    &PatchFileRequest {
                                        name: Some(name),
                                        parent_id: None,
                                    },
                                )
                                .await
                                .map(|_| "已重命名".to_owned())
                            }
                        }
                        DialogState::Delete => {
                            let count = ids.len();
                            for id in ids {
                                api::delete_file(&id).await?;
                            }
                            Ok(format!("已将 {count} 项移入回收站"))
                        }
                        DialogState::Purge => {
                            for id in ids {
                                api::purge_trash(&id).await?;
                            }
                            Ok("已永久删除所选项目".to_owned())
                        }
                        DialogState::EmptyTrash => {
                            api::empty_trash().await?;
                            Ok("回收站已清空".to_owned())
                        }
                    }
                }
                .await;

                dialog_busy.set(false);
                share_busy.set(false);
                match result {
                    Ok(message) => {
                        dialog.set(None);
                        dialog_value.set(String::new());
                        dialog_error.set(String::new());
                        archive_target.set(None);
                        if share_action {
                            // The share dialog remains open; only its link state
                            // changes after the confirmation is dismissed.
                        } else if discard_editor {
                            selected_ids.set(HashSet::new());
                            if !request_overlay_close(nav_actions, history_suppressed) {
                                editor_open.set(false);
                            }
                        } else if in_trash {
                            selected_ids.set(HashSet::new());
                            refresh_trash.run(());
                        } else {
                            selected_ids.set(HashSet::new());
                            refresh_folder.run(refresh_parent_id);
                        }
                        notify.run(Feedback::success(message));
                        if extract_archive {
                            task_center.refresh_now();
                        }
                    }
                    Err(request_error) if request_error.is_unauthorized() => {
                        dialog.set(None);
                        logout.run(());
                    }
                    Err(request_error) => {
                        if rename_action {
                            notify.run(Feedback::error(request_error.message));
                        } else if share_action {
                            // The reference confirm helper closes its own
                            // confirmation before the share request runs.
                            // Share failures are then rendered by the still
                            // open share dialog, rather than keeping the
                            // confirmation dialog on screen.
                            dialog.set(None);
                            dialog_value.set(String::new());
                            dialog_error.set(String::new());
                            share_error.set(request_error.message);
                        } else {
                            // `confirmDialog`/`promptDialog` resolve and
                            // close before the asynchronous mutation. Keep
                            // that old interaction: failures are a toast,
                            // not an inline error that traps the user in the
                            // action dialog.
                            dialog.set(None);
                            dialog_value.set(String::new());
                            dialog_error.set(String::new());
                            notify.run(Feedback::error(request_error.message));
                        }
                    }
                }
            });
        })
    };
    let close_dialog = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let dialog_busy = dialog_busy;
        Callback::new(move |(): ()| {
            if !dialog_busy.get_untracked() {
                dialog.set(None);
                dialog_value.set(String::new());
                dialog_error.set(String::new());
            }
        })
    };

    let start_transfer = {
        let transfer_open = transfer_open;
        let transfer_targets = transfer_targets;
        let transfer_mode = transfer_mode;
        let transfer_target_id = transfer_target_id;
        let transfer_error = transfer_error;
        let current_id = current_id;
        let media_file = media_file;
        let push_overlay = push_overlay.clone();
        Callback::new(move |request: TransferRequest| {
            if request.targets.is_empty() || trash_mode.get_untracked() {
                return;
            }
            push_overlay.run(());
            transfer_targets.set(request.targets);
            transfer_mode.set(request.mode);
            transfer_target_id.set(current_id.get_untracked());
            transfer_error.set(String::new());
            media_file.set(None);
            transfer_open.set(true);
        })
    };
    let show_move_selected = {
        let start_transfer = start_transfer.clone();
        let items = items;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            let targets = items
                .get_untracked()
                .into_iter()
                .filter(|item| selected_ids.get_untracked().contains(&item.id))
                .collect();
            start_transfer.run(TransferRequest {
                mode: TransferMode::Move,
                targets,
            });
        })
    };
    let close_transfer = {
        let transfer_open = transfer_open;
        let transfer_targets = transfer_targets;
        let transfer_error = transfer_error;
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        Callback::new(move |(): ()| {
            if !transfer_busy.get_untracked() {
                if request_overlay_close(nav_actions, history_suppressed) {
                    return;
                }
                transfer_open.set(false);
                transfer_targets.set(Vec::new());
                transfer_error.set(String::new());
            }
        })
    };
    let transfer_unauthorized = {
        let transfer_open = transfer_open;
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            transfer_open.set(false);
            on_logout.run(());
        })
    };
    let submit_transfer = {
        let transfer_open = transfer_open;
        let transfer_targets = transfer_targets;
        let transfer_mode = transfer_mode;
        let transfer_busy = transfer_busy;
        let transfer_error = transfer_error;
        let selected_ids = selected_ids;
        let current_id = current_id;
        let load_folder = load_folder.clone();
        let notify = notify.clone();
        let on_logout = on_logout.clone();
        Callback::new(move |parent_id: String| {
            if transfer_busy.get_untracked() {
                return;
            }
            let targets = transfer_targets.get_untracked();
            if targets.is_empty() || parent_id.is_empty() {
                return;
            }
            transfer_busy.set(true);
            transfer_error.set(String::new());
            let mode = transfer_mode.get_untracked();
            let refresh_id = current_id.get_untracked();
            let refresh = load_folder.clone();
            let logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                let mut completed = 0_usize;
                let mut first_error = None::<String>;
                let mut unauthorized = false;
                for item in targets {
                    let result = match mode {
                        TransferMode::Move => api::patch_file(
                            &item.id,
                            &PatchFileRequest {
                                name: None,
                                parent_id: Some(parent_id.clone()),
                            },
                        )
                        .await
                        .map(|_| ()),
                        TransferMode::Copy => api::copy_file(
                            &item.id,
                            &CopyFileRequest {
                                parent_id: parent_id.clone(),
                            },
                        )
                        .await
                        .map(|_| ()),
                    };
                    match result {
                        Ok(()) => completed += 1,
                        Err(request_error) if request_error.is_unauthorized() => {
                            unauthorized = true;
                            break;
                        }
                        Err(request_error) => {
                            first_error.get_or_insert_with(|| {
                                format!("{}：{}", item.name, request_error.message)
                            });
                        }
                    }
                }
                transfer_busy.set(false);
                if unauthorized {
                    transfer_open.set(false);
                    logout.run(());
                    return;
                }
                transfer_open.set(false);
                transfer_targets.set(Vec::new());
                selected_ids.set(HashSet::new());
                refresh.run(refresh_id);
                let verb = if mode == TransferMode::Copy {
                    "复制"
                } else {
                    "移动"
                };
                if let Some(message) = first_error {
                    notify.run(Feedback::error(format!(
                        "已{verb} {completed} 项，部分项目失败：{message}"
                    )));
                } else {
                    notify.run(Feedback::success(format!("已{verb} {completed} 项")));
                }
            });
        })
    };
    let move_media = {
        let start_transfer = start_transfer.clone();
        Callback::new(move |file: File| {
            start_transfer.run(TransferRequest {
                mode: TransferMode::Move,
                targets: vec![file],
            });
        })
    };
    let copy_media = {
        let start_transfer = start_transfer.clone();
        Callback::new(move |file: File| {
            start_transfer.run(TransferRequest {
                mode: TransferMode::Copy,
                targets: vec![file],
            });
        })
    };

    let open_editor = {
        let editor_open = editor_open;
        let editor_is_new = editor_is_new;
        let editor_readonly = editor_readonly;
        let editor_file_id = editor_file_id;
        let editor_name = editor_name;
        let editor_original_name = editor_original_name;
        let editor_content = editor_content;
        let editor_original = editor_original;
        let editor_etag = editor_etag;
        let editor_mode = editor_mode;
        let editor_busy = editor_busy;
        let editor_error = editor_error;
        let editor_dirty = editor_dirty;
        let editor_sequence = editor_sequence;
        let on_logout = on_logout.clone();
        let push_overlay = push_overlay.clone();
        Callback::new(move |file: File| {
            let readonly = file.deleted_at.is_some();
            push_overlay.run(());
            let sequence = editor_sequence.get_untracked().wrapping_add(1);
            editor_sequence.set(sequence);
            editor_open.set(true);
            editor_is_new.set(false);
            editor_readonly.set(readonly);
            editor_file_id.set(file.id.clone());
            editor_name.set(file.name.clone());
            editor_original_name.set(file.name.clone());
            editor_content.set(String::new());
            editor_original.set(String::new());
            editor_etag.set(file.etag.clone());
            editor_mode.set(if readonly && is_markdown_name(&file.name) {
                EditorMode::Preview
            } else {
                EditorMode::Edit
            });
            editor_error.set(String::new());
            editor_dirty.set(false);
            editor_busy.set(true);
            let id = file.id;
            let on_logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                match api::fetch_document(&id).await {
                    Ok(document) if editor_sequence.get_untracked() == sequence => {
                        editor_content.set(document.content.clone());
                        editor_original.set(document.content);
                        editor_etag.set(document.etag);
                        editor_dirty.set(false);
                        editor_busy.set(false);
                    }
                    Err(error)
                        if editor_sequence.get_untracked() == sequence
                            && error.is_unauthorized() =>
                    {
                        editor_busy.set(false);
                        editor_open.set(false);
                        on_logout.run(());
                    }
                    Err(error) if editor_sequence.get_untracked() == sequence => {
                        editor_busy.set(false);
                        editor_error.set(error.message);
                    }
                    Ok(_) | Err(_) => {}
                }
            });
        })
    };
    let new_document = {
        let editor_open = editor_open;
        let editor_is_new = editor_is_new;
        let editor_readonly = editor_readonly;
        let editor_file_id = editor_file_id;
        let editor_name = editor_name;
        let editor_original_name = editor_original_name;
        let editor_content = editor_content;
        let editor_original = editor_original;
        let editor_etag = editor_etag;
        let editor_mode = editor_mode;
        let editor_busy = editor_busy;
        let editor_error = editor_error;
        let editor_dirty = editor_dirty;
        let push_overlay = push_overlay.clone();
        Callback::new(move |(): ()| {
            push_overlay.run(());
            editor_open.set(true);
            editor_is_new.set(true);
            editor_readonly.set(false);
            editor_file_id.set(String::new());
            editor_name.set("未命名文档.md".to_owned());
            editor_original_name.set("未命名文档.md".to_owned());
            editor_content.set(String::new());
            editor_original.set(String::new());
            editor_etag.set(String::new());
            editor_mode.set(EditorMode::Edit);
            editor_busy.set(false);
            editor_error.set(String::new());
            // A new document has an enabled Save action, but it is not dirty
            // until its name or content differs from the initial values. This
            // is what lets the reference UI close an untouched blank document
            // without a discard confirmation.
            editor_dirty.set(false);
        })
    };
    let save_editor = {
        let editor_open = editor_open;
        let editor_is_new = editor_is_new;
        let editor_readonly = editor_readonly;
        let editor_file_id = editor_file_id;
        let editor_name = editor_name;
        let editor_original_name = editor_original_name;
        let editor_content = editor_content;
        let editor_original = editor_original;
        let editor_etag = editor_etag;
        let editor_busy = editor_busy;
        let editor_error = editor_error;
        let editor_dirty = editor_dirty;
        let current_id = current_id;
        let load_folder = load_folder.clone();
        let pending_editor_refresh = pending_editor_refresh;
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            if editor_readonly.get_untracked() || editor_busy.get_untracked() {
                return;
            }
            editor_error.set(String::new());
            let raw_name = editor_name.get_untracked();
            if raw_name.trim().is_empty() {
                editor_error.set("请输入文件名".to_owned());
                return;
            }
            // The reference validates the input before trimming it, then
            // trims only the name sent to the create endpoint. Preserve that
            // distinction: a trailing space after `.md` is rejected by the
            // client instead of silently changing the requested filename.
            if !classify::is_editable_name(&raw_name) {
                editor_error
                    .set("支持 Markdown、TXT、YAML、JSON、TOML、INI、CONF、LOG 和 CSV".to_owned());
                return;
            }
            let name = raw_name.trim().to_owned();
            let content = editor_content.get_untracked();
            if content.len() > revaro_core::limits::MAX_DOCUMENT_BYTES {
                editor_error.set("可编辑文档不能超过 1 MiB".to_owned());
                return;
            }
            editor_busy.set(true);
            let is_new = editor_is_new.get_untracked();
            let file_id = editor_file_id.get_untracked();
            let parent_id = current_id.get_untracked();
            let request = UpdateDocumentRequest {
                content: content.clone(),
                etag: editor_etag.get_untracked(),
            };
            let create_request = CreateDocumentRequest {
                parent_id: parent_id.clone(),
                name,
                content,
            };
            let refresh = load_folder.clone();
            let logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                let result = if is_new {
                    api::create_document(&create_request).await
                } else {
                    api::update_document(&file_id, &request).await
                };
                match result {
                    Ok(saved) => {
                        editor_is_new.set(false);
                        editor_file_id.set(saved.id);
                        editor_name.set(saved.name.clone());
                        editor_original_name.set(saved.name);
                        editor_etag.set(saved.etag);
                        editor_original.set(editor_content.get_untracked());
                        editor_dirty.set(false);
                        refresh.run(parent_id);
                        pending_editor_refresh.set(Some(current_id.get_untracked()));
                    }
                    Err(error) if error.is_unauthorized() => {
                        editor_open.set(false);
                        logout.run(());
                    }
                    Err(error) => editor_error.set(error.message),
                }
                editor_busy.set(false);
            });
        })
    };
    let close_editor = {
        let editor_open = editor_open;
        let editor_readonly = editor_readonly;
        let editor_dirty = editor_dirty;
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        Callback::new(move |(): ()| {
            if editor_dirty.get_untracked() && !editor_readonly.get_untracked() {
                dialog_value.set(String::new());
                dialog_error.set(String::new());
                dialog.set(Some(DialogState::DiscardEditor));
            } else if !request_overlay_close(nav_actions, history_suppressed) {
                editor_open.set(false);
            }
        })
    };

    let open_item = {
        let load_folder = load_folder.clone();
        let trash_mode = trash_mode;
        let media_file = media_file;
        let reader_file = reader_file;
        let preview_items = preview_items;
        let items = items;
        let open_editor = open_editor.clone();
        let push_overlay = push_overlay.clone();
        Callback::new(move |item: File| {
            if trash_mode.get_untracked() {
                if classify::is_book(&item) {
                    push_overlay.run(());
                    replace_reader_url(&item.id);
                    reader_file.set(Some(item));
                } else if item.kind == FileKind::File
                    && (classify::is_image(&item)
                        || classify::is_audio(&item)
                        || classify::is_video(&item))
                {
                    push_overlay.run(());
                    media_file.set(Some(item));
                } else if classify::is_editable(&item) {
                    open_editor.run(item);
                }
                return;
            }
            if item.kind == FileKind::Directory {
                load_folder.run(item.id);
            } else if classify::is_book(&item)
                && (!classify::is_editable(&item) || classify::is_epub_name(&item.name))
            {
                push_overlay.run(());
                replace_reader_url(&item.id);
                reader_file.set(Some(item));
            } else if classify::is_editable(&item) {
                open_editor.run(item);
            } else if classify::is_image(&item)
                || classify::is_audio(&item)
                || classify::is_video(&item)
            {
                preview_items.set(items.get_untracked());
                push_overlay.run(());
                media_file.set(Some(item));
            }
        })
    };

    let open_library_item = {
        let media_file = media_file;
        let reader_file = reader_file;
        let preview_items = preview_items;
        let library_items = library_items;
        let on_logout = on_logout.clone();
        let push_overlay = push_overlay.clone();
        Callback::new(move |item: File| {
            if classify::is_book(&item) {
                push_overlay.run(());
                replace_reader_url(&item.id);
                reader_file.set(Some(item));
            } else if classify::is_image(&item)
                || classify::is_audio(&item)
                || classify::is_video(&item)
            {
                preview_items.set(
                    library_items
                        .get_untracked()
                        .into_iter()
                        .map(|item| item.file)
                        .collect(),
                );
                push_overlay.run(());
                media_file.set(Some(item));
            } else {
                // Library buckets currently contain media only. Keep an
                // unauthorized response on the same session boundary as the
                // ordinary browser if a future server snapshot includes a
                // plain file.
                let item_id = item.id;
                leptos::task::spawn_local(async move {
                    match api::fetch_file(&item_id).await {
                        Ok(_) => {}
                        Err(error) if error.is_unauthorized() => on_logout.run(()),
                        Err(_) => {}
                    }
                });
            }
        })
    };

    let select_category = {
        let load_library = load_library.clone();
        let load_folder = load_folder.clone();
        let current_id = current_id;
        let section = section;
        let trash_mode = trash_mode;
        let library_folder_id = library_folder_id;
        let library_filter_label = library_filter_label;
        let sidebar_mobile_open = sidebar_mobile_open;
        let selected_ids = selected_ids;
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        Callback::new(move |kind: LibraryKind| {
            sidebar_mobile_open.set(false);
            library_folder_id.set(None);
            library_filter_label.set("全部位置".to_owned());
            selected_ids.set(HashSet::new());
            let previous = section.get_untracked();
            if previous == kind {
                if kind == LibraryKind::File && trash_mode.get_untracked() {
                    trash_mode.set(false);
                    // The reference keeps the last live folder in
                    // `current_id` while the trash is displayed, so leaving
                    // the trash through the file category returns there.
                    load_folder.run(current_id.get_untracked());
                }
                return;
            }
            if previous != kind && !history_suppressed.get_untracked() {
                nav_actions.update(|actions| {
                    actions.push(NavAction::Section {
                        section: previous,
                        folder_id: current_id.get_untracked(),
                    });
                });
                push_browser_history();
            }
            if kind == LibraryKind::File {
                section.set(LibraryKind::File);
                trash_mode.set(false);
                load_folder.run(current_id.get_untracked());
            } else {
                section.set(kind);
                trash_mode.set(false);
                replace_library_url(kind, None);
                load_library.run((kind, false));
            }
        })
    };

    let select_library_folder = {
        let section = section;
        let library_folder_id = library_folder_id;
        let library_items = library_items;
        let library_items_by_type = library_items_by_type;
        let library_filter_label = library_filter_label;
        let preview_items = preview_items;
        Callback::new(move |folder: Option<String>| {
            if section.get_untracked() == LibraryKind::File {
                return;
            }
            library_folder_id.set(folder.clone());
            let kind = section.get_untracked();
            let all = library_items_by_type
                .get_untracked()
                .get(&kind)
                .cloned()
                .unwrap_or_default();
            library_filter_label.set(library::filter_label(&all, folder.as_deref()));
            let filtered = library::filter_folder(&all, folder.as_deref());
            preview_items.set(filtered.iter().map(|item| item.file.clone()).collect());
            library_items.set(filtered);
        })
    };

    let navigate_directory = {
        let section = section;
        let sidebar_mobile_open = sidebar_mobile_open;
        let load_folder = load_folder.clone();
        Callback::new(move |id: String| {
            section.set(LibraryKind::File);
            sidebar_mobile_open.set(false);
            load_folder.run(id);
        })
    };

    let toggle_sidebar = {
        let sidebar_collapsed = sidebar_collapsed;
        Callback::new(move |(): ()| {
            sidebar_collapsed.update(|collapsed| *collapsed = !*collapsed);
            crate::browser::local_storage_set(
                "revaro:sidebar:collapsed",
                if sidebar_collapsed.get_untracked() {
                    "1"
                } else {
                    "0"
                },
            );
        })
    };
    let toggle_mobile_sidebar = Callback::new(move |(): ()| {
        sidebar_mobile_open.update(|open| *open = !*open);
    });

    let open_account = {
        let account_open = account_open;
        let push_overlay = push_overlay.clone();
        Callback::new(move |(): ()| {
            push_overlay.run(());
            account_open.set(true);
        })
    };
    let pathname = web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .unwrap_or_default();
    let initial_route = library_route(&pathname);
    history_suppressed.set(true);
    if let Some((kind, folder)) = initial_route
        && kind != LibraryKind::File
    {
        library_folder_id.set(folder);
        section.set(kind);
        load_library.run((kind, false));
    } else {
        let initial_folder = folder_id(&pathname, ROOT_ID);
        initial_route_pending.set(initial_folder != ROOT_ID && pathname.starts_with("/f/"));
        if initial_folder == ROOT_ID && pathname != "/" {
            replace_folder_url(&initial_folder);
        }
        load_folder.run(initial_folder);
    }
    history_suppressed.set(false);
    let username = RwSignal::new(session.username.clone());
    let has_avatar = RwSignal::new(session.has_avatar);
    let avatar_version = RwSignal::new(0_u64);
    let return_home = {
        let load_folder = load_folder.clone();
        let section = section;
        let trash_mode = trash_mode;
        let library_folder_id = library_folder_id;
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        Callback::new(move |(): ()| {
            if section.get_untracked() != LibraryKind::File && !history_suppressed.get_untracked() {
                nav_actions.update(|actions| {
                    actions.push(NavAction::Section {
                        section: section.get_untracked(),
                        folder_id: current_id.get_untracked(),
                    });
                });
                push_browser_history();
            }
            section.set(LibraryKind::File);
            trash_mode.set(false);
            library_folder_id.set(None);
            load_folder.run(ROOT_ID.to_owned());
        })
    };

    let uploads_for_view = leptos::__reexports::send_wrapper::SendWrapper::new(uploads.clone());
    let shell_upload = uploads_for_view.clone();
    let shell_upload_leave = uploads_for_view.clone();
    let shell_upload_drop = uploads_for_view.clone();
    let upload_surface = uploads_for_view.clone();
    let library_upload = uploads_for_view.clone();
    let upload_library = Callback::new(move |(): ()| library_upload.choose_files());
    let file_upload = uploads_for_view.clone();
    let upload_files = Callback::new(move |(): ()| file_upload.choose_files());
    let folder_upload = uploads_for_view.clone();
    let upload_folder = Callback::new(move |(): ()| folder_upload.choose_folder());
    let refresh_library = {
        let force_load_library = force_load_library.clone();
        let section = section;
        Callback::new(move |(): ()| {
            if let kind @ (LibraryKind::Book
            | LibraryKind::Image
            | LibraryKind::Video
            | LibraryKind::Audio) = section.get_untracked()
            {
                force_load_library.run(kind);
            }
        })
    };
    let close_media = {
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        Callback::new(move |(): ()| {
            if !request_overlay_close(nav_actions, history_suppressed) {
                media_file.set(None);
            }
        })
    };
    let download_media = Callback::new(move |file: File| download_file(&file));
    let close_reader = {
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        Callback::new(move |(): ()| {
            if !request_overlay_close(nav_actions, history_suppressed) {
                reader_file.set(None);
            }
        })
    };
    let close_account = {
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        Callback::new(move |(): ()| {
            if !request_overlay_close(nav_actions, history_suppressed) {
                account_open.set(false);
            }
        })
    };
    let reader_unauthorized = {
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            reader_file.set(None);
            on_logout.run(());
        })
    };

    let mut popstate = {
        let nav_actions = nav_actions;
        let history_suppressed = history_suppressed;
        let media_file = media_file;
        let reader_file = reader_file;
        let editor_open = editor_open;
        let transfer_open = transfer_open;
        let transfer_targets = transfer_targets;
        let transfer_error = transfer_error;
        let share_file = share_file;
        let share_error = share_error;
        let share_copied = share_copied;
        let account_open = account_open;
        let dialog = dialog;
        let section = section;
        let trash_mode = trash_mode;
        let library_folder_id = library_folder_id;
        let library_filter_label = library_filter_label;
        let current_id = current_id;
        let load_folder = load_folder.clone();
        let load_library = load_library.clone();
        browser::on_popstate(move |_| {
            let mut actions = nav_actions.get_untracked();
            let Some(action) = actions.pop() else {
                return;
            };
            nav_actions.set(actions);
            history_suppressed.set(true);
            match action {
                NavAction::Overlay => {
                    media_file.set(None);
                    reader_file.set(None);
                    editor_open.set(false);
                    transfer_open.set(false);
                    transfer_targets.set(Vec::new());
                    transfer_error.set(String::new());
                    share_file.set(None);
                    share_error.set(String::new());
                    share_copied.set(false);
                    account_open.set(false);
                    dialog.set(None);
                    if section.get_untracked() == LibraryKind::File {
                        replace_folder_url(&current_id.get_untracked());
                    } else {
                        replace_library_url(
                            section.get_untracked(),
                            library_folder_id.get_untracked().as_deref(),
                        );
                    }
                }
                NavAction::Folder { id } => {
                    load_folder.run(id);
                }
                NavAction::Section {
                    section: previous,
                    folder_id,
                } => {
                    section.set(previous);
                    trash_mode.set(false);
                    library_folder_id.set(None);
                    library_filter_label.set("全部位置".to_owned());
                    if previous == LibraryKind::File {
                        load_folder.run(folder_id);
                    } else {
                        replace_library_url(previous, None);
                        load_library.run((previous, false));
                    }
                }
            }
            history_suppressed.set(false);
        })
    };
    on_cleanup(move || popstate.release());

    view! {
        <div
            class="app-shell"
            class:sidebar-collapsed=move || sidebar_collapsed.get()
            class:library-mode=move || section.get() != LibraryKind::File
            on:dragover=move |event: web_sys::DragEvent| shell_upload.on_drag_over(event)
            on:dragleave=move |event: web_sys::DragEvent| shell_upload_leave.on_drag_leave(event)
            on:drop=move |event: web_sys::DragEvent| shell_upload_drop.on_drop(event)
        >
            <AppTopbar
                username=username
                has_avatar=has_avatar
                avatar_version=avatar_version
                task_controller=leptos::__reexports::send_wrapper::SendWrapper::new(task_center)
                on_home=return_home.clone()
                on_trash=load_trash.clone()
                on_account=open_account.clone()
            />

            <AppSidebar
                section=section
                collapsed=sidebar_collapsed
                mobile_open=sidebar_mobile_open
                counts=library_counts
                trees=library_trees
                active_folder_id=library_folder_id
                current_id=current_id
                reload_token=tree_token
                on_select_category=select_category.clone()
                on_select_folder=select_library_folder.clone()
                on_navigate_directory=navigate_directory.clone()
                on_toggle_collapse=toggle_sidebar.clone()
                on_toggle_mobile=toggle_mobile_sidebar.clone()
                on_open_trash=load_trash.clone()
            />

            <section
                class="content"
                on:click=move |event: web_sys::MouseEvent| clear_selection_from_blank.run(event)
            >
                <Show
                    when=move || section.get() == LibraryKind::File
                    fallback=move || {
                        let kind = section.get();
                        view! {
                            <LibraryView
                                kind=kind
                                items=library_items
                                loading=library_loading
                                error=library_error
                                filter_label_signal=library_filter_label
                                on_open=open_library_item.clone()
                                on_refresh=refresh_library.clone()
                                on_upload=upload_library.clone()
                            />
                        }
                        .into_any()
                    }
                >
                <FileBrowserHeader
                    breadcrumbs=breadcrumbs
                    current=current
                    item_count=items
                    total_bytes=total_bytes
                    file_count=file_count
                    trash_mode=trash_mode
                    view_mode=view_mode
                    on_open_folder=load_folder.clone()
                    on_new_document=new_document.clone()
                    on_create_folder=show_create_folder.clone()
                    on_upload_files=upload_files.clone()
                    on_upload_folder=upload_folder.clone()
                    on_leave_trash=return_home.clone()
                    on_empty_trash=show_empty_trash.clone()
                />

                <Show
                    when=move || {
                        !selected_ids.get().is_empty()
                            && media_file.get().is_none()
                            && reader_file.get().is_none()
                            && !editor_open.get()
                            && !transfer_open.get()
                            && share_file.get().is_none()
                            && !account_open.get()
                    }
                    fallback=|| ()
                >
                    <SelectionToolbar
                        items=items
                        selected_ids=selected_ids
                        trash_mode=trash_mode
                        on_clear=clear_selection.clone()
                        on_select_all=select_all.clone()
                        on_rename=show_rename.clone()
                        on_move=show_move_selected.clone()
                        on_delete=show_delete.clone()
                        on_restore=restore_selected.clone()
                        on_purge=show_purge.clone()
                        on_open=open_item.clone()
                        on_extract=show_extract.clone()
                        on_download=Callback::new({
                            let items = items;
                            let selected_ids = selected_ids;
                            let notify = notify.clone();
                            let on_logout = on_logout.clone();
                            move |(): ()| {
                                let files: Vec<File> = items
                                    .get_untracked()
                                    .into_iter()
                                    .filter(|item| {
                                        selected_ids.get_untracked().contains(&item.id)
                                            && item.kind == FileKind::File
                                    })
                                    .collect();
                                if files.is_empty() {
                                    return;
                                }
                                if files.len() == 1 {
                                    download_file(&files[0]);
                                    return;
                                }
                                let ids = files.into_iter().map(|item| item.id).collect();
                                leptos::task::spawn_local(async move {
                                    notify.run(Feedback::success("正在准备批量下载…"));
                                    match api::prepare_batch_download(ids).await {
                                        Ok(ticket) => {
                                            start_download(&format!(
                                                "/api/files/batch-download/{}",
                                                js_sys::encode_uri_component(&ticket.token)
                                                    .as_string()
                                                    .unwrap_or_default()
                                            ));
                                            notify.run(Feedback::success("已开始下载"));
                                        }
                                        Err(error) if error.is_unauthorized() => on_logout.run(()),
                                        Err(error) => notify.run(Feedback::error(error.message)),
                                    }
                                });
                            }
                        })
                        on_share=show_share.clone()
                    />
                </Show>

                {move || {
                    if loading.get() {
                        view! {
                            <div class="state" aria-live="polite">
                                <div class="spinner"></div>
                                <p>"正在读取文件…"</p>
                            </div>
                        }
                        .into_any()
                    } else if !error.get().is_empty() {
                        let message = error.get();
                        let retry = if trash_mode.get() {
                            let load_trash = load_trash.clone();
                            Callback::new(move |(): ()| load_trash.run(()))
                        } else {
                            let load_folder = load_folder.clone();
                            let id = current_id.get_untracked();
                            Callback::new(move |(): ()| load_folder.run(id.clone()))
                        };
                        view! {
                            <div class="state" role="alert">
                                <div class="empty-icon" aria-hidden="true">"!"</div>
                                <h3>"读取失败"</h3>
                                <p>{message}</p>
                                <button class="secondary" type="button" on:click=move |_| retry.run(())>
                                    "重试"
                                </button>
                            </div>
                        }
                        .into_any()
                    } else if items.get().is_empty() {
                        let heading = if trash_mode.get() {
                            "回收站是空的"
                        } else {
                            "这里还是空的"
                        };
                        let description = if trash_mode.get() {
                            "删除的项目会先来到这里。"
                        } else {
                            "拖放文件到这里，或新建一篇文档。"
                        };
                        let new_document = new_document.clone();
                        let upload_files = upload_files.clone();
                        view! {
                            <div class="state empty">
                                <div class="empty-icon" aria-hidden="true">"⌁"</div>
                                <h3>{heading}</h3>
                                <p>{description}</p>
                                <Show when=move || !trash_mode.get() fallback=|| ()>
                                    <div class="empty-actions">
                                        <button class="secondary" type="button" on:click=move |_| new_document.run(())>
                                            "新建文档"
                                        </button>
                                        <button class="primary" type="button" on:click=move |_| upload_files.run(())>
                                            "上传文件"
                                        </button>
                                    </div>
                                </Show>
                            </div>
                        }
                        .into_any()
                    } else if view_mode.get() == ViewMode::Grid {
                        view! {
                            <div class="file-grid" class:selection-mode=move || !selected_ids.get().is_empty()>
                                <For each=move || items.get() key=|item| item.id.clone() let:item>
                                    <FileTile
                                        item=item
                                        trash_mode=trash_mode
                                        selectable=false
                                        selected_ids=selected_ids
                                        on_select=toggle_selection.clone()
                                        on_open=open_item.clone()
                                    />
                                </For>
                            </div>
                        }
                        .into_any()
                    } else {
                        view! {
                            <div class="file-rows">
                                <For each=move || items.get() key=|item| item.id.clone() let:item>
                                    <FileRow
                                        item=item
                                        trash_mode=trash_mode
                                        selected_ids=selected_ids
                                        on_select=toggle_selection.clone()
                                        on_open=open_item.clone()
                                    />
                                </For>
                            </div>
                        }
                        .into_any()
                    }
                }}
                </Show>
            </section>
            <Show when=move || feedback.get().is_some() fallback=|| ()>
                <div
                    class="toast"
                    class:success=move || feedback
                        .get()
                        .is_some_and(|value| value.kind == FeedbackKind::Success)
                    role="status"
                >
                    {move || feedback.get().map(|value| value.message).unwrap_or_default()}
                </div>
            </Show>
            {move || {
                if let Some(state) = dialog.get() {
                    if matches!(&state, DialogState::Rename { .. }) {
                        view! {
                            <RenameDialog
                                value=dialog_value
                                busy=dialog_busy
                                on_cancel=close_dialog.clone()
                                on_confirm=submit_dialog.clone()
                            />
                        }
                        .into_any()
                    } else {
                        let (title, message, confirm_label, danger, input, placeholder) =
                            dialog_config(&state, selected_ids.get().len(), items.get().len());
                        view! {
                            <ActionDialog
                                title=title
                                message=message
                                confirm_label=confirm_label
                                danger=danger
                                input=input
                                placeholder=placeholder
                                value=dialog_value
                                busy=dialog_busy
                                error=dialog_error
                                on_cancel=close_dialog.clone()
                                on_confirm=submit_dialog.clone()
                            />
                        }
                        .into_any()
                    }
                } else {
                    ().into_any()
                }
            }}
            <Show when=move || media_file.get().is_some() fallback=|| ()>
                {move || {
                    if media_file.get().is_some() {
                        view! {
                            <MediaPreview
                                selected=media_file
                                items=preview_items
                                on_close=close_media
                                on_download=download_media
                                on_move=move_media
                                on_copy=copy_media
                            />
                        }
                        .into_any()
                    } else {
                        ().into_any()
                    }
                }}
            </Show>
            <Show when=move || reader_file.get().is_some() fallback=|| ()>
                {move || {
                    reader_file.get().map_or_else(
                        || ().into_any(),
                        |file| {
                            view! {
                                <ReaderView
                                    file=file
                                    on_close=close_reader
                                    on_unauthorized=reader_unauthorized
                                />
                            }
                            .into_any()
                        },
                    )
                }}
            </Show>
            {move || {
                if transfer_open.get() {
                    view! {
                        <TransferDialog
                            mode=transfer_mode.get_untracked()
                            targets=transfer_targets.get_untracked()
                            target_id=transfer_target_id
                            busy=transfer_busy
                            error=transfer_error
                            on_cancel=close_transfer.clone()
                            on_confirm=submit_transfer.clone()
                            on_unauthorized=transfer_unauthorized.clone()
                        />
                    }
                    .into_any()
                } else {
                    ().into_any()
                }
            }}
            <Show when=move || editor_open.get() fallback=|| ()>
                <div
                    class="modal-backdrop editing"
                    role="presentation"
                    on:click=move |event: web_sys::MouseEvent| {
                        if event.target() == event.current_target() {
                            close_editor.run(());
                        }
                    }
                >
                    <DocumentEditor
                        is_new=editor_is_new
                        readonly=editor_readonly
                        name=editor_name
                        content=editor_content
                        busy=editor_busy
                        error=editor_error
                        dirty=editor_dirty
                        mode=editor_mode
                        on_save=save_editor.clone()
                        on_close=close_editor.clone()
                    />
                </div>
            </Show>
            <Show when=move || account_open.get() fallback=|| ()>
                <AccountSettings
                    open=account_open
                    username=username
                    has_avatar=has_avatar
                    avatar_version=avatar_version
                    on_logout=on_logout.clone()
                    on_username_changed=on_username_changed.clone()
                    on_password_changed=on_password_changed.clone()
                    on_notify=upload_feedback.clone()
                    on_close=close_account.clone()
                />
            </Show>
            <Show when=move || share_file.get().is_some() fallback=|| ()>
                {move || {
                    share_file.get().map_or_else(
                        || ().into_any(),
                        |file| {
                            view! {
                                <ShareDialog
                                    file=file
                                    active=share_active
                                    url=share_url
                                    created_at=share_created_at
                                    busy=share_busy
                                    error=share_error
                                    copied=share_copied
                                    on_close=close_share.clone()
                                    on_copy=copy_share.clone()
                                    on_revoke=revoke_share_request.clone()
                                    on_create=create_share_request.clone()
                                />
                            }
                            .into_any()
                        },
                    )
                }}
            </Show>
            <UploadSurface controller=upload_surface />
        </div>
    }
}

fn dialog_config(
    state: &DialogState,
    selected_count: usize,
    trash_count: usize,
) -> (String, String, String, bool, bool, Option<String>) {
    match state {
        DialogState::CreateFolder => (
            "新建文件夹".to_owned(),
            "给这个文件夹起个名字。".to_owned(),
            "创建".to_owned(),
            false,
            true,
            Some("文件夹名称".to_owned()),
        ),
        DialogState::DiscardEditor => (
            "放弃未保存的修改？".to_owned(),
            "关闭后，本次修改将无法恢复。".to_owned(),
            "放弃修改".to_owned(),
            true,
            false,
            None,
        ),
        DialogState::ExtractArchive { name } => (
            "在线解压".to_owned(),
            format!("将“{name}”解压到当前目录中的新文件夹。大压缩包会在后台继续处理。"),
            "开始解压".to_owned(),
            false,
            false,
            None,
        ),
        DialogState::RegenerateShare => (
            "重新生成分享链接？".to_owned(),
            "旧分享链接会立即失效，拿到旧链接的人将无法继续访问。".to_owned(),
            "重新生成".to_owned(),
            false,
            false,
            None,
        ),
        DialogState::RevokeShare => (
            "停止分享？".to_owned(),
            "现有公开链接会立即失效。文件本身不会被删除。".to_owned(),
            "停止分享".to_owned(),
            true,
            false,
            None,
        ),
        DialogState::Rename { .. } => (
            "重命名".to_owned(),
            String::new(),
            "保存".to_owned(),
            false,
            true,
            Some("新名称".to_owned()),
        ),
        DialogState::Delete => (
            "移入回收站？".to_owned(),
            format!("选中的 {selected_count} 项会移入回收站，文件夹中的内容也会一起保留。"),
            "移入回收站".to_owned(),
            true,
            false,
            None,
        ),
        DialogState::Purge => (
            format!("永久删除 {selected_count} 项？"),
            "这个操作无法撤销。无引用的数据块会在垃圾回收后清理。".to_owned(),
            "永久删除".to_owned(),
            true,
            false,
            None,
        ),
        DialogState::EmptyTrash => (
            "清空回收站？".to_owned(),
            format!("回收站中的 {trash_count} 项及其内容都会永久删除，无法恢复。"),
            "清空回收站".to_owned(),
            true,
            false,
            None,
        ),
    }
}

#[component]
fn FileTile(
    item: File,
    trash_mode: RwSignal<bool>,
    selectable: bool,
    selected_ids: RwSignal<HashSet<String>>,
    on_select: Callback<File>,
    on_open: Callback<File>,
) -> impl IntoView {
    let name = item.name.clone();
    let name_for_aria = name.clone();
    let item_for_click = item.clone();
    let item_for_key = item.clone();
    let item_for_select_click = item.clone();
    let item_for_select_key = item.clone();
    let item_for_meta = item.clone();
    let item_for_title = item.clone();
    let item_for_cannot_open = item.clone();
    let item_id_for_class = item.id.clone();
    let item_id_for_aria = item.id.clone();
    let item_id_for_title = item.id.clone();
    let item_id_for_label = item.id.clone();
    let item_id_for_pressed = item.id.clone();
    let item_id_for_active = item.id.clone();
    let preview_available = RwSignal::new(initial_preview_available(&item));
    let class_item = item.clone();
    let on_select_click = on_select.clone();
    let on_open_click = on_open.clone();
    let on_open_key = on_open;
    let select_control = if selectable {
        view! {
            <button
                class="card-select"
                type="button"
                title=move || if selected_ids.get().contains(&item_id_for_title) { "取消选择" } else { "选择项目" }
                aria-label=move || if selected_ids.get().contains(&item_id_for_label) { "取消选择" } else { "选择项目" }
                aria-pressed=move || if selected_ids.get().contains(&item_id_for_pressed) { "true" } else { "false" }
                class:active=move || selected_ids.get().contains(&item_id_for_active)
                on:click=move |event: web_sys::MouseEvent| {
                    event.stop_propagation();
                    on_select_click.run(item_for_select_click.clone());
                }
                on:keydown=move |event: web_sys::KeyboardEvent| event.stop_propagation()
            >
                <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="m5 12 4 4L19 6"></path>
                </svg>
            </button>
        }
        .into_any()
    } else {
        ().into_any()
    };

    view! {
        <article
            class=move || tile_class(&class_item, preview_available.get())
            class:selected=move || selected_ids.get().contains(&item_id_for_class)
            role="button"
            tabindex="0"
            aria-label=move || format!("{}，{}", name_for_aria, if selected_ids.get().contains(&item_id_for_aria) { "已选择" } else { "未选择" })
            on:click=move |_| {
                if !trash_mode.get_untracked() || item_for_click.kind == FileKind::File {
                    on_open_click.run(item_for_click.clone());
                }
            }
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Enter" {
                    event.prevent_default();
                    if !trash_mode.get_untracked() || item_for_key.kind == FileKind::File {
                        on_open_key.run(item_for_key.clone());
                    }
                } else if event.key() == " " {
                    // FileCard.vue uses Vue's `.prevent` modifier on the
                    // production grid even when the grid is not selectable;
                    // keep a focused card from scrolling the page on Space.
                    event.prevent_default();
                    if selectable {
                        on_select.run(item_for_select_key.clone());
                    }
                }
            }
            on:contextmenu=move |event: web_sys::MouseEvent| event.prevent_default()
        >
            {select_control}
            <div
                class="card-preview"
                class:cannot-open=move || {
                    trash_mode.get() && item_for_cannot_open.kind == FileKind::Directory
                }
                title=move || preview_title(&item_for_title, trash_mode.get())
            >
                {file_preview_with_state(&item, Some(preview_available))}
            </div>
            <div class="card-info">
                <strong title=name.clone()>{name.clone()}</strong>
                <small>{move || display_meta(&item_for_meta, trash_mode.get())}</small>
            </div>
        </article>
    }
}

#[component]
fn FileRow(
    item: File,
    trash_mode: RwSignal<bool>,
    selected_ids: RwSignal<HashSet<String>>,
    on_select: Callback<File>,
    on_open: Callback<File>,
) -> impl IntoView {
    let name = item.name.clone();
    let name_for_aria = name.clone();
    let item_for_click = item.clone();
    let item_for_key = item.clone();
    let item_for_select_click = item.clone();
    let item_for_select_key = item.clone();
    let item_for_meta = item.clone();
    let item_for_preview = item.clone();
    let item_id_for_class = item.id.clone();
    let item_id_for_aria = item.id.clone();
    let item_id_for_title = item.id.clone();
    let item_id_for_label = item.id.clone();
    let item_id_for_pressed = item.id.clone();
    let item_id_for_active = item.id.clone();
    let class = row_class(&item);
    let on_select_click = on_select.clone();
    let on_select_touch = on_select.clone();
    let on_open_click = on_open.clone();
    let on_open_key = on_open;
    view! {
        <article
            class=class
            class:selected=move || selected_ids.get().contains(&item_id_for_class)
            role="button"
            tabindex="0"
            aria-label=move || format!("{}，{}", name_for_aria, if selected_ids.get().contains(&item_id_for_aria) { "已选择" } else { "未选择" })
            on:click=move |_| {
                // The reference list uses a tap to toggle selection once a
                // touch selection mode is active; desktop clicks continue to
                // open the row. The selection button remains the explicit
                // control for mouse/keyboard users.
                if is_coarse_pointer() && !selected_ids.get_untracked().is_empty() {
                    on_select_touch.run(item_for_click.clone());
                } else if !trash_mode.get_untracked() || item_for_click.kind == FileKind::File {
                    on_open_click.run(item_for_click.clone());
                }
            }
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Enter" {
                    event.prevent_default();
                    if !trash_mode.get_untracked() || item_for_key.kind == FileKind::File {
                        on_open_key.run(item_for_key.clone());
                    }
                } else if event.key() == " " {
                    event.prevent_default();
                    on_select.run(item_for_select_key.clone());
                }
            }
            on:contextmenu=move |event: web_sys::MouseEvent| event.prevent_default()
        >
            <button
                class="row-select"
                type="button"
                title=move || if selected_ids.get().contains(&item_id_for_title) { "取消选择" } else { "选择项目" }
                aria-label=move || if selected_ids.get().contains(&item_id_for_label) { "取消选择" } else { "选择项目" }
                aria-pressed=move || if selected_ids.get().contains(&item_id_for_pressed) { "true" } else { "false" }
                class:active=move || selected_ids.get().contains(&item_id_for_active)
                on:click=move |event: web_sys::MouseEvent| {
                    event.stop_propagation();
                    on_select_click.run(item_for_select_click.clone());
                }
                on:keydown=move |event: web_sys::KeyboardEvent| event.stop_propagation()
            >
                <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="m5 12 4 4L19 6"></path>
                </svg>
            </button>
            <div
                class="row-preview"
                class:cannot-open=move || {
                    trash_mode.get() && item_for_preview.kind == FileKind::Directory
                }
            >
                {file_preview(&item)}
            </div>
            <div class="row-info">
                <strong title=name.clone()>{name.clone()}</strong>
                <small>{move || row_meta(&item_for_meta)}</small>
            </div>
        </article>
    }
}

fn tile_class(file: &File, preview_available: bool) -> String {
    let mut class = String::from("file-card");
    if file.kind == FileKind::Directory {
        class.push_str(" folder-tile");
    } else if classify::is_editable(file) {
        class.push_str(" document-tile");
    } else if classify::is_book(file) && !classify::is_editable(file) {
        class.push_str(" book-tile");
    } else if classify::is_audio(file) {
        class.push_str(" audio-tile");
    }
    if preview_available {
        class.push_str(" preview-tile");
    } else {
        class.push_str(" fallback-tile");
    }
    if file.status != FileStatus::Ready {
        class.push_str(" mutedrow");
    }
    class
}

fn initial_preview_available(file: &File) -> bool {
    classify::is_image(file)
        || (classify::is_audio(file) && file.has_cover)
        || classify::is_video(file)
        || classify::is_epub_name(&file.name)
}

fn row_class(file: &File) -> String {
    if file.status == FileStatus::Ready {
        "file-row selectable".to_owned()
    } else {
        "file-row selectable mutedrow".to_owned()
    }
}

fn display_meta(file: &File, trash_mode: bool) -> String {
    if file.kind == FileKind::Directory {
        if trash_mode {
            return format!(
                "文件夹 · 删除于 {}",
                format_date(&file.deleted_at.unwrap_or(file.updated_at).to_rfc3339())
            );
        }
        return "文件夹".to_owned();
    }
    let size = format_size(non_negative(file.size));
    if trash_mode {
        format!(
            "{size} · 删除于 {}",
            format_date(&file.deleted_at.unwrap_or(file.updated_at).to_rfc3339())
        )
    } else {
        // FileGrid.vue's card fallback only renders the formatted size. The
        // reference deliberately keeps pending/failed lifecycle details in
        // the muted visual state instead of adding status text to the card.
        size
    }
}

fn row_meta(file: &File) -> String {
    if file.kind == FileKind::Directory {
        "文件夹".to_owned()
    } else {
        format!(
            "{} · {}",
            format_size(non_negative(file.size)),
            format_date(&file.updated_at.to_rfc3339())
        )
    }
}

fn is_coarse_pointer() -> bool {
    web_sys::window()
        .and_then(|window| {
            window
                .match_media("(hover: none), (pointer: coarse)")
                .ok()
                .flatten()
        })
        .is_some_and(|query| query.matches())
}

fn preview_title(file: &File, trash_mode: bool) -> String {
    if trash_mode && file.kind == FileKind::Directory {
        "恢复后可打开文件夹".to_owned()
    } else if classify::is_book(file) {
        "阅读".to_owned()
    } else if trash_mode && classify::is_editable(file) {
        "只读查看".to_owned()
    } else if file.kind == FileKind::Directory {
        "打开文件夹".to_owned()
    } else if classify::is_editable(file) {
        "编辑文档".to_owned()
    } else if classify::is_image(file) {
        "预览图片".to_owned()
    } else if classify::is_video(file) {
        "播放视频".to_owned()
    } else if classify::is_audio(file) {
        "播放音频".to_owned()
    } else {
        "文件".to_owned()
    }
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn is_markdown_name(name: &str) -> bool {
    let extension = classify::extension(name);
    extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
}

pub(crate) fn file_preview(file: &File) -> AnyView {
    file_preview_with_state(file, None)
}

pub(crate) fn file_preview_with_state(
    file: &File,
    preview_available: Option<RwSignal<bool>>,
) -> AnyView {
    view! { <FilePreview file=file.clone() preview_available=preview_available /> }.into_any()
}

/// Card/row thumbnails follow the old two-step fallback policy: images try a
/// generated thumbnail and then the original preview, while EPUB covers and
/// audio covers fall back directly to their type icon when the thumbnail is
/// unavailable.
#[component]
fn FilePreview(file: File, preview_available: Option<RwSignal<bool>>) -> impl IntoView {
    let is_image = classify::is_image(&file);
    let is_audio_cover = classify::is_audio(&file) && file.has_cover;
    let is_epub = classify::is_epub_name(&file.name);
    let is_video = classify::is_video(&file);
    let thumbnail = thumbnail_url(&file);
    let preview = format!("/api/files/{}/preview", file.id);
    let preview_for_src = preview.clone();
    let thumbnail_for_src = thumbnail.clone();
    let fallback_to_preview = RwSignal::new(false);
    let broken = RwSignal::new(false);
    let preview_available_for_error = preview_available;
    let file_for_error = file.clone();
    let on_image_error = move |_| {
        if is_audio_cover || is_epub {
            broken.set(true);
            if let Some(preview_available) = preview_available_for_error {
                preview_available.set(false);
            }
        } else if fallback_to_preview.get_untracked() {
            broken.set(true);
            if let Some(preview_available) = preview_available_for_error {
                preview_available.set(false);
            }
        } else {
            fallback_to_preview.set(true);
        }
    };

    view! {
        {move || {
            if is_video {
                view! { <VideoThumbnail file=file.clone() /> }.into_any()
            } else if (is_image || is_audio_cover || is_epub) && !broken.get() {
                let file = file_for_error.clone();
                let preview = preview_for_src.clone();
                let thumbnail = thumbnail_for_src.clone();
                view! {
                    <img
                        class="ui-image"
                        src=move || {
                            if fallback_to_preview.get() {
                                preview.clone()
                            } else {
                                thumbnail.clone()
                            }
                        }
                        alt=file.name.clone()
                        loading="lazy"
                        draggable="false"
                        on:error=on_image_error
                    />
                }.into_any()
            } else {
                file_icon(&file)
            }
        }}
    }
}

#[component]
fn VideoThumbnail(file: File) -> impl IntoView {
    const RETRY_DELAYS: [i32; 5] = [800, 1_600, 3_200, 6_400, 12_800];
    let attempt = RwSignal::new(0_usize);
    let loaded = RwSignal::new(false);
    let failed = RwSignal::new(false);
    let timer = RwSignal::new(None::<i32>);
    let id = StoredValue::new(file.id.clone());
    let etag = StoredValue::new(file.etag.clone());
    let on_error = move |_| {
        let current = attempt.get_untracked();
        if current >= RETRY_DELAYS.len() {
            failed.set(true);
            return;
        }
        if let Some(window) = web_sys::window() {
            if let Some(old) = timer.get_untracked() {
                window.clear_timeout_with_handle(old);
            }
            let attempt = attempt;
            let callback = Closure::once_into_js(move || attempt.update(|value| *value += 1));
            if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                RETRY_DELAYS[current],
            ) {
                timer.set(Some(id));
            }
        }
    };
    on_cleanup(move || {
        if let Some(timer) = timer.get_untracked()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
    });

    view! {
        <div class="video-thumb">
            <span class="thumb-fallback" class:hidden=move || loaded.get() && !failed.get()>
                <span class="large-video" aria-hidden="true">
                    <svg viewBox="0 0 24 24"><path d="m9 7 8 5-8 5Z"></path></svg>
                </span>
            </span>
            <Show when=move || !failed.get() fallback=|| ()>
                <img
                    class="ui-image"
                    src=move || {
                        format!(
                            "/api/files/{}/thumbnail?v={}&retry={}",
                            id.get_value(),
                            etag.get_value(),
                            attempt.get()
                        )
                    }
                    alt=""
                    loading="lazy"
                    draggable="false"
                    on:load=move |_| loaded.set(true)
                    on:error=on_error
                />
            </Show>
        </div>
    }
}

fn thumbnail_url(file: &File) -> String {
    format!(
        "/api/files/{}/thumbnail?v={}",
        file.id,
        js_sys::encode_uri_component(&file.etag)
            .as_string()
            .unwrap_or_default()
    )
}

fn file_icon(file: &File) -> AnyView {
    if file.kind == FileKind::Directory {
        view! {
            <svg class="file-type-icon folder-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="folder-back" d="M10 23c0-4 3-7 7-7h21l10 11h31c4 0 7 3 7 7v9H10Z"></path>
                <path class="folder-front" d="M8 38c0-4 3-7 7-7h66c5 0 8 4 7 9l-7 35c-1 4-4 6-8 6H16c-4 0-7-3-7-7Z"></path>
                <path class="folder-highlight" d="M17 38h62l-1 6H16Z"></path>
            </svg>
        }
        .into_any()
    } else if classify::is_video(file) {
        view! {
            <span class="large-video" aria-hidden="true">
                <svg viewBox="0 0 24 24"><path d="m9 7 8 5-8 5Z"></path></svg>
            </span>
        }
        .into_any()
    } else if classify::is_audio(file) {
        view! {
            <svg class="file-type-icon audio-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="icon-base" d="M22 10h38l17 17v58H22Z"></path>
                <path class="icon-fold" d="M60 10v17h17Z"></path>
                <path class="icon-detail audio-note" d="M62 42v27m0-27-20 5v27"></path>
                <ellipse class="icon-accent" cx="35" cy="75" rx="9" ry="7"></ellipse>
                <ellipse class="icon-accent" cx="55" cy="70" rx="9" ry="7"></ellipse>
            </svg>
        }
        .into_any()
    } else if classify::is_book(file) && !classify::is_editable(file) {
        view! {
            <svg class="file-type-icon book-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="icon-base" d="M48 24c-9-6-20-8-34-8v57c14 0 25 2 34 8 9-6 20-8 34-8V16c-14 0-25 2-34 8Z"></path>
                <path class="icon-detail" d="M48 24v57M23 31c7 0 13 1 18 4M23 44c7 0 13 1 18 4M73 31c-7 0-13 1-18 4M73 44c-7 0-13 1-18 4"></path>
            </svg>
        }
        .into_any()
    } else if classify::is_editable(file) {
        view! {
            <svg class="file-type-icon document-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="icon-base" d="M22 10h38l17 17v58H22Z"></path>
                <path class="icon-fold" d="M60 10v17h17Z"></path>
                <path class="icon-detail" d="M34 45h31M34 57h31M34 69h22"></path>
            </svg>
        }
        .into_any()
    } else {
        view! {
            <svg class="file-type-icon generic-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="icon-base" d="M22 10h38l17 17v58H22Z"></path>
                <path class="icon-fold" d="M60 10v17h17Z"></path>
                <circle class="icon-accent" cx="49" cy="58" r="5"></circle>
            </svg>
        }
        .into_any()
    }
}

fn download_file(file: &File) {
    start_download_with_name(
        &format!("/api/files/{}/download", file.id),
        Some(&file.name),
    );
}

fn start_download(path: &str) {
    start_download_with_name(path, None);
}

fn start_download_with_name(path: &str, name: Option<&str>) {
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

fn push_browser_history() {
    if let Some(window) = web_sys::window()
        && let Ok(history) = window.history()
    {
        let _ = history.push_state_with_url(&JsValue::NULL, "", None);
    }
}

fn request_overlay_close(
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

fn replace_reader_url(id: &str) {
    if let Some(window) = web_sys::window()
        && let Ok(history) = window.history()
    {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&format!("/read/{id}")));
    }
}

fn replace_folder_url(id: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let url = folder_url(id, ROOT_ID);
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
    }
}

fn replace_library_url(kind: LibraryKind, folder_id: Option<&str>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let url = library_url(kind, folder_id);
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
    }
}
