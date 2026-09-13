//! Login view and its two-step authentication flow.

use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use revaro_core::api::auth::{LoginRequest, Session};

use crate::api;

/// The login page from which a successful session enters the file browser.
#[component]
pub fn LoginView(
    username: RwSignal<String>,
    notice: RwSignal<String>,
    on_success: Callback<Session>,
) -> impl IntoView {
    let password = RwSignal::new(String::new());
    let second_factor = RwSignal::new(String::new());
    let totp_required = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());

    let submit = move |event: SubmitEvent| {
        event.prevent_default();
        if busy.get() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        notice.set(String::new());
        let request = LoginRequest {
            username: username.get_untracked(),
            password: password.get_untracked(),
            second_factor: second_factor.get_untracked(),
        };

        leptos::task::spawn_local(async move {
            match api::login(&request).await {
                Ok(session) => {
                    password.set(String::new());
                    second_factor.set(String::new());
                    totp_required.set(false);
                    on_success.run(session);
                }
                Err(login_error) if login_error.requires_second_factor() => {
                    totp_required.set(true);
                    error.set("请输入身份验证器验证码或恢复码".to_owned());
                }
                Err(login_error) if login_error.second_factor_rejected() => {
                    totp_required.set(true);
                    error.set("验证码或恢复码不正确".to_owned());
                }
                Err(login_error) => error.set(login_error.message),
            }
            busy.set(false);
        });
    };

    view! {
        <main class="login-page">
            <section class="login-visual">
                <div class="glow glow-a"></div>
                <div class="glow glow-b"></div>
                <div class="visual-copy">
                    <span class="eyebrow">"PRIVATE · DIRECT · YOURS"</span>
                    <h1>
                        "你的文件，"
                        <br />
                        "安心地留在硬盘上。"
                    </h1>
                    <p>"轻量、自托管，文件存储在你的本地磁盘。"</p>
                </div>
                <div class="revaro-card">
                    <span aria-hidden="true">"☁"</span>
                    <div>
                        <strong>"本地文件存储"</strong>
                        <small>"SQLite 元数据 · 原生 Range"</small>
                    </div>
                </div>
            </section>
            <section class="login-panel">
                <form class="login-form" on:submit=submit>
                    <div class="logo">
                        <span class="brand-mark small">
                            <img class="ui-image" src="/logo.png" alt="" draggable="false" />
                        </span>
                        <span>"revaro"</span>
                    </div>
                    <div>
                        <p class="eyebrow dark">"WELCOME BACK"</p>
                        <h2>"登录私人空间"</h2>
                        <p class="muted">"首次启动的随机凭据可在容器日志中查看"</p>
                    </div>
                    <label>
                        "用户名"
                        <input
                            type="text"
                            name="username"
                            autocomplete="username"
                            maxlength="128"
                            required
                            prop:value=move || username.get()
                            on:input=move |event| username.set(event_target_value(&event))
                        />
                    </label>
                    <label>
                        "密码"
                        <input
                            type="password"
                            name="password"
                            autocomplete="current-password"
                            maxlength="1024"
                            required
                            prop:value=move || password.get()
                            on:input=move |event| password.set(event_target_value(&event))
                        />
                    </label>
                    <Show when=move || totp_required.get() fallback=|| ()>
                        <label>
                            "验证码或恢复码"
                            <input
                                type="text"
                                name="second_factor"
                                autocomplete="one-time-code"
                                maxlength="128"
                                placeholder="6 位验证码或恢复码"
                                required
                                prop:value=move || second_factor.get()
                                on:input=move |event| second_factor.set(event_target_value(&event))
                            />
                            <small class="login-totp-hint">
                                "打开身份验证器，或输入一枚尚未使用的恢复码。"
                            </small>
                        </label>
                    </Show>
                    <Show when=move || !notice.get().is_empty() fallback=|| ()>
                        <p class="form-success" aria-live="polite">
                            {move || notice.get()}
                        </p>
                    </Show>
                    <Show when=move || !error.get().is_empty() fallback=|| ()>
                        <p class="form-error" aria-live="assertive">
                            {move || error.get()}
                        </p>
                    </Show>
                    <button class="primary wide" type="submit" prop:disabled=move || busy.get()>
                        {move || if busy.get() { "正在验证…" } else { "进入我的网盘" }}
                    </button>
                </form>
            </section>
        </main>
    }
}
