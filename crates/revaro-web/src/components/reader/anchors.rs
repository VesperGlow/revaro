//! Stable anchors from the visible DOM and reading progress.

use super::*;

pub(super) fn col_from_rect(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    rect: &DomRect,
) -> i32 {
    let state = runtime.borrow();
    if state.metrics.pitch <= 0.0 {
        return 0;
    }
    let flow_rect = flow.get_bounding_client_rect();
    let value = ((rect.left() - flow_rect.left() - state.metrics.side + 1.0) / state.metrics.pitch)
        .floor() as i32;
    value.clamp(0, state.cols.saturating_sub(1))
}

pub(super) fn col_for_anchor(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: DivRef,
    anchor: &Anchor,
) -> Option<i32> {
    let flow = flow_element(flow)?;
    let block = find_block(&flow, anchor.block)?;
    let node = resolve_path(&block, &anchor.path)?;
    if node.node_type() == Node::TEXT_NODE {
        if let Some(rect) = collapsed_range_rect(&node, anchor.offset.max(0)) {
            return Some(col_from_rect(runtime, &flow, &rect));
        }
    }
    let element = node.dyn_ref::<Element>().unwrap_or(&block);
    // Measure the element's first actually visible content, not its container
    // box: when a block's first fragment is stranded in the previous column
    // (break-inside:avoid media pushed whole into the next one) the raw box
    // resolves one column -- one page -- early. Mirrors the reference
    // `colForAnchor` (visualStartRect -> first client rect -> bounding box).
    let rect = visual_start(element)
        .map(|start| start.rect)
        .or_else(|| element.get_client_rects().item(0))
        .unwrap_or_else(|| element.get_bounding_client_rect());
    Some(col_from_rect(runtime, &flow, &rect))
}

pub(super) fn collapsed_range_rect(node: &Node, offset: i32) -> Option<DomRect> {
    let document = node.owner_document()?;
    let range = document.create_range().ok()?;
    let length = node
        .text_content()
        .map_or(0, |value| value.encode_utf16().count());
    let offset = offset.clamp(0, i32::try_from(length).unwrap_or(i32::MAX)) as u32;
    range.set_start(node, offset).ok()?;
    range.collapse();
    first_rect_of_range(&range).or_else(|| Some(range.get_bounding_client_rect()))
}

pub(super) fn find_block(flow: &Element, block: i32) -> Option<Element> {
    let nodes = flow.query_selector_all("[data-block]").ok()?;
    for index in 0..nodes.length() {
        let element = nodes.item(index)?.dyn_into::<Element>().ok()?;
        if element
            .get_attribute("data-block")
            .and_then(|value| value.parse::<i32>().ok())
            == Some(block)
        {
            return Some(element);
        }
    }
    None
}

pub(super) fn resolve_path(block: &Element, path: &[i32]) -> Option<Node> {
    let mut node: Node = block.clone().unchecked_into();
    for index in path {
        if *index < 0 {
            return None;
        }
        node = node.child_nodes().item(*index as u32)?;
    }
    Some(node)
}

pub(super) fn child_index(parent: &Node, child: &Node) -> Option<i32> {
    let children = parent.child_nodes();
    for index in 0..children.length() {
        if children
            .item(index)
            .is_some_and(|value| value.is_same_node(Some(child)))
        {
            return i32::try_from(index).ok();
        }
    }
    None
}

pub(super) fn anchor_from_node(
    manifest: &FlowManifest,
    block: &Element,
    node: &Node,
    offset: i32,
) -> Anchor {
    let block_id = block
        .get_attribute("data-block")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    let block_node: Node = block.clone().unchecked_into();
    let mut path = Vec::new();
    let mut current = node.clone();
    while !current.is_same_node(Some(&block_node)) {
        let Some(parent) = current.parent_node() else {
            return Anchor {
                spine: spine_for_block(manifest, block_id),
                block: block_id,
                path: Vec::new(),
                offset: Anchor::BOUNDARY_OFFSET,
            };
        };
        let Some(index) = child_index(&parent, &current) else {
            break;
        };
        path.push(index);
        current = parent;
    }
    path.reverse();
    Anchor {
        spine: spine_for_block(manifest, block_id),
        block: block_id,
        path,
        offset: if node.node_type() == Node::TEXT_NODE {
            offset.max(0)
        } else {
            Anchor::BOUNDARY_OFFSET
        },
    }
}

pub(super) fn rect_has_box(rect: &DomRect) -> bool {
    rect.width() > 0.0 || rect.height() > 0.0
}

pub(super) fn rect_in_current_column(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    viewport: &Element,
    rect: &DomRect,
) -> bool {
    if !rect_has_box(rect) {
        return false;
    }
    let state = runtime.borrow();
    let flow_rect = flow.get_bounding_client_rect();
    let viewport_rect = viewport.get_bounding_client_rect();
    if state.metrics.pitch <= 0.0 {
        return false;
    }
    let column = ((rect.left() - flow_rect.left() - state.metrics.side + 1.0) / state.metrics.pitch)
        .floor() as i32;
    column == state.current_col
        && rect.right() > viewport_rect.left() + state.metrics.side
        && rect.left() < viewport_rect.right() - state.metrics.side
        && rect.bottom() > viewport_rect.top() + state.metrics.top
        && rect.top() < viewport_rect.bottom() - state.metrics.bottom
}

pub(super) fn first_rect_of_range(range: &web_sys::Range) -> Option<DomRect> {
    range.get_client_rects().and_then(|rects| rects.item(0))
}

#[derive(Clone)]
pub(super) struct VisualStart {
    pub(super) rect: DomRect,
    pub(super) node: Option<Node>,
    pub(super) offset: i32,
}

pub(super) fn first_text_visual(node: &Node, depth: usize) -> Option<VisualStart> {
    if depth > MAX_TEXT_WALK_DEPTH {
        return None;
    }
    if node.node_type() == Node::TEXT_NODE {
        let text = node.text_content().unwrap_or_default();
        let (character_index, _) = text
            .char_indices()
            .find(|(_, value)| !value.is_whitespace())?;
        let offset = text[..character_index].encode_utf16().count() as i32;
        let rect = collapsed_range_rect(node, offset)?;
        return rect_has_box(&rect).then(|| VisualStart {
            rect,
            node: Some(node.clone()),
            offset,
        });
    }
    let children = node.child_nodes();
    for index in 0..children.length() {
        let Some(child) = children.item(index) else {
            continue;
        };
        if let Some(start) = first_text_visual(&child, depth + 1) {
            return Some(start);
        }
    }
    None
}

pub(super) fn first_media_visual(element: &Element) -> Option<VisualStart> {
    let nodes = element
        .query_selector_all("img,svg,video,canvas,iframe,embed,object,table")
        .ok()?;
    for index in 0..nodes.length() {
        let node = nodes.item(index)?;
        let element = node.dyn_into::<Element>().ok()?;
        let rect = element.get_bounding_client_rect();
        if rect_has_box(&rect) {
            return Some(VisualStart {
                rect,
                node: Some(element.unchecked_into()),
                offset: Anchor::BOUNDARY_OFFSET,
            });
        }
    }
    None
}

/// Find the first actual visual content of an element. A block's own first
/// fragment is not sufficient in CSS columns: a break-inside-avoid image can
/// move the content into a later column while the container fragment stays in
/// the previous one.
pub(super) fn visual_start(element: &Element) -> Option<VisualStart> {
    let root: Node = element.clone().unchecked_into();
    let text = first_text_visual(&root, 0);
    let media = first_media_visual(element);
    let content = match (text, media) {
        (Some(text), Some(media)) => {
            let text_before_media =
                text.node
                    .as_ref()
                    .zip(media.node.as_ref())
                    .is_some_and(|(text, media)| {
                        text.compare_document_position(media) & Node::DOCUMENT_POSITION_FOLLOWING
                            != 0
                    });
            if text_before_media { text } else { media }
        }
        (Some(text), None) => text,
        (None, Some(media)) => media,
        (None, None) => {
            let rect = element
                .get_client_rects()
                .item(0)
                .unwrap_or_else(|| element.get_bounding_client_rect());
            if !rect_has_box(&rect) && rect.left() == 0.0 && rect.top() == 0.0 {
                return None;
            }
            VisualStart {
                rect,
                node: None,
                offset: Anchor::BOUNDARY_OFFSET,
            }
        }
    };
    Some(content)
}

pub(super) fn anchor_at_text_fragment(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    manifest: &FlowManifest,
    flow: &Element,
    viewport: &Element,
    block: &Element,
    node: &Node,
    first_offset: i32,
) -> Option<Anchor> {
    let length = node
        .text_content()
        .map_or(0, |value| value.encode_utf16().count());
    let length = i32::try_from(length).unwrap_or(i32::MAX);
    let current_col = runtime.borrow().current_col;
    let mut low = first_offset.clamp(0, length);
    let mut high = length;
    while low < high {
        let middle = low + (high - low) / 2;
        let Some(rect) = collapsed_range_rect(node, middle) else {
            low = middle.saturating_add(1);
            continue;
        };
        if col_from_rect(runtime, flow, &rect) >= current_col {
            high = middle;
        } else {
            low = middle.saturating_add(1);
        }
    }
    let start = low.saturating_sub(2).max(first_offset);
    let end = (low.saturating_add(2)).min(length);
    for offset in start..=end {
        if let Some(rect) = collapsed_range_rect(node, offset)
            && rect_in_current_column(runtime, flow, viewport, &rect)
        {
            return Some(anchor_from_node(manifest, block, node, offset));
        }
    }
    None
}

pub(super) fn first_text_anchor(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    manifest: &FlowManifest,
    flow: &Element,
    viewport: &Element,
    block: &Element,
    node: &Node,
    depth: usize,
) -> Option<Anchor> {
    if depth > MAX_TEXT_WALK_DEPTH {
        return None;
    }
    if node.node_type() == Node::TEXT_NODE {
        let text = node.text_content().unwrap_or_default();
        let Some((character_index, _)) = text
            .char_indices()
            .find(|(_, value)| !value.is_whitespace())
        else {
            return None;
        };
        let offset = text[..character_index].encode_utf16().count() as i32;
        return anchor_at_text_fragment(runtime, manifest, flow, viewport, block, node, offset);
    }

    let children = node.child_nodes();
    for index in 0..children.length() {
        let Some(child) = children.item(index) else {
            continue;
        };
        if let Some(anchor) =
            first_text_anchor(runtime, manifest, flow, viewport, block, &child, depth + 1)
        {
            return Some(anchor);
        }
    }
    None
}

pub(super) fn anchor_from_visible_block(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    manifest: &FlowManifest,
    flow: &Element,
    viewport: &Element,
    block: &Element,
) -> Option<Anchor> {
    let rects = block.get_client_rects();
    let visible = (0..rects.length()).any(|position| {
        rects
            .item(position)
            .is_some_and(|rect| rect_in_current_column(runtime, flow, viewport, &rect))
    });
    if !visible {
        return None;
    }

    if let Some(start) = visual_start(block)
        && rect_in_current_column(runtime, flow, viewport, &start.rect)
    {
        let block_node: Node = block.clone().unchecked_into();
        if let Some(node) = start.node {
            return Some(anchor_from_node(manifest, block, &node, start.offset));
        }
        return Some(anchor_from_node(
            manifest,
            block,
            &block_node,
            Anchor::BOUNDARY_OFFSET,
        ));
    }

    let text = first_text_anchor(
        runtime,
        manifest,
        flow,
        viewport,
        block,
        &{
            let node: Node = block.clone().unchecked_into();
            node
        },
        0,
    )
    .map(|anchor| {
        let node =
            resolve_path(block, &anchor.path).unwrap_or_else(|| block.clone().unchecked_into());
        (node, anchor)
    });
    let media = first_media_in_current_column(runtime, flow, viewport, block);
    match (text, media) {
        (Some((text_node, text_anchor)), Some((media_node, media_anchor))) => {
            if text_node.compare_document_position(&media_node) & Node::DOCUMENT_POSITION_FOLLOWING
                != 0
            {
                Some(text_anchor)
            } else {
                Some(media_anchor)
            }
        }
        (Some((_, anchor)), None) | (None, Some((_, anchor))) => Some(anchor),
        (None, None) => {
            let block_node: Node = block.clone().unchecked_into();
            Some(anchor_from_node(
                manifest,
                block,
                &block_node,
                Anchor::BOUNDARY_OFFSET,
            ))
        }
    }
}

pub(super) fn first_media_in_current_column(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    viewport: &Element,
    block: &Element,
) -> Option<(Node, Anchor)> {
    let manifest = runtime.borrow().manifest.clone()?;
    let nodes = block
        .query_selector_all("img,svg,video,canvas,iframe,embed,object,table")
        .ok()?;
    for index in 0..nodes.length() {
        let node = nodes.item(index)?;
        let element = node.clone().dyn_into::<Element>().ok()?;
        let rect = element.get_bounding_client_rect();
        if rect_in_current_column(runtime, flow, viewport, &rect) {
            return Some((
                node.clone(),
                anchor_from_node(&manifest, block, &node, Anchor::BOUNDARY_OFFSET),
            ));
        }
    }
    None
}

pub(super) fn block_at_point(x: f64, y: f64) -> Option<Element> {
    let document = web_sys::window()?.document()?;
    let elements = document.elements_from_point(x as f32, y as f32);
    for value in elements.iter() {
        let element = value.dyn_into::<Element>().ok()?;
        if element.has_attribute("data-block") {
            return Some(element);
        }
        if let Ok(Some(block)) = element.closest("[data-block]") {
            return Some(block);
        }
    }
    None
}

pub(super) fn block_for_node(node: &Node) -> Option<Element> {
    let mut current = node.clone();
    loop {
        if let Some(element) = current.dyn_ref::<Element>()
            && element.has_attribute("data-block")
        {
            return Some(element.clone());
        }
        current = current.parent_node()?;
    }
}

pub(super) fn content_origin(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    viewport: &Element,
) -> (f64, f64) {
    let state = runtime.borrow();
    let rect = viewport.get_bounding_client_rect();
    (
        rect.left() + state.metrics.side + 2.0,
        rect.top() + state.metrics.top + 2.0,
    )
}

pub(super) fn capture_top_anchor(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
) -> Option<Anchor> {
    let viewport = viewport_element(viewport)?;
    let flow = flow_element(flow)?;
    let manifest = runtime.borrow().manifest.clone()?;
    let (x, y) = content_origin(runtime, &viewport);
    if let Some(document) = web_sys::window().and_then(|window| window.document())
        && let Some(caret) = document.caret_position_from_point(x as f32, y as f32)
        && let Some(node) = caret.offset_node()
        && node.node_type() == Node::TEXT_NODE
        && let Some(block) = block_for_node(&node)
    {
        let offset = i32::try_from(caret.offset()).unwrap_or(i32::MAX);
        let anchor = anchor_from_node(&manifest, &block, &node, offset);
        if collapsed_range_rect(&node, offset)
            .is_some_and(|rect| rect_in_current_column(runtime, &flow, &viewport, &rect))
        {
            return Some(anchor);
        }
        if let Some(block) = block_at_point(x, y)
            && let Some(anchor) =
                anchor_from_visible_block(runtime, &manifest, &flow, &viewport, &block)
        {
            return Some(anchor);
        }
    }
    if let Some(block) = block_at_point(x, y)
        && let Some(anchor) =
            anchor_from_visible_block(runtime, &manifest, &flow, &viewport, &block)
    {
        return Some(anchor);
    }
    let blocks = flow.query_selector_all("[data-block]").ok()?;
    for index in 0..blocks.length() {
        let Some(block) = blocks.item(index)?.dyn_into::<Element>().ok() else {
            continue;
        };
        if let Some(anchor) =
            anchor_from_visible_block(runtime, &manifest, &flow, &viewport, &block)
        {
            return Some(anchor);
        }
    }
    None
}

pub(super) fn viewport_element(viewport: DivRef) -> Option<Element> {
    viewport
        .get()
        .map(|value| value.unchecked_into::<Element>())
}

pub(super) fn refresh_ui(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: DivRef,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
) {
    let mut state = runtime.borrow_mut();
    let Some(manifest) = &state.manifest else {
        toc_active.set(-1);
        percent.set(0.0);
        return;
    };
    let Some(anchor) = &state.top_anchor else {
        toc_active.set(-1);
        percent.set(0.0);
        return;
    };
    toc_active.set(toc_active_index(manifest, anchor.block));
    let value = percent_for_anchor(manifest, flow, anchor);
    state.top_percent = value;
    percent.set(value);
}

pub(super) fn percent_for_anchor(manifest: &FlowManifest, flow: DivRef, anchor: &Anchor) -> f64 {
    if manifest.total_chars <= 0 {
        return 0.0;
    }
    let Some(chunk) = manifest.chunk_for_block(anchor.block) else {
        return 0.0;
    };
    let mut chars = chunk_prefix(manifest, chunk);
    if let Some(flow) = flow_element(flow)
        && let Ok(chunks) = flow.query_selector_all(".rf-chunk")
    {
        for index in 0..chunks.length() {
            let Some(node) = chunks.item(index) else {
                continue;
            };
            let Ok(element) = node.dyn_into::<Element>() else {
                continue;
            };
            if element
                .get_attribute("data-chunk")
                .and_then(|value| value.parse::<i32>().ok())
                != Some(chunk)
            {
                continue;
            }
            let children = element.child_nodes();
            for child_index in 0..children.length() {
                let Some(child) = children.item(child_index) else {
                    continue;
                };
                let Ok(child) = child.dyn_into::<Element>() else {
                    continue;
                };
                let Some(block) = child
                    .get_attribute("data-block")
                    .and_then(|value| value.parse::<i32>().ok())
                else {
                    continue;
                };
                if block >= anchor.block {
                    break;
                }
                chars = chars.saturating_add(
                    child
                        .text_content()
                        .map_or(0, |value| value.encode_utf16().count()) as i64,
                );
            }
            break;
        }
    }
    (chars as f64 / manifest.total_chars as f64 * 100.0).clamp(0.0, 100.0)
}
