//! Viewport metrics and CSS-column transforms.

use super::*;

pub(super) fn flow_element(flow: DivRef) -> Option<Element> {
    flow.get().map(|value| value.unchecked_into::<Element>())
}

pub(super) fn flow_html_element(flow: DivRef) -> Option<HtmlElement> {
    flow.get()
        .map(|value| value.unchecked_into::<HtmlElement>())
}

pub(super) fn viewport_size(viewport: DivRef) -> (f64, f64) {
    if let Some(viewport) = viewport
        .get()
        .map(|value| value.unchecked_into::<HtmlElement>())
    {
        let width = f64::from(viewport.client_width().max(0));
        let height = f64::from(viewport.client_height().max(0));
        if width > 0.0 && height > 0.0 {
            return (width, height);
        }
    }
    let Some(window) = web_sys::window() else {
        return (0.0, 0.0);
    };
    let width = window
        .inner_width()
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
        .max(0.0);
    let height = window
        .inner_height()
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
        .max(0.0);
    (width, height)
}

pub(super) fn apply_metrics(
    viewport: DivRef,
    flow: DivRef,
    format: &str,
    prefs: ReaderPrefs,
) -> ReaderMetrics {
    let (width, height) = viewport_size(viewport);
    let margins = compute_margins(width, height);
    let metrics = ReaderMetrics {
        width,
        height,
        side: f64::from(margins.side),
        top: f64::from(margins.top),
        bottom: f64::from(margins.bottom),
        pitch: width,
        col_height: (height - f64::from(margins.top) - f64::from(margins.bottom)).max(1.0),
    };
    if let Some(flow) = flow_html_element(flow) {
        let style = flow.style();
        let _ = style.set_property("width", &format!("{}px", metrics.width));
        let _ = style.set_property("height", &format!("{}px", metrics.height));
        let _ = style.set_property("box-sizing", "border-box");
        let _ = style.set_property(
            "padding",
            &format!("{}px {}px {}px", metrics.top, metrics.side, metrics.bottom),
        );
        let _ = style.set_property(
            "column-width",
            &format!("{}px", (metrics.width - 2.0 * metrics.side).max(1.0)),
        );
        let _ = style.set_property("column-gap", &format!("{}px", 2.0 * metrics.side));
        let _ = style.set_property("column-fill", "auto");
        let _ = style.set_property(
            "--revaro-font-family",
            r#""Noto Serif SC", "Songti SC", Georgia, "Times New Roman", "STSong", SimSun, serif"#,
        );
        let _ = style.set_property(
            "--revaro-font-size",
            &format!("{}px", clamp_font_size(prefs.font_size)),
        );
        let _ = style.set_property(
            "--revaro-line-height",
            &valid_line_height(prefs.line_height).to_string(),
        );
        let _ = style.set_property("--revaro-col-height", &format!("{}px", metrics.col_height));
        if format.eq_ignore_ascii_case("txt") {
            let _ = flow.class_list().add_1("txt");
        } else {
            let _ = flow.class_list().remove_1("txt");
        }
    }
    metrics
}

pub(super) fn measure_cols(runtime: &Rc<RefCell<ReaderRuntime>>, flow: DivRef) {
    let Some(flow) = flow_html_element(flow) else {
        return;
    };
    let mut state = runtime.borrow_mut();
    if state.metrics.pitch <= 0.0 {
        state.cols = 1;
        return;
    }
    let total = f64::from(flow.scroll_width()).max(state.metrics.pitch);
    state.cols = (total / state.metrics.pitch).round().max(1.0) as i32;
}

pub(super) fn set_x(runtime: &Rc<RefCell<ReaderRuntime>>, flow: DivRef, animated: bool) {
    let Some(flow) = flow_html_element(flow) else {
        return;
    };
    let state = runtime.borrow();
    let x = -f64::from(state.current_col) * state.metrics.pitch;
    let style = flow.style();
    let _ = style.set_property(
        "transition",
        if animated {
            "transform 260ms cubic-bezier(.22,.72,.26,1)"
        } else {
            "none"
        },
    );
    let _ = style.set_property("transform", &format!("translateX({x}px)"));
}

pub(super) fn set_x_offset(runtime: &Rc<RefCell<ReaderRuntime>>, flow: DivRef, offset: f64) {
    let Some(flow) = flow_html_element(flow) else {
        return;
    };
    let state = runtime.borrow();
    let x = -f64::from(state.current_col) * state.metrics.pitch + offset;
    let style = flow.style();
    let _ = style.set_property("transition", "none");
    let _ = style.set_property("transform", &format!("translateX({x}px)"));
}

pub(super) fn set_promote(flow: DivRef, enabled: bool) {
    if let Some(flow) = flow_html_element(flow) {
        let _ = flow
            .style()
            .set_property("will-change", if enabled { "transform" } else { "auto" });
    }
}
