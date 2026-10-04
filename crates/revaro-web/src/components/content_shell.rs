//! Personal content library, with file tools kept mounted during navigation.
mod cards;
mod collections;
mod home;
mod loading;

use cards::LibraryCover;
pub(super) use cards::{BookProgressBar, CardInfo};
use home::*;

use leptos::prelude::*;
use revaro_core::{
    api::auth::Session,
    library::{Collection, ItemUpdate, LibraryItem},
    model::{File, FileKind},
};
use wasm_bindgen::{JsCast, JsValue};

use super::{
    FileBrowser, icons,
    media::MediaPreview,
    menu::{ActionMenu, MenuIcon},
    music_player::{MusicController, PersistentMusicPlayer, display_title},
    reader::ReaderView,
    selection::{SelectionCheckbox, SelectionManagement, SelectionMode, install_long_press},
    selection_toolbar::{BatchActionBar, matches_collection},
    topbar::{AppNavigation, AppTopbar, TopbarActions, TopbarSearch},
    uploads::UploadProgress,
};
use crate::{
    api::{self, LibraryQuery},
    browser,
    logic::{
        feedback::Feedback,
        format::{format_date, format_media_time},
        library::LibraryPage,
        routing::reader_id,
    },
};

#[derive(Clone, Copy)]
pub struct ShellContext {
    pub page: RwSignal<LibraryPage>,
    pub refresh: RwSignal<u64>,
    pub music: MusicController,
    pub open: Callback<File>,
    pub transfer: RwSignal<Option<(File, bool)>>,
    pub selection: SelectionMode,
    pub selection_overlay: Signal<bool>,
}

#[derive(Clone)]
struct CollectionTarget {
    page: LibraryPage,
    files: Vec<File>,
}

fn series_name(path: &str) -> String {
    crate::logic::routing::series_id(path)
        .and_then(|name| js_sys::decode_uri_component(&name).ok())
        .and_then(|name| name.as_string())
        .unwrap_or_default()
}

fn pathname() -> String {
    web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .unwrap_or_default()
}

fn route(path: &str, push: bool) {
    if let Some(window) = web_sys::window()
        && let Ok(history) = window.history()
    {
        if push {
            let _ = history.push_state_with_url(&JsValue::NULL, "", Some(path));
        } else {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(path));
        }
    }
}

#[component]
pub fn ContentShell(
    session: Session,
    on_logout: Callback<()>,
    on_username_changed: Callback<String>,
    on_password_changed: Callback<String>,
) -> impl IntoView {
    let page = RwSignal::new(LibraryPage::from_path(&pathname()));
    let header_actions = RwSignal::new(None::<TopbarActions>);
    let refresh = RwSignal::new(0_u64);
    let music = MusicController::new();
    let transfer = RwSignal::new(None::<(File, bool)>);
    let query_text = RwSignal::new(String::new());
    let query = RwSignal::new(String::new());
    let favorites = RwSignal::new(false);
    let selected_collection = RwSignal::new(String::new());
    let selected_series = RwSignal::new(series_name(&pathname()));
    let collections = RwSignal::new(Vec::<Collection>::new());
    let items = RwSignal::new(Vec::<LibraryItem>::new());
    let total = RwSignal::new(0_i64);
    let loading = RwSignal::new(false);
    let more_loading = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let notice = RwSignal::new(String::new());
    let generation = RwSignal::new(0_u64);
    let reader = RwSignal::new(None::<File>);
    let reader_return = RwSignal::new(page.get_untracked().path().to_owned());
    let reader_pushed = RwSignal::new(false);
    let image = RwSignal::new(None::<File>);
    let image_items = RwSignal::new(Vec::<File>::new());
    let new_collection = RwSignal::new(false);
    let collection_name = RwSignal::new(String::new());
    let collection_busy = RwSignal::new(false);
    let collection_target = RwSignal::new(None::<CollectionTarget>);
    let selection = SelectionMode::new();
    install_long_press(selection);
    let selection_overlay = Signal::derive(move || {
        reader.get().is_some()
            || image.get().is_some()
            || new_collection.get()
            || collection_target.get().is_some()
    });
    let selection_scope = Memo::new(move |_| {
        (
            page.get(),
            query.get(),
            favorites.get(),
            selected_collection.get(),
            selected_series.get(),
        )
    });
    Effect::new(move |_| {
        let _ = selection_scope.get();
        selection.clear();
    });
    Effect::new(move |_| {
        if !loading.get() && !page.get().is_file_workspace() && page.get() != LibraryPage::Home {
            selection.set_library_items(
                items
                    .get()
                    .iter()
                    .flat_map(LibraryItem::selectable_files)
                    .collect(),
            );
        }
    });

    let navigate = Callback::new(move |next: LibraryPage| {
        reader.set(None);
        image.set(None);
        selected_series.set(String::new());
        if page.get_untracked() != next {
            query.set(String::new());
            query_text.set(String::new());
            favorites.set(false);
            selected_collection.set(String::new());
        }
        let same_route = page.get_untracked() == next && pathname() == next.path();
        page.set(next);
        if !same_route {
            route(next.path(), true);
        }
        if let Some(actions) = header_actions.get_untracked() {
            match next {
                LibraryPage::Files => actions.on_files.run(()),
                LibraryPage::Trash => actions.on_trash.run(()),
                _ => (),
            }
        }
        refresh.update(|r| *r += 1);
        if let Some(w) = web_sys::window() {
            w.scroll_to_with_x_and_y(0.0, 0.0);
        }
    });
    let import = Callback::new(move |()| {
        if let Some(actions) = header_actions.get_untracked() {
            actions.on_upload_files.run(());
        }
    });
    let logout = on_logout;
    let load = loading::ListingController {
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
    }
    .install();
    let open = Callback::new(move |file: File| {
        if selection.enabled.get_untracked() {
            selection.toggle(&file.id);
            return;
        }
        if revaro_core::classify::is_book(&file) {
            reader_return.set(pathname());
            reader_pushed.set(true);
            reader.set(Some(file.clone()));
            route(&format!("/read/{}", file.id), true);
        } else if revaro_core::classify::is_audio(&file) {
            music.play(
                file.clone(),
                items
                    .get_untracked()
                    .into_iter()
                    .filter(|i| i.kind == "audio")
                    .map(|i| i.file)
                    .collect(),
            );
        } else if revaro_core::classify::is_image(&file) || revaro_core::classify::is_video(&file) {
            image_items.set(
                items
                    .get_untracked()
                    .into_iter()
                    .filter(|i| revaro_core::classify::is_image(&i.file))
                    .map(|i| i.file)
                    .collect(),
            );
            if !image_items.get_untracked().iter().any(|f| f.id == file.id) {
                image_items.update(|v| v.push(file.clone()));
            }
            image.set(Some(file.clone()));
        }
        let id = file.id;
        leptos::task::spawn_local(async move {
            let _ = api::update_library_item(
                &id,
                &ItemUpdate {
                    favorite: None,
                    opened: true,
                },
            )
            .await;
        });
    });
    provide_context(ShellContext {
        page,
        refresh,
        music,
        open,
        transfer,
        selection,
        selection_overlay,
    });
    Effect::new(move |_| {
        let list = items.get();
        if matches!(page.get(), LibraryPage::Gallery | LibraryPage::Videos) {
            image_items.set(list.iter().map(|i| i.file.clone()).collect());
        }
        if let Some(current) = image.get()
            && let Some(index) = list.iter().position(|i| i.file.id == current.id)
            && list.len() - index <= 4
            && (list.len() as i64) < total.get()
            && !more_loading.get()
            && !loading.get()
            && error.get().is_empty()
        {
            load.run(true);
        }
    });
    let close_reader = Callback::new(move |()| {
        reader.set(None);
        // Return through history so reopening and browser Back have one meaning.
        if reader_pushed.get_untracked() {
            if let Some(w) = web_sys::window()
                && let Ok(h) = w.history()
            {
                let _ = h.back();
            }
        } else {
            route(&reader_return.get_untracked(), false);
            page.set(LibraryPage::from_path(&reader_return.get_untracked()));
        }
        refresh.update(|r| *r += 1);
    });
    if let Some(id) = reader_id(&pathname()) {
        reader_return.set("/library".to_owned());
        let id = js_sys::decode_uri_component(&id)
            .ok()
            .and_then(|s| s.as_string())
            .unwrap_or_default();
        leptos::task::spawn_local(async move {
            match api::fetch_file(&id).await {
                Ok(detail) if revaro_core::classify::is_book(&detail.file) => {
                    reader.set(Some(detail.file))
                }
                Err(e) if e.is_unauthorized() => logout.run(()),
                _ => {
                    error.set("这本书已不可用".to_owned());
                    route("/library", false);
                }
            }
        });
    }
    let mut popstate = browser::on_popstate(move |_| {
        let path = pathname();
        let next = LibraryPage::from_path(&path);
        if page.get_untracked() != next {
            query.set(String::new());
            query_text.set(String::new());
            favorites.set(false);
            selected_collection.set(String::new());
        }
        page.set(next);
        selected_series.set(series_name(&path));
        query_text.set(if selected_series.get_untracked().is_empty() {
            query.get_untracked()
        } else {
            String::new()
        });
        if let Some(id) = reader_id(&path) {
            reader_pushed.set(true);
            leptos::task::spawn_local(async move {
                if let Ok(detail) = api::fetch_file(&id).await
                    && revaro_core::classify::is_book(&detail.file)
                {
                    reader.set(Some(detail.file));
                }
            });
        } else {
            reader.set(None);
            image.set(None);
        }
        refresh.update(|r| *r += 1);
    });
    on_cleanup(move || popstate.release());
    let collections::CollectionController {
        collection_page,
        create,
        add_member,
        delete_collection,
    } = collections::CollectionController::install(collections::CollectionContext {
        page,
        selected_collection,
        selected_series,
        collection_target,
        collection_name,
        collection_busy,
        new_collection,
        selection,
        refresh,
        error,
        logout,
    });
    let play_all = Callback::new(move |()| {
        let request = LibraryQuery {
            kind: "audio".to_owned(),
            query: query.get_untracked(),
            favorite: favorites.get_untracked(),
            collection: selected_collection.get_untracked(),
            ..Default::default()
        };
        notice.set("正在准备播放队列…".to_owned());
        leptos::task::spawn_local(async move {
            let mut request = request;
            let mut queue = Vec::new();
            loop {
                match api::fetch_library(&request).await {
                    Ok(batch) => {
                        request.offset += batch.items.len() as i64;
                        queue.extend(batch.items.into_iter().map(|i| i.file));
                        if request.offset >= batch.total {
                            break;
                        }
                    }
                    Err(e) => {
                        if e.is_unauthorized() {
                            logout.run(());
                        } else {
                            error.set(e.message);
                        }
                        notice.set(String::new());
                        return;
                    }
                }
            }
            notice.set(String::new());
            if let Some(first) = queue.first().cloned() {
                music.play(first, queue);
            }
        });
    });
    let library_search = TopbarSearch {
        text: query_text,
        on_input: Callback::new(move |text: String| {
            query_text.set(text.clone());
            if text.is_empty() {
                query.set(text);
            }
        }),
        on_submit: Callback::new(move |()| {
            if !selected_series.get_untracked().is_empty() {
                selected_series.set(String::new());
                route("/library", true);
            }
            query.set(query_text.get_untracked());
        }),
    };
    let open_series = Callback::new(move |name: String| {
        selected_series.set(name.clone());
        query_text.set(String::new());
        route(
            &format!("/library/series/{}", js_sys::encode_uri_component(&name)),
            true,
        );
        if let Some(window) = web_sys::window() {
            window.scroll_to_with_x_and_y(0.0, 0.0);
        }
    });
    let close_series = Callback::new(move |()| {
        selected_series.set(String::new());
        query_text.set(query.get_untracked());
        route("/library", true);
    });
    let collection_text = Signal::derive(move || {
        collections
            .get()
            .into_iter()
            .find(|c| c.id == selected_collection.get())
            .map(|c| c.name)
            .unwrap_or_else(|| format!("所有{}", page.get().collection_label()))
    });
    let collection_context = Signal::from(Memo::new(move |_| page.get().path().to_owned()));
    view! {
        <div class="content-library" class:has-music=move ||music.current().is_some()
            on:click=move |event: web_sys::MouseEvent| {
                // Short listings and wide screens leave background outside the main content.
                let is_outer_blank = event.target()
                    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    .is_some_and(|target| matches!(target.matches(".content-library,.app-shell"), Ok(true)));
                if is_outer_blank {
                    selection.exit_from_blank(event);
                }
            }>
            {move || header_actions.get().map(|actions| {
                let upload_controller = actions.upload_controller.clone();
                view! {
                    <AppTopbar actions=actions library_search=library_search page=page on_navigate=navigate selection=selection />
                    <UploadProgress controller=upload_controller />
                }
            })}
            <Show when=move ||!page.get().is_file_workspace() fallback=|| ()>
                <main class="library-main" on:click=move |event| selection.exit_from_blank(event)>
                    {move || selection.actions.get().map(|actions| view! { <BatchActionBar selection=selection actions=actions /> })}
                    <Show when=move ||page.get()==LibraryPage::Home fallback=move ||view! {

                        <Show when=move ||selected_series.get().is_empty() fallback=move ||view! {
                            <header class="series-header">
                                <button class="secondary" on:click=move |_|close_series.run(())>"← 返回书架"</button>
                                <div><h1>{move ||selected_series.get()}</h1><small>{move ||format!("{} 本书",total.get())}</small></div>
                            </header>
                        }>
                        <div class="library-toolbar">
                            <div class="library-tabs">
                                <button type="button" class:active=move || !favorites.get() && selected_collection.get().is_empty()
                                    on:click=move |_| { favorites.set(false); selected_collection.set(String::new()); }>
                                    "全部"<small>{move || total.get()}</small>
                                </button>
                                <button type="button" class="favorite-filter" class:active=move || favorites.get() title="我的收藏" aria-label="我的收藏"
                                    aria-pressed=move || favorites.get().to_string()
                                    on:click=move |_| { favorites.update(|v| *v = !*v); selected_collection.set(String::new()); }>
                                    {icons::heart()}
                                </button>
                                <ActionMenu label="选择集合".to_owned() icon=MenuIcon::More text=collection_text context=collection_context>
                                    <button type="button" data-close-menu="true" aria-pressed=move || selected_collection.get().is_empty().to_string()
                                        on:click=move |_| { selected_collection.set(String::new()); favorites.set(false); }>
                                        {move || format!("所有{}", page.get().collection_label())}
                                    </button>
                                    <For each=move || { collections.get().into_iter().filter(|c| c.kind == page.get().kind()).collect::<Vec<_>>() }
                                        key=|c| (c.id.clone(), c.name.clone(), c.item_count) children=move |c| {
                                            let id = c.id.clone();
                                            view! { <button type="button" data-close-menu="true"
                                                aria-pressed=move || (selected_collection.get() == id).to_string()
                                                on:click=move |_| { selected_collection.set(c.id.clone()); favorites.set(false); }>
                                                {format!("{} · {}", c.name, c.item_count)}
                                            </button> }
                                        } />
                                </ActionMenu>
                            </div>
                            <div class="library-toolbar-actions">
                                <button class="secondary collection-create" type="button"
                                    title=move || format!("新建{}", page.get().collection_label())
                                    aria-label=move || format!("新建{}", page.get().collection_label())
                                    on:click=move |_| { new_collection.set(true); error.set(String::new()); }>{icons::plus()}</button>
                                <Show when=move || page.get() == LibraryPage::Music fallback=|| ()><button class="primary library-play" type="button" on:click=move |_| play_all.run(())>{icons::play()}"播放"</button></Show>
                            </div>
                        </div>
                        <Show when=move ||!selected_collection.get().is_empty() fallback=|| ()><div class="collection-caption"><span>"集合中的内容仍保存在原文件夹，移除成员不会删除原文件。"</span><button on:click=move |_|delete_collection.run(())>"删除集合"</button></div></Show>
                        </Show>
                        <Show when=move ||loading.get() fallback=move ||view! {
                            <Show when=move ||items.get().is_empty() && error.get().is_empty() fallback=|| ()><div class="library-empty"><span>"＋"</span><h2>"这里等着你的收藏"</h2><p>"已有文件会自动出现在对应内容库，也可以现在导入。"</p><button class="primary" on:click=move |_|import.run(())>"导入内容"</button></div></Show>
                            <div class="library-grid" class:selection-mode=move || selection.enabled.get() class:book-grid=move ||page.get()==LibraryPage::Books class:song-list=move ||page.get()==LibraryPage::Music class:photo-grid=move ||matches!(page.get(),LibraryPage::Gallery | LibraryPage::Videos) class:video-grid=move ||page.get()==LibraryPage::Videos>
                                <For each=move || { items.get().into_iter().enumerate().collect::<Vec<_>>() }
                                    key=|(_,i)|(i.file.id.clone(),i.favorite,i.file.name.clone(),i.file.etag.clone(),i.reading_progress.map(f64::to_bits),i.series_files.iter().map(|f|f.id.clone()).collect::<Vec<_>>())
                                    children=move |(index,item)| {
                                        let file_id = item.file.id.clone();
                                        let group = item.selectable_files().into_iter().map(|f|f.id).collect::<Vec<_>>();
                                        let selected_group = group.clone();
                                        let background_group = group.clone();
                                        let is_series = !item.series_files.is_empty();
                                        let name = if is_series { item.series.clone().unwrap_or_default() } else { display_title(&item.file.name) };
                                        let detail = if is_series { format!("{} 本书", item.series_files.len()) }
                                            else if item.kind == "book" {
                                                let progress = item.reading_progress.map(|p|format!("已读 {p:.1}%")).unwrap_or_else(|| "未读".to_owned());
                                                item.series_index.map(|n| format!("第 {n} 卷 · {progress}")).unwrap_or(progress)
                                            } else if item.kind == "audio" { format!("{} · 本地音乐",revaro_core::classify::extension(&item.file.name).to_uppercase()) }
                                            else { format_date(&item.file.created_at.to_rfc3339()) };
                                        let aria = if is_series { format!("打开系列 {name}，{} 本书",item.series_files.len()) } else { format!("打开 {}",item.file.name) };
                                        let open_item = item.clone();
                                        let open_group = group.clone();
                                        view! {
                                            <article class="library-card" class:series-card=is_series data-selection-ids=serde_json::to_string(&group).unwrap_or_default()
                                                on:click=move |event|selection.toggle_group_from_card_background(event,&background_group)
                                                class:selected=move ||selection.ids.with(|ids|selected_group.iter().all(|id|ids.contains(id)))
                                                class:is-playing=move ||music.current().is_some_and(|f|f.id==file_id)>
                                                <SelectionCheckbox id=item.file.id.clone() name=name.clone() group=group.clone() selection=selection />
                                                <Show when=move ||page.get()==LibraryPage::Music fallback=|| ()><span class="song-number-slot"><span class="song-number">{format!("{:02}",index+1)}</span></span></Show>
                                                <button class="library-card-open" aria-label=aria on:click=move |_| {
                                                    if selection.enabled.get_untracked() { selection.toggle_group(&open_group); }
                                                    else if is_series { open_series.run(open_item.series.clone().unwrap_or_default()); }
                                                    else { open.run(open_item.file.clone()); }
                                                }>
                                                    <LibraryCover item=item.clone() />
                                                    {is_series.then(||view! { <span class="series-count">{format!("{} 本",item.series_files.len())}</span> })}
                                                    <CardInfo name=name detail=Signal::derive(move ||detail.clone()) />
                                                </button>
                                            </article>
                                        }
                                    } />
                            </div>
                            <Show when=move || { (items.get().len() as i64)<total.get() } fallback=|| ()><div class="library-load-more"><button class="secondary" disabled=move ||more_loading.get() on:click=move |_|load.run(true)>{move ||if more_loading.get(){"正在加载…"}else{"加载更多"}}</button><small>{move ||format!("已显示 {} / {}",items.get().len(),total.get())}</small></div></Show>
                        }><div class="library-loading"><div class="spinner"></div><p>"正在打开内容库…"</p></div></Show>
                    }>
                        <HomeDashboard refresh=refresh on_open=open on_navigate=navigate on_import=Callback::new(move |()|import.run(())) on_unauthorized=logout />
                    </Show>
                    <Show when=move ||!error.get().is_empty() fallback=|| ()><div class="library-error" role="alert">{move ||error.get()}<button on:click=move |_|{refresh.update(|r|*r+=1);}>"重试"</button></div></Show>
                    <Show when=move ||!notice.get().is_empty() fallback=|| ()><div class="library-notice" role="status">{move ||notice.get()}<button aria-label="关闭提示" on:click=move |_|notice.set(String::new())>"×"</button></div></Show>
                </main>
            </Show>
            <FileBrowser session=session on_logout=on_logout on_username_changed=on_username_changed on_password_changed=on_password_changed on_header_ready=Callback::new(move |actions|header_actions.set(Some(actions))) />
            <PersistentMusicPlayer controller=music />
            <AppNavigation page=page on_navigate=navigate mobile=true />
            <Show when=move ||reader.get().is_some() fallback=|| ()>{move ||reader.get().map(|file|view!{<ReaderView file=file on_close=close_reader on_unauthorized=on_logout />})}</Show>
            <Show when=move ||image.get().is_some() fallback=|| ()><MediaPreview selected=image items=image_items on_close=Callback::new(move |()|image.set(None)) on_download=Callback::new(|file:File|super::file_browser::download_file(&file)) on_move=Callback::new(move |file:File|{image.set(None);transfer.set(Some((file,false)));}) on_copy=Callback::new(move |file:File|{image.set(None);transfer.set(Some((file,true)));}) /></Show>
            <Show when=move ||new_collection.get() ||collection_target.get().is_some() fallback=|| ()>
                <div class="modal-backdrop"><section class="modal library-collection-dialog" role="dialog" aria-modal="true" aria-label="管理集合">
                    <header><h2>{move ||format!("{}{}",if new_collection.get(){"新建"}else{"加入"},collection_page.get().collection_label())}</h2><button aria-label="关闭集合对话框" disabled=move ||collection_busy.get() on:click=move |_|{new_collection.set(false);collection_target.set(None);}>"×"</button></header>
                    <Show when=move ||collection_target.get().is_some() fallback=|| ()><p class="collection-target-count">{move ||collection_target.get().map(|target|format!("将所选的 {} 项{}加入{}",target.files.len(),target.page.label(),target.page.collection_label())).unwrap_or_default()}</p></Show>
                    <Show when=move ||new_collection.get() fallback=move ||view! {
                        <div class="collection-options">
                            <For each=move || { collections.get().into_iter().filter(|c|c.kind==collection_page.get().kind()).collect::<Vec<_>>() } key=|c|(c.id.clone(),c.name.clone(),c.item_count) children=move |c|view! {
                                <button disabled=move ||collection_busy.get() on:click=move |_|add_member.run(c.id.clone())>{c.name}<small>{format!("{} 项",c.item_count)}</small></button>
                            } />
                            <Show when=move ||!collections.get().iter().any(|c|c.kind==collection_page.get().kind()) fallback=|| ()><p>"先创建一个集合，就能把喜欢的内容放在一起。"</p></Show>
                            <button class="secondary" disabled=move ||collection_busy.get() on:click=move |_|new_collection.set(true)>"＋ 创建新集合"</button>
                        </div>
                    }>
                        <form on:submit=move |ev|{ev.prevent_default();create.run(());}><label>"名称"<input aria-label="集合名称" maxlength="80" placeholder="给它一个名字" prop:value=move ||collection_name.get() on:input=move |ev|collection_name.set(event_target_value(&ev)) /></label><footer><button class="primary" type="submit" disabled=move ||collection_busy.get() ||collection_name.get().trim().is_empty()>"创建"</button></footer></form>
                    </Show>
                    <Show when=move ||!error.get().is_empty() fallback=|| ()><p class="form-error" role="alert">{move ||error.get()}</p></Show>
                </section></div>
            </Show>
        </div>
    }
}
