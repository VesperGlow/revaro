//! Application bootstrap and authentication boundary.
//!
//! The browser starts with one small session check. Once it has a profile, the
//! authenticated file-browser component owns its folder and trash state; a
//! logout or an expired session returns to the login component without a full
//! page reload.

use leptos::prelude::*;
use revaro_core::api::auth::Session;

use crate::api;
use crate::components::{FileBrowser, LoginView};

/// Mount the application into the page body.
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let checking = RwSignal::new(true);
    let session = RwSignal::new(None::<Session>);

    leptos::task::spawn_local(async move {
        session.set(api::fetch_session().await);
        checking.set(false);
    });

    let on_login = Callback::new(move |profile: Session| session.set(Some(profile)));
    let on_logout = Callback::new(move |(): ()| session.set(None));

    view! {
        {move || {
            if checking.get() {
                view! {
                    <main class="splash" aria-live="polite">
                        <div class="brand-mark">
                            <img class="ui-image" src="/logo.png" alt="" draggable="false" />
                        </div>
                        <div class="spinner" aria-label="正在连接服务"></div>
                    </main>
                }
                .into_any()
            } else if let Some(profile) = session.get() {
                view! { <FileBrowser session=profile on_logout=on_logout.clone() /> }.into_any()
            } else {
                view! { <LoginView on_success=on_login.clone() /> }.into_any()
            }
        }}
    }
}
