//! One mixed history of explicitly opened content, newest first.

use super::*;

#[component]
pub(super) fn HomeDashboard(
    items: RwSignal<Vec<LibraryItem>>,
    refresh: RwSignal<u64>,
    on_open: Callback<File>,
    on_navigate: Callback<LibraryPage>,
    on_unauthorized: Callback<()>,
) -> impl IntoView {
    let total = RwSignal::new(0_i64);
    let loading = RwSignal::new(true);
    let more_loading = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let retry = RwSignal::new(0_u64);
    let generation = RwSignal::new(0_u64);
    let selection = expect_context::<ShellContext>().selection;
    items.set(Vec::new());
    selection.set_library_items(Vec::new());

    let load = Callback::new(move |more: bool| {
        if more && (loading.get_untracked() || more_loading.get_untracked()) {
            return;
        }
        if more {
            more_loading.set(true);
        } else {
            generation.update(|value| *value += 1);
            loading.set(true);
            more_loading.set(false);
        }
        error.set(String::new());
        let revision = generation.get_untracked();
        let query = LibraryQuery {
            recent: true,
            opened_only: true,
            offset: if more {
                items.get_untracked().len() as i64
            } else {
                0
            },
            ..Default::default()
        };
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let result = api::fetch_library(&query).await;
            if generation.try_get_untracked() != Some(revision) {
                return;
            }
            loading.set(false);
            more_loading.set(false);
            match result {
                Ok(listing) => {
                    total.set(listing.total);
                    if more {
                        items.update(|list| list.extend(listing.items));
                    } else {
                        items.set(listing.items);
                    }
                    selection.set_library_items(
                        items
                            .get_untracked()
                            .into_iter()
                            .map(|item| item.file)
                            .collect(),
                    );
                }
                Err(e) if e.is_unauthorized() => on_unauthorized.run(()),
                Err(e) => error.set(e.message),
            }
        });
    });
    Effect::new(move |_| {
        let _ = (refresh.get(), retry.get());
        load.run(false);
    });

    view! {
        <section class="home-section" aria-label="最近使用" aria-busy=move || (loading.get() || more_loading.get()).to_string()>
            <header><h2>"最近使用"</h2></header>
            <div class="home-content-row" class:selection-mode=move || selection.enabled.get()>
                <Show when=move || loading.get() && items.get().is_empty() fallback=|| ()>
                    <div class="home-loading" role="status" aria-label="正在加载最近使用">
                        <div class="spinner"></div><span>"正在加载…"</span>
                    </div>
                </Show>
                <Show when=move || !loading.get() && error.get().is_empty() && items.get().is_empty() fallback=|| ()>
                    <div class="home-empty">
                        <strong>"还没有最近使用的内容"</strong>
                        <p>"打开过的书籍、音乐、图片和视频会显示在这里"</p>
                        <button class="secondary" on:click=move |_| on_navigate.run(LibraryPage::Files)>"前往文件"</button>
                    </div>
                </Show>
                <For each=move || items.get()
                    key=|item| (item.file.id.clone(), item.file.name.clone(), item.file.etag.clone(), item.favorite, item.duration_ms, item.reading_progress.map(f64::to_bits))
                    children=move |item| {
                        let file = item.file.clone();
                        let open_file = file.clone();
                        let selected_id = file.id.clone();
                        let background_id = file.id.clone();
                        let detail = item.last_opened.map(|stamp| format_date(&stamp.to_rfc3339())).unwrap_or_default();
                        view! {
                            <article class="home-card"
                                data-selection-ids=serde_json::to_string(&vec![file.id.clone()]).unwrap_or_default()
                                on:click=move |event| selection.toggle_from_card_background(event, &background_id)
                                class:selected=move || selection.ids.with(|ids| ids.contains(&selected_id))>
                                <SelectionCheckbox id=file.id.clone() name=file.name.clone() selection=selection />
                                <button class="home-item" aria-label=format!("打开 {}", file.name)
                                    on:click=move |_| on_open.run(open_file.clone())>
                                    <FileCard name=file.name.clone() detail=Signal::derive(move || detail.clone())>
                                        <LibraryCover item=item />
                                    </FileCard>
                                </button>
                            </article>
                        }
                    } />
            </div>
            <Show when=move || !error.get().is_empty() fallback=|| ()>
                <div class="library-error" role="alert">{move || error.get()}
                    <button aria-label="重试最近使用" on:click=move |_| retry.update(|value| *value += 1)>"重试"</button>
                </div>
            </Show>
            <Show when=move || !loading.get() && error.get().is_empty() && (items.get().len() as i64) < total.get() fallback=|| ()>
                <div class="library-load-more">
                    <button class="secondary" disabled=move || more_loading.get() on:click=move |_| load.run(true)>
                        {move || if more_loading.get() { "正在加载…" } else { "加载更多" }}
                    </button>
                </div>
            </Show>
        </section>
    }
}
