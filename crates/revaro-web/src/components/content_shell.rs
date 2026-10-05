//! Personal content library, with file tools kept mounted during navigation.
mod book_picker;
mod cards;
mod collections;
mod home;
mod loading;
mod songs;
mod stack_management;
mod stacks;

pub(super) use cards::BookProgressBar;
use cards::LibraryCover;
use home::*;
use stack_management::{StackGesture, StackManagementBar};
use stacks::{StackDialogs, StackOperation};

use leptos::prelude::*;
use revaro_core::{
    api::auth::Session,
    library::{Collection, ItemUpdate, LibraryItem},
    model::{File, FileKind},
};
use wasm_bindgen::{JsCast, JsValue};

use super::{
    FileBrowser,
    dialogs::DialogBackdrop,
    file_card::{FileCard, file_icon},
    icons,
    media::MediaPreview,
    menu::{ActionMenu, MenuIcon},
    music_player::{MusicController, PersistentMusicPlayer},
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

fn stack_id(path: &str) -> String {
    crate::logic::routing::stack_id(path)
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
    let selected_stack = RwSignal::new(stack_id(&pathname()));
    let collections = RwSignal::new(Vec::<Collection>::new());
    let items = RwSignal::new(Vec::<LibraryItem>::new());
    let total = RwSignal::new(0_i64);
    let loading = RwSignal::new(false);
    let more_loading = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let notice = RwSignal::new(String::new());
    let generation = RwSignal::new(0_u64);
    let reader = RwSignal::new(None::<File>);
    let reader_revision = RwSignal::new(0_u64);
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
    let stack_controller = stacks::StackController::install(
        page,
        selected_stack,
        items,
        selection,
        refresh,
        on_logout,
    );
    let stack_gesture = StackGesture::install(stack_controller, selection);
    let selection_overlay = Signal::derive(move || {
        reader.get().is_some()
            || image.get().is_some()
            || new_collection.get()
            || collection_target.get().is_some()
            || stack_controller.dialog.get().is_some()
    });
    let selection_scope = Memo::new(move |_| {
        (
            page.get(),
            query.get(),
            favorites.get(),
            selected_collection.get(),
            selected_stack.get(),
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
        reader_revision.update(|revision| *revision += 1);
        reader.set(None);
        image.set(None);
        selected_stack.set(String::new());
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
    }
    .install();
    let open = Callback::new(move |file: File| {
        if selection.enabled.get_untracked() {
            selection.toggle(&file.id);
            return;
        }
        if revaro_core::classify::is_book(&file) {
            reader_revision.update(|revision| *revision += 1);
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
            if revaro_core::classify::is_video(&file) {
                music.pause();
            }
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
            // The preview records each displayed image/video, including gallery navigation.
            return;
        }
        let id = file.id;
        leptos::task::spawn_local(async move {
            let result = api::update_library_item(
                &id,
                &ItemUpdate {
                    favorite: None,
                    opened: true,
                },
            )
            .await;
            if result.is_ok() && page.try_get_untracked() == Some(LibraryPage::Home) {
                refresh.update(|r| *r += 1);
            }
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
        reader_revision.update(|revision| *revision += 1);
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
    let restore_reader = Callback::new(move |(path, pushed): (String, bool)| {
        reader_revision.update(|revision| *revision += 1);
        reader.set(None);
        image.set(None);
        let Some(id) = reader_id(&path) else {
            return;
        };
        reader_pushed.set(pushed);
        let id = js_sys::decode_uri_component(&id)
            .ok()
            .and_then(|s| s.as_string())
            .unwrap_or_default();
        let revision = reader_revision.get_untracked();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let result = api::fetch_file(&id).await;
            // Navigation can leave and revisit this URL while a request is pending.
            if reader_revision.try_get_untracked() != Some(revision) || pathname() != path {
                return;
            }
            match result {
                Ok(detail) if revaro_core::classify::is_book(&detail.file) => {
                    let id = detail.file.id.clone();
                    reader.set(Some(detail.file));
                    let _ = api::update_library_item(
                        &id,
                        &ItemUpdate {
                            favorite: None,
                            opened: true,
                        },
                    )
                    .await;
                }
                Err(e) if e.is_unauthorized() => logout.run(()),
                _ => {
                    page.set(LibraryPage::Books);
                    error.set("这本书已不可用".to_owned());
                    route("/library", false);
                }
            }
        });
    });
    if reader_id(&pathname()).is_some() {
        reader_return.set("/library".to_owned());
        restore_reader.run((pathname(), false));
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
        selected_stack.set(stack_id(&path));
        query_text.set(query.get_untracked());
        restore_reader.run((path, true));
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
            query.set(query_text.get_untracked());
        }),
    };
    let open_stack = Callback::new(move |id: String| {
        selected_stack.set(id.clone());
        route(
            &format!("/library/stacks/{}", js_sys::encode_uri_component(&id)),
            true,
        );
        if let Some(window) = web_sys::window() {
            window.scroll_to_with_x_and_y(0.0, 0.0);
        }
    });
    let close_stack = Callback::new(move |()| {
        selection.exit();
        selected_stack.set(String::new());
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
                <main class="library-main" class:music-page=move || page.get()==LibraryPage::Music on:click=move |event| selection.exit_from_blank(event)>
                    <Show when=move ||selected_stack.get().is_empty() fallback=|| ()>
                        {move || selection.actions.get().map(|actions| view! { <BatchActionBar selection=selection actions=actions /> })}
                    </Show>
                    <Show when=move ||page.get()==LibraryPage::Home fallback=move ||view! {

                        <Show when=move ||selected_stack.get().is_empty() fallback=move ||view! {
                            <header class="stack-header">
                                <button type="button" class="stack-icon-button" aria-label="返回书架" title="返回书架" on:click=move |_|close_stack.run(())>{icons::arrow_left()}</button>
                                <div class="stack-header-title"><h1>{move ||stack_controller.all.get().into_iter().find(|s|s.id==selected_stack.get()).map(|s|s.name).unwrap_or_else(||"堆叠".to_owned())}</h1><small>{move ||format!("{} 本书",total.get())}</small></div>
                                <div class="stack-header-actions">
                                    <button type="button" class="stack-icon-button" aria-label="添加书籍" title="添加书籍" disabled=move ||stack_controller.busy.get() on:click=move |_|stack_controller.add_books.run(())>{icons::plus()}</button>
                                    <ActionMenu label="堆叠更多操作".to_owned() icon=MenuIcon::MoreVertical disabled=Signal::derive(move ||stack_controller.busy.get())>
                                        <button type="button" data-close-menu="true" on:click=move |_|stack_controller.rename()>"重命名"</button>
                                        <button type="button" data-close-menu="true" on:click=move |_|{selection.clear();selection.enabled.set(true);stack_controller.order_notice.set(String::new());}>"管理书籍"</button>
                                        <button type="button" class="danger" data-close-menu="true" on:click=move |_|stack_controller.run.run(StackOperation::Dissolve(selected_stack.get_untracked()))>"解除堆叠"</button>
                                    </ActionMenu>
                                </div>
                            </header>
                            <StackManagementBar controller=stack_controller selection=selection />
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
                                <Show when=move ||page.get()==LibraryPage::Books fallback=|| ()><button class="secondary stack-recommend" disabled=move ||stack_controller.busy.get() on:click=move |_|stack_controller.recommend.run(())>"推荐堆叠"</button></Show>
                                <button class="secondary collection-create" type="button"
                                    title=move || format!("新建{}", page.get().collection_label())
                                    aria-label=move || format!("新建{}", page.get().collection_label())
                                    on:click=move |_| { new_collection.set(true); error.set(String::new()); }>{icons::plus()}</button>
                                <Show when=move || page.get() == LibraryPage::Music fallback=|| ()><button class="primary library-play" type="button" title="播放全部" aria-label="播放全部" on:click=move |_| play_all.run(())>{icons::play()}</button></Show>
                            </div>
                        </div>
                        <Show when=move ||!selected_collection.get().is_empty() fallback=|| ()><div class="collection-caption"><span>"集合中的内容仍保存在原文件夹，移除成员不会删除原文件。"</span><button on:click=move |_|delete_collection.run(())>"删除集合"</button></div></Show>
                        </Show>
                        <Show when=move ||loading.get() fallback=move ||view! {
                            <Show when=move ||items.get().is_empty() && error.get().is_empty() fallback=|| ()><div class="library-empty"><span>"＋"</span><h2>"这里等着你的收藏"</h2><p>"已有文件会自动出现在对应内容库，也可以现在导入。"</p><button class="primary" on:click=move |_|import.run(())>"导入内容"</button></div></Show>
                            <div class="library-grid" class:selection-mode=move || selection.enabled.get() class:book-grid=move ||page.get()==LibraryPage::Books class:song-list=move ||page.get()==LibraryPage::Music class:photo-grid=move ||matches!(page.get(),LibraryPage::Gallery | LibraryPage::Videos) class:video-grid=move ||page.get()==LibraryPage::Videos>
                                <For each=move || { items.get().into_iter().enumerate().collect::<Vec<_>>() }
                                    key=|(_,i)|(i.file.id.clone(),i.favorite,i.file.name.clone(),i.file.etag.clone(),i.reading_progress.map(f64::to_bits),i.stack.as_ref().map(|s|(s.id.clone(),s.name.clone(),s.files.iter().map(|f|f.id.clone()).collect::<Vec<_>>())))
                                    children=move |(index,item)| {
                                        if item.kind == "audio" {
                                            return view! {
                                                <songs::SongRow item=item index=index on_open=open busy=collection_busy.into()
                                                    on_collect=Callback::new(move |file| collection_target.set(Some(CollectionTarget { page: LibraryPage::Music, files: vec![file] })))
                                                    on_favorite=Callback::new(move |(file, favorite): (File, bool)| {
                                                        if collection_busy.get_untracked() { return; }
                                                        collection_busy.set(true);
                                                        error.set(String::new());
                                                        leptos::task::spawn_local(async move {
                                                            match api::update_library_item(&file.id, &ItemUpdate { favorite: Some(favorite), opened: false }).await {
                                                                Ok(()) => refresh.update(|value| *value += 1),
                                                                Err(e) if e.is_unauthorized() => logout.run(()),
                                                                Err(e) => error.set(e.message),
                                                            }
                                                            collection_busy.set(false);
                                                        });
                                                    }) />
                                            }.into_any();
                                        }
                                        let file_id = item.file.id.clone();
                                        let group = item.selectable_files().into_iter().map(|f|f.id).collect::<Vec<_>>();
                                        let selected_group = group.clone();
                                        let background_group = group.clone();
                                        let is_stack = item.stack.is_some();
                                        let name = if is_stack { item.stack.as_ref().unwrap().name.clone() } else { item.file.name.clone() };
                                        let detail = if is_stack { format!("{} 本书", item.stack.as_ref().unwrap().files.len()) }
                                            else if item.kind == "book" {
                                                let progress = item.reading_progress.map(|p|format!("已读 {p:.1}%")).unwrap_or_else(|| "未读".to_owned());
                                                progress
                                            } else if item.kind == "audio" { format!("{} · 本地音乐",revaro_core::classify::extension(&item.file.name).to_uppercase()) }
                                            else { format_date(&item.file.created_at.to_rfc3339()) };
                                        let aria = if is_stack { format!("展开堆叠 {name}，{} 本书",item.stack.as_ref().unwrap().files.len()) } else { format!("打开 {}",item.file.name) };
                                        let open_item = item.clone();
                                        let cover_item = item.clone();
                                        let open_group = group.clone();
                                        let drag_item = item.clone();
                                        let drop_item = item.clone();
                                        let drag_file_id = item.file.id.clone();
                                        let order_file_id = item.file.id.clone();
                                        let gesture_file_id = item.file.id.clone();
                                        let target_file_id = item.file.id.clone();
                                        let stack_book_id = item.file.id.clone();
                                        let drag_hover = RwSignal::new(false);
                                        let draggable = item.kind=="book" && !is_stack;
                                        view! {
                                            <article class="library-card" class:stack-card=is_stack data-file-id=item.file.id.clone() data-stack-id=item.stack.as_ref().map(|s|s.id.clone()) data-selection-ids=serde_json::to_string(&group).unwrap_or_default()
                                                data-stack-book-id=move ||(!selected_stack.get().is_empty() && draggable).then(||stack_book_id.clone())
                                                draggable=move ||(draggable && selected_stack.get().is_empty() && !selection.enabled.get() && !stack_controller.busy.get()).to_string()
                                                class:stack-drop-target=move ||drag_hover.get() && stack_controller.dragging.get().is_some()
                                                class:stack-drag-source=move ||stack_gesture.dragging.get().as_ref()==Some(&gesture_file_id)
                                                class:stack-order-target=move ||stack_gesture.target.get().as_ref()==Some(&target_file_id)
                                                on:dragstart=move |ev: web_sys::DragEvent| {
                                                    if !draggable || !selected_stack.get_untracked().is_empty() || selection.enabled.get_untracked() { ev.prevent_default(); return; }
                                                    if let Some(data) = ev.data_transfer() {
                                                        let _ = data.set_data("application/x-revaro-book", &drag_file_id);
                                                        data.set_effect_allowed("move");
                                                        stack_controller.dragging.set(Some(drag_file_id.clone()));
                                                    }
                                                }
                                                on:dragend=move |_|{stack_controller.dragging.set(None);drag_hover.set(false);}
                                                on:dragover=move |ev: web_sys::DragEvent| {
                                                    if drag_item.kind=="book" && stack_controller.dragging.get_untracked().is_some_and(|id|id!=drag_item.file.id) {
                                                        ev.prevent_default();
                                                        if let Some(data)=ev.data_transfer(){data.set_drop_effect("move");}
                                                        drag_hover.set(true);
                                                    }
                                                }
                                                on:dragleave=move |_|drag_hover.set(false)
                                                on:drop=move |ev: web_sys::DragEvent| {ev.prevent_default();drag_hover.set(false);stack_controller.drop_on.run(drop_item.clone());}
                                                on:click=move |event|selection.toggle_group_from_card_background(event,&background_group)
                                                class:selected=move ||selection.ids.with(|ids|selected_group.iter().all(|id|ids.contains(id)))
                                                class:is-playing=move ||music.current().is_some_and(|f|f.id==file_id)>
                                                <SelectionCheckbox id=item.file.id.clone() name=name.clone() group=group.clone() selection=selection />
                                                <button class="library-card-open" aria-label=aria aria-describedby=move ||(selection.enabled.get() && !selected_stack.get().is_empty()).then_some("stack-order-hint")
                                                    on:keydown=move |event: web_sys::KeyboardEvent| {
                                                        if !event.alt_key() || !selection.enabled.get_untracked() || selected_stack.get_untracked().is_empty() { return; }
                                                        let direction = match event.key().as_str() { "ArrowLeft" | "ArrowUp"=>-1, "ArrowRight" | "ArrowDown"=>1, _=>return };
                                                        event.prevent_default();event.stop_propagation();
                                                        let books=items.get_untracked();
                                                        if let Some(index)=books.iter().position(|item|item.file.id==order_file_id)
                                                            && let Some(next)=index.checked_add_signed(direction)
                                                            && let Some(target)=books.get(next) {
                                                            stack_controller.reorder.run((order_file_id.clone(),target.file.id.clone()));
                                                        }
                                                    }
                                                    on:click=move |_| {
                                                    if selection.enabled.get_untracked() { selection.toggle_group(&open_group); }
                                                    else if is_stack { open_stack.run(open_item.stack.as_ref().unwrap().id.clone()); }
                                                    else { open.run(open_item.file.clone()); }
                                                }>
                                                    <FileCard name=name detail=Signal::derive(move ||detail.clone())>
                                                        <cards::StackCover item=cover_item.clone() />
                                                        {cover_item.stack.as_ref().map(|s|view! { <span class="stack-count">{format!("{} 本",s.files.len())}</span> })}
                                                    </FileCard>
                                                </button>
                                            </article>
                                        }.into_any()
                                    } />
                            </div>
                            <Show when=move || { (items.get().len() as i64)<total.get() } fallback=|| ()><div class="library-load-more"><button class="secondary" disabled=move ||more_loading.get() on:click=move |_|load.run(true)>{move ||if more_loading.get(){"正在加载…"}else{"加载更多"}}</button><small>{move ||format!("已显示 {} / {}",items.get().len(),total.get())}</small></div></Show>
                        }><div class="library-loading"><div class="spinner"></div><p>"正在打开内容库…"</p></div></Show>
                    }>
                        <HomeDashboard items=items refresh=refresh on_open=open on_navigate=navigate on_unauthorized=logout />
                    </Show>
                    <Show when=move ||!error.get().is_empty() fallback=|| ()><div class="library-error" role="alert">{move ||error.get()}<button on:click=move |_|{refresh.update(|r|*r+=1);}>"重试"</button></div></Show>
                    <Show when=move ||!stack_controller.error.get().is_empty() && stack_controller.dialog.get().is_none() fallback=|| ()><div class="library-error" role="alert">{move ||stack_controller.error.get()}<button on:click=move |_|{stack_controller.error.set(String::new());refresh.update(|r|*r+=1);}>"重试"</button></div></Show>
                    <Show when=move ||!notice.get().is_empty() fallback=|| ()><div class="library-notice" role="status">{move ||notice.get()}<button aria-label="关闭提示" on:click=move |_|notice.set(String::new())>"×"</button></div></Show>
                </main>
            </Show>
            <FileBrowser session=session on_logout=on_logout on_username_changed=on_username_changed on_password_changed=on_password_changed on_header_ready=Callback::new(move |actions|header_actions.set(Some(actions))) />
            <StackDialogs controller=stack_controller />
            <PersistentMusicPlayer controller=music />
            <AppNavigation page=page on_navigate=navigate mobile=true />
            <Show when=move ||reader.get().is_some() fallback=|| ()>{move ||reader.get().map(|file|view!{<ReaderView file=file on_close=close_reader on_unauthorized=on_logout />})}</Show>
            <Show when=move ||image.get().is_some() fallback=|| ()><MediaPreview selected=image items=image_items on_close=Callback::new(move |()|{image.set(None);refresh.update(|r|*r+=1);}) on_download=Callback::new(|file:File|super::file_browser::download_file(&file)) on_move=Callback::new(move |file:File|{image.set(None);transfer.set(Some((file,false)));}) on_copy=Callback::new(move |file:File|{image.set(None);transfer.set(Some((file,true)));}) /></Show>
            <Show when=move ||new_collection.get() ||collection_target.get().is_some() fallback=|| ()>
                <DialogBackdrop on_close=Callback::new(move |()| {new_collection.set(false);collection_target.set(None);})><section class="modal library-collection-dialog" role="dialog" aria-modal="true" aria-label="管理集合">
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
                </section></DialogBackdrop>
            </Show>
        </div>
    }
}
