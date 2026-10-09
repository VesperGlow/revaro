//! Manual grouping gestures and dialogs share the independent Stack API.
use std::collections::HashSet;

use revaro_core::stacks::{Stack, StackSuggestion};

use super::*;
use crate::components::selection::SelectionStackManagement;

#[derive(Clone)]
pub(super) enum StackDialog {
    Group(Vec<String>),
    Rename(String),
    Pick(String),
    Suggestions,
}

pub(super) enum StackOperation {
    Create(String, Vec<String>),
    Rename(String, String),
    Add(String, Vec<String>),
    Remove(String, Vec<String>),
    Dissolve(String),
}

#[derive(Clone, Copy)]
pub(super) struct StackController {
    pub all: RwSignal<Vec<Stack>>,
    pub dialog: RwSignal<Option<StackDialog>>,
    pub busy: RwSignal<bool>,
    pub name: RwSignal<String>,
    pub error: RwSignal<String>,
    pub candidates: RwSignal<Vec<File>>,
    pub pick_loading: RwSignal<bool>,
    pub picked: RwSignal<HashSet<String>>,
    pub suggestions: RwSignal<Vec<StackSuggestion>>,
    pub recommendation_loading: RwSignal<bool>,
    pub dragging: RwSignal<Option<String>>,
    pub run: Callback<StackOperation>,
    pub drop_on: Callback<LibraryItem>,
    pub recommend: Callback<()>,
    pub add_books: Callback<()>,
    pub reorder: Callback<(String, String)>,
    pub order_notice: RwSignal<String>,
    pub selected: RwSignal<String>,
}

impl StackController {
    pub fn install(
        page: RwSignal<LibraryPage>,
        selected: RwSignal<String>,
        items: RwSignal<Vec<LibraryItem>>,
        selection: SelectionMode,
        refresh: RwSignal<u64>,
        logout: Callback<()>,
    ) -> Self {
        let all = RwSignal::new(Vec::<Stack>::new());
        let dialog = RwSignal::new(None::<StackDialog>);
        let busy = RwSignal::new(false);
        let name = RwSignal::new(String::new());
        let error = RwSignal::new(String::new());
        let candidates = RwSignal::new(Vec::<File>::new());
        let pick_loading = RwSignal::new(false);
        let pick_revision = RwSignal::new(0_u64);
        let picked = RwSignal::new(HashSet::<String>::new());
        let suggestions = RwSignal::new(Vec::<StackSuggestion>::new());
        let recommendation_loading = RwSignal::new(false);
        let dragging = RwSignal::new(None::<String>);
        let order_notice = RwSignal::new(String::new());
        let revision = RwSignal::new(0_u64);
        let recommendation_revision = RwSignal::new(0_u64);
        let owner = Owner::current().expect("stacks belong to the content shell");
        let run = Callback::new(move |operation: StackOperation| {
            if busy.get_untracked() {
                return;
            }
            busy.set(true);
            error.set(String::new());
            leptos::task::spawn_local(async move {
                let result = match operation {
                    StackOperation::Create(name, ids) => {
                        api::create_stack(&name, ids).await.map(|_| ())
                    }
                    StackOperation::Rename(id, name) => api::rename_stack(&id, &name).await,
                    StackOperation::Add(id, ids) => api::stack_members(&id, ids, true).await,
                    StackOperation::Remove(id, ids) => api::stack_members(&id, ids, false).await,
                    StackOperation::Dissolve(id) => api::dissolve_stack(&id).await,
                };
                match result {
                    Ok(()) => {
                        dialog.set(None);
                        selection.exit();
                        refresh.update(|r| *r += 1);
                    }
                    Err(e) if e.is_unauthorized() => logout.run(()),
                    Err(e) => error.set(e.message),
                }
                busy.set(false);
            });
        });
        // Stack refreshes have their own generation, so late responses cannot undo an edit.
        Effect::new(move |_| {
            let _ = refresh.get();
            let _ = page.get();
            revision.update(|r| *r += 1);
            let current = revision.get_untracked();
            leptos::task::spawn_local(async move {
                let result = api::fetch_stacks().await;
                if revision.try_get_untracked() != Some(current) {
                    return;
                }
                match result {
                    Ok(list) => {
                        if !selected.get_untracked().is_empty()
                            && !list.iter().any(|s| s.id == selected.get_untracked())
                        {
                            selection.exit();
                            selected.set(String::new());
                            route("/library", false);
                        }
                        all.set(list);
                    }
                    Err(e) if e.is_unauthorized() => logout.run(()),
                    Err(e) => error.set(e.message),
                }
            });
        });
        let reorder = Callback::new(move |(source, target): (String, String)| {
            if busy.get_untracked() || source == target {
                return;
            }
            let id = selected.get_untracked();
            let Some(stack) = all.get_untracked().into_iter().find(|s| s.id == id) else {
                return;
            };
            let mut files = stack.files.clone();
            let Some(from) = files.iter().position(|f| f.id == source) else {
                return;
            };
            let Some(to) = files.iter().position(|f| f.id == target) else {
                return;
            };
            let moved = files.remove(from);
            files.insert(to, moved);
            let ids = files.iter().map(|f| f.id.clone()).collect::<Vec<_>>();
            // Use the complete Stack order, including members outside the current filter/page.
            // Only the visible cards move; membership, shelves and categories are untouched.
            all.update(|stacks| {
                if let Some(stack) = stacks.iter_mut().find(|s| s.id == id) {
                    stack.files = files;
                }
            });
            items.update(|items| {
                items.sort_by_key(|item| ids.iter().position(|id| id == &item.file.id))
            });
            revision.update(|r| *r += 1);
            busy.set(true);
            error.set(String::new());
            order_notice.set("正在保存顺序…".to_owned());
            leptos::task::spawn_local(async move {
                match api::reorder_stack(&id, ids).await {
                    Ok(()) => order_notice.set("顺序已保存".to_owned()),
                    Err(e) => {
                        all.update(|stacks| {
                            if let Some(current) = stacks.iter_mut().find(|s| s.id == id) {
                                current.files = stack.files;
                            }
                        });
                        // Refetch the current view: its filter/page may have changed while saving.
                        refresh.update(|r| *r += 1);
                        order_notice.set("顺序保存失败，已恢复".to_owned());
                        if e.is_unauthorized() {
                            logout.run(());
                        } else {
                            error.set(e.message);
                        }
                    }
                }
                busy.set(false);
            });
        });
        let on_stack = Callback::new(move |()| {
            let ids = selection
                .selected_files()
                .into_iter()
                .filter(revaro_core::classify::is_book)
                .map(|f| f.id)
                .collect::<Vec<_>>();
            if ids.is_empty() {
                return;
            }
            name.set("新堆叠".to_owned());
            error.set(String::new());
            dialog.set(Some(StackDialog::Group(ids)));
        });
        selection.stacks.set(Some(SelectionStackManagement {
            busy,
            on_stack,
            on_remove: Callback::new(move |()| {
                let ids = selection
                    .selected_files()
                    .into_iter()
                    .filter(revaro_core::classify::is_book)
                    .map(|f| f.id)
                    .collect::<Vec<_>>();
                let id = selected.get_untracked();
                if !id.is_empty() && !ids.is_empty() {
                    run.run(StackOperation::Remove(id, ids));
                }
            }),
            can_remove: Signal::derive(move || {
                page.get() == LibraryPage::Books && !selected.get().is_empty()
            }),
        }));
        let drop_on = Callback::new(move |item: LibraryItem| {
            let Some(source) = dragging.get_untracked() else {
                return;
            };
            dragging.set(None);
            if item.kind != "book" || source == item.file.id {
                return;
            }
            let destination = item
                .stack
                .map(|s| s.id)
                .unwrap_or_else(|| selected.get_untracked());
            if !destination.is_empty() {
                run.run(StackOperation::Add(destination, vec![source]));
            } else {
                run.run(StackOperation::Create(
                    "新堆叠".to_owned(),
                    vec![item.file.id, source],
                ));
            }
        });
        let recommend = Callback::new(move |()| {
            if busy.get_untracked() {
                return;
            }
            error.set(String::new());
            suggestions.set(Vec::new());
            dialog.set(Some(StackDialog::Suggestions));
            recommendation_loading.set(true);
            recommendation_revision.update(|r| *r += 1);
            let current = recommendation_revision.get_untracked();
            // The refresh button may unmount when loading starts. Attach the
            // task to the persistent shell, and only poll while this dialog lives.
            owner.with(|| {
                leptos::task::spawn_local_scoped_with_cancellation(async move {
                    for attempt in 0..20 {
                        let result = api::fetch_stack_suggestions().await;
                        if recommendation_revision.try_get_untracked() != Some(current) {
                            return;
                        }
                        if !matches!(
                            dialog.try_get_untracked().flatten(),
                            Some(StackDialog::Suggestions)
                        ) {
                            let _ = recommendation_loading.try_set(false);
                            return;
                        }
                        match result {
                            Ok((list, pending)) => {
                                suggestions.set(list);
                                if !pending || attempt == 19 {
                                    break;
                                }
                            }
                            Err(e) if e.is_unauthorized() => {
                                logout.run(());
                                return;
                            }
                            Err(e) => {
                                error.set(e.message);
                                break;
                            }
                        }
                        browser::delay(500).await;
                    }
                    recommendation_loading.set(false);
                })
            });
        });
        let add_books = Callback::new(move |()| {
            if busy.get_untracked() {
                return;
            }
            let id = selected.get_untracked();
            picked.set(HashSet::new());
            candidates.set(Vec::new());
            error.set(String::new());
            dialog.set(Some(StackDialog::Pick(id.clone())));
            pick_loading.set(true);
            pick_revision.update(|r| *r += 1);
            let current = pick_revision.get_untracked();
            leptos::task::spawn_local(async move {
                let result: Result<_, api::RequestError> = async {
                    let stacks = api::fetch_stacks().await?;
                    let existing = stacks
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| s.files.iter().map(|f| f.id.clone()).collect::<HashSet<_>>())
                        .unwrap_or_default();
                    let mut request = LibraryQuery {
                        kind: "book".to_owned(),
                        ..Default::default()
                    };
                    let mut books = Vec::new();
                    loop {
                        let batch = api::fetch_library(&request).await?;
                        let count = batch.items.len();
                        request.offset += count as i64;
                        books.extend(
                            batch
                                .items
                                .into_iter()
                                .map(|i| i.file)
                                .filter(|f| !existing.contains(&f.id)),
                        );
                        if count == 0 || request.offset >= batch.total {
                            break;
                        }
                    }
                    Ok((stacks, books))
                }
                .await;
                if pick_revision.try_get_untracked() != Some(current)
                    || !matches!(dialog.get_untracked(), Some(StackDialog::Pick(ref active)) if active == &id)
                {
                    return;
                }
                match result {
                    Ok((stacks, books)) => {
                        revision.update(|r| *r += 1);
                        all.set(stacks);
                        candidates.set(books);
                    }
                    Err(e) if e.is_unauthorized() => logout.run(()),
                    Err(e) => error.set(e.message),
                }
                let _ = pick_loading.try_set(false);
            });
        });
        Self {
            all,
            dialog,
            busy,
            name,
            error,
            candidates,
            pick_loading,
            picked,
            suggestions,
            recommendation_loading,
            dragging,
            run,
            drop_on,
            recommend,
            add_books,
            reorder,
            order_notice,
            selected,
        }
    }

    pub fn rename(self) {
        if let Some(stack) = self
            .all
            .get_untracked()
            .into_iter()
            .find(|s| s.id == self.selected.get_untracked())
        {
            self.name.set(stack.name);
            self.error.set(String::new());
            self.dialog.set(Some(StackDialog::Rename(stack.id)));
        }
    }
}

#[component]
pub(super) fn StackDialogs(controller: StackController) -> impl IntoView {
    let c = controller;
    let close = Callback::new(move |()| {
        if !c.busy.get_untracked() {
            c.dialog.set(None);
        }
    });
    view! {
        <Show when=move ||c.dialog.get().is_some() fallback=|| ()>
            <Show when=move ||!matches!(c.dialog.get(),Some(StackDialog::Pick(_))) fallback=move ||view! {
                {move || match c.dialog.get() {
                    Some(StackDialog::Pick(id)) => view! { <super::book_picker::StackBookPicker controller=c stack_id=id /> }.into_any(),
                    _ => ().into_any(),
                }}
            }>
            <DialogBackdrop on_close=close><section class="modal stack-dialog" role="dialog" aria-modal="true" aria-label="管理堆叠">
                <header><h2>{move ||match c.dialog.get() {
                    Some(StackDialog::Group(_))=>"堆叠书籍", Some(StackDialog::Rename(_))=>"重命名堆叠",
                    Some(StackDialog::Pick(_))=>"添加书籍", _=>"推荐堆叠",
                }}</h2><button aria-label="关闭堆叠对话框" disabled=move ||c.busy.get() on:click=move |_|close.run(())>"×"</button></header>
                <p>"堆叠只整理展示；书架、分类和原文件保持原样。"</p>
                {move ||c.dialog.get().map(|state| match state {
                    StackDialog::Group(ids) => {
                        let create_ids = ids.clone();
                        view! {
                            <p>{format!("已选择 {} 本书；加入已有堆叠会移出原堆叠。", ids.len())}</p>
                            <div class="stack-options">{c.all.get().into_iter().map(|stack| {
                                let ids = ids.clone();
                                view! { <button type="button" disabled=move ||c.busy.get() on:click=move |_|c.run.run(StackOperation::Add(stack.id.clone(),ids.clone()))>{stack.name}<small>{format!("{} 本",stack.files.len())}</small></button> }
                            }).collect_view()}</div>
                            <Show when=move ||{create_ids.len()>=2} fallback=|| view! { <p>"选择至少两本书可新建堆叠。"</p> }>
                                <form on:submit={let ids=ids.clone();move |ev|{ev.prevent_default();c.run.run(StackOperation::Create(c.name.get_untracked(),ids.clone()));}}>
                                    <label>"堆叠名称"<input aria-label="堆叠名称" maxlength="80" prop:value=move ||c.name.get() on:input=move |ev|c.name.set(event_target_value(&ev)) /></label>
                                    <footer><button class="primary" type="submit" disabled=move ||c.busy.get() ||c.name.get().trim().is_empty()>"新建堆叠"</button></footer>
                                </form>
                            </Show>
                        }.into_any()
                    },
                    StackDialog::Rename(id) => view! {
                        <form on:submit=move |ev|{ev.prevent_default();c.run.run(StackOperation::Rename(id.clone(),c.name.get_untracked()));}>
                            <label>"堆叠名称"<input aria-label="堆叠名称" maxlength="80" prop:value=move ||c.name.get() on:input=move |ev|c.name.set(event_target_value(&ev)) /></label>
                            <footer><button class="primary" type="submit" disabled=move ||c.busy.get() ||c.name.get().trim().is_empty()>"保存名称"</button></footer>
                        </form>
                    }.into_any(),
                    StackDialog::Pick(_) => ().into_any(),
                    StackDialog::Suggestions => view! {
                        <p>"根据系列信息推荐，选择后可编辑名称并手动创建。"</p>
                        <div class="stack-options"><For each=move ||c.suggestions.get() key=|s|s.name.clone() children=move |suggestion|view! {
                            <button type="button" disabled=move ||c.busy.get() on:click=move |_| {
                                c.name.set(suggestion.name.chars().take(80).collect());
                                c.dialog.set(Some(StackDialog::Group(suggestion.file_ids.clone())));
                            }>{suggestion.name.clone()}<small>{format!("{} 本",suggestion.file_ids.len())}</small></button>
                        } /></div>
                        <Show when=move ||!c.recommendation_loading.get() && c.suggestions.get().is_empty() fallback=|| ()><p>"暂无可推荐的同系列书籍。新导入书籍的系列信息会在后台整理，稍后可重新打开查看；你也可以手动堆叠。"</p><button type="button" on:click=move |_|c.recommend.run(())>"刷新推荐"</button></Show>
                        <Show when=move ||c.recommendation_loading.get() fallback=|| ()><p role="status">"正在读取推荐，系列信息会自动更新…"</p></Show>
                    }.into_any(),
                })}
                <Show when=move ||c.busy.get() fallback=|| ()><p role="status">"正在处理…"</p></Show>
                <Show when=move ||!c.error.get().is_empty() fallback=|| ()><p class="form-error" role="alert">{move ||c.error.get()}</p></Show>
            </section></DialogBackdrop>
            </Show>
        </Show>
    }
}
