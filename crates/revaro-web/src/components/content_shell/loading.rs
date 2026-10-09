//! Library listing, filters, pagination and collection-list refresh.

use super::*;

#[derive(PartialEq, Eq)]
struct LibraryScope {
    page: LibraryPage,
    query: String,
    favorite: bool,
    collection: String,
    stack: String,
}

pub(super) struct ListingController {
    pub page: RwSignal<LibraryPage>,
    pub query: RwSignal<String>,
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
        let last_scope = StoredValue::new(None::<LibraryScope>);
        let load = Callback::new(move |more: bool| {
            let current_page = page.get_untracked();
            if matches!(
                current_page,
                LibraryPage::Home | LibraryPage::Files | LibraryPage::Trash
            ) {
                // Home owns the shared items signal while it is mounted.
                // Invalidate requests from the library page we just left.
                generation.update(|g| *g += 1);
                last_scope.set_value(None);
                loading.set(false);
                more_loading.set(false);
                error.set(String::new());
                return;
            }
            if more && more_loading.get_untracked() {
                return;
            }
            let request = LibraryQuery {
                kind: current_page.kind().to_owned(),
                query: query.get_untracked(),
                favorite: favorites.get_untracked(),
                collection: selected_collection.get_untracked(),
                stack: selected_stack.get_untracked(),
                group_stacks: current_page == LibraryPage::Books
                    && selected_stack.get_untracked().is_empty(),
                offset: if more {
                    items.get_untracked().len() as i64
                } else {
                    0
                },
                ..Default::default()
            };
            if !more {
                let scope = LibraryScope {
                    page: current_page,
                    query: request.query.clone(),
                    favorite: request.favorite,
                    collection: request.collection.clone(),
                    stack: request.stack.clone(),
                };
                // Upload callbacks can refresh while a card is being dragged.
                // Keep those keyed nodes until the same listing is replaced.
                let keep_items = last_scope
                    .with_value(|previous| previous.as_ref() == Some(&scope))
                    && items.with_untracked(|list| !list.is_empty());
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
            let gen_id = generation.get_untracked();
            leptos::task::spawn_local(async move {
                let result = api::fetch_library(&request).await;
                if generation.try_get_untracked() != Some(gen_id) {
                    return;
                }
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
                    }
                    Err(e) if e.is_unauthorized() => logout.run(()),
                    Err(e) => error.set(e.message),
                }
            });
        });
        Effect::new(move |_| {
            let _ = (
                page.get(),
                query.get(),
                favorites.get(),
                selected_collection.get(),
                selected_stack.get(),
                refresh.get(),
            );
            more_loading.set(false);
            load.run(false);
            leptos::task::spawn_local(async move {
                match api::fetch_collections().await {
                    Ok(list) => {
                        if collections.try_get_untracked().is_some() {
                            collections.set(list);
                        }
                    }
                    Err(e) if e.is_unauthorized() => logout.run(()),
                    Err(e) => {
                        if error.try_get_untracked().is_some() {
                            error.set(e.message);
                        }
                    }
                }
            });
        });
        load
    }
}
