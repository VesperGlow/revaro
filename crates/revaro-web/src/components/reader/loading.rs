//! Book opening, chunk requests and the bounded DOM window.

use super::*;

pub(super) async fn open_reader(
    runtime: Rc<RefCell<ReaderRuntime>>,
    file_id: String,
    title: RwSignal<String>,
    manifest_signal: RwSignal<Option<FlowManifest>>,
    stage: RwSignal<ReaderStage>,
    loading_text: RwSignal<String>,
    error_text: RwSignal<String>,
    on_unauthorized: Callback<()>,
    viewport: DivRef,
    flow: DivRef,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    prefs: RwSignal<ReaderPrefs>,
) {
    let (book_result, progress_result) = futures_util::join!(
        api::fetch_book(&file_id),
        api::fetch_book_progress(&file_id)
    );

    if let Err(error) = &book_result
        && error.is_unauthorized()
    {
        on_unauthorized.run(());
        return;
    }
    if let Err(error) = &progress_result
        && error.is_unauthorized()
    {
        on_unauthorized.run(());
        return;
    }
    if let Ok(book) = book_result {
        let display = if !book.name.is_empty() {
            book.name
        } else {
            book.title
        };
        if !display.is_empty() {
            title.set(classify::reader_display_title(&display));
        }
    }
    let saved = progress_result.ok().and_then(|value| value.anchor);

    let cached = reader_cache::load_manifest(&file_id);
    let mut rendered_from_cache = false;
    if let Some(cached_manifest) = cached.clone() {
        loading_text.set("正在读取缓存书页…".to_owned());
        if setup_view(
            runtime.clone(),
            file_id.clone(),
            cached_manifest,
            saved.clone(),
            manifest_signal,
            stage,
            loading_text,
            viewport,
            flow,
            toc_active,
            percent,
            prefs.get_untracked(),
        )
        .await
        .is_ok()
        {
            rendered_from_cache = true;
        } else {
            reset_view(&runtime, flow, manifest_signal);
        }
    }

    // Keep the network phase wording from the reference reader. The flow
    // endpoint is an implementation detail; users see the same book-loading
    // state while its manifest is being fetched.
    loading_text.set("正在读取书籍…".to_owned());
    let network = api::fetch_book_flow(&file_id).await;
    if runtime.borrow().closing {
        return;
    }
    match network {
        Ok(network_manifest) => {
            if !validate_manifest(&network_manifest) {
                if !rendered_from_cache {
                    stage.set(ReaderStage::Error);
                    error_text.set("阅读流清单无效".to_owned());
                }
                return;
            }
            reader_cache::store_manifest(&file_id, &network_manifest);
            let same = rendered_from_cache
                && runtime
                    .borrow()
                    .manifest
                    .as_ref()
                    .is_some_and(|current| same_layout(current, &network_manifest));
            if same {
                return;
            }
            if rendered_from_cache {
                reset_view(&runtime, flow, manifest_signal);
            }
            loading_text.set("正在排版…".to_owned());
            if let Err(error) = setup_view(
                runtime,
                file_id,
                network_manifest,
                saved,
                manifest_signal,
                stage,
                loading_text,
                viewport,
                flow,
                toc_active,
                percent,
                prefs.get_untracked(),
            )
            .await
            {
                stage.set(ReaderStage::Error);
                error_text.set(error);
            }
        }
        Err(error) if error.is_unauthorized() => on_unauthorized.run(()),
        Err(error) if !rendered_from_cache => {
            stage.set(ReaderStage::Error);
            error_text.set(error.message);
        }
        Err(_) => {
            // A cached, authenticated view remains useful while offline. The
            // next open will validate it against the no-cache manifest again.
        }
    }
}

pub(super) async fn setup_view(
    runtime: Rc<RefCell<ReaderRuntime>>,
    file_id: String,
    value: FlowManifest,
    saved: Option<Anchor>,
    manifest_signal: RwSignal<Option<FlowManifest>>,
    stage: RwSignal<ReaderStage>,
    loading_text: RwSignal<String>,
    viewport: DivRef,
    flow: DivRef,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    prefs: ReaderPrefs,
) -> Result<(), String> {
    if !validate_manifest(&value) {
        return Err("阅读流清单无效".to_owned());
    }
    let total = total_blocks(&value);
    let first_block = value.spines.first().map_or(0, |spine| spine.block_start);
    let anchor = saved
        .filter(|anchor| anchor.is_valid() && anchor.block >= 0 && anchor.block < total.max(1))
        .unwrap_or(Anchor {
            spine: 0,
            block: first_block,
            path: Vec::new(),
            offset: Anchor::BOUNDARY_OFFSET,
        });
    let metrics = apply_metrics(viewport, flow, &value.format, prefs);
    {
        let mut state = runtime.borrow_mut();
        state.generation = state.generation.wrapping_add(1);
        state.manifest = Some(value.clone());
        state.metrics = metrics;
        state.top_anchor = Some(anchor.clone());
        state.current_col = 0;
        state.cols = 1;
    }
    manifest_signal.set(Some(value.clone()));
    loading_text.set("正在加载书页…".to_owned());
    let (first, last) = stable_window_range(&value, anchor.block, AHEAD_MARGIN);
    ensure_window(&runtime, &file_id, &value, flow, first, last).await?;
    measure_cols(&runtime, flow);
    stage.set(ReaderStage::Reading);
    let col = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
    {
        let mut state = runtime.borrow_mut();
        state.current_col = col.clamp(0, state.cols.saturating_sub(1));
    }
    set_x(&runtime, flow, false);
    if let Some(captured) = capture_top_anchor(&runtime, viewport, flow) {
        runtime.borrow_mut().top_anchor = Some(captured);
    }
    refresh_ui(&runtime, flow, toc_active, percent);
    schedule_progress_save(runtime.clone(), file_id.clone());
    prefetch_chunks(runtime, file_id, value, anchor.block);
    Ok(())
}

pub(super) fn reset_view(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: DivRef,
    manifest_signal: RwSignal<Option<FlowManifest>>,
) {
    if let Some(flow) = flow_element(flow) {
        flow.set_inner_html("");
    }
    let mut state = runtime.borrow_mut();
    state.generation = state.generation.wrapping_add(1);
    state.manifest = None;
    state.cache.clear();
    state.in_flight.clear();
    state.waiters.clear();
    state.first_chunk = 0;
    state.last_chunk = -1;
    state.cols = 1;
    state.current_col = 0;
    state.top_anchor = None;
    manifest_signal.set(None);
}

pub(super) async fn load_chunk(
    runtime: Rc<RefCell<ReaderRuntime>>,
    file_id: String,
    manifest: FlowManifest,
    index: i32,
) -> Result<String, String> {
    let (key, receiver) = {
        let mut state = runtime.borrow_mut();
        if let Some(value) = state.cache.get(index) {
            return Ok(value);
        }
        let key = ChunkLoadKey {
            generation: state.generation,
            index,
        };
        if state.in_flight.insert(key.clone()) {
            (key, None)
        } else {
            let (sender, receiver) = oneshot::channel();
            state.waiters.entry(key.clone()).or_default().push(sender);
            (key, Some(receiver))
        }
    };
    if let Some(receiver) = receiver {
        return receiver
            .await
            .unwrap_or_else(|_| Err("内容片段加载已取消".to_owned()));
    }

    let cache_key = reader_cache::chunk_cache_key(&file_id, &manifest, index);
    let result = if let Some(value) = reader_cache::get_chunk(&cache_key).await {
        Ok(value)
    } else {
        api::fetch_book_chunk(&file_id, index)
            .await
            .map_err(|error| error.message)
    };
    let (waiters, should_persist) = {
        let mut state = runtime.borrow_mut();
        let accepts_result = state
            .manifest
            .as_ref()
            .is_some_and(|current| same_layout(current, &manifest));
        let accepts_current = accepts_result && state.generation == key.generation;
        if let Ok(value) = &result
            && accepts_current
        {
            state.cache.insert(index, value.clone());
        }
        state.in_flight.remove(&key);
        (
            state.waiters.remove(&key).unwrap_or_default(),
            accepts_current,
        )
    };
    for waiter in waiters {
        let _ = waiter.send(result.clone());
    }
    if should_persist && result.is_ok() {
        if let Ok(value) = &result {
            let value = value.clone();
            let cache_key = cache_key.clone();
            leptos::task::spawn_local(async move {
                reader_cache::put_chunk(&cache_key, &value).await;
            });
        }
    }
    result
}

pub(super) async fn ensure_window(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    file_id: &str,
    manifest: &FlowManifest,
    flow: DivRef,
    first: i32,
    last: i32,
) -> Result<bool, String> {
    if runtime.borrow().closing {
        return Ok(false);
    }
    let Some(flow) = flow_element(flow) else {
        return Ok(false);
    };
    let generation = runtime.borrow().generation;
    if manifest.chunks.is_empty() {
        let mut state = runtime.borrow_mut();
        state.first_chunk = 0;
        state.last_chunk = -1;
        return Ok(false);
    }
    let final_chunk = manifest.chunks.len().saturating_sub(1) as i32;
    let first = first.clamp(0, final_chunk);
    let last = last.clamp(first, final_chunk);

    let nodes = flow
        .query_selector_all(".rf-chunk")
        .map_err(|_| "阅读流 DOM 不可用".to_owned())?;
    let mut existing = HashSet::new();
    for index in 0..nodes.length() {
        let Some(node) = nodes.item(index) else {
            continue;
        };
        let Ok(element) = node.dyn_into::<Element>() else {
            continue;
        };
        let Some(chunk) = element
            .get_attribute("data-chunk")
            .and_then(|value| value.parse::<i32>().ok())
        else {
            let _ = element
                .parent_node()
                .map(|parent| parent.remove_child(&element));
            continue;
        };
        if chunk < first || chunk > last {
            let _ = element
                .parent_node()
                .map(|parent| parent.remove_child(&element));
        } else {
            existing.insert(chunk);
        }
    }

    let missing: Vec<i32> = (first..=last)
        .filter(|index| !existing.contains(index))
        .collect();
    if missing.is_empty() {
        let mut state = runtime.borrow_mut();
        let changed = state.first_chunk != first || state.last_chunk != last;
        state.first_chunk = first;
        state.last_chunk = last;
        return Ok(changed);
    }

    let state = runtime.clone();
    let id = file_id.to_owned();
    let value = manifest.clone();
    let concurrency = missing.len().min(6).max(1);
    let loaded: Vec<(i32, Result<String, String>)> = stream::iter(missing.iter().copied())
        .map(|index| {
            let state = state.clone();
            let id = id.clone();
            let value = value.clone();
            async move { (index, load_chunk(state, id, value, index).await) }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;
    if runtime.borrow().closing || runtime.borrow().generation != generation {
        return Ok(false);
    }
    let mut html = HashMap::new();
    for (index, result) in loaded {
        html.insert(index, result?);
    }

    for index in missing {
        let Some(value) = html.remove(&index) else {
            continue;
        };
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return Err("浏览器文档不可用".to_owned());
        };
        let element = document
            .create_element("div")
            .map_err(|_| "无法创建阅读流节点".to_owned())?;
        element.set_class_name("rf-chunk");
        element
            .set_attribute("data-chunk", &index.to_string())
            .map_err(|_| "无法标记阅读流节点".to_owned())?;
        // The server's reader sanitizer is the trust boundary for this HTML;
        // the wrapper is created locally so arbitrary response markup cannot
        // escape the flow structure.
        element.set_inner_html(&value);
        let reference = flow.query_selector_all(".rf-chunk").ok().and_then(|nodes| {
            (0..nodes.length()).find_map(|position| {
                let node = nodes.item(position)?;
                let element = node.clone().dyn_into::<Element>().ok()?;
                let chunk = element.get_attribute("data-chunk")?.parse::<i32>().ok()?;
                (chunk > index).then_some(node)
            })
        });
        let node: Node = element.clone().unchecked_into();
        if let Some(reference) = reference {
            let _ = flow.insert_before(&node, Some(&reference));
        } else {
            let _ = flow.append_child(&node);
        }
    }

    let mut state = runtime.borrow_mut();
    state.first_chunk = first;
    state.last_chunk = last;
    Ok(true)
}

pub(super) fn prefetch_chunks(
    runtime: Rc<RefCell<ReaderRuntime>>,
    file_id: String,
    manifest: FlowManifest,
    block: i32,
) {
    let Some(center) = manifest.chunk_for_block(block) else {
        return;
    };
    for distance in 1..=2 {
        for index in [center + distance, center - distance] {
            if index < 0 || index >= manifest.chunks.len() as i32 {
                continue;
            }
            let runtime = runtime.clone();
            let file_id = file_id.clone();
            let manifest = manifest.clone();
            leptos::task::spawn_local(async move {
                let _ = load_chunk(runtime, file_id, manifest, index).await;
            });
        }
    }
}
