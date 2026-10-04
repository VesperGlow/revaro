//! Independently loaded home sections and queues scoped to their visible songs.

use super::*;

const HOME_PAGES: [LibraryPage; 4] = [
    LibraryPage::Books,
    LibraryPage::Music,
    LibraryPage::Gallery,
    LibraryPage::Videos,
];

#[component]
pub(super) fn HomeDashboard(
    refresh: RwSignal<u64>,
    on_open: Callback<File>,
    on_navigate: Callback<LibraryPage>,
    on_import: Callback<()>,
    on_unauthorized: Callback<()>,
) -> impl IntoView {
    let sections = RwSignal::new(
        HOME_PAGES
            .into_iter()
            .map(|page| (page, Vec::<LibraryItem>::new()))
            .collect::<Vec<_>>(),
    );
    let selection = expect_context::<ShellContext>().selection;
    let on_loaded = Callback::new(move |(page, items): (LibraryPage, Vec<LibraryItem>)| {
        sections.update(|sections| {
            if let Some((_, list)) = sections.iter_mut().find(|(p, _)| *p == page) {
                *list = items;
            }
        });
        selection.set_library_items(
            sections
                .get_untracked()
                .into_iter()
                .flat_map(|(_, items)| items.into_iter().map(|item| item.file))
                .collect(),
        );
    });
    view! {
        <For each=|| HOME_PAGES key=|page| page.path() children=move |page| view! {
            <HomeSection page=page refresh=refresh on_open=on_open on_navigate=on_navigate
                on_import=on_import on_unauthorized=on_unauthorized on_loaded=on_loaded />
        } />
    }
}

async fn fetch_home_items(page: LibraryPage) -> Result<Vec<LibraryItem>, api::RequestError> {
    let mut query = LibraryQuery {
        kind: page.kind().to_owned(),
        recent: matches!(page, LibraryPage::Books | LibraryPage::Music),
        ..Default::default()
    };
    let mut listing = api::fetch_library(&query).await?;
    if query.recent && listing.items.is_empty() {
        query.recent = false;
        listing = api::fetch_library(&query).await?;
    }
    Ok(listing.items.into_iter().take(6).collect())
}

#[component]
fn HomeSection(
    page: LibraryPage,
    refresh: RwSignal<u64>,
    on_open: Callback<File>,
    on_navigate: Callback<LibraryPage>,
    on_import: Callback<()>,
    on_unauthorized: Callback<()>,
    on_loaded: Callback<(LibraryPage, Vec<LibraryItem>)>,
) -> impl IntoView {
    let items = RwSignal::new(Vec::<LibraryItem>::new());
    let loading = RwSignal::new(true);
    let error = RwSignal::new(String::new());
    let retry = RwSignal::new(0_u64);
    let generation = RwSignal::new(0_u64);
    let ShellContext {
        selection, music, ..
    } = expect_context::<ShellContext>();
    Effect::new(move |_| {
        let _ = (refresh.get(), retry.get());
        generation.update(|value| *value += 1);
        let revision = generation.get_untracked();
        loading.set(true);
        error.set(String::new());
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let result = fetch_home_items(page).await;
            if generation.try_get_untracked() != Some(revision) {
                return;
            }
            loading.set(false);
            match result {
                Ok(list) => {
                    items.set(list.clone());
                    on_loaded.run((page, list));
                }
                Err(e) if e.is_unauthorized() => on_unauthorized.run(()),
                Err(e) => error.set(e.message),
            }
        });
    });
    let open = Callback::new(move |file: File| {
        if page == LibraryPage::Music && !selection.enabled.get_untracked() {
            music.play(
                file,
                items
                    .get_untracked()
                    .into_iter()
                    .map(|item| item.file)
                    .collect(),
            );
        } else {
            on_open.run(file);
        }
    });
    view! {
        <section class="home-section" aria-busy=move || loading.get().to_string()>
            <header>
                <div><h2>{match page {
                    LibraryPage::Books => "继续阅读",
                    LibraryPage::Music => "最近播放与收藏",
                    LibraryPage::Videos => "最近添加的视频",
                    _ => "最近添加的图片",
                }}</h2></div>
                <button on:click=move |_| on_navigate.run(page)>"查看全部"<span>"→"</span></button>
            </header>
            <div class="home-content-row" class:selection-mode=move || selection.enabled.get()
                class:home-books=page == LibraryPage::Books
                class:home-images=matches!(page, LibraryPage::Gallery | LibraryPage::Videos)>
                <Show when=move || loading.get() && items.get().is_empty() fallback=|| ()>
                    <div class="home-loading" role="status" aria-label=format!("正在加载{}", page.label())>
                        <div class="spinner"></div><span>"正在加载…"</span>
                    </div>
                </Show>
                <Show when=move || !loading.get() && error.get().is_empty() && items.get().is_empty() fallback=|| ()>
                    <button class="home-empty" on:click=move |_| on_import.run(())>
                        "还没有内容，导入你的第一份收藏 →"
                    </button>
                </Show>
                <For each=move || items.get()
                    key=|item| (item.file.id.clone(), item.file.name.clone(), item.file.etag.clone(), item.favorite, item.last_opened.is_some(), item.duration_ms, item.reading_progress.map(f64::to_bits))
                    children=move |item| {
                        let file = item.file.clone();
                        let open_file = file.clone();
                        let selected_id = file.id.clone();
                        let background_id = file.id.clone();
                        view! {
                            <article class="home-card"
                                data-selection-ids=serde_json::to_string(&vec![file.id.clone()]).unwrap_or_default()
                                on:click=move |event| selection.toggle_from_card_background(event, &background_id)
                                class:selected=move || selection.ids.with(|ids| ids.contains(&selected_id))>
                                <SelectionCheckbox id=file.id.clone() name=file.name.clone() selection=selection />
                                <button class="home-item" aria-label=format!("打开 {}", file.name)
                                    on:click=move |_| open.run(open_file.clone())>
                                    <LibraryCover item=item.clone() />
                                    <CardInfo name=display_title(&item.file.name) detail=Signal::derive(move || {
                                        if item.last_opened.is_some() { "继续打开".to_owned() }
                                        else { "新加入你的内容库".to_owned() }
                                    }) />
                                </button>
                            </article>
                        }
                    } />
            </div>
            <Show when=move || !error.get().is_empty() fallback=|| ()>
                <div class="library-error" role="alert">{move || error.get()}
                    <button aria-label=format!("重试{}", page.label()) on:click=move |_| retry.update(|value| *value += 1)>"重试"</button>
                </div>
            </Show>
        </section>
    }
}
