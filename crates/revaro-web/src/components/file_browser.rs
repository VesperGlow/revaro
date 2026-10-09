//! The authenticated file browser and its basic lifecycle actions.
//!
//! This slice owns folder navigation, breadcrumbs, the folder grid, selection,
//! file and folder mutations, and the trash view. Media previews are mounted in
//! the same authenticated shell so their session and gallery state stay
//! attached to the listing.

mod actions;
mod downloads;
mod loading;
mod navigation;
mod tiles;

use actions::dialog_config;
pub(super) use downloads::download_file;
use downloads::start_download;
use loading::{ListingContext, ListingController};
use navigation::*;
use tiles::*;

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use futures_channel::oneshot;
use leptos::prelude::*;
use revaro_core::api::auth::Session;
use revaro_core::api::files::{
    CopyFileRequest, CreateDirectoryRequest, CreateDocumentRequest, FileDetail, PatchFileRequest,
    UpdateDocumentRequest,
};
use revaro_core::classify;
use revaro_core::ids::ROOT_ID;
use revaro_core::model::{File, FileKind, FileStatus};
use revaro_core::time::Timestamp;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

use super::resource_url::thumbnail_url;
use crate::api;
use crate::browser;
use crate::logic::feedback::{Feedback, FeedbackKind};
use crate::logic::format::{format_date, format_size};
use crate::logic::routing::{folder_id, folder_url, reader_id};

use super::account::AccountSettings;
use super::content_shell::{BookProgressBar, ShellContext};
use super::dialogs::{ActionDialog, DialogBackdrop, RenameDialog};
use super::editor::{DocumentEditor, EditorMode};
use super::file_browser_header::FileBrowserHeader;
use super::file_card::{FileCard, file_icon};
use super::media::MediaPreview;
use super::reader::ReaderView;
use super::selection::{SelectionCheckbox, SelectionMode};
use super::selection_toolbar::{BatchActionBar, SelectionActions};
use super::share::ShareDialog;
use super::topbar::TopbarActions;
use super::transfer::{TransferDialog, TransferMode};
use super::uploads::{UploadController, UploadRefresh, UploadSurface};
use crate::logic::library::LibraryPage;

#[derive(Debug, Clone, PartialEq, Eq)]
enum DialogState {
    CreateFolder,
    DiscardEditor,
    Rename { id: String },
    Delete,
    RegenerateShare,
    RevokeShare,
    Purge,
    EmptyTrash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NavAction {
    /// The folder to restore when the browser moves one level back.
    Folder { id: String },
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

struct TrashLoadRequest {
    completion: Option<oneshot::Sender<bool>>,
}

fn finish_folder_load(completion: &mut Option<oneshot::Sender<()>>) {
    if let Some(sender) = completion.take() {
        let _ = sender.send(());
    }
}

fn finish_trash_load(completion: &mut Option<oneshot::Sender<bool>>, success: bool) {
    if let Some(sender) = completion.take() {
        let _ = sender.send(success);
    }
}

/// The authenticated file browser.
#[component]
pub fn FileBrowser(
    session: Session,
    on_logout: Callback<()>,
    on_username_changed: Callback<String>,
    on_password_changed: Callback<String>,
    on_header_ready: Callback<TopbarActions>,
) -> impl IntoView {
    let shell_context = use_context::<ShellContext>();
    let current_id = RwSignal::new(ROOT_ID.to_owned());
    let current = RwSignal::new(None::<File>);
    let breadcrumbs = RwSignal::new(Vec::<File>::new());
    let items = RwSignal::new(Vec::<File>::new());
    let listing_empty = Memo::new(move |_| items.with(Vec::is_empty));
    let total_bytes = RwSignal::new(0_i64);
    let file_count = RwSignal::new(0_i64);
    let search_text = RwSignal::new(String::new());
    let search_query = RwSignal::new(String::new());
    let sort_order = RwSignal::new("name".to_owned());
    let listing_next_offset = RwSignal::new(0_i64);
    let loading_more = RwSignal::new(false);
    let loading_more_error = RwSignal::new(String::new());
    let listing_end = NodeRef::<leptos::html::Div>::new();
    let listing_end_visible = RwSignal::new(false);
    let listing_total = RwSignal::new(0_i64);
    let loading = RwSignal::new(false);
    // `current_id` changes only once navigation succeeds. Background refreshes
    // must also check the in-flight destination, or they can supersede a user
    // navigating away from the still-current folder (including into trash).
    let loading_folder = RwSignal::new(None::<String>);
    let error = RwSignal::new(String::new());
    let trash_mode = RwSignal::new(false);
    let request_sequence = RwSignal::new(0_u64);
    let selection = shell_context.map_or_else(SelectionMode::new, |c| c.selection);
    let selected_ids = selection.ids;
    let operation_items = Signal::derive(move || match shell_context {
        Some(context) if !context.page.get().is_file_workspace() => selection.library_items.get(),
        _ => items.get(),
    });
    let operation_trash_mode = Signal::derive(move || {
        trash_mode.get() && shell_context.is_none_or(|c| c.page.get().is_file_workspace())
    });
    let dialog = RwSignal::new(None::<DialogState>);
    let dialog_value = RwSignal::new(String::new());
    let dialog_busy = RwSignal::new(false);
    let dialog_error = RwSignal::new(String::new());
    let feedback = RwSignal::new(None::<Feedback>);
    let feedback_timer = RwSignal::new(None::<i32>);
    let notify = {
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
            let callback = Closure::once(move || {
                feedback_for_timer.set(None);
                timer_for_callback.set(None);
            })
            .into_js_value();
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
    let share_file = RwSignal::new(None::<File>);
    let share_sequence = RwSignal::new(0_u64);
    let share_active = RwSignal::new(false);
    let share_url = RwSignal::new(String::new());
    let share_created_at = RwSignal::new(String::new());
    let share_expires_at = RwSignal::new(String::new());
    let share_expiry = RwSignal::new("604800".to_owned());
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
    // This application has one administrator. File IDs stay stable when its
    // username changes; a display name must not strand an unsaved draft.
    let editor_draft_key = Signal::derive(move || {
        format!(
            "revaro-editor-draft:{}",
            if editor_is_new.get() {
                format!("new:{}", current_id.get())
            } else {
                editor_file_id.get()
            }
        )
    });
    let mut unload_guard = browser::on_window_capture("beforeunload", move |event| {
        if editor_open.get_untracked()
            && editor_dirty.get_untracked()
            && !editor_readonly.get_untracked()
        {
            event.prevent_default();
            let _ = js_sys::Reflect::set(
                event.as_ref(),
                &JsValue::from_str("returnValue"),
                &JsValue::from_str(""),
            );
        }
    });
    on_cleanup(move || unload_guard.release());
    {
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

    let ListingController {
        load_folder_request,
        load_folder,
        load_more,
        load_trash_request,
        load_trash,
    } = ListingController::new(ListingContext {
        current_id,
        search_text,
        search_query,
        sort_order,
        error,
        loading_more_error,
        editor_error,
        current,
        breadcrumbs,
        items,
        preview_items,
        total_bytes,
        file_count,
        listing_total,
        listing_next_offset,
        loading,
        loading_more,
        trash_mode,
        history_suppressed,
        initial_route_pending,
        fallback_to_root,
        loading_folder,
        pending_editor_refresh,
        request_sequence,
        selection,
        shell_context,
        nav_actions,
        notify,
        on_logout,
    });
    Effect::new(move |_| {
        let Some(end) = listing_end.get() else {
            return;
        };
        let callback = Closure::<dyn FnMut(js_sys::Array)>::new(move |entries: js_sys::Array| {
            if listing_end_visible.try_get_untracked().is_some()
                && let Ok(entry) = entries
                    .get(0)
                    .dyn_into::<web_sys::IntersectionObserverEntry>()
            {
                listing_end_visible.set(entry.is_intersecting());
            }
        });
        let options = web_sys::IntersectionObserverInit::new();
        options.set_root_margin("320px");
        if let Ok(observer) = web_sys::IntersectionObserver::new_with_options(
            callback.as_ref().unchecked_ref(),
            &options,
        ) {
            observer.observe(&end);
            let handle = leptos::__reexports::send_wrapper::SendWrapper::new((observer, callback));
            on_cleanup(move || {
                let (observer, _callback) = handle.take();
                observer.disconnect();
            });
        }
    });
    let auto_load_more = load_more.clone();
    Effect::new(move |_| {
        if listing_end_visible.get()
            && !loading.get()
            && !loading_more.get()
            && !trash_mode.get()
            && loading_more_error.get().is_empty()
            && listing_next_offset.get() < listing_total.get()
        {
            // Measure after the grid has rendered. The observer can still be
            // reporting the short loading placeholder when a refresh completes.
            if let Some(window) = web_sys::window() {
                let load_more = auto_load_more.clone();
                let callback = Closure::once(move || {
                    if request_sequence.try_get_untracked().is_none()
                        || !loading_more_error.get_untracked().is_empty()
                    {
                        return;
                    }
                    if let Some(end) = listing_end.get_untracked()
                        && let Some(height) = web_sys::window()
                            .and_then(|window| window.inner_height().ok())
                            .and_then(|height| height.as_f64())
                        && end.get_bounding_client_rect().top() <= height + 320.0
                    {
                        load_more.run(());
                    }
                })
                .into_js_value();
                let _ = window.request_animation_frame(callback.unchecked_ref());
            }
        }
    });
    let submit_search = {
        let load_folder = load_folder.clone();
        Callback::new(move |(): ()| {
            search_query.set(search_text.get_untracked().trim().to_owned());
            load_folder.run(current_id.get_untracked());
        })
    };
    let change_sort = {
        let load_folder = load_folder.clone();
        Callback::new(move |order: String| {
            sort_order.set(order);
            load_folder.run(current_id.get_untracked());
        })
    };

    let fallback_loader = load_folder.clone();
    Effect::new(move |_| {
        if fallback_to_root.get() {
            fallback_to_root.set(false);
            fallback_loader.run(ROOT_ID.to_owned());
        }
    });

    let file_input = NodeRef::<leptos::html::Input>::new();
    let folder_input = NodeRef::<leptos::html::Input>::new();
    let upload_refresh = {
        let load_folder_request = load_folder_request.clone();
        Callback::new(move |request: UploadRefresh| {
            let current_folder = !trash_mode.get_untracked()
                && current_id.get_untracked() == request.parent_id
                && (!loading.get_untracked()
                    || loading_folder.get_untracked().as_deref()
                        == Some(request.parent_id.as_str()));
            if current_folder {
                load_folder_request.run(FolderLoadRequest {
                    id: request.parent_id,
                    completion: request.completion,
                });
                return;
            }
            if let Some(context) = shell_context {
                context.refresh.update(|r| *r += 1);
            }
            request.finish();
        })
    };
    let upload_feedback = notify.clone();
    let upload_destination = Signal::derive(move || {
        if shell_context.is_some_and(|context| !context.page.get().is_file_workspace()) {
            ROOT_ID.to_owned()
        } else {
            current_id.get()
        }
    });
    let upload_trash = Signal::derive(move || {
        trash_mode.get()
            && shell_context.is_none_or(|context| context.page.get() == LibraryPage::Trash)
    });
    let uploads = UploadController::new(
        upload_destination,
        current,
        upload_trash,
        file_input,
        folder_input,
        upload_refresh,
        upload_feedback,
        on_logout.clone(),
    );

    let push_overlay = {
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

    let show_share = {
        let on_logout = on_logout.clone();
        let push_overlay = push_overlay.clone();
        Callback::new(move |file: File| {
            if file.kind != FileKind::File {
                return;
            }
            push_overlay.run(());
            let id = file.id.clone();
            // Reopening even the same file starts a distinct dialog session.
            let sequence = share_sequence.get_untracked().wrapping_add(1);
            share_sequence.set(sequence);
            share_file.set(Some(file));
            share_active.set(false);
            share_url.set(String::new());
            share_created_at.set(String::new());
            share_error.set(String::new());
            share_copied.set(false);
            share_busy.set(true);
            let on_logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                let result = api::fetch_share(&id).await;
                if share_sequence.try_get_untracked() != Some(sequence)
                    || share_file.try_with_untracked(Option::is_some) != Some(true)
                {
                    return;
                }
                match result {
                    Ok(status) => {
                        share_expires_at.set(
                            status
                                .expires_at
                                .map(|v| v.to_rfc3339())
                                .unwrap_or_default(),
                        );
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
            let sequence = share_sequence.get_untracked();
            let on_logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                let result = api::create_share(
                    &id,
                    share_expiry
                        .get_untracked()
                        .parse::<i64>()
                        .ok()
                        .filter(|v| *v > 0),
                )
                .await;
                if share_sequence.try_get_untracked() != Some(sequence)
                    || share_file.try_with_untracked(Option::is_some) != Some(true)
                {
                    return;
                }
                match result {
                    Ok(status) => {
                        share_expires_at.set(
                            status
                                .expires_at
                                .map(|v| v.to_rfc3339())
                                .unwrap_or_default(),
                        );
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
        Callback::new(move |(): ()| {
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::RevokeShare));
        })
    };
    let copy_share = {
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
                } else {
                    share_error.set("复制失败，请手动选择链接复制".to_owned());
                }
            });
        })
    };
    let close_share = {
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

    let clear_selection = Callback::new(move |(): ()| selection.exit());

    let select_all = {
        let items = operation_items;
        Callback::new(move |(): ()| {
            if !selection.enabled.get_untracked() {
                return;
            }
            let entries = items.get_untracked();
            let selected = selected_ids.get_untracked();
            let all_selected = entries.iter().all(|item| selected.contains(&item.id));
            if all_selected {
                selected_ids.set(HashSet::new());
            } else {
                selected_ids.set(entries.into_iter().map(|item| item.id).collect());
            }
        })
    };

    let show_create_folder = {
        Callback::new(move |(): ()| {
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::CreateFolder));
        })
    };
    let show_rename = {
        let items = operation_items;
        let push_overlay = push_overlay.clone();
        Callback::new(move |(): ()| {
            let Some(item) = items
                .get_untracked()
                .into_iter()
                .find(|item| selected_ids.get_untracked().contains(&item.id))
            else {
                return;
            };
            push_overlay.run(());
            dialog_value.set(item.name);
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::Rename { id: item.id }));
        })
    };
    let show_delete = {
        Callback::new(move |(): ()| {
            if selected_ids.get_untracked().is_empty() {
                return;
            }
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::Delete));
        })
    };
    let restore_selected = actions::TrashRestoreContext {
        selected_ids,
        items,
        load_trash_request,
        notify,
        on_logout,
    }
    .callback();
    let show_purge = {
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
        Callback::new(move |(): ()| {
            if !trash_mode.get_untracked() || items.get_untracked().is_empty() {
                return;
            }
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::EmptyTrash));
        })
    };

    let submit_dialog = actions::FileActionContext {
        dialog,
        dialog_value,
        dialog_busy,
        dialog_error,
        selected_ids,
        items: operation_items,
        current_id,
        trash_mode: operation_trash_mode,
        notify,
        load_trash_request,
        load_folder_request,
        on_logout,
        editor_open,
        editor_draft_key,
        share_file,
        share_sequence,
        share_active,
        share_url,
        share_created_at,
        share_expires_at,
        share_expiry,
        share_busy,
        share_error,
        share_copied,
        nav_actions,
        history_suppressed,
        upload_destination,
    }
    .submit();
    let close_dialog = {
        Callback::new(move |(): ()| {
            // The reference rename modal keeps its close button and backdrop
            // active while PATCH is pending. Generic confirmation dialogs are
            // removed before their mutation starts, so only rename needs this
            // exception to the busy guard.
            let rename_can_close =
                matches!(dialog.get_untracked(), Some(DialogState::Rename { .. }));
            if rename_can_close && request_overlay_close(nav_actions, history_suppressed) {
                return;
            }
            if !dialog_busy.get_untracked() || rename_can_close {
                dialog.set(None);
                dialog_value.set(String::new());
                dialog_error.set(String::new());
            }
        })
    };

    let start_transfer = {
        let push_overlay = push_overlay.clone();
        Callback::new(move |request: TransferRequest| {
            if request.targets.is_empty() || operation_trash_mode.get_untracked() {
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
        let items = operation_items;
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
    let show_copy_selected = {
        let start_transfer = start_transfer.clone();
        let items = operation_items;
        Callback::new(move |(): ()| {
            let targets = items
                .get_untracked()
                .into_iter()
                .filter(|item| selected_ids.get_untracked().contains(&item.id))
                .collect();
            start_transfer.run(TransferRequest {
                mode: TransferMode::Copy,
                targets,
            });
        })
    };
    let close_transfer = {
        Callback::new(move |(): ()| {
            if request_overlay_close(nav_actions, history_suppressed) {
                return;
            }
            transfer_open.set(false);
            transfer_targets.set(Vec::new());
            transfer_error.set(String::new());
        })
    };
    let transfer_unauthorized = {
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            transfer_open.set(false);
            on_logout.run(());
        })
    };
    let submit_transfer = {
        let load_folder_request = load_folder_request.clone();
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
            let logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                let mut completed = 0_usize;
                let mut failed = 0_usize;
                let mut first_error = None::<String>;
                let mut unauthorized = false;
                for item in targets {
                    let result = match mode {
                        TransferMode::Move => {
                            api::patch_file(
                                &item.id,
                                &PatchFileRequest {
                                    name: None,
                                    parent_id: Some(parent_id.clone()),
                                },
                            )
                            .await
                        }
                        TransferMode::Copy => {
                            api::copy_file(
                                &item.id,
                                &CopyFileRequest {
                                    parent_id: parent_id.clone(),
                                },
                            )
                            .await
                        }
                    };
                    match result {
                        Ok(()) => completed += 1,
                        Err(request_error) if request_error.is_unauthorized() => {
                            unauthorized = true;
                            break;
                        }
                        Err(request_error) => {
                            failed += 1;
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
                let (sender, receiver) = oneshot::channel();
                load_folder_request.run(FolderLoadRequest {
                    id: refresh_id,
                    completion: Some(sender),
                });
                let _ = receiver.await;
                let verb = if mode == TransferMode::Copy {
                    "复制"
                } else {
                    "移动"
                };
                if let Some(message) = first_error {
                    notify.run(Feedback::error(format!(
                        "已{verb} {completed} 项，{failed} 项失败：{message}"
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
                let result = api::fetch_document(&id).await;
                if editor_open.try_get_untracked() != Some(true) {
                    return;
                }
                match result {
                    Ok(document) if editor_sequence.try_get_untracked() == Some(sequence) => {
                        editor_content.set(document.content.clone());
                        editor_original.set(document.content);
                        editor_etag.set(document.etag);
                        editor_dirty.set(false);
                        editor_busy.set(false);
                    }
                    Err(error)
                        if editor_sequence.try_get_untracked() == Some(sequence)
                            && error.is_unauthorized() =>
                    {
                        editor_busy.set(false);
                        editor_open.set(false);
                        on_logout.run(());
                    }
                    Err(error) if editor_sequence.try_get_untracked() == Some(sequence) => {
                        editor_busy.set(false);
                        editor_error.set(error.message);
                    }
                    Ok(_) | Err(_) => {}
                }
            });
        })
    };
    let new_document = {
        let push_overlay = push_overlay.clone();
        Callback::new(move |(): ()| {
            push_overlay.run(());
            // A new draft is a new editor session too. A read from the
            // previously closed document must never replace its contents.
            editor_sequence.update(|sequence| *sequence = sequence.wrapping_add(1));
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
        let load_folder = load_folder.clone();
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
            let submitted_draft_key = editor_draft_key.get_untracked();
            let sequence = editor_sequence.get_untracked();
            let is_new = editor_is_new.get_untracked();
            let file_id = editor_file_id.get_untracked();
            let parent_id = upload_destination.get_untracked();
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
                if editor_sequence.try_get_untracked() != Some(sequence)
                    || editor_open.try_get_untracked() != Some(true)
                {
                    // The write may have succeeded after closing the editor.
                    // Refresh its directory if still visible, without changing
                    // any newer draft or navigating away from another folder.
                    if result.is_ok()
                        && current_id.try_get_untracked().as_deref() == Some(parent_id.as_str())
                    {
                        refresh.run(parent_id);
                    }
                    return;
                }
                match result {
                    Ok(saved) => {
                        if is_new {
                            super::editor_draft::remove(&submitted_draft_key);
                        }
                        editor_is_new.set(false);
                        editor_file_id.set(saved.id);
                        editor_name.set(saved.name.clone());
                        editor_original_name.set(saved.name);
                        editor_etag.set(saved.etag);
                        // Only the submitted snapshot was persisted. Typing
                        // while the request is in flight remains unsaved.
                        editor_dirty.set(editor_content.get_untracked() != request.content);
                        editor_original.set(request.content);
                        pending_editor_refresh.set(Some(parent_id.clone()));
                        refresh.run(parent_id);
                    }
                    Err(error) if error.is_unauthorized() => {
                        logout.run(());
                    }
                    Err(error) => editor_error.set(error.message),
                }
                editor_busy.set(false);
            });
        })
    };
    let close_editor = {
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
        let open_editor = open_editor.clone();
        let push_overlay = push_overlay.clone();
        Callback::new(move |item: File| {
            if let Some(context) = shell_context
                && !context.page.get_untracked().is_file_workspace()
            {
                context.open.run(item);
                return;
            }
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
            if classify::is_book(&item)
                && (!classify::is_editable(&item) || classify::is_epub_name(&item.name))
                && let Some(context) = shell_context
            {
                context.open.run(item);
                return;
            }
            if classify::is_audio(&item)
                && let Some(context) = shell_context
            {
                context.music.play(
                    item.clone(),
                    items
                        .get_untracked()
                        .into_iter()
                        .filter(classify::is_audio)
                        .collect(),
                );
                return;
            }
            if classify::is_video(&item)
                && let Some(context) = shell_context
            {
                context.music.pause();
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

    let open_account = {
        let push_overlay = push_overlay.clone();
        Callback::new(move |(): ()| {
            push_overlay.run(());
            account_open.set(true);
        })
    };
    let pathname = web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .unwrap_or_default();
    let initial_reader = shell_context
        .is_none()
        .then(|| reader_id(&pathname))
        .flatten()
        .and_then(|encoded| {
            js_sys::decode_uri_component(&encoded)
                .ok()
                .and_then(|value| value.as_string())
                .filter(|id| !id.contains('/'))
        });
    history_suppressed.set(true);
    let initial_folder = folder_id(&pathname, ROOT_ID);
    initial_route_pending.set(initial_folder != ROOT_ID && pathname.starts_with("/f/"));
    if initial_folder == ROOT_ID && pathname != "/files" {
        if shell_context.is_none_or(|c| c.page.get_untracked() == LibraryPage::Files) {
            replace_folder_url(&initial_folder);
        }
    }
    if let Some(reader_id) = initial_reader {
        let (sender, receiver) = oneshot::channel();
        load_folder_request.run(FolderLoadRequest {
            id: initial_folder,
            completion: Some(sender),
        });
        let push_overlay = push_overlay.clone();
        let logout = on_logout.clone();
        leptos::task::spawn_local(async move {
            let _ = receiver.await;
            match api::fetch_file(&reader_id).await {
                Ok(detail) if classify::is_book(&detail.file) => {
                    push_overlay.run(());
                    replace_reader_url(&detail.file.id);
                    reader_file.set(Some(detail.file));
                }
                Err(error) if error.is_unauthorized() => logout.run(()),
                _ => {}
            }
        });
    } else if shell_context.is_some_and(|c| c.page.get_untracked() == LibraryPage::Trash) {
        trash_mode.set(true);
        load_trash.run(());
    } else {
        load_folder.run(initial_folder);
    }
    history_suppressed.set(false);
    let username = RwSignal::new(session.username.clone());
    let has_avatar = RwSignal::new(session.has_avatar);
    let avatar_version = RwSignal::new(0_u64);
    let return_home = {
        let load_folder = load_folder.clone();
        Callback::new(move |(): ()| {
            trash_mode.set(false);
            load_folder.run(ROOT_ID.to_owned());
        })
    };

    let uploads_for_view = leptos::__reexports::send_wrapper::SendWrapper::new(uploads.clone());
    let shell_upload = uploads_for_view.clone();
    let shell_upload_leave = uploads_for_view.clone();
    let shell_upload_drop = uploads_for_view.clone();
    let upload_surface = uploads_for_view.clone();
    let file_upload = uploads_for_view.clone();
    let upload_files = Callback::new(move |(): ()| file_upload.choose_files());
    let folder_upload = uploads_for_view.clone();
    let upload_folder = Callback::new(move |(): ()| folder_upload.choose_folder());
    let close_media = {
        Callback::new(move |(): ()| {
            if !request_overlay_close(nav_actions, history_suppressed) {
                media_file.set(None);
            }
        })
    };
    let download_media = Callback::new(move |file: File| download_file(&file));
    let close_reader = {
        Callback::new(move |(): ()| {
            if !request_overlay_close(nav_actions, history_suppressed) {
                reader_file.set(None);
            }
        })
    };
    let close_account = {
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

    if let Some(context) = shell_context {
        let move_action = move_media.clone();
        let copy_action = copy_media.clone();
        Effect::new(move |_| {
            if let Some((file, copy)) = context.transfer.get() {
                context.transfer.set(None);
                if copy {
                    copy_action.run(file);
                } else {
                    move_action.run(file);
                }
            }
        });
    }

    let popstate_queue = Rc::new(RefCell::new(Vec::<NavAction>::new()));
    let popstate_processing = Rc::new(Cell::new(false));
    let mut popstate = {
        let load_folder_request = load_folder_request.clone();
        let popstate_queue = popstate_queue.clone();
        let popstate_processing = popstate_processing.clone();
        let load_trash = load_trash.clone();
        browser::on_popstate(move |_| {
            if let Some(context) = shell_context
                && !nav_actions
                    .get_untracked()
                    .last()
                    .is_some_and(|action| matches!(action, NavAction::Overlay))
            {
                if context.page.get_untracked() == LibraryPage::Trash {
                    load_trash.run(());
                } else if context.page.get_untracked() == LibraryPage::Files {
                    let path = web_sys::window()
                        .and_then(|w| w.location().pathname().ok())
                        .unwrap_or_default();
                    history_suppressed.set(true);
                    load_folder_request.run(FolderLoadRequest {
                        id: folder_id(&path, ROOT_ID),
                        completion: None,
                    });
                    history_suppressed.set(false);
                }
                return;
            }
            let mut actions = nav_actions.get_untracked();
            let Some(action) = actions.pop() else {
                return;
            };
            nav_actions.set(actions);
            popstate_queue.borrow_mut().push(action);
            if popstate_processing.replace(true) {
                return;
            }

            let popstate_queue = popstate_queue.clone();
            let popstate_processing = popstate_processing.clone();
            let load_folder_request = load_folder_request.clone();
            leptos::task::spawn_local(async move {
                loop {
                    let Some(action) = popstate_queue.borrow_mut().pop() else {
                        break;
                    };
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
                            if matches!(dialog.get_untracked(), Some(DialogState::Rename { .. })) {
                                dialog.set(None);
                                dialog_value.set(String::new());
                                dialog_error.set(String::new());
                            }
                            if shell_context
                                .is_none_or(|c| c.page.get_untracked() == LibraryPage::Files)
                            {
                                replace_folder_url(&current_id.get_untracked());
                            }
                        }
                        NavAction::Folder { id } => {
                            let (sender, receiver) = oneshot::channel();
                            load_folder_request.run(FolderLoadRequest {
                                id,
                                completion: Some(sender),
                            });
                            let _ = receiver.await;
                        }
                    }
                    history_suppressed.set(false);
                }
                popstate_processing.set(false);
            });
        })
    };
    on_cleanup(move || popstate.release());

    let header_files = return_home;
    let header_root = Callback::new(move |()| {
        history_suppressed.set(true);
        header_files.run(());
        history_suppressed.set(false);
    });
    on_header_ready.run(TopbarActions {
        username,
        has_avatar,
        avatar_version,
        upload_controller: leptos::__reexports::send_wrapper::SendWrapper::new(uploads.clone()),
        on_files: header_root,
        on_upload_files: upload_files,
        on_upload_folder: upload_folder,
        on_new_document: new_document,
        on_create_folder: show_create_folder,
        on_trash: load_trash,
        on_account: open_account,
        search_text,
        on_search: submit_search,
    });

    let selection_actions = SelectionActions {
        on_feedback: notify.clone(),
        items: operation_items,
        trash_mode: operation_trash_mode,
        file_tools: Signal::derive(move || {
            shell_context.is_none_or(|c| c.page.get() == LibraryPage::Files)
        }),
        visible: Signal::derive(move || {
            media_file.get().is_none()
                && reader_file.get().is_none()
                && !editor_open.get()
                && !transfer_open.get()
                && share_file.get().is_none()
                && !account_open.get()
                && dialog.get().is_none()
                && shell_context.is_none_or(|c| !c.selection_overlay.get())
        }),
        on_clear: clear_selection,
        on_select_all: select_all,
        on_rename: show_rename,
        on_move: show_move_selected,
        on_copy: show_copy_selected,
        on_delete: show_delete,
        on_restore: restore_selected,
        on_purge: show_purge,
        on_open: open_item,
        on_share: show_share,
        on_download: Callback::new({
            let items = operation_items;
            let notify = notify.clone();
            let on_logout = on_logout.clone();
            move |(): ()| {
                let files: Vec<File> = items
                    .get_untracked()
                    .into_iter()
                    .filter(|item| selected_ids.get_untracked().contains(&item.id))
                    .collect();
                if files.is_empty() {
                    return;
                }
                if files.len() == 1 && files[0].kind == FileKind::File {
                    download_file(&files[0]);
                    return;
                }
                let file_count = files.len();
                let ids = files.into_iter().map(|item| item.id).collect();
                leptos::task::spawn_local(async move {
                    notify.run(Feedback::success(format!(
                        "正在准备 {} 个文件…",
                        file_count
                    )));
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
        }),
    };
    selection.actions.set(Some(selection_actions.clone()));

    view! {
        <div
            class="app-shell"
            class:file-tools-background=move || shell_context.is_some_and(|c| !c.page.get().is_file_workspace())
            on:dragover=move |event: web_sys::DragEvent| shell_upload.on_drag_over(event)
            on:dragleave=move |event: web_sys::DragEvent| shell_upload_leave.on_drag_leave(event)
            on:drop=move |event: web_sys::DragEvent| shell_upload_drop.on_drop(event)
        >
            <section class="content" on:click=move |event| selection.exit_from_blank(event)>
                <FileBrowserHeader
                    breadcrumbs=breadcrumbs
                    current=current
                    item_count=items
                    listing_total=listing_total
                    sort_order=sort_order
                    on_sort=change_sort.clone()
                    total_bytes=total_bytes
                    file_count=file_count
                    trash_mode=trash_mode
                    on_open_folder=load_folder.clone()
                    on_empty_trash=show_empty_trash.clone()
                />

                <Show when=move ||shell_context.is_none_or(|context|context.page.get().is_file_workspace()) fallback=|| ()>
                    <BatchActionBar selection=selection actions=selection_actions.clone() />
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
                    } else if listing_empty.get() {
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
                        view! {
                            <div class="state empty">
                                <div class="empty-icon" aria-hidden="true">"⌁"</div>
                                <h3>{heading}</h3>
                                <p>{description}</p>

                            </div>
                        }
                        .into_any()
                    } else {
                        // Grid-only since the list view was removed. Global
                        // selection turns card clicks into toggles and exposes
                        // the shared batch toolbar after an item is selected.
                        view! {
                            <div class="file-grid" class:selection-mode=move || selection.enabled.get()>
                                <For each=move || items.get() key=|item| item.id.clone() let:item>
                                    <FileTile
                                        item=item
                                        trash_mode=trash_mode
                                        selectable=true
                                        selection=selection
                                        on_open=open_item.clone()
                                    />
                                </For>
                            </div>
                        }
                        .into_any()
                    }
                }}
                <div node_ref=listing_end class="listing-end" aria-live="polite">
                    <Show when=move || !trash_mode.get() && !loading.get() && !items.get().is_empty() fallback=|| ()>
                        <Show when=move || loading_more.get() fallback=|| ()>
                            <div class="spinner" aria-hidden="true"></div>
                            <span>"正在加载更多文件…"</span>
                        </Show>
                        <Show when=move || !loading_more_error.get().is_empty() fallback=|| ()>
                            <span role="alert">{move || loading_more_error.get()}</span>
                            <button class="secondary" type="button" on:click=move |_| load_more.run(())>"重试加载"</button>
                        </Show>
                        <Show when=move || !loading_more.get() && loading_more_error.get().is_empty() fallback=|| ()>
                            <span>{move || if listing_next_offset.get() >= listing_total.get() {
                                format!("已显示全部 {} 个项目", listing_total.get())
                            } else {
                                format!("已显示 {} / {} 个项目，向下滚动继续加载", items.get().len(), listing_total.get())
                            }}</span>
                        </Show>
                    </Show>
                </div>
            </section>
            <Show when=move || feedback.get().is_some() fallback=|| ()>
                <div
                    class="toast"
                    class:success=move || feedback
                        .get()
                        .is_some_and(|value| value.kind == FeedbackKind::Success)
                    class:error=move || feedback
                        .get()
                        .is_some_and(|value| value.kind == FeedbackKind::Error)
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
                <MediaPreview
                    selected=media_file
                    items=preview_items
                    on_close=close_media
                    on_download=download_media
                    on_move=move_media
                    on_copy=copy_media
                />
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
                <DialogBackdrop
                    class="modal-backdrop editing"
                    on_close=close_editor
                >
                    <DocumentEditor
                        file_id=editor_file_id
                        draft_key=editor_draft_key
                        etag=editor_etag
                        on_restored=Callback::new({let load=load_folder.clone();move |_|{editor_original.set(editor_content.get_untracked());editor_dirty.set(false);load.run(current_id.get_untracked());}})
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
                </DialogBackdrop>
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
                                    expires_at=share_expires_at
                                    expiry=share_expiry
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
