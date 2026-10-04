//! Directory and trash loading, pagination and stale-response rejection.

use super::*;

pub(super) struct ListingContext {
    pub current_id: RwSignal<String>,
    pub search_text: RwSignal<String>,
    pub search_query: RwSignal<String>,
    pub sort_order: RwSignal<String>,
    pub error: RwSignal<String>,
    pub loading_more_error: RwSignal<String>,
    pub editor_error: RwSignal<String>,
    pub current: RwSignal<Option<File>>,
    pub breadcrumbs: RwSignal<Vec<File>>,
    pub items: RwSignal<Vec<File>>,
    pub preview_items: RwSignal<Vec<File>>,
    pub total_bytes: RwSignal<i64>,
    pub file_count: RwSignal<i64>,
    pub listing_total: RwSignal<i64>,
    pub listing_next_offset: RwSignal<i64>,
    pub loading: RwSignal<bool>,
    pub loading_more: RwSignal<bool>,
    pub trash_mode: RwSignal<bool>,
    pub history_suppressed: RwSignal<bool>,
    pub initial_route_pending: RwSignal<bool>,
    pub fallback_to_root: RwSignal<bool>,
    pub loading_folder: RwSignal<Option<String>>,
    pub pending_editor_refresh: RwSignal<Option<String>>,
    pub request_sequence: RwSignal<u64>,
    pub selection: SelectionMode,
    pub shell_context: Option<ShellContext>,
    pub nav_actions: RwSignal<Vec<NavAction>>,
    pub notify: Callback<Feedback>,
    pub on_logout: Callback<()>,
}

pub(super) struct ListingController {
    pub load_folder_request: Callback<FolderLoadRequest>,
    pub load_folder: Callback<String>,
    pub load_more: Callback<()>,
    pub load_trash_request: Callback<TrashLoadRequest>,
    pub load_trash: Callback<()>,
}

impl ListingController {
    pub fn new(context: ListingContext) -> Self {
        let ListingContext {
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
        } = context;
        let load_folder_request = {
            let notify = notify.clone();
            let on_logout = on_logout.clone();
            Callback::new(move |request: FolderLoadRequest| {
                let FolderLoadRequest {
                    id: requested_id,
                    completion,
                } = request;
                let sequence = request_sequence.get_untracked().wrapping_add(1);
                request_sequence.set(sequence);
                loading_folder.set(Some(requested_id.clone()));
                loading.set(true);
                loading_more.set(false);
                loading_more_error.set(String::new());
                error.set(String::new());
                if pending_editor_refresh
                    .get_untracked()
                    .is_some_and(|expected_id| expected_id != requested_id)
                {
                    pending_editor_refresh.set(None);
                }
                let suppress_history = history_suppressed.get_untracked()
                    || shell_context
                        .is_some_and(|context| context.page.get_untracked() != LibraryPage::Files);
                let initial_request = initial_route_pending.get_untracked();
                initial_route_pending.set(false);
                let logout = on_logout.clone();

                if requested_id != current_id.get_untracked() {
                    search_query.set(String::new());
                    search_text.set(String::new());
                }
                let offset = 0;
                let query = search_query.get_untracked();
                let sort = sort_order.get_untracked();
                let mut completion = completion;
                leptos::task::spawn_local(async move {
                    let result = async {
                        // The reference `openFolder` starts metadata and children
                        // with `Promise.all`. Keep navigation latency and the
                        // observable request ordering equivalent instead of
                        // waiting for the detail response before asking for the
                        // listing.
                        let (detail, children) = futures_util::join!(
                            api::fetch_file(&requested_id),
                            api::fetch_listing(&requested_id, &query, &sort, offset),
                        );
                        let detail = detail?;
                        let children = children?;
                        Ok::<(FileDetail, revaro_core::features::Listing), api::RequestError>((
                            detail, children,
                        ))
                    }
                    .await;

                    if request_sequence.try_get_untracked() != Some(sequence) {
                        finish_folder_load(&mut completion);
                        return;
                    }
                    loading_folder.set(None);

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
                            listing_total.set(children.total);
                            listing_next_offset.set(children.items.len() as i64);
                            preview_items.set(children.items.clone());
                            items.set(children.items);
                            total_bytes.set(children.total_bytes);
                            file_count.set(children.file_count);
                            if shell_context
                                .is_none_or(|c| c.page.get_untracked().is_file_workspace())
                            {
                                selection.clear();
                            }
                            trash_mode.set(false);
                            if shell_context
                                .is_none_or(|c| c.page.get_untracked() == LibraryPage::Files)
                            {
                                replace_folder_url(&requested_id);
                            }
                            loading.set(false);
                            if let Some(context) = shell_context {
                                context.refresh.update(|r| *r += 1);
                            }
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
                                if shell_context
                                    .is_none_or(|c| c.page.get_untracked() == LibraryPage::Files)
                                {
                                    replace_folder_url(ROOT_ID);
                                }
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

        // Appending a batch never replaces the grid or clears its selection. A
        // navigation/refresh invalidates the request before it can append stale files.
        let load_more = {
            let on_logout = on_logout.clone();
            Callback::new(move |(): ()| {
                if loading.get_untracked()
                    || loading_more.get_untracked()
                    || trash_mode.get_untracked()
                    || listing_next_offset.get_untracked() >= listing_total.get_untracked()
                {
                    return;
                }
                let sequence = request_sequence.get_untracked();
                let id = current_id.get_untracked();
                let query = search_query.get_untracked();
                let sort = sort_order.get_untracked();
                let offset = listing_next_offset.get_untracked();
                let logout = on_logout.clone();
                loading_more.set(true);
                loading_more_error.set(String::new());
                leptos::task::spawn_local(async move {
                    let result = api::fetch_listing(&id, &query, &sort, offset).await;
                    if request_sequence.try_get_untracked() != Some(sequence) {
                        return;
                    }
                    match result {
                        Ok(listing) => {
                            let received = listing.items.len() as i64;
                            // A concurrent deletion may make the final batch empty.
                            listing_next_offset.set(if received == 0 {
                                listing.total
                            } else {
                                offset + received
                            });
                            listing_total.set(listing.total);
                            total_bytes.set(listing.total_bytes);
                            file_count.set(listing.file_count);
                            items.update(|files| {
                                let existing: HashSet<_> =
                                    files.iter().map(|file| file.id.clone()).collect();
                                files.extend(
                                    listing
                                        .items
                                        .into_iter()
                                        .filter(|file| !existing.contains(&file.id)),
                                );
                            });
                            preview_items.set(items.get_untracked());
                        }
                        Err(request_error) if request_error.is_unauthorized() => {
                            loading_more.set(false);
                            logout.run(());
                            return;
                        }
                        Err(request_error) => loading_more_error.set(request_error.message),
                    }
                    loading_more.set(false);
                });
            })
        };
        let load_trash_request = {
            let notify = notify.clone();
            let on_logout = on_logout.clone();
            Callback::new(move |request: TrashLoadRequest| {
                let mut completion = request.completion;
                let sequence = request_sequence.get_untracked().wrapping_add(1);
                request_sequence.set(sequence);
                loading_folder.set(None);
                trash_mode.set(true);
                items.set(Vec::new());
                total_bytes.set(0);
                file_count.set(0);
                loading.set(true);
                loading_more.set(false);
                loading_more_error.set(String::new());
                error.set(String::new());
                let logout = on_logout.clone();

                leptos::task::spawn_local(async move {
                    let mut success = false;
                    match api::fetch_trash().await {
                        Ok(trash) if request_sequence.try_get_untracked() == Some(sequence) => {
                            current.set(None);
                            breadcrumbs.set(Vec::new());
                            preview_items.set(trash.items.clone());
                            items.set(trash.items);
                            total_bytes.set(trash.total_bytes);
                            file_count.set(trash.file_count);
                            if shell_context
                                .is_none_or(|c| c.page.get_untracked().is_file_workspace())
                            {
                                selection.clear();
                            }
                            trash_mode.set(true);
                            loading.set(false);
                            success = true;
                        }
                        Err(request_error)
                            if request_sequence.try_get_untracked() == Some(sequence)
                                && request_error.is_unauthorized() =>
                        {
                            loading.set(false);
                            logout.run(());
                        }
                        Err(request_error)
                            if request_sequence.try_get_untracked() == Some(sequence) =>
                        {
                            loading.set(false);
                            notify.run(Feedback::error(request_error.message));
                        }
                        Ok(_) | Err(_) => {}
                    }
                    finish_trash_load(&mut completion, success);
                });
            })
        };
        let load_trash = {
            let load_trash_request = load_trash_request.clone();
            Callback::new(move |(): ()| {
                load_trash_request.run(TrashLoadRequest { completion: None });
            })
        };

        Self {
            load_folder_request,
            load_folder,
            load_more,
            load_trash_request,
            load_trash,
        }
    }
}
