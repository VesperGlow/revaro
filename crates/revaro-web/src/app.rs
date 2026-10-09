//! Application bootstrap and authentication boundary.
//!
//! The browser starts with one small session check. Once it has a profile, the
//! authenticated file-browser component owns its folder and trash state; a
//! logout or an expired session returns to the login component without a full
//! page reload.

use leptos::prelude::*;
use revaro_core::api::auth::Session;
use wasm_bindgen::JsCast as _;

use crate::api;
use crate::components::{ContentShell, LoginView};

/// Called by the bootstrap only after WASM, styles and transport are ready.
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn start() {
    console_error_panic_hook::set_once();
    let app = document()
        .get_element_by_id("app")
        .expect("HTML shell provides #app")
        .unchecked_into::<web_sys::HtmlElement>();
    app.set_inner_html("");
    leptos::mount::mount_to(app, App).forget();
}

#[component]
fn App() -> impl IntoView {
    let checking = RwSignal::new(false);
    let session_error = RwSignal::new(String::new());
    let session = RwSignal::new(None::<Session>);
    let login_username = RwSignal::new("admin".to_owned());
    let login_notice = RwSignal::new(String::new());

    let owner = Owner::current().expect("session check belongs to the app");
    let connect = Callback::new(move |()| {
        if checking.get_untracked() {
            return;
        }
        checking.set(true);
        session_error.set(String::new());
        owner.with(|| {
            leptos::task::spawn_local_scoped_with_cancellation(async move {
                match api::fetch_session().await {
                    Ok(profile) => session.set(profile),
                    Err(error) => session_error.set(error.message),
                }
                checking.set(false);
            })
        });
    });
    connect.run(());

    let on_login = {
        let session = session;
        Callback::new(move |profile: Session| session.set(Some(profile)))
    };
    let on_logout = Callback::new(move |(): ()| {
        crate::components::editor_draft::flush_all();
        crate::components::reader_cache::clear_all();
        session.set(None);
    });
    let on_username_changed = {
        let login_username = login_username;
        Callback::new(move |username: String| login_username.set(username))
    };
    let on_password_changed = {
        let session = session;
        let login_username = login_username;
        let login_notice = login_notice;
        Callback::new(move |username: String| {
            login_username.set(username);
            login_notice.set("密码已更新，请重新登录".to_owned());
            crate::components::reader_cache::clear_all();
            crate::components::editor_draft::clear_all();
            session.set(None);
        })
    };

    view! {
        {move || {
            if checking.get() {
                view! {
                    <main class="splash" aria-live="polite">
                        <img class="brand-logo ui-image" src="/revaro-logo.svg" alt="revaro" draggable="false" />
                        <div class="spinner" aria-label="正在连接服务"></div>
                    </main>
                }
                .into_any()
            } else if !session_error.get().is_empty() {
                view! {
                    <main class="splash" aria-live="polite">
                        <img class="brand-logo ui-image" src="/revaro-logo.svg" alt="revaro" draggable="false" />
                        <p role="alert">"暂时无法连接服务，请重试。"</p>
                        <button type="button" class="primary" on:click=move |_|connect.run(())>"重新连接"</button>
                    </main>
                }.into_any()
            } else if let Some(profile) = session.get() {
                view! {
                    <ContentShell
                        session=profile
                        on_logout=on_logout.clone()
                        on_username_changed=on_username_changed.clone()
                        on_password_changed=on_password_changed.clone()
                    />
                }
                .into_any()
            } else {
                view! {
                    <LoginView
                        username=login_username
                        notice=login_notice
                        on_success=on_login.clone()
                    />
                }
                .into_any()
            }
        }}
    }
}
