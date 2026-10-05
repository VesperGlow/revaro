//! A compact, whole-row book selector with a persistent selection footer.
use std::collections::HashMap;

use wasm_bindgen::closure::Closure;

use super::stacks::{StackController, StackOperation};
use super::*;
use crate::components::music_player::display_title;

#[component]
pub(super) fn StackBookPicker(controller: StackController, stack_id: String) -> impl IntoView {
    let c = controller;
    let search = RwSignal::new(String::new());
    let dialog = NodeRef::<leptos::html::Section>::new();
    let search_input = NodeRef::<leptos::html::Input>::new();
    let close_button = NodeRef::<leptos::html::Button>::new();
    let close = Callback::new(move |()| {
        if !c.busy.get_untracked() {
            c.dialog.set(None);
        }
    });
    let destination = stack_id.clone();
    let memberships = Memo::new(move |_| {
        c.all
            .get()
            .into_iter()
            .filter(|stack| stack.id != destination)
            .flat_map(|stack| {
                stack
                    .files
                    .into_iter()
                    .map(move |file| (file.id, stack.name.clone()))
            })
            .collect::<HashMap<_, _>>()
    });
    let visible = Memo::new(move |_| {
        let query = search.get().trim().to_lowercase();
        c.candidates
            .get()
            .into_iter()
            .filter(|file| file.name.to_lowercase().contains(&query))
            .collect::<Vec<_>>()
    });

    // The visual viewport keeps the footer above the mobile search keyboard.
    let viewport = RwSignal::new((0.0_f64, 0.0_f64));
    let measure = Callback::new(move |()| {
        if let Some(window) = web_sys::window() {
            let dimensions = window
                .visual_viewport()
                .map(|v| (v.height(), v.offset_top()))
                .unwrap_or_else(|| {
                    (
                        window
                            .inner_height()
                            .ok()
                            .and_then(|h| h.as_f64())
                            .unwrap_or(800.0),
                        0.0,
                    )
                });
            let _ = viewport.try_set(dimensions);
        }
    });
    measure.run(());
    let mut resize = browser::on_resize(move |_| measure.run(()));
    on_cleanup(move || resize.release());
    if let Some(visual) = web_sys::window().and_then(|w| w.visual_viewport()) {
        let update = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| measure.run(()));
        for name in ["resize", "scroll"] {
            let _ = visual.add_event_listener_with_callback(name, update.as_ref().unchecked_ref());
        }
        let cleanup = leptos::__reexports::send_wrapper::SendWrapper::new((visual, update));
        on_cleanup(move || {
            let (visual, update) = cleanup.take();
            for name in ["resize", "scroll"] {
                let _ = visual
                    .remove_event_listener_with_callback(name, update.as_ref().unchecked_ref());
            }
        });
    }
    if let Some(document) = web_sys::window().and_then(|w| w.document())
        && let Some(body) = document.body()
    {
        let previous = document
            .active_element()
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok());
        let overflow = body
            .style()
            .get_property_value("overflow")
            .unwrap_or_default();
        let _ = body.style().set_property("overflow", "hidden");
        let cleanup =
            leptos::__reexports::send_wrapper::SendWrapper::new((body, overflow, previous));
        on_cleanup(move || {
            let (body, overflow, previous) = cleanup.take();
            if overflow.is_empty() {
                let _ = body.style().remove_property("overflow");
            } else {
                let _ = body.style().set_property("overflow", &overflow);
            }
            if let Some(previous) = previous.filter(|e| e.is_connected()) {
                let options = web_sys::FocusOptions::new();
                options.set_prevent_scroll(true);
                let _ = previous.focus_with_options(&options);
            }
        });
    }
    Effect::new(move |_| {
        if dialog.get().is_some() {
            let desktop = web_sys::window()
                .and_then(|w| w.inner_width().ok())
                .and_then(|w| w.as_f64())
                .is_some_and(|w| w > 850.0);
            if desktop {
                if let Some(input) = search_input.get_untracked() {
                    let _ = input.focus();
                }
            } else if let Some(button) = close_button.get_untracked() {
                let _ = button.focus();
            }
        }
    });

    view! {
        <div class="stack-picker-viewport" style=move || {
            let (height, top) = viewport.get();
            format!("height:{height}px;top:{top}px;--picker-viewport-height:{height}px")
        }>
            <DialogBackdrop class="modal-backdrop stack-picker-backdrop" on_close=close>
                <section class="modal stack-book-picker" node_ref=dialog role="dialog" aria-modal="true" aria-labelledby="stack-picker-title" aria-describedby="stack-picker-description"
                    on:keydown=move |event: web_sys::KeyboardEvent| {
                        if event.key()=="Escape" { event.prevent_default();event.stop_propagation();close.run(()); }
                        if event.key()=="Tab" && let Some(section)=dialog.get_untracked()
                            && let Ok(nodes)=section.query_selector_all("button:not(:disabled),input:not(:disabled)") {
                            let controls=(0..nodes.length()).filter_map(|i|nodes.item(i).and_then(|n|n.dyn_into::<web_sys::HtmlElement>().ok())).collect::<Vec<_>>();
                            let active=web_sys::window().and_then(|w|w.document()).and_then(|d|d.active_element());
                            if let (Some(first),Some(last))=(controls.first(),controls.last()) {
                                let target=if event.shift_key() && active.as_ref()==Some(first.as_ref()){Some(last)}
                                    else if !event.shift_key() && active.as_ref()==Some(last.as_ref()){Some(first)}else{None};
                                if let Some(target)=target {event.prevent_default();let _=target.focus();}
                            }
                        }
                    }>
                    <div class="stack-picker-grip" aria-hidden="true"></div>
                    <header><h2 id="stack-picker-title">"添加书籍"</h2><button node_ref=close_button type="button" aria-label="关闭书籍选择器" disabled=move ||c.busy.get() on:click=move |_|close.run(())>{icons::x()}</button></header>
                    <div class="stack-picker-search" role="search">
                        <div class="stack-picker-search-field">{icons::search()}<input node_ref=search_input type="search" aria-label="搜索待添加书籍" placeholder="搜索书名" prop:value=move ||search.get() on:input=move |event|search.set(event_target_value(&event)) /></div>
                        <p id="stack-picker-description">"书籍只改变堆叠关系，不影响原文件和分类"</p>
                    </div>
                    <div class="stack-picker-scroll" aria-busy=move ||c.pick_loading.get().to_string()>
                        <Show when=move ||!c.pick_loading.get() fallback=||view! {<div class="stack-picker-empty" role="status"><div class="spinner"></div><p>"正在加载书籍…"</p></div>}>
                            <ul class="stack-picker-list" aria-label="可添加书籍">
                                <For each=move ||visible.get() key=|file|file.id.clone() children=move |file| {
                                    let selected_id=file.id.clone();
                                    let selected=Memo::new(move |_|c.picked.with(|ids|ids.contains(&selected_id)));
                                    let source_id=file.id.clone();
                                    let origin=Memo::new(move |_|memberships.with(|map|map.get(&source_id).cloned()));
                                    let metadata_id=format!("stack-picker-meta-{}",file.id);
                                    let metadata_ref=metadata_id.clone();
                                    let failed=RwSignal::new(false);
                                    let url=crate::components::resource_url::thumbnail_url(&file);
                                    let title=display_title(&file.name);
                                    let format=revaro_core::classify::extension(&file.name).to_uppercase();
                                    let metadata=format!("{} · {}",format,crate::logic::format::format_size(file.size.max(0) as u64));
                                    view! {
                                        <li><button type="button" class="stack-picker-row" role="checkbox" aria-label=format!("添加 {}",file.name) aria-describedby=metadata_ref aria-checked=move ||selected.get().to_string()
                                            class:selected=move ||selected.get() disabled=move ||c.busy.get() data-file-id=file.id.clone()
                                            on:click=move |_|c.picked.update(|ids|{if !ids.remove(&file.id){ids.insert(file.id.clone());}})>
                                            <span class="stack-picker-cover" aria-hidden="true"><Show when=move ||!failed.get() fallback=move ||view! {<span class="stack-picker-cover-fallback">{icons::book_open()}<small>{format.clone()}</small></span>}><img loading="lazy" decoding="async" src=url.clone() alt="" draggable="false" on:error=move |_|failed.set(true) /></Show></span>
                                            <span class="stack-picker-copy"><strong title=title.clone()>{title.clone()}</strong><span id=metadata_id><small>{metadata}</small>
                                                {move ||origin.get().map(|name| { let tooltip=name.clone(); view! {
                                                    <small class="stack-picker-origin" class:moving=move ||selected.get() title=tooltip>
                                                        <span>{move ||if selected.get(){format!("来自 {name}")}else{format!("所属堆叠：{name}")}}</span>
                                                        <Show when=move ||selected.get() fallback=|| ()><span class="stack-picker-move">"将移入当前堆叠"</span></Show>
                                                    </small>
                                                } })}
                                            </span></span>
                                            <span class="stack-picker-check" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12 4 4L19 6"></path></svg></span>
                                        </button></li>
                                    }
                                } />
                            </ul>
                            <Show when=move ||visible.get().is_empty() && c.error.get().is_empty() fallback=|| ()><div class="stack-picker-empty" role="status"><p>{move ||if search.get().trim().is_empty(){"暂无可添加的书籍"}else{"没有找到相关书籍"}}</p></div></Show>
                        </Show>
                    </div>
                    <Show when=move ||!c.error.get().is_empty() fallback=|| ()><div class="stack-picker-error" role="alert"><span>{move ||c.error.get()}</span><Show when=move ||c.candidates.get().is_empty() && !c.pick_loading.get() fallback=|| ()><button type="button" on:click=move |_|c.add_books.run(())>"重试"</button></Show></div></Show>
                    <footer class="stack-picker-footer"><span role="status" aria-live="polite">{move ||format!("已选 {} 本",c.picked.get().len())}</span>
                        <button class="primary" type="button" disabled=move ||c.busy.get() ||c.pick_loading.get() ||c.picked.get().is_empty() on:click=move |_| {
                            let picked=c.picked.get_untracked();
                            let ids=c.candidates.get_untracked().into_iter().filter(|f|picked.contains(&f.id)).map(|f|f.id).collect();
                            c.run.run(StackOperation::Add(stack_id.clone(),ids));
                        }>{move ||if c.busy.get(){"添加中…"}else{"添加"}}</button>
                    </footer>
                </section>
            </DialogBackdrop>
        </div>
    }
}
