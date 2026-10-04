//! Library listing, filters, pagination and collection-list refresh.

use super::*;

pub(super) struct ListingController {
    pub page: RwSignal<LibraryPage>,
    pub query: RwSignal<String>,
    pub favorites: RwSignal<bool>,
    pub selected_collection: RwSignal<String>,
    pub selected_series: RwSignal<String>,
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
            selected_series,
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
        let load = Callback::new(move |more: bool| {
            let current_page = page.get_untracked();
            if matches!(
                current_page,
                LibraryPage::Home | LibraryPage::Files | LibraryPage::Trash
            ) || (more && more_loading.get_untracked())
            {
                return;
            }
            let request = LibraryQuery {
                kind: current_page.kind().to_owned(),
                query: if selected_series.get_untracked().is_empty() {
                    query.get_untracked()
                } else {
                    String::new()
                },
                favorite: selected_series.get_untracked().is_empty() && favorites.get_untracked(),
                collection: if selected_series.get_untracked().is_empty() {
                    selected_collection.get_untracked()
                } else {
                    String::new()
                },
                series: selected_series.get_untracked(),
                group_series: current_page == LibraryPage::Books
                    && selected_series.get_untracked().is_empty(),
                offset: if more {
                    items.get_untracked().len() as i64
                } else {
                    0
                },
                ..Default::default()
            };
            if !more {
                generation.update(|g| *g += 1);
                loading.set(true);
                items.set(Vec::new());
                total.set(0);
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
                selected_series.get(),
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
