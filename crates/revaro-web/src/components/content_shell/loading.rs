//! Library listing, filters, pagination and collection-list refresh.

use super::*;

#[derive(PartialEq, Eq)]
struct LibraryScope {
    page: LibraryPage,
    query: String,
    sort: String,
    favorite: bool,
    collection: String,
    stack: String,
}

pub(super) struct ListingController {
    pub page: RwSignal<LibraryPage>,
    pub query: RwSignal<String>,
    pub sort: RwSignal<String>,
    pub favorites: RwSignal<bool>,
    pub selected_collection: RwSignal<String>,
    pub selected_stack: RwSignal<String>,
    pub items: RwSignal<Vec<LibraryItem>>,
    pub total: RwSignal<i64>,
    pub generation: RwSignal<u64>,
    pub loading: RwSignal<bool>,
    pub more_loading: RwSignal<bool>,
    pub error: RwSignal<String>,
    pub refresh: RwSignal<u64>,
    pub collections: RwSignal<Vec<Collection>>,
    pub logout: Callback<()>,
}

impl ListingController {
    pub fn install(self) -> Callback<bool> {
        let Self {
            page,
            query,
            sort,
            favorites,
            selected_collection,
            selected_stack,
            items,
            total,
            generation,
            loading,
            more_loading,
            error,
            refresh,
            collections,
            logout,
        } = self;
        let snapshots = StoredValue::new(super::super::view_memory::Snapshots::<
            revaro_core::library::LibraryListing,
        >::new());
        let active = StoredValue::new_local(None::<web_sys::AbortController>);
        on_cleanup(move || {
            active.with_value(|controller| {
                if let Some(controller) = controller {
                    controller.abort();
                }
            });
        });
        let last_scope = StoredValue::new(None::<LibraryScope>);
        let request_in_flight = StoredValue::new(false);
        let started_revision = StoredValue::new(0_u64);
        let pending_refresh = StoredValue::new(false);
        let reload = StoredValue::new(None::<Callback<bool>>);
        let load = Callback::new(move |more: bool| {
            let current_page = page.get_untracked();
            if matches!(
                current_page,
                LibraryPage::Home | LibraryPage::Files | LibraryPage::Trash
            ) {
                active.with_value(|controller| {
                    if let Some(controller) = controller {
                        controller.abort();
                    }
                });
                // Home owns the shared items signal while it is mounted.
                // Invalidate requests from the library page we just left.
                generation.update(|g| *g += 1);
                last_scope.set_value(None);
                request_in_flight.set_value(false);
                pending_refresh.set_value(false);
                loading.set(false);
                more_loading.set(false);
                error.set(String::new());
                return;
            }
            if more && request_in_flight.get_value() {
                return;
            }
            let snapshot_key = serde_json::to_string(&(
                current_page.path(),
                query.get_untracked(),
                sort.get_untracked(),
                favorites.get_untracked(),
                selected_collection.get_untracked(),
                selected_stack.get_untracked(),
            ))
            .unwrap_or_default();
            let mut request = LibraryQuery {
                kind: current_page.kind().to_owned(),
                query: query.get_untracked(),
                sort: sort.get_untracked(),
                favorite: favorites.get_untracked(),
                collection: selected_collection.get_untracked(),
                stack: selected_stack.get_untracked(),
                group_stacks: current_page == LibraryPage::Books
                    && selected_stack.get_untracked().is_empty(),
                offset: if more {
                    items.with_untracked(Vec::len) as i64
                } else {
                    0
                },
                ..Default::default()
            };
            if !more {
                let scope = LibraryScope {
                    page: current_page,
                    query: request.query.clone(),
                    sort: request.sort.clone(),
                    favorite: request.favorite,
                    collection: request.collection.clone(),
                    stack: request.stack.clone(),
                };
                let same_scope =
                    last_scope.with_value(|previous| previous.as_ref() == Some(&scope));
                // A same-list mutation can arrive while dragging or uploading.
                // Finish that read, then coalesce newer revisions into one
                // follow-up; only a changed scope cancels the previous read.
                if same_scope && request_in_flight.get_value() {
                    if refresh.get_untracked() != started_revision.get_value() {
                        pending_refresh.set_value(true);
                    }
                    return;
                }
                active.with_value(|controller| {
                    if let Some(controller) = controller {
                        controller.abort();
                    }
                });
                pending_refresh.set_value(false);
                // Upload callbacks can refresh while a card is being dragged.
                // Keep those keyed nodes until the same listing is replaced.
                let mut cached = None;
                if !same_scope {
                    snapshots.update_value(|cache| cached = cache.get(&snapshot_key));
                }
                let has_cached = cached.is_some();
                if let Some(cached) = cached {
                    items.set(cached.items);
                    total.set(cached.total);
                }
                let keep_items =
                    items.with_untracked(|list| !list.is_empty()) && (same_scope || has_cached);
                if keep_items {
                    request.limit = Some(items.with_untracked(Vec::len).clamp(60, 200) as i64);
                    loading.set(false);
                }
                last_scope.set_value(Some(scope));
                generation.update(|g| *g += 1);
                if !keep_items {
                    loading.set(true);
                    items.set(Vec::new());
                    total.set(0);
                }
            } else {
                more_loading.set(true);
            }
            error.set(String::new());
            let controller = web_sys::AbortController::new().ok();
            active.set_value(controller.clone());
            request_in_flight.set_value(true);
            started_revision.set_value(refresh.get_untracked());
            let gen_id = generation.get_untracked();
            leptos::task::spawn_local(async move {
                let result = api::fetch_library_with_signal(
                    &request,
                    controller
                        .as_ref()
                        .map(web_sys::AbortController::signal)
                        .as_ref(),
                )
                .await;
                if generation.try_get_untracked() != Some(gen_id) {
                    return;
                }
                request_in_flight.set_value(false);
                loading.set(false);
                more_loading.set(false);
                match result {
                    Ok(result) => {
                        total.set(result.total);
                        if more {
                            items.update(|list| list.extend(result.items));
                        } else {
                            items.set(result.items);
                        }
                        snapshots.update_value(|cache| {
                            cache.put(
                                snapshot_key,
                                revaro_core::library::LibraryListing {
                                    items: items.get_untracked(),
                                    total: total.get_untracked(),
                                    offset: 0,
                                    limit: 60,
                                },
                            )
                        });
                    }
                    Err(e) if e.is_unauthorized() => {
                        logout.run(());
                        return;
                    }
                    Err(e) => error.set(e.message),
                }
                if pending_refresh.get_value() {
                    pending_refresh.set_value(false);
                    reload.with_value(|callback| {
                        if let Some(callback) = callback {
                            callback.run(false);
                        }
                    });
                }
            });
        });
        reload.set_value(Some(load));
        Effect::new(move |_| {
            let _ = (
                page.get(),
                query.get(),
                sort.get(),
                favorites.get(),
                selected_collection.get(),
                selected_stack.get(),
                refresh.get(),
            );
            more_loading.set(false);
            load.run(false);
        });
        // Filters and pagination do not invalidate collections. Navigation
        // revalidates changes made in another tab or through the API; the
        // shared mutation revision covers this document's writes.
        let pending_revision = StoredValue::new(0_u64);
        let collections_loading = RwSignal::new(false);
        Effect::new(move |_| {
            let _ = (refresh.get(), page.get());
            // Returning to the same page while a response is held still
            // invalidates it. Comparing only the final page would lose that
            // navigation and changes made outside this document.
            pending_revision.update_value(|revision| *revision += 1);
            if collections_loading.get_untracked() {
                return;
            }
            collections_loading.set(true);
            leptos::task::spawn_local(async move {
                loop {
                    let revision = pending_revision.get_value();
                    let result = api::fetch_collections().await;
                    if collections.try_get_untracked().is_none() {
                        return;
                    }
                    match result {
                        Ok(list) if revision == pending_revision.get_value() => {
                            let selected = selected_collection.get_untracked();
                            if !selected.is_empty() && !list.iter().any(|c| c.id == selected) {
                                selected_collection.set(String::new());
                            }
                            collections.set(list);
                        }
                        Ok(_) => {}
                        Err(e) if e.is_unauthorized() => {
                            logout.run(());
                            return;
                        }
                        Err(e) => error.set(e.message),
                    }
                    if revision == pending_revision.get_value() {
                        break;
                    }
                }
                collections_loading.set(false);
            });
        });
        load
    }
}
