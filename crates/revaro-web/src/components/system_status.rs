//! The live system-status disclosure used by the top bar.
//!
//! The old Vue component deliberately used the status SSE as its source of
//! truth. Keeping the stream lifecycle here (rather than polling from the
//! shell) preserves the status orb, reconnect behaviour and cleanup when the
//! authenticated shell is replaced after logout.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use leptos::prelude::*;
use revaro_core::api::system::Status;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Event, EventSource, MessageEvent};

use crate::browser;
use crate::logic::format::format_size;

type EventListener = Closure<dyn FnMut(Event)>;

struct StatusRuntime {
    disposed: Cell<bool>,
    source: RefCell<Option<EventSource>>,
    listeners: RefCell<Vec<EventListener>>,
    reconnect_timer: Cell<Option<i32>>,
    retry_delay_ms: Cell<u32>,
    status: RwSignal<Option<Status>>,
    error: RwSignal<String>,
}

impl StatusRuntime {
    fn new(status: RwSignal<Option<Status>>, error: RwSignal<String>) -> Self {
        Self {
            disposed: Cell::new(false),
            source: RefCell::new(None),
            listeners: RefCell::new(Vec::new()),
            reconnect_timer: Cell::new(None),
            retry_delay_ms: Cell::new(1_000),
            status,
            error,
        }
    }

    fn connect(self: &Rc<Self>) {
        if self.disposed.get() || self.source.borrow().is_some() {
            return;
        }
        self.clear_reconnect_timer();
        self.listeners.borrow_mut().clear();
        let source = match EventSource::new("/api/system/status/stream") {
            Ok(source) => source,
            Err(error) => {
                self.error.set(format!("状态流不可用：{error:?}"));
                self.schedule_reconnect();
                return;
            }
        };

        let status_runtime = Rc::clone(self);
        let status_listener = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(message) = event.dyn_ref::<MessageEvent>() else {
                return;
            };
            let Some(data) = message.data().as_string() else {
                status_runtime.error.set("状态数据不可用".to_owned());
                return;
            };
            match serde_json::from_str::<Status>(&data) {
                Ok(status) => {
                    status_runtime.status.set(Some(status));
                    status_runtime.error.set(String::new());
                }
                Err(_) => status_runtime.error.set("状态数据不可用".to_owned()),
            }
        });
        let _ = source
            .add_event_listener_with_callback("status", status_listener.as_ref().unchecked_ref());

        let open_runtime = Rc::clone(self);
        let open_listener = Closure::<dyn FnMut(Event)>::new(move |_| {
            open_runtime.retry_delay_ms.set(1_000);
            open_runtime.clear_reconnect_timer();
        });
        let _ =
            source.add_event_listener_with_callback("open", open_listener.as_ref().unchecked_ref());

        let error_runtime = Rc::clone(self);
        let error_listener = Closure::<dyn FnMut(Event)>::new(move |_| {
            error_runtime.event_source_failed();
        });
        let _ = source
            .add_event_listener_with_callback("error", error_listener.as_ref().unchecked_ref());

        *self.source.borrow_mut() = Some(source);
        let mut listeners = self.listeners.borrow_mut();
        listeners.push(status_listener);
        listeners.push(open_listener);
        listeners.push(error_listener);
    }

    fn event_source_failed(self: &Rc<Self>) {
        if self.disposed.get() {
            return;
        }
        if let Some(source) = self.source.borrow_mut().take() {
            source.close();
        }
        self.listeners.borrow_mut().clear();
        self.schedule_reconnect();
    }

    fn schedule_reconnect(self: &Rc<Self>) {
        if self.disposed.get() || self.reconnect_timer.get().is_some() {
            return;
        }
        let delay = self.retry_delay_ms.get();
        self.retry_delay_ms.set(delay.saturating_mul(2).min(30_000));
        let runtime = Rc::clone(self);
        let callback = Closure::once_into_js(move || {
            runtime.reconnect_timer.set(None);
            runtime.connect();
        });
        let Some(window) = web_sys::window() else {
            return;
        };
        if let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.unchecked_ref(),
            i32::try_from(delay).unwrap_or(i32::MAX),
        ) {
            self.reconnect_timer.set(Some(timer));
        }
    }

    fn clear_reconnect_timer(&self) {
        if let Some(timer) = self.reconnect_timer.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
    }

    fn dispose(&self) {
        if self.disposed.replace(true) {
            return;
        }
        self.clear_reconnect_timer();
        if let Some(source) = self.source.borrow_mut().take() {
            source.close();
        }
        self.listeners.borrow_mut().clear();
    }
}

/// The status orb and its live service overview.
#[component]
pub fn SystemStatus() -> impl IntoView {
    let status = RwSignal::new(None::<Status>);
    let error = RwSignal::new(String::new());
    let runtime = Rc::new(StatusRuntime::new(status, error));
    runtime.connect();

    let panel = NodeRef::<leptos::html::Details>::new();
    let outside_panel = panel;
    let mut outside = browser::on_pointerdown(move |event| {
        let Some(details) = outside_panel.get() else {
            return;
        };
        if !details.open() {
            return;
        }
        let inside = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            .is_some_and(|target| details.contains(Some(&target)));
        if !inside {
            details.set_open(false);
        }
    });
    let escape_panel = panel;
    let mut escape = browser::on_keydown(move |event| {
        if event.key() != "Escape" {
            return;
        }
        let Some(details) = escape_panel.get() else {
            return;
        };
        if details.open() {
            details.set_open(false);
            if let Ok(Some(summary)) = details.query_selector("summary")
                && let Ok(summary) = summary.dyn_into::<web_sys::HtmlElement>()
            {
                let _ = summary.focus();
            }
        }
    });
    let cleanup_runtime = leptos::__reexports::send_wrapper::SendWrapper::new(runtime);
    on_cleanup(move || {
        outside.release();
        escape.release();
        cleanup_runtime.take().dispose();
    });

    let overall_class = Signal::derive_local(move || {
        status
            .get()
            .map(|value| value.status)
            .unwrap_or_else(|| "pending".to_owned())
    });

    view! {
        <details node_ref=panel class=move || format!("system-status {}", overall_class.get())>
            <summary title="系统状态" aria-label="打开系统状态">
                <i aria-hidden="true"></i>
            </summary>
            <section class="status-panel">
                <header>
                    <div>
                        <strong>"系统状态"</strong>
                        <small>"存储使用量与核心服务概览"</small>
                    </div>
                    {move || status_badge(status.get())}
                </header>
                <Show
                    when=move || error.get().is_empty()
                    fallback=move || view! { <p class="status-error">{move || error.get()}</p> }
                >
                    <Show
                        when=move || status.get().is_some()
                        fallback=|| view! { <p class="status-empty">"正在获取状态…"</p> }
                    >
                        {move || status.get().map_or_else(
                            || ().into_any(),
                            |value| view! {
                                <div class="status-grid">
                                    <ServiceCard
                                        title="数据库"
                                        detail=format!("数据占用 {}", format_size(non_negative(value.database.bytes)))
                                        badge=state_label(&value.database.status)
                                        tone=status_tone(&value.database.status)
                                        icon=ServiceIcon::Database
                                    />
                                    <ServiceCard
                                        title="网盘存储使用量"
                                        detail=format!("{} 个文件 · 回收站 {}", value.storage.file_count, format_size(non_negative(value.storage.trash_bytes)))
                                        badge=format_size(non_negative(value.storage.bytes))
                                        tone=status_tone(&value.storage.status)
                                        icon=ServiceIcon::Cloud
                                    />
                                    <ServiceCard
                                        title="服务端缓存"
                                        detail=format!("内存 {} · 磁盘 {} · {}", format_size(non_negative(value.cache.memory_bytes)), format_size(non_negative(value.cache.disk_bytes)), cache_hit_label(&value))
                                        badge=state_label(&value.cache.status)
                                        tone=status_tone(&value.cache.status)
                                        icon=ServiceIcon::HardDrive
                                    />
                                </div>
                            }.into_any(),
                        )}
                    </Show>
                </Show>
            </section>
        </details>
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceIcon {
    Database,
    Cloud,
    HardDrive,
}

#[component]
fn ServiceCard(
    title: &'static str,
    detail: String,
    badge: String,
    tone: &'static str,
    icon: ServiceIcon,
) -> impl IntoView {
    view! {
        <article class=format!("service-card tone-{tone}")>
            <div class="service-card-head">
                <span class="service-icon" aria-hidden="true">{service_icon(icon)}</span>
                <span class=format!("status-badge tone-{tone} size-sm")>{badge}</span>
            </div>
            <div class="service-copy">
                <b>{title}</b>
                <small>{detail}</small>
            </div>
        </article>
    }
}

fn service_icon(icon: ServiceIcon) -> AnyView {
    match icon {
        ServiceIcon::Database => super::icons::database().into_any(),
        ServiceIcon::Cloud => super::icons::cloud().into_any(),
        ServiceIcon::HardDrive => super::icons::hard_drive().into_any(),
    }
}

fn status_badge(status: Option<Status>) -> AnyView {
    let (tone, label) = match status {
        None => ("neutral", "连接中"),
        Some(value) if value.status == "degraded" => ("warning", "部分服务异常"),
        Some(_) => ("success", "所有服务正常"),
    };
    view! { <span class=format!("status-badge size-md tone-{tone}")>{label}</span> }.into_any()
}

fn status_tone(status: &str) -> &'static str {
    match status {
        "critical" => "danger",
        "degraded" => "warning",
        _ => "success",
    }
}

fn state_label(status: &str) -> String {
    match status {
        "critical" => "异常".to_owned(),
        "degraded" => "需注意".to_owned(),
        _ => "正常".to_owned(),
    }
}

fn cache_hit_label(status: &Status) -> String {
    let (hits, misses) = status
        .cache
        .classes
        .as_ref()
        .map(|classes| {
            classes
                .values()
                .fold((0_i64, 0_i64), |(hits, misses), class| {
                    (hits + class.hits, misses + class.misses)
                })
        })
        .unwrap_or_default();
    let total = hits + misses;
    if total > 0 {
        format!(
            "命中 {}%（{total} 次读取）",
            (hits as f64 * 100.0 / total as f64).round() as i64
        )
    } else {
        "暂无读取".to_owned()
    }
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text_matches_the_reference_states() {
        assert_eq!(status_tone("ok"), "success");
        assert_eq!(status_tone("degraded"), "warning");
        assert_eq!(status_tone("critical"), "danger");
        assert_eq!(state_label("ok"), "正常");
        assert_eq!(state_label("degraded"), "需注意");
        assert_eq!(state_label("critical"), "异常");
    }
}
