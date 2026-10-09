//! Confirmed file mutations and refresh/feedback ordering.

use super::*;

pub(super) struct FileActionContext {
    pub dialog: RwSignal<Option<DialogState>>,
    pub dialog_value: RwSignal<String>,
    pub dialog_busy: RwSignal<bool>,
    pub dialog_error: RwSignal<String>,
    pub selected_ids: RwSignal<HashSet<String>>,
    pub items: Signal<Vec<File>>,
    pub current_id: RwSignal<String>,
    pub trash_mode: Signal<bool>,
    pub notify: Callback<Feedback>,
    pub load_trash_request: Callback<TrashLoadRequest>,
    pub load_folder_request: Callback<FolderLoadRequest>,
    pub on_logout: Callback<()>,
    pub editor_open: RwSignal<bool>,
    pub editor_draft_key: Signal<String>,
    pub share_file: RwSignal<Option<File>>,
    pub share_sequence: RwSignal<u64>,
    pub share_active: RwSignal<bool>,
    pub share_url: RwSignal<String>,
    pub share_created_at: RwSignal<String>,
    pub share_expires_at: RwSignal<String>,
    pub share_expiry: RwSignal<String>,
    pub share_busy: RwSignal<bool>,
    pub share_error: RwSignal<String>,
    pub share_copied: RwSignal<bool>,
    pub nav_actions: RwSignal<Vec<NavAction>>,
    pub history_suppressed: RwSignal<bool>,
    pub upload_destination: Signal<String>,
}

impl FileActionContext {
    pub fn submit(self) -> Callback<String> {
        let Self {
            dialog,
            dialog_value,
            dialog_busy,
            dialog_error,
            selected_ids,
            items,
            current_id,
            trash_mode,
            notify,
            load_trash_request,
            load_folder_request,
            on_logout,
            editor_open,
            editor_draft_key,
            share_file,
            share_sequence,
            share_active,
            share_url,
            share_created_at,
            share_expires_at,
            share_expiry,
            share_busy,
            share_error,
            share_copied,
            nav_actions,
            history_suppressed,
            upload_destination,
        } = self;
        let submit_dialog = {
            let notify = notify.clone();
            let load_trash_request = load_trash_request.clone();
            let on_logout = on_logout.clone();
            let load_folder_request = load_folder_request.clone();
            Callback::new(move |value: String| {
                let Some(state) = dialog.get_untracked() else {
                    return;
                };
                if dialog_busy.get_untracked() {
                    return;
                }
                dialog_busy.set(true);
                dialog_error.set(String::new());
                let discard_editor = matches!(&state, DialogState::DiscardEditor);
                let rename_action = matches!(&state, DialogState::Rename { .. });
                let delete_action = matches!(&state, DialogState::Delete);
                let regenerate_share = matches!(&state, DialogState::RegenerateShare);
                let share_action = matches!(
                    &state,
                    DialogState::RegenerateShare | DialogState::RevokeShare
                );
                let share_session = share_sequence.get_untracked();
                let share_is_current = move || {
                    share_sequence.try_get_untracked() == Some(share_session)
                        && share_file.try_with_untracked(Option::is_some) == Some(true)
                };
                let selected = selected_ids.get_untracked();
                let delete_targets = items
                    .get_untracked()
                    .into_iter()
                    .filter(|item| selected.contains(&item.id))
                    .map(|item| (item.id, item.name))
                    .collect::<Vec<_>>();
                let parent_id = if matches!(state, DialogState::CreateFolder) {
                    upload_destination.get_untracked()
                } else {
                    current_id.get_untracked()
                };
                let refresh_parent_id = parent_id.clone();
                let in_trash = trash_mode.get_untracked();
                let logout = on_logout.clone();

                // The reference `confirmDialog`/`promptDialog` resolves and
                // removes AppDialog synchronously. The mutation continues after
                // the confirmation surface is gone; only RenameDialog stays
                // mounted while its save request is in flight.
                if !rename_action {
                    dialog.set(None);
                    dialog_value.set(String::new());
                    dialog_error.set(String::new());
                }

                leptos::task::spawn_local(async move {
                    let result: Result<String, api::RequestError> = async {
                        match state {
                            DialogState::CreateFolder => {
                                let name = value.trim().to_owned();
                                if name.is_empty() {
                                    Err(api::RequestError {
                                        status: 0,
                                        code: None,
                                        message: "文件夹名称不能为空".to_owned(),
                                    })
                                } else {
                                    api::create_directory_action(&CreateDirectoryRequest {
                                        parent_id,
                                        name,
                                    })
                                    .await
                                    .map(|_| "文件夹已创建".to_owned())
                                }
                            }
                            DialogState::DiscardEditor => Ok(String::new()),
                            DialogState::RegenerateShare => {
                                let Some(file) = share_file.get_untracked() else {
                                    return Err(api::RequestError {
                                        status: 0,
                                        code: None,
                                        message: "分享文件已关闭".to_owned(),
                                    });
                                };
                                share_busy.set(true);
                                let result = api::create_share(
                                    &file.id,
                                    share_expiry
                                        .get_untracked()
                                        .parse::<i64>()
                                        .ok()
                                        .filter(|v| *v > 0),
                                )
                                .await;
                                if !share_is_current() {
                                    return Ok(String::new());
                                }
                                match result {
                                    Ok(status) => {
                                        share_expires_at.set(
                                            status
                                                .expires_at
                                                .map(|v| v.to_rfc3339())
                                                .unwrap_or_default(),
                                        );
                                        share_active.set(status.active);
                                        share_url.set(status.url.unwrap_or_default());
                                        share_created_at.set(
                                            status
                                                .created_at
                                                .map(|value| value.to_rfc3339())
                                                .unwrap_or_default(),
                                        );
                                        share_copied.set(false);
                                        share_error.set(String::new());
                                        Ok("分享链接已重新生成".to_owned())
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            DialogState::RevokeShare => {
                                let Some(file) = share_file.get_untracked() else {
                                    return Err(api::RequestError {
                                        status: 0,
                                        code: None,
                                        message: "分享文件已关闭".to_owned(),
                                    });
                                };
                                share_busy.set(true);
                                api::revoke_share(&file.id).await?;
                                if !share_is_current() {
                                    return Ok(String::new());
                                }
                                share_active.set(false);
                                share_url.set(String::new());
                                share_created_at.set(String::new());
                                share_copied.set(false);
                                Ok("分享已停止".to_owned())
                            }
                            DialogState::Rename { id } => {
                                // RenameDialog in the reference forwards the
                                // original input verbatim. Server-side name
                                // validation remains authoritative, including
                                // leading/trailing whitespace and empty names.
                                api::patch_file(
                                    &id,
                                    &PatchFileRequest {
                                        name: Some(value),
                                        parent_id: None,
                                    },
                                )
                                .await
                                .map(|_| "已重命名".to_owned())
                            }
                            DialogState::Delete => {
                                let mut removed = 0_usize;
                                let mut errors = Vec::new();
                                for (id, name) in delete_targets {
                                    match api::delete_file(&id).await {
                                        Ok(()) => removed += 1,
                                        Err(error) if error.is_unauthorized() => return Err(error),
                                        Err(error) => {
                                            errors.push(format!("{name}：{}", error.message))
                                        }
                                    }
                                }
                                if !errors.is_empty() {
                                    return Err(api::RequestError {
                                        status: 500,
                                        code: None,
                                        message: format!(
                                            "已移入 {removed} 项，{} 项失败：{}",
                                            errors.len(),
                                            errors.join("；")
                                        ),
                                    });
                                }
                                Ok(format!("已将 {removed} 项移入回收站"))
                            }
                            DialogState::Purge => {
                                let mut removed = 0;
                                let mut errors = Vec::new();
                                for (id, name) in delete_targets {
                                    match api::purge_trash(&id).await {
                                        Ok(()) => removed += 1,
                                        Err(error) if error.is_unauthorized() => return Err(error),
                                        Err(error) => {
                                            errors.push(format!("{name}：{}", error.message))
                                        }
                                    }
                                }
                                if !errors.is_empty() {
                                    return Err(api::RequestError {
                                        status: 500,
                                        code: None,
                                        message: format!(
                                            "已删除 {removed} 项，{} 项失败：{}",
                                            errors.len(),
                                            errors.join("；")
                                        ),
                                    });
                                }
                                Ok(format!("已永久删除 {removed} 项"))
                            }
                            DialogState::EmptyTrash => {
                                api::empty_trash().await?;
                                Ok("回收站已清空".to_owned())
                            }
                        }
                    }
                    .await;

                    dialog_busy.set(false);
                    if share_action && !share_is_current() {
                        return;
                    }
                    share_busy.set(false);
                    match result {
                        Ok(message) => {
                            dialog.set(None);
                            dialog_value.set(String::new());
                            dialog_error.set(String::new());
                            if rename_action {
                                let _ = request_overlay_close(nav_actions, history_suppressed);
                            }
                            if share_action {
                                // The share dialog remains open; only its link state
                                // changes after the confirmation is dismissed.
                            } else if discard_editor {
                                super::super::editor_draft::remove(
                                    &editor_draft_key.get_untracked(),
                                );
                                selected_ids.set(HashSet::new());
                                if !request_overlay_close(nav_actions, history_suppressed) {
                                    editor_open.set(false);
                                }
                            } else if in_trash {
                                let (sender, receiver) = oneshot::channel();
                                load_trash_request.run(TrashLoadRequest {
                                    completion: Some(sender),
                                });
                                if receiver.await.unwrap_or(false) {
                                    // Keep the selection toolbar until the
                                    // reference-style trash refresh completes.
                                    selected_ids.set(HashSet::new());
                                }
                            } else {
                                selected_ids.set(HashSet::new());
                                let (sender, receiver) = oneshot::channel();
                                load_folder_request.run(FolderLoadRequest {
                                    id: refresh_parent_id,
                                    completion: Some(sender),
                                });
                                let _ = receiver.await;
                            }
                            // The reference keeps share regeneration and editor
                            // discard as local modal state transitions: neither
                            // emits or clears the global toast. Revoke-share and
                            // all ordinary successful mutations still do.
                            if !regenerate_share && !discard_editor {
                                notify.run(Feedback::success(message));
                            }
                        }
                        Err(request_error) if request_error.is_unauthorized() => {
                            dialog.set(None);
                            logout.run(());
                        }
                        Err(request_error) => {
                            if rename_action {
                                notify.run(Feedback::error(request_error.message));
                            } else if share_action {
                                // The reference confirm helper closes its own
                                // confirmation before the share request runs.
                                // Share failures are then rendered by the still
                                // open share dialog, rather than keeping the
                                // confirmation dialog on screen.
                                dialog.set(None);
                                dialog_value.set(String::new());
                                dialog_error.set(String::new());
                                share_error.set(request_error.message);
                            } else {
                                // `confirmDialog`/`promptDialog` resolve and
                                // close before the asynchronous mutation. Keep
                                // that old interaction: failures are a toast,
                                // not an inline error that traps the user in the
                                // action dialog.
                                dialog.set(None);
                                dialog_value.set(String::new());
                                dialog_error.set(String::new());
                                if delete_action {
                                    selected_ids.set(HashSet::new());
                                    let (sender, receiver) = oneshot::channel();
                                    load_folder_request.run(FolderLoadRequest {
                                        id: refresh_parent_id,
                                        completion: Some(sender),
                                    });
                                    let _ = receiver.await;
                                }
                                notify.run(Feedback::error(request_error.message));
                            }
                        }
                    }
                });
            })
        };
        submit_dialog
    }
}

pub(super) fn dialog_config(
    state: &DialogState,
    selected_count: usize,
    trash_count: usize,
) -> (String, String, String, bool, bool, Option<String>) {
    match state {
        DialogState::CreateFolder => (
            "新建文件夹".to_owned(),
            "给这个文件夹起个名字。".to_owned(),
            "创建".to_owned(),
            false,
            true,
            Some("文件夹名称".to_owned()),
        ),
        DialogState::DiscardEditor => (
            "放弃未保存的修改？".to_owned(),
            "关闭后，本次修改将无法恢复。".to_owned(),
            "放弃修改".to_owned(),
            true,
            false,
            None,
        ),
        DialogState::RegenerateShare => (
            "重新生成分享链接？".to_owned(),
            "旧分享链接会立即失效，拿到旧链接的人将无法继续访问。".to_owned(),
            "重新生成".to_owned(),
            false,
            false,
            None,
        ),
        DialogState::RevokeShare => (
            "停止分享？".to_owned(),
            "现有公开链接会立即失效。文件本身不会被删除。".to_owned(),
            "停止分享".to_owned(),
            true,
            false,
            None,
        ),
        DialogState::Rename { .. } => (
            "重命名".to_owned(),
            String::new(),
            "保存".to_owned(),
            false,
            true,
            Some("新名称".to_owned()),
        ),
        DialogState::Delete => (
            "移入回收站？".to_owned(),
            format!("选中的 {selected_count} 项会移入回收站，文件夹中的内容也会一起保留。"),
            "移入回收站".to_owned(),
            true,
            false,
            None,
        ),
        DialogState::Purge => (
            format!("永久删除 {selected_count} 项？"),
            "这个操作无法撤销。无引用的数据块会在垃圾回收后清理。".to_owned(),
            "永久删除".to_owned(),
            true,
            false,
            None,
        ),
        DialogState::EmptyTrash => (
            "清空回收站？".to_owned(),
            format!("回收站中的 {trash_count} 项及其内容都会永久删除，无法恢复。"),
            "清空回收站".to_owned(),
            true,
            false,
            None,
        ),
    }
}

pub(super) struct TrashRestoreContext {
    pub selected_ids: RwSignal<HashSet<String>>,
    pub items: RwSignal<Vec<File>>,
    pub load_trash_request: Callback<TrashLoadRequest>,
    pub notify: Callback<Feedback>,
    pub on_logout: Callback<()>,
}

impl TrashRestoreContext {
    pub fn callback(self) -> Callback<()> {
        let Self {
            selected_ids,
            items,
            load_trash_request,
            notify,
            on_logout,
        } = self;
        let restore_selected = {
            let load_trash_request = load_trash_request.clone();
            let notify = notify.clone();
            let on_logout = on_logout.clone();
            Callback::new(move |(): ()| {
                let targets = items
                    .get_untracked()
                    .into_iter()
                    .filter(|item| selected_ids.get_untracked().contains(&item.id))
                    .collect::<Vec<_>>();
                if targets.is_empty() {
                    return;
                }
                let logout = on_logout.clone();
                leptos::task::spawn_local(async move {
                    let mut restored = 0;
                    let mut errors = Vec::new();
                    let mut failed = HashSet::new();
                    for item in targets {
                        match api::restore_trash(&item.id).await {
                            Ok(()) => restored += 1,
                            Err(error) if error.is_unauthorized() => {
                                logout.run(());
                                return;
                            }
                            Err(error) => {
                                failed.insert(item.id);
                                errors.push(format!("{}：{}", item.name, error.message));
                            }
                        }
                    }
                    let (sender, receiver) = oneshot::channel();
                    load_trash_request.run(TrashLoadRequest {
                        completion: Some(sender),
                    });
                    let _ = receiver.await;
                    selected_ids.set(failed);
                    if errors.is_empty() {
                        notify.run(Feedback::success(format!("已恢复 {restored} 项")));
                    } else {
                        notify.run(Feedback::error(format!(
                            "已恢复 {restored} 项，{} 项失败：{}",
                            errors.len(),
                            errors.join("；")
                        )));
                    }
                });
            })
        };
        restore_selected
    }
}
