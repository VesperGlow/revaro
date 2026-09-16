//! The authenticated background-task centre.
//!
//! The server deliberately keeps [`/api/events`](crate::api) opaque: an SSE
//! notification only says that jobs changed, and the browser reads the durable
//! task projection again. This component owns that refresh lifecycle, the
//! archive-password dialog and the actions for terminal task history. Uploads
//! remain in their own byte-transfer queue because their progress and
//! cancellation are browser-local.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use futures_util::future::join_all;
use leptos::prelude::*;
use revaro_core::api::tasks::TaskInputRequest;
use revaro_core::model::{Task, TaskStatus, task_type};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Event, EventSource};

use crate::api::{self, RequestError};
use crate::browser;
use crate::logic::feedback::Feedback;
use crate::logic::format::format_size;
use crate::logic::task_status::{
    is_active_task_status, task_display_name, task_progress_percent, task_progress_width,
    task_status_label, task_type_label,
};

const FALLBACK_INTERVAL_MS: i32 = 30_000;
const MAX_RECONNECT_DELAY_MS: u32 = 30_000;
const MAX_COMPLETED_DISPLAY: usize = 4;

type EventListener = Closure<dyn FnMut(Event)>;

/// The group whose row layout and actions are being rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskGroup {
    Active,
    Completed,
    Failed,
}

/// Browser handles belonging to one task-centre instance.
struct TaskRuntime {
    disposed: Cell<bool>,
    started: Cell<bool>,
    source: RefCell<Option<EventSource>>,
    event_listeners: RefCell<Vec<EventListener>>,
    reconnect_timer: Cell<Option<i32>>,
    fallback_timer: Cell<Option<i32>>,
    fallback_listener: RefCell<Option<Closure<dyn FnMut()>>>,
    retry_delay_ms: Cell<u32>,
    refresh_running: Cell<bool>,
    refresh_pending: Cell<bool>,
}

impl TaskRuntime {
    fn new() -> Self {
        Self {
            disposed: Cell::new(false),
            started: Cell::new(false),
            source: RefCell::new(None),
            event_listeners: RefCell::new(Vec::new()),
            reconnect_timer: Cell::new(None),
            fallback_timer: Cell::new(None),
            fallback_listener: RefCell::new(None),
            retry_delay_ms: Cell::new(1_000),
            refresh_running: Cell::new(false),
            refresh_pending: Cell::new(false),
        }
    }
}

/// State and controls shared by the task-centre view and its asynchronous
/// callbacks.
#[derive(Clone)]
pub struct TaskController {
    tasks: RwSignal<Vec<Task>>,
    loading: RwSignal<bool>,
    error: RwSignal<String>,
    password_task: RwSignal<Option<Task>>,
    password: RwSignal<String>,
    password_error: RwSignal<String>,
    show_all_completed: RwSignal<bool>,
    center: NodeRef<leptos::html::Details>,
    runtime: Rc<TaskRuntime>,
    on_logout: Callback<()>,
    on_refresh_folder: Callback<()>,
    feedback: Callback<Feedback>,
    on_upload_cancel: Option<Callback<String, bool>>,
    on_upload_retry: Option<Callback<String, bool>>,
}

/// Document-level listeners owned by one rendered task-centre disclosure.
///
/// The desktop and mobile top bars swap their `TaskCenter` child at the
/// responsive breakpoint, but they share the same controller and its SSE
/// connection.  These listeners therefore need a view-scoped lifetime that
/// is separate from the controller's application lifetime.
pub struct TaskViewListeners {
    outside: browser::OwnedListener,
    escape: browser::OwnedListener,
}

impl TaskViewListeners {
    fn new(outside: browser::OwnedListener, escape: browser::OwnedListener) -> Self {
        Self { outside, escape }
    }
}

impl Drop for TaskViewListeners {
    fn drop(&mut self) {
        self.outside.release();
        self.escape.release();
    }
}

/// The wrapper is required by Leptos view callbacks because the controller
/// owns browser-only `Rc` state while the generated view type is `Send + Sync`.
pub type UiTaskController = leptos::__reexports::send_wrapper::SendWrapper<TaskController>;

impl TaskController {
    /// Create a task centre bound to the authenticated shell.
    pub fn new(
        on_logout: Callback<()>,
        on_refresh_folder: Callback<()>,
        feedback: Callback<Feedback>,
    ) -> Self {
        Self {
            tasks: RwSignal::new(Vec::new()),
            loading: RwSignal::new(true),
            error: RwSignal::new(String::new()),
            password_task: RwSignal::new(None),
            password: RwSignal::new(String::new()),
            password_error: RwSignal::new(String::new()),
            show_all_completed: RwSignal::new(false),
            center: NodeRef::new(),
            runtime: Rc::new(TaskRuntime::new()),
            on_logout,
            on_refresh_folder,
            feedback,
            on_upload_cancel: None,
            on_upload_retry: None,
        }
    }

    /// Bind the browser-local upload queue to the unified task centre. The
    /// reference client delegates upload actions to that queue so XHRs are
    /// aborted before the remote session is removed.
    pub fn set_upload_actions(
        &mut self,
        on_cancel: Callback<String, bool>,
        on_retry: Callback<String, bool>,
    ) {
        self.on_upload_cancel = Some(on_cancel);
        self.on_upload_retry = Some(on_retry);
    }

    /// Start the initial snapshot and the event-stream lifecycle.
    pub fn mount(&self) -> TaskViewListeners {
        if !self.runtime.started.replace(true) {
            self.refresh();
        }
        self.connect_events();

        let center = self.center;
        let outside_controller = self.clone();
        let outside = browser::on_pointerdown(move |event| {
            let Some(details) = center.get() else {
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
                outside_controller.close_center();
            }
        });

        let escape_controller = self.clone();
        let escape = browser::on_keydown(move |event| {
            if event.key() != "Escape" {
                return;
            }
            if escape_controller.close_password() {
                return;
            }
            if escape_controller.is_center_open() {
                escape_controller.close_center();
                escape_controller.focus_center_summary();
            }
        });

        TaskViewListeners::new(outside, escape)
    }

    /// Close the native disclosure panel.
    pub fn close_center(&self) {
        if let Some(details) = self.center.get() {
            details.set_open(false);
        }
    }

    fn focus_center_summary(&self) {
        let Some(details) = self.center.get() else {
            return;
        };
        if let Ok(Some(summary)) = details.query_selector("summary")
            && let Ok(summary) = summary.dyn_into::<web_sys::HtmlElement>()
        {
            let _ = summary.focus();
        }
    }

    /// Open the native disclosure panel from the mobile account/tools menu.
    pub fn open_center(&self) {
        if let Some(details) = self.center.get() {
            details.set_open(true);
        }
    }

    /// Refresh the durable task projection after a foreground action starts a
    /// job (for example archive extraction).
    pub fn refresh_now(&self) {
        self.refresh_coalesced();
    }

    /// Expose the reactive snapshot to the mobile account/tools summary.
    pub fn task_signal(&self) -> RwSignal<Vec<Task>> {
        self.tasks
    }

    fn is_center_open(&self) -> bool {
        self.center.get().is_some_and(|details| details.open())
    }

    /// Shut down the SSE connection and every timer owned by this instance.
    pub fn dispose(&self) {
        if self.runtime.disposed.replace(true) {
            return;
        }
        self.stop_reconnect();
        self.stop_fallback();
        if let Some(source) = self.runtime.source.borrow_mut().take() {
            source.close();
        }
        // The source was closed before the closures are dropped, so no
        // callback can be executing while these listener handles are released.
        self.runtime.event_listeners.borrow_mut().clear();
    }

    /// Start a cancellation request for one active task.
    pub fn cancel(&self, id: String) {
        if let Some(task) = self.find_task(&id)
            && task.source_type == "upload"
            && let Some(callback) = self.on_upload_cancel
            && callback.run(task.source_id)
        {
            return;
        }
        let controller = self.clone();
        leptos::task::spawn_local(async move {
            let result = api::cancel_task(&id).await;
            match result {
                Ok(()) => controller.refresh_coalesced(),
                Err(error) => controller.handle_action_error(&error),
            }
        });
    }

    /// Start a retry request for one failed task.
    pub fn retry(&self, id: String) {
        let Some(task) = self.find_task(&id) else {
            return;
        };
        if task.status != TaskStatus::Failed || task.retry_count >= task.max_retries {
            return;
        }
        if task.source_type == "upload"
            && let Some(callback) = self.on_upload_retry
            && callback.run(task.source_id.clone())
        {
            self.refresh_coalesced();
            return;
        }
        let controller = self.clone();
        leptos::task::spawn_local(async move {
            let result = api::retry_task(&id).await;
            match result {
                Ok(()) => controller.refresh_coalesced(),
                Err(error) => controller.handle_action_error(&error),
            }
        });
    }

    /// Remove all visible completed and cancelled task notifications.
    pub fn clear_finished(&self) {
        let ids: Vec<String> = self
            .tasks
            .get_untracked()
            .into_iter()
            .filter(|task| matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled))
            .map(|task| task.id)
            .collect();
        if ids.is_empty() {
            return;
        }

        let controller = self.clone();
        leptos::task::spawn_local(async move {
            // The reference fires all terminal-row deletes together and does
            // not turn an individual delete failure into a toast. Preserve
            // the concurrent request timing while retaining the Rust shell's
            // existing expired-session hardening.
            let results = join_all(
                ids.into_iter()
                    .map(|id| async move { api::delete_task(&id).await }),
            )
            .await;
            if results
                .iter()
                .any(|result| result.as_ref().is_err_and(|error| error.is_unauthorized()))
            {
                controller.on_logout.run(());
            }
            controller.refresh_coalesced();
        });
    }

    /// Open the archive-password dialog for a task waiting for input.
    pub fn open_password(&self, task: Task) {
        if task.status != TaskStatus::WaitingInput || task.task_type != task_type::ARCHIVE_EXTRACT {
            return;
        }
        self.close_center();
        self.password.set(String::new());
        self.password_error.set(String::new());
        self.password_task.set(Some(task));
    }

    fn close_password(&self) -> bool {
        if self.password_task.get_untracked().is_none() {
            return false;
        }
        self.password_task.set(None);
        self.password.set(String::new());
        self.password_error.set(String::new());
        true
    }

    /// Submit the password currently shown in the dialog.
    pub fn submit_password(&self) {
        let Some(task) = self.password_task.get_untracked() else {
            return;
        };
        let password = self.password.get_untracked();
        if password.is_empty() {
            return;
        }

        self.password_error.set(String::new());
        let controller = self.clone();
        leptos::task::spawn_local(async move {
            let result = api::submit_task_input(&task.id, &TaskInputRequest { password }).await;
            match result {
                Ok(()) => {
                    controller.password_task.set(None);
                    controller.password.set(String::new());
                    controller.refresh_coalesced();
                }
                Err(error) if error.is_unauthorized() => {
                    controller.on_logout.run(());
                }
                Err(error) => controller.password_error.set(error.message),
            }
        });
    }

    fn find_task(&self, id: &str) -> Option<Task> {
        self.tasks
            .get_untracked()
            .into_iter()
            .find(|task| task.id == id)
    }

    fn handle_action_error(&self, error: &RequestError) {
        if error.is_unauthorized() {
            self.on_logout.run(());
        } else {
            self.feedback.run(Feedback::error(error.message.clone()));
        }
    }

    fn refresh_coalesced(&self) {
        if self.runtime.disposed.get() {
            return;
        }
        if self.runtime.refresh_running.get() {
            self.runtime.refresh_pending.set(true);
        } else {
            self.refresh();
        }
    }

    fn refresh(&self) {
        if self.runtime.disposed.get() {
            return;
        }
        if self.runtime.refresh_running.replace(true) {
            self.runtime.refresh_pending.set(true);
            return;
        }
        self.loading.set(true);
        let controller = self.clone();
        leptos::task::spawn_local(async move {
            let before = controller.tasks.get_untracked();
            let result = api::fetch_tasks().await;
            if !controller.runtime.disposed.get() {
                match result {
                    Ok(list) => {
                        controller.error.set(String::new());
                        controller.apply_tasks(before, list.items);
                    }
                    Err(error) if error.is_unauthorized() => {
                        controller.on_logout.run(());
                    }
                    Err(error) => controller.error.set(error.message),
                }
                controller.loading.set(false);
            }
            controller.runtime.refresh_running.set(false);
            if controller.runtime.refresh_pending.replace(false)
                && !controller.runtime.disposed.get()
            {
                controller.refresh();
            }
        });
    }

    fn apply_tasks(&self, before: Vec<Task>, after: Vec<Task>) {
        let previous: HashMap<String, TaskStatus> = before
            .into_iter()
            .map(|task| (task.id, task.status))
            .collect();
        for task in &after {
            let Some(old) = previous.get(&task.id).copied() else {
                continue;
            };
            if !is_active_task_status(old) || !task.status.is_terminal() {
                continue;
            }
            match task.status {
                TaskStatus::Completed => {
                    self.feedback
                        .run(Feedback::success(format!("「{}」任务完成", task.name)));
                    if task.task_type == task_type::ARCHIVE_EXTRACT {
                        self.on_refresh_folder.run(());
                    }
                }
                TaskStatus::Failed => {
                    self.feedback.run(Feedback::error(if task.error.is_empty() {
                        format!("「{}」任务失败", task.name)
                    } else {
                        task.error.clone()
                    }));
                }
                TaskStatus::Cancelled => {}
                TaskStatus::Queued
                | TaskStatus::Running
                | TaskStatus::WaitingInput
                | TaskStatus::Retrying
                | TaskStatus::Unknown => {}
            }
        }
        self.tasks.set(after);
    }

    fn connect_events(&self) {
        if self.runtime.disposed.get() || self.runtime.source.borrow().is_some() {
            return;
        }
        self.stop_reconnect();
        // The previous source is already closed when this is called from a
        // reconnect timer, so releasing its callbacks cannot race an event.
        self.runtime.event_listeners.borrow_mut().clear();

        // The reference calls the constructor directly. A constructor-level
        // failure therefore has no user-visible fallback/notification; only
        // a source that was successfully created participates in the later
        // reconnect and polling lifecycle.
        let Ok(source) = EventSource::new("/api/events") else {
            return;
        };
        let jobs_controller = self.clone();
        let jobs = Closure::<dyn FnMut(Event)>::new(move |_| {
            jobs_controller.refresh_coalesced();
        });
        let open_controller = self.clone();
        let open = Closure::<dyn FnMut(Event)>::new(move |_| {
            open_controller.runtime.retry_delay_ms.set(1_000);
            open_controller.stop_reconnect();
            open_controller.stop_fallback();
        });
        let error_controller = self.clone();
        let error = Closure::<dyn FnMut(Event)>::new(move |_| {
            error_controller.event_source_failed();
        });

        let _ = source.add_event_listener_with_callback("jobs", jobs.as_ref().unchecked_ref());
        source.set_onopen(Some(open.as_ref().unchecked_ref()));
        source.set_onerror(Some(error.as_ref().unchecked_ref()));
        self.runtime
            .event_listeners
            .borrow_mut()
            .extend([jobs, open, error]);
        self.runtime.source.borrow_mut().replace(source);
    }

    fn event_source_failed(&self) {
        if self.runtime.disposed.get() {
            return;
        }
        let Some(source) = self.runtime.source.borrow_mut().take() else {
            return;
        };
        source.close();
        self.start_fallback();
        self.schedule_reconnect();
    }

    fn start_fallback(&self) {
        if self.runtime.fallback_timer.get().is_some() || self.runtime.disposed.get() {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        let controller = self.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            if controller.runtime.source.borrow().is_none() {
                controller.refresh_coalesced();
            }
        });
        let Ok(timer) = window.set_interval_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            FALLBACK_INTERVAL_MS,
        ) else {
            return;
        };
        self.runtime
            .fallback_listener
            .borrow_mut()
            .replace(callback);
        self.runtime.fallback_timer.set(Some(timer));
    }

    fn stop_fallback(&self) {
        if let Some(timer) = self.runtime.fallback_timer.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_interval_with_handle(timer);
        }
        self.runtime.fallback_listener.borrow_mut().take();
    }

    fn schedule_reconnect(&self) {
        if self.runtime.disposed.get() {
            return;
        }
        self.stop_reconnect();
        let delay = self.runtime.retry_delay_ms.get();
        let next = delay.saturating_mul(2).min(MAX_RECONNECT_DELAY_MS);
        self.runtime.retry_delay_ms.set(next);
        let controller = self.clone();
        let callback = Closure::once_into_js(move || controller.connect_events());
        let Some(window) = web_sys::window() else {
            return;
        };
        if let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.unchecked_ref(),
            i32::try_from(delay).unwrap_or(i32::MAX),
        ) {
            self.runtime.reconnect_timer.set(Some(timer));
        }
    }

    fn stop_reconnect(&self) {
        if let Some(timer) = self.runtime.reconnect_timer.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
    }
}

/// Render the task trigger, panel and archive input dialog.
#[component]
pub fn TaskCenter(controller: UiTaskController, hide_trigger: bool) -> impl IntoView {
    let listeners = controller.mount();
    on_cleanup(move || drop(listeners));

    let center = controller.center;
    let tasks = controller.tasks;
    let active = Signal::derive_local(move || {
        tasks
            .get()
            .into_iter()
            .filter(|task| is_active_task_status(task.status))
            .collect::<Vec<_>>()
    });
    let completed = Signal::derive_local(move || {
        tasks
            .get()
            .into_iter()
            .filter(|task| matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled))
            .collect::<Vec<_>>()
    });
    let failed = Signal::derive_local(move || {
        tasks
            .get()
            .into_iter()
            .filter(|task| task.status == TaskStatus::Failed)
            .collect::<Vec<_>>()
    });
    let active_count = Signal::derive_local(move || active.get().len());
    let completed_count = Signal::derive_local(move || completed.get().len());
    let failed_count = Signal::derive_local(move || failed.get().len());
    let active_progress = Signal::derive_local(move || {
        let active = active.get();
        if active.is_empty() {
            0
        } else {
            let total = active
                .iter()
                .map(|task| {
                    if task.progress.is_finite() {
                        task.progress.clamp(0.0, 100.0)
                    } else {
                        0.0
                    }
                })
                .sum::<f64>();
            (total / active.len() as f64).round().clamp(0.0, 100.0) as u8
        }
    });

    let cancel = {
        let controller = controller.clone();
        Callback::new(move |id: String| controller.cancel(id))
    };
    let retry = {
        let controller = controller.clone();
        Callback::new(move |id: String| controller.retry(id))
    };
    let password = {
        let controller = controller.clone();
        Callback::new(move |task: Task| controller.open_password(task))
    };
    let clear = {
        let controller = controller.clone();
        Callback::new(move |_: ()| controller.clear_finished())
    };
    let completed_for_rows = completed_count;
    let show_all = controller.show_all_completed;
    let password_task = controller.password_task;
    let password_value = controller.password;
    let password_error = controller.password_error;
    let submit_password = {
        let controller = controller.clone();
        Callback::new(move |_: ()| controller.submit_password())
    };
    let close_password = {
        let controller = controller.clone();
        Callback::new(move |_: ()| {
            controller.close_password();
        })
    };

    view! {
        <details node_ref=center class="task-center">
            // Keep a real (hidden) summary for the mobile, trigger-less
            // instance. Without it, HTML details inserts its UA "Details"
            // summary, which was not present in the reference top bar.
            <summary
                class:task-trigger-hidden=hide_trigger
                title="任务中心"
                aria-label="打开任务中心"
            >
                {crate::components::icons::activity()}
                <Show when=move || active_count.get() != 0 fallback=|| ()>
                    <span>{move || active_count.get()}</span>
                </Show>
            </summary>
            <section class="task-panel">
                <header>
                    <div>
                        <strong>"任务中心"</strong>
                        <small>
                            {move || if active_count.get() > 0 {
                                format!("{} 项进行中 · {}%", active_count.get(), active_progress.get())
                            } else {
                                "任务通知".to_owned()
                            }}
                        </small>
                    </div>
                    <Show
                        when=move || failed_count.get() != 0
                        fallback=move || view! {
                            <Show when=move || active_count.get() != 0 fallback=|| ()>
                                <span class="status-badge tone-info size-sm">
                                    {move || format!("{} 项活动", active_count.get())}
                                </span>
                            </Show>
                        }
                    >
                        <span class="status-badge tone-danger size-sm">
                            {move || format!("{} 项失败", failed_count.get())}
                        </span>
                    </Show>
                </header>
                <Show
                    when=move || !tasks.get().is_empty()
                    fallback=|| view! { <p class="empty">"还没有后台任务"</p> }
                >
                        <div class="task-list">
                            <Show when=move || active_count.get() != 0 fallback=|| ()>
                                <section class="task-group active-group">
                                    <h3>{"进行中 "}<span>{move || active_count.get()}</span></h3>
                                    <For each=move || active.get() key=|task| task.id.clone() let:task>
                                        <TaskRow
                                            task=task
                                            tasks=tasks
                                            group=TaskGroup::Active
                                            on_cancel=cancel
                                            on_retry=retry
                                            on_password=password
                                        />
                                    </For>
                                </section>
                            </Show>
                            <Show when=move || completed_count.get() != 0 fallback=|| ()>
                                <section class="task-group completed-group">
                                    <h3>
                                        {"最近完成 "}<span>{move || completed_for_rows.get()}</span>
                                        <button
                                            class="clear-completed"
                                            type="button"
                                            on:click=move |_| clear.run(())
                                        >
                                            "清除完成"
                                        </button>
                                    </h3>
                                    <For
                                        each=move || {
                                            let entries = completed.get();
                                            if show_all.get() { entries } else { entries.into_iter().take(MAX_COMPLETED_DISPLAY).collect() }
                                        }
                                        key=|task| task.id.clone()
                                        let:task
                                    >
                                        <TaskRow
                                            task=task
                                            tasks=tasks
                                            group=TaskGroup::Completed
                                            on_cancel=cancel
                                            on_retry=retry
                                            on_password=password
                                        />
                                    </For>
                                    <Show when=move || { completed_count.get() > MAX_COMPLETED_DISPLAY } fallback=|| ()>
                                        <button class="expand" type="button" on:click=move |_| show_all.update(|value| *value = !*value)>
                                            <span>{move || if show_all.get() { "收起".to_owned() } else { format!("展开其余 {} 项", completed_count.get() - MAX_COMPLETED_DISPLAY) }}</span>
                                            <span class="expand-chevron" class:up=move || show_all.get()>
                                                {crate::components::icons::chevron_down()}
                                            </span>
                                        </button>
                                    </Show>
                                </section>
                            </Show>
                            <Show when=move || failed_count.get() != 0 fallback=|| ()>
                                <section class="task-group failed-group">
                                    <h3>{"失败 "}<span>{move || failed_count.get()}</span></h3>
                                    <For each=move || failed.get() key=|task| task.id.clone() let:task>
                                        <TaskRow
                                            task=task
                                            tasks=tasks
                                            group=TaskGroup::Failed
                                            on_cancel=cancel
                                            on_retry=retry
                                            on_password=password
                                        />
                                    </For>
                                </section>
                            </Show>
                        </div>
                </Show>
            </section>
        </details>
        <Show when=move || password_task.get().is_some() fallback=|| ()>
            <div class="input-backdrop" on:pointerdown=move |event: web_sys::PointerEvent| {
                if event.target() == event.current_target() {
                    close_password.run(());
                }
            }>
                <form
                    class="input-dialog"
                    role="dialog"
                    aria-modal="true"
                    aria-labelledby="task-password-title"
                    on:pointerdown=move |event: web_sys::PointerEvent| event.stop_propagation()
                    on:submit=move |event: web_sys::SubmitEvent| {
                        event.prevent_default();
                        submit_password.run(());
                    }
                >
                    <strong id="task-password-title">"输入压缩包密码"</strong>
                    <small>{move || password_task.get().map(|task| task.name).unwrap_or_default()}</small>
                    <input
                        type="password"
                        maxlength="1024"
                        autofocus
                        prop:value=move || password_value.get()
                        on:input=move |event| password_value.set(event_target_value(&event))
                    />
                    <Show when=move || !password_error.get().is_empty() fallback=|| ()>
                        <p>{move || password_error.get()}</p>
                    </Show>
                    <footer>
                        <button type="button" on:click=move |_| close_password.run(())>
                            "取消"
                        </button>
                        <button type="submit" prop:disabled=move || password_value.get().is_empty()>
                            "继续任务"
                        </button>
                    </footer>
                </form>
            </div>
        </Show>
    }
}

#[component]
fn TaskRow(
    task: Task,
    tasks: RwSignal<Vec<Task>>,
    group: TaskGroup,
    on_cancel: Callback<String>,
    on_retry: Callback<String>,
    on_password: Callback<Task>,
) -> impl IntoView {
    let id = task.id.clone();
    let name = task_display_name(&task.name, &task.task_type, &task.id);
    let title = task.name.clone();
    let kind = task_type_label(&task.task_type);
    let task_id = id.clone();
    let current_status = Signal::derive_local({
        let id = id.clone();
        move || current_task(tasks, &id).status
    });
    let current_progress = Signal::derive_local({
        let id = id.clone();
        move || task_progress_percent(current_task(tasks, &id).progress)
    });
    let current_progress_width = Signal::derive_local({
        let id = id.clone();
        move || task_progress_width(current_task(tasks, &id).progress)
    });
    let current_retry = Signal::derive_local({
        let id = id.clone();
        move || {
            let task = current_task(tasks, &id);
            (task.retry_count, task.max_retries)
        }
    });
    let current_label = Signal::derive_local({
        let id = id.clone();
        move || {
            let task = current_task(tasks, &id);
            let status = task_status_label(task.status, &task.task_type, &task.phase, &task.error);
            if task.speed > 0 && task.status == TaskStatus::Running {
                format!(
                    "{status} · {}/s",
                    format_size(u64::try_from(task.speed).unwrap_or(0))
                )
            } else {
                status
            }
        }
    });
    let cancel_id = task_id.clone();
    let retry_id = task_id.clone();
    let password_task = task.clone();
    let cancel = {
        let cancel_id = cancel_id.clone();
        Callback::new(move |_: ()| on_cancel.run(cancel_id.clone()))
    };
    let retry = {
        let retry_id = retry_id.clone();
        Callback::new(move |_: ()| on_retry.run(retry_id.clone()))
    };
    let password = {
        let password_task = password_task.clone();
        Callback::new(move |_: ()| on_password.run(password_task.clone()))
    };
    let row_class = match group {
        TaskGroup::Active => "task-group-row active-task-row",
        TaskGroup::Completed => "task-group-row completed-task-row",
        TaskGroup::Failed => "task-group-row failed-task-row",
    };
    let progress_class = Signal::derive_local(move || match current_status.get() {
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        status => status.as_str(),
    });
    let progress_width = Signal::derive_local(move || {
        if matches!(group, TaskGroup::Completed | TaskGroup::Failed) {
            "100".to_owned()
        } else {
            current_progress_width.get()
        }
    });
    let show_cancel = group == TaskGroup::Active;
    let show_retry = group == TaskGroup::Failed;
    let show_password = Signal::derive_local({
        let id = task_id.clone();
        move || {
            let task = current_task(tasks, &id);
            show_cancel
                && task.status == TaskStatus::WaitingInput
                && task.task_type == task_type::ARCHIVE_EXTRACT
        }
    });
    let open_row = {
        let password = password.clone();
        move || {
            if show_password.get_untracked() {
                password.run(());
            }
        }
    };

    view! {
        <article class=row_class on:click=move |_| open_row()>
            <span class="kind">{kind}</span>
            <div>
                <strong title=title>{name.clone()}</strong>
                <small>{move || current_label.get()}</small>
                <i>
                    <b
                        class=move || progress_class.get()
                        style=move || format!("width: {}%", progress_width.get())
                    ></b>
                </i>
            </div>
            <em>{move || if group == TaskGroup::Failed { "失败".to_owned() } else if group == TaskGroup::Completed { "100%".to_owned() } else { format!("{}%", current_progress.get()) }}</em>
            <span class="actions">
                <Show when=move || show_cancel fallback=|| ()>
                    <button
                        type="button"
                        title="取消"
                        aria-label="取消任务"
                        on:click=move |event: web_sys::MouseEvent| {
                            event.stop_propagation();
                            cancel.run(())
                        }
                    >
                        {crate::components::icons::close_square()}
                    </button>
                    <Show when=move || show_password.get() fallback=|| ()>
                        <button
                            type="button"
                            title="输入密码"
                            aria-label="输入压缩包密码"
                            on:click=move |event: web_sys::MouseEvent| {
                                event.stop_propagation();
                                password.run(())
                            }
                        >
                            {crate::components::icons::key_round()}
                        </button>
                    </Show>
                </Show>
                <Show
                    when=move || {
                        let (retry_count, max_retries) = current_retry.get();
                        show_retry && retry_count < max_retries
                    }
                    fallback=|| ()
                >
                    <button
                        type="button"
                        title="重试"
                        aria-label="重试任务"
                        on:click=move |event: web_sys::MouseEvent| {
                            event.stop_propagation();
                            retry.run(())
                        }
                    >
                        {crate::components::icons::rotate_ccw()}
                    </button>
                </Show>
            </span>
        </article>
    }
}

fn current_task(tasks: RwSignal<Vec<Task>>, id: &str) -> Task {
    tasks
        .get()
        .into_iter()
        .find(|task| task.id == id)
        .unwrap_or_default()
}
