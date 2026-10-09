//! Breadcrumbs and file-browser actions.

use leptos::prelude::*;
use revaro_core::model::File;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::browser;
use crate::logic::format::format_size;

use super::icons;

/// The file-browser header, including sorting.
#[component]
pub fn FileBrowserHeader(
    breadcrumbs: RwSignal<Vec<File>>,
    current: RwSignal<Option<File>>,
    item_count: RwSignal<Vec<File>>,
    listing_total: RwSignal<i64>,
    sort_order: RwSignal<String>,
    on_sort: Callback<String>,
    total_bytes: RwSignal<i64>,
    file_count: RwSignal<i64>,
    trash_mode: RwSignal<bool>,
    on_open_folder: Callback<String>,
    on_empty_trash: Callback<()>,
) -> impl IntoView {
    let sort_container = NodeRef::<leptos::html::Div>::new();
    let sort_toggle = NodeRef::<leptos::html::Button>::new();
    let sort_open = RwSignal::new(false);
    let sort_panel = NodeRef::<leptos::html::Div>::new();
    browser::anchor_popover(sort_toggle, sort_panel);

    browser::dismiss_popover(
        sort_open.into(),
        move || {
            sort_container
                .get_untracked()
                .map(|node| node.unchecked_into())
        },
        move |target| {
            sort_container
                .get_untracked()
                .is_some_and(|node| node.contains(Some(target)))
        },
        Callback::new(move |restore_focus| {
            sort_open.set(false);
            if restore_focus && let Some(button) = sort_toggle.get() {
                let _ = button.focus();
            }
        }),
    );

    let path_items = Signal::derive_local(move || {
        let path = breadcrumbs.get();
        if path.is_empty() {
            current.get().into_iter().collect::<Vec<_>>()
        } else {
            path
        }
    });

    // The Vue header always reveals the current end of a long breadcrumb
    // after navigation. Keep that behavior on narrow screens and deep paths;
    // waiting one task lets the reactive path children finish rendering before
    // measuring the scroll width.
    let breadcrumb_nav = NodeRef::<leptos::html::Nav>::new();
    let breadcrumb_current = current;
    Effect::new(move |_| {
        let _ = breadcrumb_current.get();
        let Some(window) = web_sys::window() else {
            return;
        };
        let nav = breadcrumb_nav;
        let callback = Closure::once(move || {
            if let Some(nav) = nav.get() {
                let options = web_sys::ScrollToOptions::new();
                options.set_left(f64::from(nav.scroll_width()));
                options.set_behavior(web_sys::ScrollBehavior::Smooth);
                nav.scroll_to_with_scroll_to_options(&options);
            }
        })
        .into_js_value();
        let _ = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
    });
    let select_sort = Callback::new(move |field: &'static str| {
        let current = sort_order.get_untracked();
        if !current.starts_with(field) {
            on_sort.run(if field == "updated" {
                format!("{field}_desc")
            } else {
                field.to_owned()
            });
        }
        sort_open.set(false);
        if let Some(button) = sort_toggle.get() {
            let _ = button.focus();
        }
    });
    let reverse_sort = Callback::new(move |(): ()| {
        let current = sort_order.get_untracked();
        let next = if let Some(field) = current.strip_suffix("_desc") {
            field.to_owned()
        } else {
            format!("{current}_desc")
        };
        sort_open.set(false);
        on_sort.run(next);
    });
    let sort_label = move || {
        let order = sort_order.get();
        if order.starts_with("size") {
            "大小"
        } else if order.starts_with("updated") {
            "时间"
        } else {
            "名称"
        }
    };

    view! {
        <div class="content-head" class:trash-toolbar=move || trash_mode.get()>
                <Show when=move || !trash_mode.get() && !path_items.get().is_empty() fallback=|| ()>
                    <nav node_ref=breadcrumb_nav class="breadcrumbs" aria-label="当前路径">
                        {move || {
                            let path = path_items.get();
                            let last = path.len().saturating_sub(1);
                            path.into_iter().enumerate().map(|(index, crumb)| {
                                let id = crumb.id;
                                let label = if crumb.name.is_empty() { "我的文件".to_owned() } else { crumb.name };
                                let open = on_open_folder.clone();
                                view! {
                                    <Show when=move || { index > 0 } fallback=|| ()>
                                        {icons::breadcrumb_separator()}
                                    </Show>
                                    <button
                                        type="button"
                                        class:current=index == last
                                        title=label.clone()
                                        aria-current=if index == last { Some("page") } else { None }
                                        on:click=move |_| open.run(id.clone())
                                    >{label.clone()}</button>
                                }
                            }).collect_view()
                        }}
                    </nav>
                </Show>
                <p class="folder-meta">
                    <span>{move || format!("{} 个项目", if trash_mode.get() { item_count.get().len() as i64 } else { listing_total.get() })}</span><i></i>
                    <Show
                        when=move || trash_mode.get()
                        fallback=move || view! {
                            <span>{move || format!("共 {} 个文件", file_count.get().max(0))}</span><i></i>
                            <span>{move || format_size(non_negative(total_bytes.get()))}</span>
                        }
                    >
                        <span>{move || format_size(non_negative(total_bytes.get()))}</span><i></i>
                        <span>"已删除的文件将在 30 天后永久删除"</span>
                    </Show>
                </p>
            <Show
                when=move || trash_mode.get()
                fallback=move || view! {
                    <div class="actions">
                        <div node_ref=sort_container class="file-sort" role="group" aria-label="文件排序">
                            <button node_ref=sort_toggle class="sort-field-toggle" type="button"
                                aria-label="选择排序字段" aria-haspopup="true"
                                aria-expanded=move || sort_open.get().to_string() aria-controls="file-sort-fields"
                                on:click=move |_| sort_open.update(|open| *open = !*open)>
                                <span>{sort_label}</span>
                            </button>
                            <button class="sort-direction" type="button" aria-label="切换排序方向"
                                aria-description=move || if sort_order.get().ends_with("_desc") { "当前降序，点击切换为升序" } else { "当前升序，点击切换为降序" }
                                on:click=move |_| reverse_sort.run(())>
                                <span aria-hidden="true">{move || if sort_order.get().ends_with("_desc") { "↓" } else { "↑" }}</span>
                            </button>
                            <Show when=move || sort_open.get() fallback=|| ()>
                                <div node_ref=sort_panel id="file-sort-fields" class="sort-field-options" role="group" aria-label="排序字段">
                                    {[("name", "名称"), ("size", "大小"), ("updated", "时间")]
                                        .into_iter().map(|(field, label)| view! {
                                            <button type="button" aria-pressed=move || sort_order.get().starts_with(field).to_string()
                                                on:click=move |_| select_sort.run(field)>{label}</button>
                                        }).collect_view()}
                                </div>
                            </Show>
                        </div>
                    </div>
                }
            >
                <div class="actions"><button class="trash-empty-action" type="button" prop:disabled=move || item_count.get().is_empty() on:click=move |_| on_empty_trash.run(())>"清空回收站"</button></div>
            </Show>
        </div>
    }
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}
