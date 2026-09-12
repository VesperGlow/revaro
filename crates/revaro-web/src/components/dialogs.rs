//! Small modal primitives used by file mutations.
//!
//! Keeping the modal's submit and cancel behaviour here means file-browser
//! state only decides which operation is pending. The same component handles
//! a validated name input and destructive confirmations, while the server
//! remains the authority for name, parent and lifecycle validation.

use leptos::ev::{MouseEvent, SubmitEvent};
use leptos::prelude::*;

use super::icons;

/// A focused confirmation or text-entry modal.
#[component]
pub fn ActionDialog(
    title: String,
    message: String,
    confirm_label: String,
    danger: bool,
    input: bool,
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

    view! {
        <div
            class="modal-backdrop"
            role="presentation"
            on:click=move |event: MouseEvent| {
                if event.target() == event.current_target() && !busy.get_untracked() {
                    cancel_from_backdrop.run(());
                }
            }
        >
            <form
                class="modal action-dialog"
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
                        icons::file_text().into_any()
                    }}
                </div>
                <div class="dialog-copy">
                    <h2 id="action-dialog-title">{title}</h2>
                    <p>{message}</p>
                </div>
                <Show when=move || input fallback=|| ()>
                    <input
                        type="text"
                        maxlength="1024"
                        autofocus
                        prop:value=move || value.get()
                        on:input=move |event| value.set(event_target_value(&event))
                    />
                </Show>
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
