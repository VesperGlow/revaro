//! Personal content library, with file tools kept mounted during navigation.
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
    selection::{SelectionCheckbox, SelectionManagement, SelectionMode},
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

enum LibraryBatchOperation {
    Favorite(bool),
    Membership { collection: String, add: bool },
}

/// Use the existing APIs and global toast for every listing; keep failed targets retryable.
async fn apply_library_batch(
    files: Vec<File>,
    operation: LibraryBatchOperation,
    selection: SelectionMode,
    refresh: RwSignal<u64>,
    error: RwSignal<String>,
    logout: Callback<()>,
) -> Vec<File> {
    let mut failed = Vec::new();
    let mut first_error = None;
    for file in &files {
        let result = match &operation {
            LibraryBatchOperation::Favorite(favorite) => {
                api::update_library_item(
                    &file.id,
                    &ItemUpdate {
                        favorite: Some(*favorite),
                        opened: false,
                    },
                )
                .await
            }
            LibraryBatchOperation::Membership { collection, add } => {
                api::collection_member(collection, &file.id, *add).await
            }
        };
        if let Err(e) = result {
            if e.is_unauthorized() {
                logout.run(());
                return files;
            }
            first_error.get_or_insert_with(|| format!("{}：{}", file.name, e.message));
            failed.push(file.clone());
        }
    }
    let completed = files.len() - failed.len();
    if completed > 0 {
        refresh.update(|r| *r += 1);
    }
    let feedback = if let Some(message) = first_error {
        let message = format!("已完成 {completed}/{} 项，{message}", files.len());
        error.set(message.clone());
        Feedback::error(message)
    } else {
        let verb = match operation {
            LibraryBatchOperation::Favorite(true) => "已收藏",
            LibraryBatchOperation::Favorite(false) => "已取消收藏",
            LibraryBatchOperation::Membership { add: true, .. } => "已加入集合",
            LibraryBatchOperation::Membership { add: false, .. } => "已移出集合",
        };
        Feedback::success(format!("{verb} {completed} 项"))
    };
    if let Some(actions) = selection.actions.get_untracked() {
        actions.on_feedback.run(feedback);
    }
    failed
}

/// Names and metadata stay within the cover instead of adding a second card boundary.
#[component]
pub(super) fn CardInfo(name: String, detail: Signal<String>) -> impl IntoView {
    view! {
        <div class="card-info">
            <strong title=name.clone()>{name.clone()}</strong>
            <small>{move || detail.get()}</small>
        </div>
    }
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
        )
    });
    Effect::new(move |_| {
        let _ = selection_scope.get();
        selection.clear();
    });
    Effect::new(move |_| {
        if !loading.get() && !page.get().is_file_workspace() && page.get() != LibraryPage::Home {
            selection.set_library_items(items.get().into_iter().map(|item| item.file).collect());
        }
    });

    let navigate = Callback::new(move |next: LibraryPage| {
        reader.set(None);
        image.set(None);
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
            query: query.get_untracked(),
            favorite: favorites.get_untracked(),
            collection: selected_collection.get_untracked(),
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
    let collection_page = Signal::derive(move || {
        collection_target
            .get()
            .map(|target| target.page)
            .unwrap_or_else(|| page.get())
    });
    let create = Callback::new(move |()| {
        if collection_busy.get_untracked() {
            return;
        }
        collection_busy.set(true);
        error.set(String::new());
        let kind = collection_page.get_untracked().kind().to_owned();
        let name = collection_name.get_untracked();
        let target = collection_target.get_untracked();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            match api::create_collection(&name, &kind).await {
                Ok(c) => {
                    if let Some(mut target) = target {
                        target.files = apply_library_batch(
                            target.files,
                            LibraryBatchOperation::Membership {
                                collection: c.id,
                                add: true,
                            },
                            selection,
                            refresh,
                            error,
                            logout,
                        )
                        .await;
                        collection_target.set((!target.files.is_empty()).then_some(target));
                    } else {
                        selected_collection.set(c.id);
                    }
                    // Keep the form's owner alive until all membership requests finish.
                    new_collection.set(false);
                    collection_name.set(String::new());
                    refresh.update(|r| *r += 1);
                }
                Err(e) if e.is_unauthorized() => logout.run(()),
                Err(e) => error.set(e.message),
            }
            collection_busy.set(false);
        });
    });
    let add_member = Callback::new(move |collection_id: String| {
        if collection_busy.get_untracked() {
            return;
        }
        let Some(mut target) = collection_target.get_untracked() else {
            return;
        };
        collection_busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            target.files = apply_library_batch(
                target.files,
                LibraryBatchOperation::Membership {
                    collection: collection_id,
                    add: true,
                },
                selection,
                refresh,
                error,
                logout,
            )
            .await;
            collection_target.set((!target.files.is_empty()).then_some(target));
            collection_busy.set(false);
        });
    });
    let favorite_selected = Callback::new(move |favorite: bool| {
        if collection_busy.get_untracked() {
            return;
        }
        let files = selection
            .selected_files()
            .into_iter()
            .filter(|file| file.kind == FileKind::File)
            .collect::<Vec<_>>();
        if files.is_empty() {
            return;
        }
        collection_busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            apply_library_batch(
                files,
                LibraryBatchOperation::Favorite(favorite),
                selection,
                refresh,
                error,
                logout,
            )
            .await;
            collection_busy.set(false);
        });
    });
    let collect_selected = Callback::new(move |target_page: LibraryPage| {
        if collection_busy.get_untracked() {
            return;
        }
        let files = selection
            .selected_files()
            .into_iter()
            .filter(|file| matches_collection(file, target_page))
            .collect::<Vec<_>>();
        if files.is_empty() {
            return;
        }
        error.set(String::new());
        collection_target.set(Some(CollectionTarget {
            page: target_page,
            files,
        }));
    });
    let remove_member = Callback::new(move |()| {
        if collection_busy.get_untracked() {
            return;
        }
        let collection_id = selected_collection.get_untracked();
        let files = selection.selected_files();
        if collection_id.is_empty() || files.is_empty() {
            return;
        }
        collection_busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            apply_library_batch(
                files,
                LibraryBatchOperation::Membership {
                    collection: collection_id,
                    add: false,
                },
                selection,
                refresh,
                error,
                logout,
            )
            .await;
            collection_busy.set(false);
        });
    });
    selection.management.set(Some(SelectionManagement {
        busy: collection_busy.into(),
        on_favorite: favorite_selected,
        on_collection: collect_selected,
        on_remove: remove_member,
        can_remove: Signal::derive(move || {
            !page.get().is_file_workspace()
                && page.get() != LibraryPage::Home
                && !selected_collection.get().is_empty()
        }),
    }));
    let delete_collection = Callback::new(move |()| {
        let id = selected_collection.get_untracked();
        leptos::task::spawn_local(async move {
            match api::delete_collection(&id).await {
                Ok(()) => {
                    selected_collection.set(String::new());
                    refresh.update(|r| *r += 1);
                }
                Err(e) if e.is_unauthorized() => logout.run(()),
                Err(e) => error.set(e.message),
            }
        });
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
        on_submit: Callback::new(move |()| query.set(query_text.get_untracked())),
    };
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

                        <div class="library-toolbar">
                            <div class="library-tabs">
                                <button class:active=move || !favorites.get() && selected_collection.get().is_empty()
                                    on:click=move |_| { favorites.set(false); selected_collection.set(String::new()); }>
                                    "全部"<small>{move || total.get()}</small>
                                </button>
                                <button class="favorite-filter" class:active=move || favorites.get() title="我的收藏" aria-label="我的收藏"
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
                                <button class="secondary" on:click=move |_| { new_collection.set(true); error.set(String::new()); }>{move || format!("＋ 新建{}", page.get().collection_label())}</button>
                                <Show when=move || page.get() == LibraryPage::Music fallback=|| ()><button class="primary" on:click=move |_| play_all.run(())>{icons::play()}"播放全部"</button></Show>
                            </div>
                        </div>
                        <Show when=move ||!selected_collection.get().is_empty() fallback=|| ()><div class="collection-caption"><span>"集合中的内容仍保存在原文件夹，移除成员不会删除原文件。"</span><button on:click=move |_|delete_collection.run(())>"删除集合"</button></div></Show>
                        <Show when=move ||loading.get() fallback=move ||view! {
                            <Show when=move ||items.get().is_empty() && error.get().is_empty() fallback=|| ()><div class="library-empty"><span>"＋"</span><h2>"这里等着你的收藏"</h2><p>"已有文件会自动出现在对应内容库，也可以现在导入。"</p><button class="primary" on:click=move |_|import.run(())>"导入内容"</button></div></Show>
                            <div class="library-grid" class:selection-mode=move || selection.enabled.get() class:book-grid=move ||page.get()==LibraryPage::Books class:song-list=move ||page.get()==LibraryPage::Music class:photo-grid=move ||matches!(page.get(),LibraryPage::Gallery | LibraryPage::Videos) class:video-grid=move ||page.get()==LibraryPage::Videos>
                                <For each=move || { items.get().into_iter().enumerate().collect::<Vec<_>>() } key=|(_,i)|(i.file.id.clone(),i.favorite,i.file.name.clone(),i.file.etag.clone(),i.last_opened.is_some()) children=move |(index,item)| {
                                    let open_file = item.file.clone();
                                    let selected_id = item.file.id.clone();
                                    let card_background_id = item.file.id.clone();
                                    let detail = if item.kind == "book" {
                                        if item.last_opened.is_some() { "继续阅读".to_owned() } else { "未读".to_owned() }
                                    } else if item.kind == "audio" {
                                        format!("{} · 本地音乐", revaro_core::classify::extension(&item.file.name).to_uppercase())
                                    } else { format_date(&item.file.created_at.to_rfc3339()) };
                                    view! {
                                        <article class="library-card" on:click=move |event|selection.toggle_from_card_background(event, &card_background_id)
                                            class:selected=move ||selection.ids.with(|ids|ids.contains(&selected_id))
                                            class:is-playing=move ||music.current().is_some_and(|f|f.id==open_file.id)>
                                            <SelectionCheckbox id=item.file.id.clone() name=item.file.name.clone() selection=selection />
                                            <Show when=move ||page.get()==LibraryPage::Music fallback=|| ()><span class="song-number-slot"><span class="song-number">{format!("{:02}",index+1)}</span></span></Show>
                                            <button class="library-card-open" aria-label=format!("打开 {}",item.file.name) on:click={let file=item.file.clone();move |_|open.run(file.clone())}>
                                                <LibraryCover item=item.clone() />
                                                <CardInfo name=display_title(&item.file.name) detail=Signal::derive(move ||detail.clone()) />
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

#[component]
fn LibraryCover(item: LibraryItem) -> impl IntoView {
    let failed = RwSignal::new(false);
    let kind = item.kind.clone();
    let class = format!("library-cover {}-cover", kind);
    let is_video = item.kind == "video";
    let duration = RwSignal::new(
        item.duration_ms
            .filter(|ms| *ms > 0)
            .map(|ms| format_media_time(ms as f64 / 1000.0)),
    );
    let thumbnail_url = format!("/api/files/{}/thumbnail?v={}", item.file.id, item.file.etag);
    let retry = RwSignal::new(0_u8);
    let retry_thumbnail = move |_| {
        failed.set(true);
        if is_video
            && retry.get_untracked() < 4
            && let Some(window) = web_sys::window()
        {
            use wasm_bindgen::{JsCast, closure::Closure};
            let callback = Closure::once_into_js(move || {
                if let Some(attempt) = retry.try_get_untracked() {
                    retry.set(attempt + 1);
                    failed.set(false);
                }
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                1500,
            );
        }
    };
    if is_video && duration.get_untracked().is_none() {
        let id = item.file.id.clone();
        leptos::task::spawn_local(async move {
            if let Ok(metadata) = api::fetch_video_media(&id).await
                && metadata.duration_ms > 0
            {
                let _ = duration.try_set(Some(format_media_time(
                    metadata.duration_ms as f64 / 1000.0,
                )));
            }
        });
    }
    view! {
        <div class=class>
            <Show when=move || !failed.get() fallback=move || view! {
                <div class="cover-placeholder"><span>{match kind.as_str() { "book" => view! { <span>"READ"</span> }.into_any(), "audio" => view! { <span>"♫"</span> }.into_any(), "video" => icons::video().into_any(), _ => icons::image().into_any() }}</span><strong>{display_title(&item.file.name)}</strong></div>
            }>
                <img loading="lazy" src={let url = thumbnail_url.clone(); move || format!("{}&retry={}", url, retry.get())} alt="" on:error=retry_thumbnail />
            </Show>
            {is_video.then(|| view! { <span class="video-cover-play">{icons::play()}</span><span class="video-duration">{move || duration.get().unwrap_or_else(|| "—:—".to_owned())}</span> })}
        </div>
    }
}

#[component]
fn HomeDashboard(
    refresh: RwSignal<u64>,
    on_open: Callback<File>,
    on_navigate: Callback<LibraryPage>,
    on_import: Callback<()>,
    on_unauthorized: Callback<()>,
) -> impl IntoView {
    let sections = RwSignal::new(Vec::<(LibraryPage, Vec<LibraryItem>)>::new());
    let selection = expect_context::<ShellContext>().selection;
    let error = RwSignal::new(String::new());
    let generation = RwSignal::new(0_u64);
    Effect::new(move |_| {
        let _ = refresh.get();
        generation.update(|g| *g += 1);
        let gen_id = generation.get_untracked();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let mut list = Vec::new();
            for page in [
                LibraryPage::Books,
                LibraryPage::Music,
                LibraryPage::Gallery,
                LibraryPage::Videos,
            ] {
                let base = LibraryQuery {
                    kind: page.kind().to_owned(),
                    ..Default::default()
                };
                match api::fetch_library(&base).await {
                    Ok(all) => {
                        let recent = if page != LibraryPage::Gallery {
                            api::fetch_library(&LibraryQuery {
                                recent: true,
                                ..base
                            })
                            .await
                            .ok()
                            .filter(|r| !r.items.is_empty())
                            .map(|r| r.items)
                        } else {
                            None
                        };
                        list.push((
                            page,
                            recent
                                .unwrap_or(all.items)
                                .into_iter()
                                .take(6)
                                .collect::<Vec<_>>(),
                        ));
                    }
                    Err(e) => {
                        if e.is_unauthorized() {
                            on_unauthorized.run(());
                        } else {
                            error.set(e.message);
                        }
                        return;
                    }
                }
            }
            if generation.try_get_untracked() == Some(gen_id) {
                error.set(String::new());
                selection.set_library_items(
                    list.iter()
                        .flat_map(|(_, items)| items.iter().map(|item| item.file.clone()))
                        .collect(),
                );
                sections.set(list);
            }
        });
    });
    view! {


        <For each=move ||sections.get() key=move |(p,_)|(p.path(),refresh.get_untracked()) children=move |(p,list)|view!{
            <section class="home-section"><header><div><h2>{match p{LibraryPage::Books=>"继续阅读",LibraryPage::Music=>"最近播放与收藏",LibraryPage::Videos=>"最近添加的视频",_=>"最近添加的图片"}}</h2></div><button on:click=move |_|on_navigate.run(p)>"查看全部"<span>"→"</span></button></header>
                <div class="home-content-row" class:selection-mode=move || selection.enabled.get() class:home-books=p==LibraryPage::Books class:home-images=matches!(p,LibraryPage::Gallery | LibraryPage::Videos)>
                    {if list.is_empty(){view!{<button class="home-empty" on:click=move |_|on_import.run(())>"还没有内容，导入你的第一份收藏 →"</button>}.into_any()}else{list.into_iter().map(|item|{let file=item.file.clone();let selected_id=file.id.clone();let card_background_id=file.id.clone();view!{<article class="home-card" on:click=move |event| selection.toggle_from_card_background(event, &card_background_id) class:selected=move || selection.ids.with(|ids| ids.contains(&selected_id))><SelectionCheckbox id=item.file.id.clone() name=item.file.name.clone() selection=selection /><button class="home-item" aria-label=format!("打开 {}",item.file.name) on:click=move |_|on_open.run(file.clone())><LibraryCover item=item.clone()/><CardInfo name=display_title(&item.file.name) detail=Signal::derive(move ||if item.last_opened.is_some(){"继续打开".to_owned()}else{"新加入你的内容库".to_owned()}) /></button></article>}}).collect_view().into_any()}}
                </div>
            </section>
        } />
        <Show when=move ||!error.get().is_empty() fallback=|| ()><div class="library-error" role="alert">{move ||error.get()}<button on:click=move |_|refresh.update(|r|*r+=1)>"重试"</button></div></Show>
    }
}
