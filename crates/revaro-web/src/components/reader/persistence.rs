//! Debounced and teardown-safe durable reading progress.

use super::*;

pub(super) fn schedule_progress_save(runtime: Rc<RefCell<ReaderRuntime>>, file_id: String) {
    let old_timer = {
        let mut state = runtime.borrow_mut();
        state.progress_timer.take()
    };
    clear_timer_value(old_timer);
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback_state = runtime.clone();
    let callback_id = file_id;
    let callback = Closure::once_into_js(move || {
        callback_state.borrow_mut().progress_timer = None;
        save_current_progress(callback_state, callback_id);
    });
    if let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.unchecked_ref(),
        PROGRESS_DELAY_MS,
    ) {
        runtime.borrow_mut().progress_timer = Some(timer);
    }
}

pub(super) fn clear_timer_value(timer: Option<i32>) {
    if let Some(timer) = timer
        && let Some(window) = web_sys::window()
    {
        window.clear_timeout_with_handle(timer);
    }
}

pub(super) fn save_current_progress(runtime: Rc<RefCell<ReaderRuntime>>, file_id: String) {
    let (anchor, percent) = {
        let state = runtime.borrow();
        if state.closing {
            return;
        }
        (state.top_anchor.clone(), state.top_percent)
    };
    let Some(anchor) = anchor else {
        return;
    };
    leptos::task::spawn_local(async move {
        let _ = api::save_book_progress(
            &file_id,
            &SaveProgressRequest {
                anchor: Some(anchor),
                percent: Some(percent),
            },
        )
        .await;
    });
}

pub(super) fn flush_progress(runtime: Rc<RefCell<ReaderRuntime>>, file_id: String) {
    let anchor = runtime.borrow().top_anchor.clone();
    let percent = runtime.borrow().top_percent;
    let refresh = runtime.borrow().library_refresh;
    if let Some(anchor) = anchor {
        leptos::task::spawn_local(async move {
            let result = api::save_book_progress(
                &file_id,
                &SaveProgressRequest {
                    anchor: Some(anchor),
                    percent: Some(percent),
                },
            )
            .await;
            if result.is_ok()
                && let Some(refresh) = refresh
            {
                let _ = refresh.try_update(|r| *r += 1);
            }
        });
    }
}
