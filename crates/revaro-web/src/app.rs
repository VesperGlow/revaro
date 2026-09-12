//! The application shell.
//!
//! This is deliberately thin. It exists to prove the whole pipeline end to end —
//! Rust compiled to wasm, `wasm-bindgen` glue, the server's static handler, and
//! a real call into the shared HTTP contract — before the feature modules
//! (library, browser, reader, players, tasks) are migrated onto it.
//!
//! Note that the health response is parsed with [`revaro_core::api::Health`],
//! the same type the server serializes. That is the point of the shared crate:
//! there is no second, hand-written copy of the API shape in the client.

use gloo_net::http::Request;
use leptos::prelude::*;
use revaro_core::api::Health;

/// Where the shell is in its startup handshake with the server.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Connection {
    /// The request is in flight.
    Pending,
    /// The server answered and reported itself healthy.
    Ready,
    /// The request or the response was not usable.
    Failed(String),
}

/// Mount the application into the page body.
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let connection = RwSignal::new(Connection::Pending);

    // Runs once when the component is created.
    leptos::task::spawn_local(async move {
        connection.set(handshake().await);
    });

    view! {
        <main class="shell">
            <header class="shell__header">
                <h1 class="shell__title">"Revaro"</h1>
                <p class="shell__subtitle">"私人网盘"</p>
            </header>
            <section class="shell__status">
                {move || match connection.get() {
                    Connection::Pending => view! { <p class="status status--pending">"正在连接服务…"</p> }.into_any(),
                    Connection::Ready => view! { <p class="status status--ready">"服务已就绪"</p> }.into_any(),
                    Connection::Failed(message) => view! {
                        <p class="status status--failed">{format!("无法连接服务：{message}")}</p>
                    }.into_any(),
                }}
            </section>
            <footer class="shell__footer">{format!("客户端版本 {}", crate::VERSION)}</footer>
        </main>
    }
}

/// Ask the server how it is doing, using the shared contract type.
async fn handshake() -> Connection {
    let response = match Request::get("/healthz").send().await {
        Ok(response) => response,
        Err(error) => return Connection::Failed(error.to_string()),
    };
    if !response.ok() {
        return Connection::Failed(format!("HTTP {}", response.status()));
    }
    match response.json::<Health>().await {
        Ok(health) if health.status == "ok" => Connection::Ready,
        Ok(health) => Connection::Failed(format!("服务状态：{}", health.status)),
        Err(error) => Connection::Failed(error.to_string()),
    }
}
