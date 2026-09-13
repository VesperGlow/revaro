//! Small modal primitives used by file mutations.
//!
//! Keeping the modal's submit and cancel behaviour here means file-browser
//! state only decides which operation is pending. The same component handles
//! a validated name input and destructive confirmations, while the server
//! remains the authority for name, parent and lifecycle validation.

use leptos::ev::{MouseEvent, SubmitEvent};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use super::icons;

/// A focused confirmation or text-entry modal.
#[component]
pub fn ActionDialog(
    title: String,
    message: String,
    confirm_label: String,
    danger: bool,
    input: bool,
    placeholder: Option<String>,
    value: RwSignal<String>,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    on_cancel: Callback<()>,
    on_confirm: Callback<String>,
) -> impl IntoView {
    let submit = move |event: SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let submitted = value.get_untracked();
        if input && submitted.trim().is_empty() {
            return;
        }
        on_confirm.run(submitted);
    };
    let cancel_from_backdrop = on_cancel.clone();
    let cancel_from_escape = on_cancel.clone();
    let cancel_from_button = on_cancel;
    let input_ref = NodeRef::<leptos::html::Input>::new();
    if input {
        focus_input_after_render(input_ref);
    }
    let input_view = if input {
        view! {
            <input
                node_ref=input_ref
                type="text"
                maxlength="1024"
                autofocus
                placeholder=placeholder.unwrap_or_default()
                prop:value=move || value.get()
                on:input=move |event| value.set(event_target_value(&event))
            />
        }
        .into_any()
    } else {
        ().into_any()
    };

    view! {
        <div
            class="dialog-backdrop"
            role="presentation"
            on:click=move |event: MouseEvent| {
                if event.target() == event.current_target() && !busy.get_untracked() {
                    cancel_from_backdrop.run(());
                }
            }
        >
            <form
                class="app-dialog"
                role="dialog"
                aria-modal="true"
                aria-labelledby="action-dialog-title"
                on:submit=submit
                on:keydown=move |event: web_sys::KeyboardEvent| {
                    if event.key() == "Escape" && !busy.get_untracked() {
                        event.prevent_default();
                        cancel_from_escape.run(());
                    }
                }
            >
                <div class="dialog-icon" class:danger=danger aria-hidden="true">
                    {if danger {
                        icons::trash().into_any()
                    } else {
                        icons::circle_alert().into_any()
                    }}
                </div>
                <div class="dialog-copy">
                    <h2 id="action-dialog-title">{title}</h2>
                    <p>{message}</p>
                </div>
                {input_view}
                <Show when=move || !error.get().is_empty() fallback=|| ()>
                    <p class="form-error action-dialog-error" role="alert">
                        {move || error.get()}
                    </p>
                </Show>
                <footer>
                    <button
                        class="secondary"
                        type="button"
                        prop:disabled=move || busy.get()
                        on:click=move |_| cancel_from_button.run(())
                    >
                        "取消"
                    </button>
                    <button
                        class="dialog-confirm"
                        class:danger=danger
                        type="submit"
                        prop:disabled=move || busy.get() || (input && value.get().trim().is_empty())
                    >
                        {move || if busy.get() { "处理中…".to_owned() } else { confirm_label.clone() }}
                    </button>
                </footer>
            </form>
        </div>
    }
}

/// The reference rename form is a normal modal rather than an alert dialog.
/// Keeping it separate preserves its label, placeholder, header hierarchy and
/// empty-name submission semantics.
#[component]
pub fn RenameDialog(
    value: RwSignal<String>,
    busy: RwSignal<bool>,
    on_cancel: Callback<()>,
    on_confirm: Callback<String>,
) -> impl IntoView {
    let close_backdrop = on_cancel.clone();
    let close_header = on_cancel.clone();
    let close_footer = on_cancel;
    let confirm_input = on_confirm.clone();
    let confirm_button = on_confirm;
    let input_ref = NodeRef::<leptos::html::Input>::new();
    focus_input_after_render(input_ref);

    view! {
        <div
            class="modal-backdrop"
            role="presentation"
            on:click=move |event: MouseEvent| {
                if event.target() == event.current_target() {
                    close_backdrop.run(())
                }
            }
        >
            <section
                class="modal"
                role="dialog"
                aria-modal="true"
                aria-labelledby="rename-dialog-title"
            >
                <header>
                    <div>
                        <p class="eyebrow dark">"EDIT"</p>
                        <h2 id="rename-dialog-title">"重命名"</h2>
                    </div>
                    <button type="button" on:click=move |_| close_header.run(())>"×"</button>
                </header>
                <label>
                    "新名称"
                    <input
                        node_ref=input_ref
                        type="text"
                        maxlength="1024"
                        autofocus
                        prop:value=move || value.get()
                        on:input=move |event| value.set(event_target_value(&event))
                        on:keydown=move |event: web_sys::KeyboardEvent| {
                            if event.key() == "Enter" && !busy.get_untracked() {
                                event.prevent_default();
                                confirm_input.run(value.get_untracked());
                            }
                        }
                    />
                </label>
                <footer>
                    <button type="button" class="secondary" on:click=move |_| close_footer.run(())>
                        "取消"
                    </button>
                    <button
                        type="button"
                        class="primary"
                        prop:disabled=move || busy.get()
                        on:click=move |_| {
                            if !busy.get_untracked() {
                                confirm_button.run(value.get_untracked());
                            }
                        }
                    >
                        "保存"
                    </button>
                </footer>
            </section>
        </div>
    }
}

fn focus_input_after_render(input: NodeRef<leptos::html::Input>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once_into_js(move || {
        if let Some(input) = input.get() {
            let _ = input.focus();
        }
    });
    let _ =
        window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
}
