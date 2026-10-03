//! Shared disclosure menu, extracted from the media preview toolbar.
use super::icons;
use crate::browser;
use leptos::ev::MouseEvent;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, KeyboardEvent};

/// The static icon used by a small native details menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuIcon {
    /// An overflow action menu.
    More,
    /// Playback settings.
    Settings,
    /// Volume controls.
    Volume,
    /// Create a document or directory.
    Create,
    /// Upload files or a directory.
    Upload,
    Link,
    SystemStatus,
}

/// A disclosure menu that closes when focus moves outside it.
///
/// Native `<details>` preserves keyboard and screen-reader semantics. The
/// document listener only adds the outside-pointer behaviour that native
/// disclosure elements do not provide consistently across browsers.
#[component]
pub fn ActionMenu(
    label: String,
    icon: MenuIcon,
    #[prop(optional)] volume: Option<RwSignal<f64>>,
    #[prop(optional)] muted: Option<RwSignal<bool>>,
    #[prop(optional)] on_toggle: Option<Callback<bool>>,
    #[prop(optional)] disabled: Option<Signal<bool>>,
    #[prop(optional)] context: Option<Signal<String>>,
    #[prop(optional)] text: Option<Signal<String>>,
    #[prop(optional)] usage: Option<Signal<Option<f64>>>,
    #[prop(optional, into)] panel_class: String,
    children: Children,
) -> impl IntoView {
    let menu = NodeRef::<leptos::html::Details>::new();
    let trigger = NodeRef::<leptos::html::Summary>::new();
    let panel = NodeRef::<leptos::html::Div>::new();
    let position = browser::anchor_popover(trigger, panel);
    let outside_menu = menu;
    let mut outside = browser::on_pointerdown(move |event| {
        let Some(details) = outside_menu.get() else {
            return;
        };
        if !details.open() {
            return;
        }
        let inside = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            .is_some_and(|target| details.contains(Some(&target)));
        if !inside {
            details.set_open(false);
        }
    });
    on_cleanup(move || outside.release());

    if let Some(context) = context {
        Effect::new(move |_| {
            let _ = context.get();
            if let Some(details) = menu.get_untracked() {
                details.set_open(false);
            }
        });
    }
    let menu_for_escape = menu;
    let on_toggle = on_toggle.clone();
    view! {
        <details
            node_ref=menu
            class="action-menu"
            class:collection-menu=text.is_some()
            on:focusout=move |event| {
                if let Some(details) = menu.get() {
                    let inside = event.related_target()
                        .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
                        .is_some_and(|target| details.contains(Some(&target)));
                    // A revoke can remove the focused row. Keep the disclosure open
                    // when the browser sends a focusout without a new target.
                    if event.related_target().is_some() && !inside { details.set_open(false); }
                }
            }
            on:toggle=move |_| {
                position.run(());
                if let Some(callback) = on_toggle.as_ref()
                    && let Some(details) = menu_for_escape.get()
                {
                    callback.run(details.open());
                }
            }
            on:keydown=move |event: KeyboardEvent| {
                if event.key() == "Escape" {
                    if let Some(details) = menu_for_escape.get() {
                        if details.open() {
                            event.prevent_default();
                            event.stop_propagation();
                            details.set_open(false);
                            let _ = details
                                .query_selector("summary")
                                .ok()
                                .flatten()
                                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                                .map(|element| element.focus());
                        }
                    }
                }
            }
        >
            <summary node_ref=trigger
                aria-label=label.clone()
                title=label
                tabindex=move || if disabled.is_some_and(|value| value.get()) { -1 } else { 0 }
                aria-disabled=move || disabled.is_some_and(|value| value.get()).to_string()
                on:click=move |event| {
                    if disabled.is_some_and(|value| value.get_untracked()) {
                        event.prevent_default();
                    }
                }
            >
                {move || {
                    if let Some(text) = text {
                        view! { <span>{move || text.get()}</span>{icons::chevron_down()} }.into_any()
                    } else if icon == MenuIcon::Volume
                        && (muted.is_some_and(|value| value.get())
                            || volume.is_some_and(|value| value.get() <= 0.0))
                    {
                        icons::volume_x().into_any()
                    } else {
                        menu_icon(icon, usage)
                    }
                }}
            </summary>
            <div
                node_ref=panel class=format!("action-menu-panel {panel_class}")
                on:click=move |event: MouseEvent| {
                    let Some(target) = event
                        .target()
                        .and_then(|target| target.dyn_into::<Element>().ok())
                    else {
                        return;
                    };
                    if target.closest("[data-close-menu]").ok().flatten().is_some()
                        && let Some(details) = menu.get()
                    {
                        details.set_open(false);
                    }
                }
            >{children()}</div>
        </details>
    }
}

fn menu_icon(icon: MenuIcon, usage: Option<Signal<Option<f64>>>) -> AnyView {
    match icon {
        MenuIcon::More => icons::more_horizontal().into_any(),
        MenuIcon::Settings => icons::settings_2().into_any(),
        MenuIcon::Volume => icons::volume_2().into_any(),
        MenuIcon::Create => icons::circle_plus().into_any(),
        MenuIcon::Upload => icons::upload().into_any(),
        MenuIcon::Link => icons::link().into_any(),
        MenuIcon::SystemStatus => view! {
            <span class="system-status-ball" class:pending=move || usage.and_then(|s| s.get()).is_none()>
                <svg viewBox="0 0 36 36" aria-hidden="true" fill="none">
                    <circle class="storage-ring-track" cx="18" cy="18" r="15" stroke-width="2" />
                    <circle class="storage-ring-progress" cx="18" cy="18" r="15" stroke-width="2" pathLength="100"
                        stroke-dasharray=move || format!("{} 100", usage.and_then(|s| s.get()).unwrap_or(0.0)) transform="rotate(-90 18 18)" />
                </svg>
                <span>{move || usage.and_then(|s| s.get()).map(|value| format!("{value:.0}%")).unwrap_or_else(|| "—".to_owned())}</span>
            </span>
        }.into_any(),
    }
}
