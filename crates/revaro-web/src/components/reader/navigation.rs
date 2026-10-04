//! TOC targeting, column navigation and anchor refresh.

use super::*;

pub(super) fn resolve_text_target(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    entry: &TocTarget,
) -> Option<(Anchor, DomRect)> {
    let manifest = runtime.borrow().manifest.clone()?;
    let block = find_block(flow, entry.block)?;
    let node = resolve_path(&block, &entry.text_path)?;
    if node.node_type() != Node::TEXT_NODE {
        return None;
    }
    let length = node
        .text_content()
        .map_or(0, |value| value.encode_utf16().count());
    let offset = entry
        .text_offset
        .clamp(0, i32::try_from(length).unwrap_or(i32::MAX));
    let rect = collapsed_range_rect(&node, offset)?;
    rect_has_box(&rect).then(|| (anchor_from_node(&manifest, &block, &node, offset), rect))
}

pub(super) fn resolve_nav_target(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    entry: &TocTarget,
) -> Option<(Anchor, DomRect)> {
    if entry.nav_anchor.is_empty() {
        return None;
    }
    let nodes = flow.query_selector_all("[data-rv-anchor]").ok()?;
    let manifest = runtime.borrow().manifest.clone()?;
    for index in 0..nodes.length() {
        let element = nodes.item(index)?.dyn_into::<Element>().ok()?;
        if element.get_attribute("data-rv-anchor").as_deref() != Some(&entry.nav_anchor) {
            continue;
        }
        let block = element
            .closest("[data-block]")
            .ok()
            .flatten()
            .or_else(|| find_block(flow, entry.block))?;
        let rect = element.get_bounding_client_rect();
        if !rect_has_box(&rect) {
            continue;
        }
        let node: Node = element.clone().unchecked_into();
        return Some((anchor_from_node(&manifest, &block, &node, -1), rect));
    }
    None
}

pub(super) fn decode_fragment(value: &str) -> String {
    js_sys::decode_uri_component(value)
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| value.to_owned())
}

pub(super) fn fragment_matches(element: &Element, fragment: &str, decoded: &str) -> bool {
    if element
        .get_attribute("id")
        .is_some_and(|value| value == fragment || value == decoded)
    {
        return true;
    }
    element.get_attribute("data-frag-ids").is_some_and(|value| {
        value
            .split_whitespace()
            .any(|part| part == fragment || part == decoded)
    })
}

pub(super) fn resolve_fragment_target(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    entry: &TocTarget,
) -> Option<(Anchor, DomRect)> {
    if entry.source_fragment.is_empty() {
        return None;
    }
    let block = find_block(flow, entry.block)?;
    let decoded = decode_fragment(&entry.source_fragment);
    let candidates = block.query_selector_all("[id], [data-frag-ids]").ok()?;
    let manifest = runtime.borrow().manifest.clone()?;
    // The fragment names the block itself or, failing that, the first matching
    // descendant in document order. Like the reference fallback, the landing
    // position is that match's first actually visible content: both the column
    // and the committed anchor come from the hit node, so the later window sync
    // and relayout re-align to the same content instead of the container box
    // (whose first fragment can stay in the previous column).
    let target = if fragment_matches(&block, &entry.source_fragment, &decoded) {
        block.clone()
    } else {
        let mut found = None;
        for index in 0..candidates.length() {
            let element = candidates.item(index)?.dyn_into::<Element>().ok()?;
            if fragment_matches(&element, &entry.source_fragment, &decoded) {
                found = Some(element);
                break;
            }
        }
        found?
    };
    let start = visual_start(&target)?;
    let node: Node = start
        .node
        .clone()
        .unwrap_or_else(|| target.clone().unchecked_into());
    Some((
        anchor_from_node(&manifest, &block, &node, start.offset),
        start.rect,
    ))
}

pub(super) fn spawn_toc_jump(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    entry: TocTarget,
) {
    leptos::task::spawn_local(async move {
        jump_to_toc(
            runtime, viewport, flow, file_id, stage, toc_active, percent, entry,
        )
        .await;
    });
}

pub(super) async fn jump_to_toc(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    entry: TocTarget,
) {
    if runtime.borrow().closing || stage.try_get_untracked() != Some(ReaderStage::Reading) {
        return;
    }
    let old_timer = runtime.borrow_mut().sync_timer.take();
    clear_timer_value(old_timer);
    let depth = runtime.borrow().nav_depth;
    runtime.borrow_mut().nav_depth = depth.saturating_add(1);
    let _nav_guard = NavGuard(runtime.clone());
    let Some(manifest) = runtime.borrow().manifest.clone() else {
        return;
    };
    let total = total_blocks(&manifest).max(1);
    let block = entry.block.clamp(0, total - 1);
    let (first, last) = stable_window_range(&manifest, block, AHEAD_MARGIN);
    if ensure_window(&runtime, &file_id, &manifest, flow, first, last)
        .await
        .is_err()
        || runtime.borrow().closing
    {
        return;
    }
    measure_cols(&runtime, flow);
    let Some(flow_element) = flow_element(flow) else {
        return;
    };
    let fixed = TocTarget {
        block,
        ..entry.clone()
    };
    let target = resolve_text_target(&runtime, &flow_element, &fixed)
        .or_else(|| resolve_nav_target(&runtime, &flow_element, &fixed))
        .or_else(|| resolve_fragment_target(&runtime, &flow_element, &fixed));
    if let Some((anchor, rect)) = target {
        let column = col_from_rect(&runtime, &flow_element, &rect);
        move_to_column(&runtime, flow, column, false).await;
        runtime.borrow_mut().top_anchor = Some(anchor);
        refresh_ui(&runtime, flow, toc_active, percent);
        schedule_progress_save(runtime.clone(), file_id.clone());
        schedule_window_sync(runtime, viewport, flow, file_id, stage, toc_active, percent);
        return;
    }
    jump_to_block(
        runtime, viewport, flow, file_id, stage, toc_active, percent, block,
    )
    .await;
}

pub(super) async fn jump_to_block(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    block: i32,
) {
    if runtime.borrow().closing || stage.try_get_untracked() != Some(ReaderStage::Reading) {
        return;
    }
    let Some(manifest) = runtime.borrow().manifest.clone() else {
        return;
    };
    let total = total_blocks(&manifest).max(1);
    let block = block.clamp(0, total - 1);
    let (first, last) = stable_window_range(&manifest, block, AHEAD_MARGIN);
    if ensure_window(&runtime, &file_id, &manifest, flow, first, last)
        .await
        .is_err()
        || runtime.borrow().closing
    {
        return;
    }
    measure_cols(&runtime, flow);
    let anchor = Anchor {
        spine: spine_for_block(&manifest, block),
        block,
        path: Vec::new(),
        offset: Anchor::BOUNDARY_OFFSET,
    };
    let column = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
    move_to_column(&runtime, flow, column, false).await;
    runtime.borrow_mut().top_anchor = Some(anchor);
    refresh_ui(&runtime, flow, toc_active, percent);
    schedule_progress_save(runtime.clone(), file_id.clone());
    schedule_window_sync(runtime, viewport, flow, file_id, stage, toc_active, percent);
    let _ = viewport;
}

pub(super) async fn move_to_column(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: DivRef,
    column: i32,
    animated: bool,
) {
    if runtime.borrow().closing {
        return;
    }
    {
        let mut state = runtime.borrow_mut();
        state.current_col = column.clamp(0, state.cols.saturating_sub(1));
    }
    set_promote(flow, animated);
    set_x(runtime, flow, animated);
    if animated {
        pause(ANIMATION_MS).await;
        // Closing disposes the NodeRef while the animation timer is pending.
        if runtime.borrow().closing {
            return;
        }
        if let Some(flow) = flow_html_element(flow) {
            let _ = flow.style().set_property("transition", "none");
        }
        set_promote(flow, false);
    } else {
        set_promote(flow, false);
    }
}

pub(super) fn spawn_snap_to_column(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    column: i32,
) {
    leptos::task::spawn_local(async move {
        move_to_column(&runtime, flow, column, true).await;
        if runtime.borrow().closing {
            return;
        }
        capture_and_refresh(&runtime, viewport, flow, toc_active, percent);
        schedule_progress_save(runtime.clone(), file_id.clone());
        schedule_window_sync(runtime, viewport, flow, file_id, stage, toc_active, percent);
    });
}

pub(super) fn capture_and_refresh(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
) {
    if let Some(anchor) = capture_top_anchor(runtime, viewport, flow) {
        runtime.borrow_mut().top_anchor = Some(anchor);
    }
    refresh_ui(runtime, flow, toc_active, percent);
}
