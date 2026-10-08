//! Shared seek gestures for both presentations of the same audio clock.
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlInputElement, PointerEvent};

#[component]
pub(super) fn AudioSeekInput(
    duration: Signal<f64>,
    position: Signal<f64>,
    enabled: Signal<bool>,
    label: &'static str,
    #[prop(default = "")] class: &'static str,
    on_preview: Callback<f64>,
    on_commit: Callback<f64>,
    on_cancel: Callback<()>,
    #[prop(optional)] on_hover: Option<Callback<PointerEvent>>,
    #[prop(optional)] on_leave: Option<Callback<PointerEvent>>,
) -> impl IntoView {
    let pointer = StoredValue::new(None::<i32>);
    let update_pointer = move |event: &PointerEvent| {
        let input = event
            .current_target()?
            .dyn_into::<HtmlInputElement>()
            .ok()?;
        let bounds = input.get_bounding_client_rect();
        if bounds.width() <= 0.0 {
            return None;
        }
        let ratio =
            ((f64::from(event.client_x()) - bounds.left()) / bounds.width()).clamp(0.0, 1.0);
        let target = ratio * duration.get_untracked();
        input.set_value(&target.to_string());
        on_preview.run(target);
        Some(target)
    };
    view! {
        <input class=class aria-label=label type="range" min="0"
            max=move || {let total=duration.get();if total>0.0 {total}else{1.0}}.to_string() step="0.1"
            prop:disabled=move ||!enabled.get()
            prop:value=move ||position.get().clamp(0.0,duration.get()).to_string()
            on:input=move |event| {
                if pointer.get_value().is_some() {
                    // Native touch range handling can run after pointermove.
                    // Keep its DOM value on the same clock as our preview.
                    if let Some(input)=event.target().and_then(|target|target.dyn_into::<HtmlInputElement>().ok()) {
                        input.set_value(&position.get_untracked().to_string());
                    }
                } else if let Ok(value)=event_target_value(&event).parse::<f64>() {on_preview.run(value);}
            }
            on:change=move |event| {
                if pointer.get_value().is_none()
                    && let Ok(value)=event_target_value(&event).parse::<f64>() {on_commit.run(value);}
            }
            on:pointerdown=move |event: PointerEvent| {
                if !enabled.get_untracked() || !event.is_primary() || event.button()!=0 {return;}
                event.prevent_default();
                if let Some(input)=event.current_target().and_then(|target|target.dyn_into::<HtmlInputElement>().ok()) {
                    let options=web_sys::FocusOptions::new();
                    options.set_prevent_scroll(true);
                    let _=input.focus_with_options(&options);
                    let _=input.set_pointer_capture(event.pointer_id());
                    pointer.set_value(Some(event.pointer_id()));
                    update_pointer(&event);
                }
            }
            on:pointermove=move |event: PointerEvent| {
                if pointer.get_value()==Some(event.pointer_id()) {event.prevent_default();update_pointer(&event);}
                if let Some(hover)=on_hover {hover.run(event);}
            }
            on:pointerup=move |event: PointerEvent| {
                if pointer.get_value()!=Some(event.pointer_id()) {return;}
                event.prevent_default();
                if let Some(target)=update_pointer(&event) {on_commit.run(target);}
                pointer.set_value(None);
                if let Some(input)=event.current_target().and_then(|target|target.dyn_into::<HtmlInputElement>().ok()) {
                    let _=input.release_pointer_capture(event.pointer_id());
                }
            }
            on:pointercancel=move |_| {pointer.set_value(None);on_cancel.run(());}
            on:lostpointercapture=move |_| {
                if pointer.get_value().is_some() {pointer.set_value(None);on_cancel.run(());}
            }
            on:pointerleave=move |event| {if let Some(leave)=on_leave {leave.run(event);}}
        />
    }
}
