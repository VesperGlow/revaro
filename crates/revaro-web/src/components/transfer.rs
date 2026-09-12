//! Move and copy dialog for live files and directories.

use leptos::ev::MouseEvent;
use leptos::prelude::*;
use revaro_core::model::File;

use super::directory_picker::DirectoryPicker;

/// The operation a transfer dialog performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    /// Relink the existing item below the destination.
    Move,
    /// Create a copy below the destination.
    Copy,
}

/// A reviewable target selection before the browser starts a transfer.
#[component]
pub fn TransferDialog(
    mode: TransferMode,
    targets: Vec<File>,
    target_id: RwSignal<String>,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    on_cancel: Callback<()>,
    on_confirm: Callback<String>,
    on_unauthorized: Callback<()>,
) -> impl IntoView {
    let target_text = if targets.len() == 1 {
        format!("“{}”", targets[0].name)
    } else {
        format!("{} 项", targets.len())
    };
    let title = match mode {
        TransferMode::Move => "移动到",
        TransferMode::Copy => "复制到",
    };
    let action = match mode {
        TransferMode::Move => "移动",
        TransferMode::Copy => "复制",
    };
    let excluded_ids = if mode == TransferMode::Move {
        targets.iter().map(|item| item.id.clone()).collect()
    } else {
        Vec::new()
    };
    let cancel_backdrop = on_cancel.clone();
    let cancel_close = on_cancel.clone();
    let cancel_footer = on_cancel;
    let confirm = on_confirm.clone();

    view! {
        <div
            class="modal-backdrop transfer-backdrop"
            role="presentation"
            on:click=move |event: MouseEvent| {
                if event.target() == event.current_target() && !busy.get_untracked() {
                    cancel_backdrop.run(())
                }
            }
        >
            <section class="modal move-copy-dialog" role="dialog" aria-modal="true" aria-labelledby="transfer-dialog-title">
                <header>
                    <div>
                        <p class="eyebrow dark">"DIRECTORY"</p>
                        <h2 id="transfer-dialog-title">{title}</h2>
                        <p>{format!("为 {} 选择目标文件夹", target_text)}</p>
                    </div>
                    <button type="button" aria-label="关闭" prop:disabled=move || busy.get() on:click=move |_| cancel_close.run(())>"×"</button>
                </header>
                <div class="move-copy-body">
                    <DirectoryPicker
                        initial_id=target_id.get_untracked()
                        excluded_ids=excluded_ids
                        disabled=busy
                        on_change=Callback::new(move |id: String| target_id.set(id))
                        on_unauthorized=on_unauthorized
                    />
                    <Show when=move || !error.get().is_empty() fallback=|| ()>
                        <p class="form-error transfer-error" role="alert">{move || error.get()}</p>
                    </Show>
                </div>
                <footer>
                    <button type="button" class="secondary" prop:disabled=move || busy.get() on:click=move |_| cancel_footer.run(())>"取消"</button>
                    <button type="button" class="primary" prop:disabled=move || busy.get() on:click=move |_| confirm.run(target_id.get_untracked())>
                        {move || if busy.get() { "正在处理…" } else { action }}
                    </button>
                </footer>
            </section>
        </div>
    }
}
