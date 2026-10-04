//! Collection creation, membership and batch feedback.

use super::*;

pub(super) struct CollectionContext {
    pub page: RwSignal<LibraryPage>,
    pub selected_collection: RwSignal<String>,
    pub selected_series: RwSignal<String>,
    pub collection_target: RwSignal<Option<CollectionTarget>>,
    pub collection_name: RwSignal<String>,
    pub collection_busy: RwSignal<bool>,
    pub new_collection: RwSignal<bool>,
    pub selection: SelectionMode,
    pub refresh: RwSignal<u64>,
    pub error: RwSignal<String>,
    pub logout: Callback<()>,
}

pub(super) struct CollectionController {
    pub collection_page: Signal<LibraryPage>,
    pub create: Callback<()>,
    pub add_member: Callback<String>,
    pub delete_collection: Callback<()>,
}

impl CollectionController {
    pub fn install(context: CollectionContext) -> Self {
        let CollectionContext {
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
        } = context;
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
                    && selected_series.get().is_empty()
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
        Self {
            collection_page,
            create,
            add_member,
            delete_collection,
        }
    }
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
