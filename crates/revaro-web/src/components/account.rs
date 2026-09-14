//! Account settings and security flows from the reference client.
//!
//! The account entry is deliberately a modal surface rather than a logout
//! shortcut. Besides the profile controls, the old client exposed avatar,
//! password and TOTP recovery-code workflows here; keeping them together also
//! makes the session boundary explicit when a password is changed.

use leptos::ev::{MouseEvent, SubmitEvent};
use leptos::prelude::*;
use revaro_core::api::auth::{
    AvatarRequest, ChangePasswordRequest, ChangeUsernameRequest, PasswordCodeRequest,
    PasswordRequest,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Event, File as BrowserFile, FileReader, HtmlElement, HtmlInputElement};

use super::dialogs::ActionDialog;
use super::icons;
use crate::api;
use crate::logic::feedback::Feedback;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountPanel {
    Password,
    Totp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TotpStage {
    Idle,
    Setup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmAction {
    RegenerateRecoveryCodes,
    DisableTotp,
}

/// The reference account-settings modal.
#[component]
pub fn AccountSettings(
    open: RwSignal<bool>,
    username: RwSignal<String>,
    has_avatar: RwSignal<bool>,
    avatar_version: RwSignal<u64>,
    on_logout: Callback<()>,
    on_username_changed: Callback<String>,
    on_password_changed: Callback<String>,
    on_notify: Callback<Feedback>,
    on_close: Callback<()>,
) -> impl IntoView {
    let account_username = RwSignal::new(username.get_untracked());
    let username_editing = RwSignal::new(false);
    let username_saving = RwSignal::new(false);
    let username_error = RwSignal::new(String::new());
    let username_input = NodeRef::<leptos::html::Input>::new();

    let avatar_busy = RwSignal::new(false);
    let avatar_error = RwSignal::new(String::new());
    let avatar_input = NodeRef::<leptos::html::Input>::new();

    let panel = RwSignal::new(None::<AccountPanel>);
    let password_current = RwSignal::new(String::new());
    let password_new = RwSignal::new(String::new());
    let password_confirm = RwSignal::new(String::new());
    let password_busy = RwSignal::new(false);
    let password_error = RwSignal::new(String::new());

    let totp_enabled = RwSignal::new(false);
    let recovery_remaining = RwSignal::new(0_i64);
    let totp_loading = RwSignal::new(true);
    let totp_busy = RwSignal::new(false);
    let totp_stage = RwSignal::new(TotpStage::Idle);
    let totp_current_password = RwSignal::new(String::new());
    let totp_code = RwSignal::new(String::new());
    let totp_secret = RwSignal::new(String::new());
    let totp_qr_data_url = RwSignal::new(String::new());
    let recovery_codes = RwSignal::new(Vec::<String>::new());
    let recovery_copied = RwSignal::new(false);
    let totp_error = RwSignal::new(String::new());
    let confirm_action = RwSignal::new(None::<ConfirmAction>);
    let confirm_value = RwSignal::new(String::new());

    let modal = NodeRef::<leptos::html::Section>::new();

    let close = on_close.clone();
    let close_panel = {
        let panel = panel;
        let password_current = password_current;
        let password_new = password_new;
        let password_confirm = password_confirm;
        let password_error = password_error;
        let totp_stage = totp_stage;
        let totp_current_password = totp_current_password;
        let totp_code = totp_code;
        let totp_secret = totp_secret;
        let totp_qr_data_url = totp_qr_data_url;
        let recovery_codes = recovery_codes;
        let recovery_copied = recovery_copied;
        let totp_error = totp_error;
        Callback::new(move |(): ()| {
            panel.set(None);
            password_current.set(String::new());
            password_new.set(String::new());
            password_confirm.set(String::new());
            password_error.set(String::new());
            totp_stage.set(TotpStage::Idle);
            totp_current_password.set(String::new());
            totp_code.set(String::new());
            totp_secret.set(String::new());
            totp_qr_data_url.set(String::new());
            recovery_codes.set(Vec::new());
            recovery_copied.set(false);
            totp_error.set(String::new());
        })
    };

    let load_totp = {
        let totp_enabled = totp_enabled;
        let recovery_remaining = recovery_remaining;
        let totp_loading = totp_loading;
        let totp_error = totp_error;
        Callback::new(move |(): ()| {
            totp_loading.set(true);
            totp_error.set(String::new());
            leptos::task::spawn_local(async move {
                match api::fetch_totp_status().await {
                    Ok(status) => {
                        totp_enabled.set(status.enabled);
                        recovery_remaining.set(status.recovery_codes.max(0));
                    }
                    Err(error) => totp_error.set(error.message),
                }
                totp_loading.set(false);
            });
        })
    };
    load_totp.run(());

    let start_username_edit = {
        let account_username = account_username;
        let username = username;
        let username_editing = username_editing;
        let username_input = username_input;
        let username_saving = username_saving;
        let username_error = username_error;
        Callback::new(move |(): ()| {
            if username_saving.get_untracked() {
                return;
            }
            account_username.set(username.get_untracked());
            username_error.set(String::new());
            username_editing.set(true);
            if let Some(window) = web_sys::window() {
                let callback = Closure::once_into_js(move || {
                    if let Some(input) = username_input.get() {
                        let _ = input.focus();
                        let _ = input.select();
                    }
                });
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.unchecked_ref(),
                    0,
                );
            }
        })
    };
    let cancel_username_edit = {
        let account_username = account_username;
        let username = username;
        let username_editing = username_editing;
        let username_error = username_error;
        Callback::new(move |(): ()| {
            account_username.set(username.get_untracked());
            username_error.set(String::new());
            username_editing.set(false);
        })
    };
    let save_username = {
        let account_username = account_username;
        let username = username;
        let username_editing = username_editing;
        let username_saving = username_saving;
        let username_error = username_error;
        let on_notify = on_notify.clone();
        let on_username_changed = on_username_changed.clone();
        Callback::new(move |(): ()| {
            if !username_editing.get_untracked() || username_saving.get_untracked() {
                return;
            }
            let value = account_username.get_untracked().trim().to_owned();
            if value.is_empty() {
                username_error.set("用户名不能为空".to_owned());
                return;
            }
            if value == username.get_untracked() {
                username_editing.set(false);
                username_error.set(String::new());
                return;
            }
            username_saving.set(true);
            username_error.set(String::new());
            let on_notify = on_notify.clone();
            leptos::task::spawn_local(async move {
                let request = ChangeUsernameRequest {
                    username: value.clone(),
                };
                match api::change_username(&request).await {
                    Ok(()) => {
                        account_username.set(value.clone());
                        username.set(value.clone());
                        username_editing.set(false);
                        on_username_changed.run(value);
                        on_notify.run(Feedback::success("用户名已保存"));
                    }
                    Err(error) => username_error.set(error.message),
                }
                username_saving.set(false);
            });
        })
    };

    let choose_avatar = {
        let avatar_input = avatar_input;
        Callback::new(move |(): ()| {
            if let Some(input) = avatar_input.get() {
                input.click();
            }
        })
    };
    let upload_avatar = {
        let avatar_busy = avatar_busy;
        let avatar_error = avatar_error;
        let has_avatar = has_avatar;
        let avatar_version = avatar_version;
        let on_notify = on_notify.clone();
        Callback::new(move |file: BrowserFile| {
            avatar_error.set(String::new());
            let accepted = ["image/jpeg", "image/png", "image/gif", "image/webp"];
            if !accepted.iter().any(|mime| *mime == file.type_()) {
                avatar_error.set("请选择 JPG、PNG、GIF 或 WebP 图片".to_owned());
                return;
            }
            if file.size() > 2.0 * 1024.0 * 1024.0 {
                avatar_error.set("头像不能超过 2 MiB".to_owned());
                return;
            }
            let reader = match FileReader::new() {
                Ok(reader) => reader,
                Err(_) => {
                    avatar_error.set("无法读取图片".to_owned());
                    return;
                }
            };
            avatar_busy.set(true);
            let reader_for_load = reader.clone();
            let avatar_busy_for_load = avatar_busy;
            let avatar_error_for_load = avatar_error;
            let has_avatar_for_load = has_avatar;
            let avatar_version_for_load = avatar_version;
            let on_notify_for_load = on_notify.clone();
            let onload = Closure::<dyn FnMut(Event)>::new(move |_| {
                let result = reader_for_load
                    .result()
                    .ok()
                    .and_then(|value| value.as_string());
                let Some(data_url) = result else {
                    avatar_busy_for_load.set(false);
                    avatar_error_for_load.set("无法读取图片".to_owned());
                    return;
                };
                leptos::task::spawn_local(async move {
                    match api::update_avatar(&AvatarRequest { data_url }).await {
                        Ok(()) => {
                            has_avatar_for_load.set(true);
                            avatar_version_for_load
                                .update(|version| *version = version.wrapping_add(1));
                            on_notify_for_load.run(Feedback::success("头像已更新"));
                        }
                        Err(error) => avatar_error_for_load.set(error.message),
                    }
                    avatar_busy_for_load.set(false);
                });
            });
            let avatar_busy_for_error = avatar_busy;
            let avatar_error_for_error = avatar_error;
            let onerror = Closure::<dyn FnMut(Event)>::new(move |_| {
                avatar_busy_for_error.set(false);
                avatar_error_for_error.set("无法读取图片".to_owned());
            });
            reader.set_onload(Some(onload.as_ref().unchecked_ref()));
            reader.set_onerror(Some(onerror.as_ref().unchecked_ref()));
            if reader
                .read_as_data_url(file.unchecked_ref::<web_sys::Blob>())
                .is_err()
            {
                avatar_busy.set(false);
                avatar_error.set("无法读取图片".to_owned());
            }
            onload.forget();
            onerror.forget();
        })
    };
    let avatar_changed = {
        let upload_avatar = upload_avatar.clone();
        move |event: Event| {
            let input = event
                .target()
                .and_then(|target| target.dyn_into::<HtmlInputElement>().ok());
            if let Some(input) = input {
                if let Some(files) = input.files()
                    && let Some(file) = files.get(0)
                {
                    upload_avatar.run(file);
                }
                input.set_value("");
            }
        }
    };
    let remove_avatar = {
        let avatar_busy = avatar_busy;
        let avatar_error = avatar_error;
        let has_avatar = has_avatar;
        let avatar_version = avatar_version;
        let on_notify = on_notify.clone();
        Callback::new(move |(): ()| {
            avatar_busy.set(true);
            avatar_error.set(String::new());
            let on_notify = on_notify.clone();
            leptos::task::spawn_local(async move {
                match api::delete_avatar().await {
                    Ok(()) => {
                        has_avatar.set(false);
                        avatar_version.update(|version| *version = version.wrapping_add(1));
                        on_notify.run(Feedback::success("头像已移除"));
                    }
                    Err(error) => avatar_error.set(error.message),
                }
                avatar_busy.set(false);
            });
        })
    };

    let open_password = {
        let panel = panel;
        let password_current = password_current;
        let password_new = password_new;
        let password_confirm = password_confirm;
        let password_error = password_error;
        Callback::new(move |(): ()| {
            panel.set(Some(AccountPanel::Password));
            password_current.set(String::new());
            password_new.set(String::new());
            password_confirm.set(String::new());
            password_error.set(String::new());
        })
    };
    let save_password = {
        let password_current = password_current;
        let password_new = password_new;
        let password_confirm = password_confirm;
        let password_busy = password_busy;
        let password_error = password_error;
        let panel = panel;
        let open = open;
        let username = username;
        let on_password_changed = on_password_changed.clone();
        Callback::new(move |(): ()| {
            password_error.set(String::new());
            let new_password = password_new.get_untracked();
            if new_password.chars().count() < 12 {
                password_error.set("新密码至少需要 12 个字符".to_owned());
                return;
            }
            if new_password != password_confirm.get_untracked() {
                password_error.set("两次输入的新密码不一致".to_owned());
                return;
            }
            password_busy.set(true);
            let username = username.get_untracked();
            let on_password_changed = on_password_changed.clone();
            let request = ChangePasswordRequest {
                current_password: password_current.get_untracked(),
                password: new_password,
            };
            leptos::task::spawn_local(async move {
                match api::change_password(&request).await {
                    Ok(()) => {
                        panel.set(None);
                        open.set(false);
                        on_password_changed.run(username);
                    }
                    Err(error) => password_error.set(error.message),
                }
                password_busy.set(false);
            });
        })
    };

    let open_totp = {
        let panel = panel;
        let totp_current_password = totp_current_password;
        let totp_code = totp_code;
        let totp_error = totp_error;
        let totp_stage = totp_stage;
        let recovery_codes = recovery_codes;
        let recovery_copied = recovery_copied;
        Callback::new(move |(): ()| {
            panel.set(Some(AccountPanel::Totp));
            totp_current_password.set(String::new());
            totp_code.set(String::new());
            totp_error.set(String::new());
            totp_stage.set(TotpStage::Idle);
            recovery_codes.set(Vec::new());
            recovery_copied.set(false);
        })
    };
    let begin_totp_setup = {
        let totp_current_password = totp_current_password;
        let totp_error = totp_error;
        let totp_busy = totp_busy;
        let totp_stage = totp_stage;
        let totp_secret = totp_secret;
        let totp_qr_data_url = totp_qr_data_url;
        Callback::new(move |(): ()| {
            totp_error.set(String::new());
            let current_password = totp_current_password.get_untracked();
            if current_password.is_empty() {
                totp_error.set("请输入当前密码".to_owned());
                return;
            }
            totp_busy.set(true);
            leptos::task::spawn_local(async move {
                match api::begin_totp_setup(&PasswordRequest { current_password }).await {
                    Ok(setup) => {
                        totp_secret.set(setup.secret);
                        totp_qr_data_url.set(setup.qr_data_url);
                        totp_stage.set(TotpStage::Setup);
                    }
                    Err(error) => totp_error.set(error.message),
                }
                totp_busy.set(false);
            });
        })
    };
    let cancel_totp_setup = {
        let totp_stage = totp_stage;
        let totp_code = totp_code;
        let totp_secret = totp_secret;
        let totp_qr_data_url = totp_qr_data_url;
        let totp_error = totp_error;
        Callback::new(move |(): ()| {
            totp_stage.set(TotpStage::Idle);
            totp_code.set(String::new());
            totp_secret.set(String::new());
            totp_qr_data_url.set(String::new());
            totp_error.set(String::new());
        })
    };
    let enable_totp = {
        let totp_current_password = totp_current_password;
        let totp_code = totp_code;
        let totp_error = totp_error;
        let totp_busy = totp_busy;
        let totp_enabled = totp_enabled;
        let recovery_remaining = recovery_remaining;
        let recovery_codes = recovery_codes;
        let totp_stage = totp_stage;
        let totp_secret = totp_secret;
        let totp_qr_data_url = totp_qr_data_url;
        let on_notify = on_notify.clone();
        Callback::new(move |(): ()| {
            totp_error.set(String::new());
            let code = totp_code.get_untracked().trim().to_owned();
            if code.is_empty() {
                totp_error.set("请输入身份验证器中的六位验证码".to_owned());
                return;
            }
            totp_busy.set(true);
            let request = PasswordCodeRequest {
                current_password: totp_current_password.get_untracked(),
                code,
            };
            let on_notify = on_notify.clone();
            leptos::task::spawn_local(async move {
                match api::enable_totp(&request).await {
                    Ok(result) => {
                        totp_enabled.set(true);
                        recovery_remaining.set(result.recovery_codes.len() as i64);
                        recovery_codes.set(result.recovery_codes);
                        totp_stage.set(TotpStage::Idle);
                        totp_current_password.set(String::new());
                        totp_code.set(String::new());
                        totp_secret.set(String::new());
                        totp_qr_data_url.set(String::new());
                        on_notify.run(Feedback::success("两步验证已启用"));
                    }
                    Err(error) => totp_error.set(error.message),
                }
                totp_busy.set(false);
            });
        })
    };

    let request_regenerate = {
        let confirm_action = confirm_action;
        let totp_error = totp_error;
        let totp_current_password = totp_current_password;
        let totp_code = totp_code;
        Callback::new(move |(): ()| {
            totp_error.set(String::new());
            if totp_current_password.get_untracked().is_empty()
                || totp_code.get_untracked().trim().is_empty()
            {
                totp_error.set("请输入当前密码和验证码或恢复码".to_owned());
            } else {
                confirm_action.set(Some(ConfirmAction::RegenerateRecoveryCodes));
            }
        })
    };
    let request_disable = {
        let confirm_action = confirm_action;
        let totp_error = totp_error;
        let totp_current_password = totp_current_password;
        let totp_code = totp_code;
        Callback::new(move |(): ()| {
            totp_error.set(String::new());
            if totp_current_password.get_untracked().is_empty()
                || totp_code.get_untracked().trim().is_empty()
            {
                totp_error.set("请输入当前密码和验证码或恢复码".to_owned());
            } else {
                confirm_action.set(Some(ConfirmAction::DisableTotp));
            }
        })
    };
    let confirm_cancel = {
        let confirm_action = confirm_action;
        Callback::new(move |(): ()| confirm_action.set(None))
    };
    let confirm_yes = {
        let confirm_action = confirm_action;
        let totp_busy = totp_busy;
        let totp_error = totp_error;
        let totp_enabled = totp_enabled;
        let recovery_remaining = recovery_remaining;
        let recovery_codes = recovery_codes;
        let totp_current_password = totp_current_password;
        let totp_code = totp_code;
        let on_notify = on_notify.clone();
        Callback::new(move |_: String| {
            let Some(action) = confirm_action.get_untracked() else {
                return;
            };
            confirm_action.set(None);
            totp_busy.set(true);
            let request = PasswordCodeRequest {
                current_password: totp_current_password.get_untracked(),
                code: totp_code.get_untracked(),
            };
            let on_notify = on_notify.clone();
            leptos::task::spawn_local(async move {
                match action {
                    ConfirmAction::RegenerateRecoveryCodes => {
                        match api::regenerate_totp_recovery_codes(&request).await {
                            Ok(result) => {
                                recovery_remaining.set(result.recovery_codes.len() as i64);
                                recovery_codes.set(result.recovery_codes);
                                totp_current_password.set(String::new());
                                totp_code.set(String::new());
                                on_notify.run(Feedback::success("恢复码已重新生成"));
                            }
                            Err(error) => totp_error.set(error.message),
                        }
                    }
                    ConfirmAction::DisableTotp => match api::disable_totp(&request).await {
                        Ok(()) => {
                            totp_enabled.set(false);
                            recovery_remaining.set(0);
                            recovery_codes.set(Vec::new());
                            totp_current_password.set(String::new());
                            totp_code.set(String::new());
                            on_notify.run(Feedback::success("两步验证已关闭"));
                        }
                        Err(error) => totp_error.set(error.message),
                    },
                }
                totp_busy.set(false);
            });
        })
    };

    let copy_recovery = {
        let recovery_codes = recovery_codes;
        let recovery_copied = recovery_copied;
        let totp_error = totp_error;
        let on_notify = on_notify.clone();
        Callback::new(move |(): ()| {
            let text = recovery_codes.get_untracked().join("\n");
            let clipboard = web_sys::window().map(|window| window.navigator().clipboard());
            let Some(clipboard) = clipboard else {
                totp_error.set("复制失败，请手动保存恢复码".to_owned());
                return;
            };
            let promise = clipboard.write_text(&text);
            let on_notify = on_notify.clone();
            leptos::task::spawn_local(async move {
                if JsFuture::from(promise).await.is_ok() {
                    recovery_copied.set(true);
                    on_notify.run(Feedback::success("恢复码已复制"));
                } else {
                    totp_error.set("复制失败，请手动保存恢复码".to_owned());
                }
            });
        })
    };
    let download_recovery = {
        let recovery_codes = recovery_codes;
        Callback::new(move |(): ()| {
            let text = format!(
                "revaro 恢复码\n生成时间：{}\n\n{}\n",
                js_sys::Date::new_0().to_iso_string(),
                recovery_codes.get_untracked().join("\n")
            );
            let encoded = js_sys::encode_uri_component(&text)
                .as_string()
                .unwrap_or_default();
            let Some(document) = web_sys::window().and_then(|window| window.document()) else {
                return;
            };
            let Ok(anchor) = document.create_element("a") else {
                return;
            };
            let _ =
                anchor.set_attribute("href", &format!("data:text/plain;charset=utf-8,{encoded}"));
            let _ = anchor.set_attribute("download", "revaro-recovery-codes.txt");
            let _ = anchor.set_attribute("hidden", "");
            let Some(body) = document.body() else {
                return;
            };
            if body.append_child(&anchor).is_ok()
                && let Ok(anchor) = anchor.dyn_into::<HtmlElement>()
            {
                anchor.click();
                anchor.remove();
            }
        })
    };
    let logout = {
        let open = open;
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            open.set(false);
            let on_logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                api::logout().await;
                on_logout.run(());
            });
        })
    };

    view! {
        <div
            class="modal-backdrop accounting"
            role="presentation"
            on:click=move |event: MouseEvent| {
                if event.target() == event.current_target() && !password_busy.get_untracked() && !totp_busy.get_untracked() {
                    close.run(());
                }
            }
        >
            <section node_ref=modal class="modal account-modal" role="dialog" aria-modal="true" aria-labelledby="account-settings-title">
                <header>
                    <div><h2 id="account-settings-title">"账户设置"</h2></div>
                    <button type="button" aria-label="关闭" on:click=move |_| close.run(())>"×"</button>
                </header>
                <div class="account-layout">
                    <section class="avatar-settings">
                        <div class="avatar-large">
                            <Show
                                when=move || has_avatar.get()
                                fallback=move || view! { <span>{move || username.get().chars().next().unwrap_or('R').to_uppercase().collect::<String>()}</span> }
                            >
                                <img class="ui-image" src=move || format!("/api/profile/avatar?v={}", avatar_version.get()) alt="个人头像" draggable="false" />
                            </Show>
                        </div>
                        <h3>"个人头像"</h3>
                        <p>"支持 JPG、PNG、GIF 和 WebP，最大 2 MiB。"</p>
                        <div class="avatar-actions">
                            <button class="secondary" type="button" prop:disabled=move || avatar_busy.get() on:click=move |_| choose_avatar.run(())>
                                {move || if avatar_busy.get() { "处理中…" } else if has_avatar.get() { "更换头像" } else { "上传头像" }}
                            </button>
                            <Show when=move || has_avatar.get() fallback=|| ()>
                                <button class="danger-text" type="button" prop:disabled=move || avatar_busy.get() on:click=move |_| remove_avatar.run(())>"移除"</button>
                            </Show>
                        </div>
                        <input node_ref=avatar_input hidden type="file" accept="image/jpeg,image/png,image/gif,image/webp" on:change=avatar_changed />
                        <Show when=move || !avatar_error.get().is_empty() fallback=|| ()>
                            <p class="form-error">{move || avatar_error.get()}</p>
                        </Show>
                    </section>
                    <div class="account-overview">
                        <section class="account-setting-row identity-row">
                            <div class="setting-copy">
                                <span class="setting-label">"用户名"</span>
                                <div class="username-line">
                                    <Show
                                        when=move || username_editing.get()
                                        fallback=move || view! {
                                            <strong>{move || account_username.get()}</strong>
                                            <button class="edit-username" type="button" aria-label="编辑用户名" on:click=move |_| start_username_edit.run(())>
                                                {icons::edit()}
                                                <span>"编辑"</span>
                                            </button>
                                        }
                                    >
                                        <input
                                            node_ref=username_input
                                            class="username-input"
                                            type="text"
                                            autocomplete="username"
                                            maxlength="128"
                                            aria-label="用户名"
                                            autofocus
                                            prop:value=move || account_username.get()
                                            prop:disabled=move || username_saving.get()
                                            on:input=move |event| account_username.set(event_target_value(&event))
                                            on:blur=move |_| save_username.run(())
                                            on:keydown=move |event: web_sys::KeyboardEvent| {
                                                if event.key() == "Enter" {
                                                    event.prevent_default();
                                                    save_username.run(());
                                                } else if event.key() == "Escape" {
                                                    event.prevent_default();
                                                    cancel_username_edit.run(());
                                                }
                                            }
                                        />
                                        <Show when=move || username_saving.get() fallback=|| ()><small>"保存中…"</small></Show>
                                    </Show>
                                </div>
                                <Show when=move || !username_error.get().is_empty() fallback=|| ()>
                                    <p class="form-error username-error">{move || username_error.get()}</p>
                                </Show>
                            </div>
                            <button class="secondary password-entry" type="button" on:click=move |_| open_password.run(())>"修改密码"</button>
                        </section>

                        <section class="account-setting-row security-row">
                            <div class="setting-copy">
                                <div class="setting-title"><span class="setting-label">"两步验证"</span><span class:enabled=move || totp_enabled.get() class="security-badge">{move || if totp_enabled.get() { "已启用" } else { "未启用" }}</span></div>
                                <p>{move || if totp_enabled.get() { format!("身份验证器已启用，剩余 {} 枚恢复码。", recovery_remaining.get()) } else { "使用 TOTP 验证码保护管理员登录。".to_owned() }}</p>
                            </div>
                            <button class="secondary" type="button" prop:disabled=move || totp_loading.get() on:click=move |_| open_totp.run(())>
                                {move || if totp_loading.get() { "读取中…" } else if totp_enabled.get() { "管理" } else { "设置" }}
                            </button>
                        </section>
                        <section class="account-session-row">
                            <div><span class="setting-label">"当前会话"</span><p>"退出这台设备上的 Revaro 账户"</p></div>
                            <button type="button" on:click=move |_| logout.run(())>"退出登录"</button>
                        </section>
                        <Show when=move || !totp_error.get().is_empty() && panel.get().is_none() fallback=|| ()>
                            <p class="form-error">{move || totp_error.get()}</p>
                        </Show>
                    </div>
                </div>

                <Show when=move || panel.get() == Some(AccountPanel::Password) fallback=|| ()>
                    <div class="account-subdialog-backdrop" role="presentation" on:click=move |event: MouseEvent| if event.target() == event.current_target() { close_panel.run(()) }>
                        <section class="modal account-subdialog password-dialog" role="dialog" aria-modal="true">
                            <header><div><p class="eyebrow dark">"SECURITY"</p><h2>"修改密码"</h2><p class="subdialog-hint">"修改成功后，所有设备都需要使用新密码重新登录。"</p></div><button type="button" aria-label="关闭" on:click=move |_| close_panel.run(())>"×"</button></header>
                            <form on:submit=move |event: SubmitEvent| { event.prevent_default(); save_password.run(()) }>
                                <label>"当前密码"<input type="password" autocomplete="current-password" maxlength="1024" autofocus required prop:value=move || password_current.get() on:input=move |event| password_current.set(event_target_value(&event)) /></label>
                                <label>"新密码"<input type="password" autocomplete="new-password" minlength="12" maxlength="1024" required prop:value=move || password_new.get() on:input=move |event| password_new.set(event_target_value(&event)) /></label>
                                <label>"确认新密码"<input type="password" autocomplete="new-password" minlength="12" maxlength="1024" required prop:value=move || password_confirm.get() on:input=move |event| password_confirm.set(event_target_value(&event)) /></label>
                                <Show when=move || !password_error.get().is_empty() fallback=|| ()><p class="form-error">{move || password_error.get()}</p></Show>
                                <footer><button class="secondary" type="button" on:click=move |_| close_panel.run(())>"取消"</button><button class="primary" type="submit" prop:disabled=move || password_busy.get()>{move || if password_busy.get() { "正在修改…" } else { "修改密码" }}</button></footer>
                            </form>
                        </section>
                    </div>
                </Show>

                <Show when=move || panel.get() == Some(AccountPanel::Totp) fallback=|| ()>
                    <div class="account-subdialog-backdrop" role="presentation" on:click=move |event: MouseEvent| if event.target() == event.current_target() && !totp_busy.get_untracked() { close_panel.run(()) }>
                        <section class="modal account-subdialog totp-dialog" role="dialog" aria-modal="true">
                            <header><div><p class="eyebrow dark">"SECURITY"</p><h2>"两步验证"</h2><p class="subdialog-hint">"使用兼容 TOTP 的身份验证器保护管理员登录。"</p></div><button type="button" aria-label="关闭" on:click=move |_| close_panel.run(())>"×"</button></header>
                            <Show when=move || totp_loading.get() fallback=move || view! {
                                <>
                                    <Show when=move || !recovery_codes.get().is_empty() fallback=|| ()>
                                        <section class="recovery-panel"><div><strong>"立即保存恢复码"</strong><p>"每枚恢复码只能使用一次。关闭窗口后将无法再次查看。"</p></div><div class="recovery-grid"><For each=move || recovery_codes.get() key=|code| code.clone() let:code><code>{code}</code></For></div><div class="recovery-actions"><button class="secondary" type="button" on:click=move |_| copy_recovery.run(())>{move || if recovery_copied.get() { "已复制" } else { "复制恢复码" }}</button><button class="secondary" type="button" on:click=move |_| download_recovery.run(())>"下载文本"</button></div></section>
                                    </Show>
                                    <Show when=move || !totp_enabled.get() fallback=move || view! {
                                        <div class="two-factor-enabled"><p>"剩余 "<strong>{move || recovery_remaining.get()}</strong>" 枚恢复码。重新生成或关闭验证前，需要再次确认当前密码和验证码。"</p><div class="two-factor-fields"><label>"当前密码"<input type="password" autocomplete="current-password" maxlength="1024" prop:value=move || totp_current_password.get() on:input=move |event| totp_current_password.set(event_target_value(&event)) /></label><label>"验证码或恢复码"<input autocomplete="one-time-code" maxlength="128" prop:value=move || totp_code.get() on:input=move |event| totp_code.set(event_target_value(&event)) /></label></div><div class="two-factor-actions"><button class="secondary" type="button" prop:disabled=move || totp_busy.get() on:click=move |_| request_regenerate.run(())>"重新生成恢复码"</button><button class="danger-button" type="button" prop:disabled=move || totp_busy.get() on:click=move |_| request_disable.run(())>"关闭两步验证"</button></div></div>
                                    }>
                                        <Show when=move || totp_stage.get() == TotpStage::Idle fallback=move || view! {
                                            <div class="totp-enroll"><div class="totp-qr"><img src=move || totp_qr_data_url.get() alt="两步验证二维码" /></div><div class="totp-instructions"><h4>"扫描二维码"</h4><p>"用身份验证器扫描二维码，然后输入应用中显示的验证码完成绑定。"</p><p class="manual-secret">"无法扫码？手动输入密钥"<code>{move || totp_secret.get()}</code></p><label>"6 位验证码"<input autocomplete="one-time-code" inputmode="numeric" maxlength="8" placeholder="000000" prop:value=move || totp_code.get() on:input=move |event| totp_code.set(event_target_value(&event)) /></label><div class="two-factor-actions"><button class="secondary" type="button" prop:disabled=move || totp_busy.get() on:click=move |_| cancel_totp_setup.run(())>"返回"</button><button class="primary" type="button" prop:disabled=move || totp_busy.get() on:click=move |_| enable_totp.run(())>{move || if totp_busy.get() { "正在验证…" } else { "启用并生成恢复码" }}</button></div></div></div>
                                        }>
                                            <div class="two-factor-idle"><p>"启用后，登录时除密码外还需输入身份验证器生成的 6 位验证码。"</p><label>"当前密码"<input type="password" autocomplete="current-password" maxlength="1024" placeholder="确认是你本人" prop:value=move || totp_current_password.get() on:input=move |event| totp_current_password.set(event_target_value(&event)) /></label><button class="primary" type="button" prop:disabled=move || totp_busy.get() on:click=move |_| begin_totp_setup.run(())>{move || if totp_busy.get() { "正在生成…" } else { "开始设置" }}</button></div>
                                        </Show>
                                    </Show>
                                    <Show when=move || !totp_error.get().is_empty() fallback=|| ()><p class="form-error two-factor-error">{move || totp_error.get()}</p></Show>
                                </>
                            }>
                                <div class="two-factor-loading"><div class="spinner"></div><span>"正在读取安全设置…"</span></div>
                            </Show>
                        </section>
                    </div>
                </Show>
                {move || match confirm_action.get() {
                    Some(ConfirmAction::RegenerateRecoveryCodes) => view! { <ActionDialog title="重新生成恢复码？".to_owned() message="现有恢复码会立即全部失效，请保存新生成的恢复码。".to_owned() confirm_label="重新生成".to_owned() danger=false input=false placeholder=None value=confirm_value busy=totp_busy error=totp_error on_cancel=confirm_cancel.clone() on_confirm=confirm_yes.clone() /> }.into_any(),
                    Some(ConfirmAction::DisableTotp) => view! { <ActionDialog title="关闭两步验证？".to_owned() message="关闭后，只凭管理员密码即可登录。现有恢复码也会全部失效。".to_owned() confirm_label="关闭验证".to_owned() danger=true input=false placeholder=None value=confirm_value busy=totp_busy error=totp_error on_cancel=confirm_cancel.clone() on_confirm=confirm_yes.clone() /> }.into_any(),
                    None => ().into_any(),
                }}
            </section>
        </div>
    }
}
