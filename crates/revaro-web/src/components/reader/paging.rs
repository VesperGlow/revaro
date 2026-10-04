//! Window synchronization, animated page turns and relayout.

use super::*;

pub(super) fn schedule_window_sync(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
) {
    let old_timer = {
        let mut state = runtime.borrow_mut();
        state.sync_timer.take()
    };
    clear_timer_value(old_timer);
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback_runtime = runtime.clone();
    let callback = Closure::once_into_js(move || {
        callback_runtime.borrow_mut().sync_timer = None;
        leptos::task::spawn_local(window_sync(
            callback_runtime,
            viewport,
            flow,
            file_id,
            stage,
            toc_active,
            percent,
        ));
    });
    if let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.unchecked_ref(),
        WINDOW_SYNC_DELAY_MS,
    ) {
        runtime.borrow_mut().sync_timer = Some(timer);
    }
}

pub(super) async fn window_sync(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
) {
    if runtime.borrow().closing || stage.try_get_untracked() != Some(ReaderStage::Reading) {
        return;
    }
    {
        let mut state = runtime.borrow_mut();
        if state.closing || state.syncing || state.turn_busy || state.nav_depth > 0 {
            return;
        }
        state.syncing = true;
    }
    let result = async {
        let (manifest, anchor) = {
            let state = runtime.borrow();
            (state.manifest.clone(), state.top_anchor.clone())
        };
        let (Some(manifest), Some(anchor)) = (manifest, anchor) else {
            return Ok::<(), String>(());
        };
        let (first, last) = stable_window_range(&manifest, anchor.block, AHEAD_MARGIN);
        let changed = ensure_window(&runtime, &file_id, &manifest, flow, first, last).await?;
        if runtime.borrow().closing || runtime.borrow().nav_depth > 0 {
            return Ok(());
        }
        if changed {
            measure_cols(&runtime, flow);
            let column = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
            move_to_column(&runtime, flow, column, false).await;
        }
        if runtime.borrow().closing || runtime.borrow().nav_depth > 0 {
            return Ok(());
        }
        refresh_ui(&runtime, flow, toc_active, percent);
        Ok(())
    }
    .await;
    if result.is_err() {
        // A background prefetch failure should not cover readable content with
        // an error panel. The next page turn retries the missing chunk.
    }
    let pending = {
        let mut state = runtime.borrow_mut();
        state.syncing = false;
        if state.closing || state.nav_depth > 0 {
            0
        } else {
            state.pending_turns
        }
    };
    if pending != 0 {
        let direction = if pending > 0 { 1 } else { -1 };
        runtime.borrow_mut().pending_turns -= direction;
        spawn_turn(
            runtime, viewport, flow, file_id, stage, toc_active, percent, direction,
        );
    }
}

pub(super) fn spawn_turn(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    direction: i32,
) {
    leptos::task::spawn_local(async move {
        turn(
            runtime,
            viewport,
            flow,
            file_id,
            stage,
            toc_active,
            percent,
            direction.signum(),
        )
        .await;
    });
}

pub(super) async fn turn(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    direction: i32,
) {
    if runtime.borrow().closing
        || stage.try_get_untracked() != Some(ReaderStage::Reading)
        || direction == 0
    {
        return;
    }
    {
        let mut state = runtime.borrow_mut();
        if state.closing {
            return;
        }
        if state.turn_busy || state.syncing {
            state.pending_turns += direction;
            return;
        }
        state.turn_busy = true;
    }

    let mut can_move = true;
    let current = runtime.borrow().current_col;
    let cols = runtime.borrow().cols;
    let target = current + direction;
    if target < 0 || target >= cols {
        let (manifest, first, last) = {
            let state = runtime.borrow();
            (state.manifest.clone(), state.first_chunk, state.last_chunk)
        };
        if let Some(manifest) = manifest {
            let final_chunk = manifest.chunks.len().saturating_sub(1) as i32;
            let range = if direction > 0 {
                if last >= final_chunk {
                    None
                } else {
                    Some((first, last + 1))
                }
            } else if first <= 0 {
                None
            } else {
                Some((first - 1, last))
            };
            if let Some((new_first, new_last)) = range {
                if ensure_window(&runtime, &file_id, &manifest, flow, new_first, new_last)
                    .await
                    .is_ok()
                {
                    if runtime.borrow().closing {
                        return;
                    }
                    measure_cols(&runtime, flow);
                    if let Some(anchor) = runtime.borrow().top_anchor.clone() {
                        let column = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
                        runtime.borrow_mut().current_col = column;
                        set_x(&runtime, flow, false);
                    }
                } else {
                    can_move = false;
                }
            } else {
                can_move = false;
            }
        } else {
            can_move = false;
        }
    }

    if can_move {
        let current = runtime.borrow().current_col;
        let cols = runtime.borrow().cols;
        let next = current + direction;
        if next >= 0 && next < cols {
            move_to_column(&runtime, flow, next, true).await;
            if !runtime.borrow().closing {
                capture_and_refresh(&runtime, viewport, flow, toc_active, percent);
                schedule_progress_save(runtime.clone(), file_id.clone());
                schedule_window_sync(
                    runtime.clone(),
                    viewport,
                    flow,
                    file_id.clone(),
                    stage,
                    toc_active,
                    percent,
                );
            }
        }
    }

    let pending = {
        let mut state = runtime.borrow_mut();
        state.turn_busy = false;
        if state.closing {
            0
        } else {
            state.pending_turns
        }
    };
    if pending != 0 {
        let next = if pending > 0 { 1 } else { -1 };
        runtime.borrow_mut().pending_turns -= next;
        spawn_turn(
            runtime, viewport, flow, file_id, stage, toc_active, percent, next,
        );
    }
}

pub(super) async fn pause(milliseconds: i32) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let (sender, receiver) = oneshot::channel::<()>();
    let callback = Closure::once_into_js(move || {
        let _ = sender.send(());
    });
    if window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.unchecked_ref(),
            milliseconds,
        )
        .is_ok()
    {
        let _ = receiver.await;
    }
}

pub(super) fn schedule_relayout(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    manifest_signal: RwSignal<Option<FlowManifest>>,
    prefs: RwSignal<ReaderPrefs>,
    stage: RwSignal<ReaderStage>,
) {
    let old_timer = {
        let mut state = runtime.borrow_mut();
        state.relayout_timer.take()
    };
    clear_timer_value(old_timer);
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback_runtime = runtime.clone();
    let callback = Closure::once_into_js(move || {
        callback_runtime.borrow_mut().relayout_timer = None;
        relayout(
            callback_runtime,
            viewport,
            flow,
            manifest_signal,
            prefs,
            stage,
        );
    });
    if let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.unchecked_ref(),
        RELAYOUT_DELAY_MS,
    ) {
        runtime.borrow_mut().relayout_timer = Some(timer);
    }
}

pub(super) fn relayout(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    manifest_signal: RwSignal<Option<FlowManifest>>,
    prefs: RwSignal<ReaderPrefs>,
    stage: RwSignal<ReaderStage>,
) {
    if stage.get_untracked() != ReaderStage::Reading {
        return;
    }
    let Some(manifest) = manifest_signal.get_untracked() else {
        return;
    };
    let keep = runtime
        .borrow()
        .top_anchor
        .clone()
        .or_else(|| capture_top_anchor(&runtime, viewport, flow));
    let metrics = apply_metrics(viewport, flow, &manifest.format, prefs.get_untracked());
    runtime.borrow_mut().metrics = metrics;
    measure_cols(&runtime, flow);
    if let Some(anchor) = keep {
        let column = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
        runtime.borrow_mut().current_col = column;
        runtime.borrow_mut().top_anchor = Some(anchor);
    } else {
        runtime.borrow_mut().current_col = 0;
    }
    set_x(&runtime, flow, false);
}
