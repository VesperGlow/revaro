//! Library cover cards and reusable reading progress.

use super::*;

#[component]
pub(in crate::components) fn BookProgressBar(
    #[prop(optional_no_strip)] percent: Option<f64>,
    #[prop(optional)] file_id: Option<String>,
) -> impl IntoView {
    let progress = RwSignal::new(percent.filter(|p| p.is_finite() && *p > 0.0));
    if let Some(id) = file_id {
        leptos::task::spawn_local(async move {
            if let Ok(value) = api::fetch_book_progress(&id).await {
                let _ = progress.try_set(value.percent.filter(|p| p.is_finite() && *p > 0.0));
            }
        });
    }
    view! {
        {move ||progress.get().map(|p|view! {
            <div class="book-reading-progress" role="progressbar" aria-label="阅读进度" aria-valuemin="0" aria-valuemax="100" aria-valuenow=p.clamp(0.0,100.0) title=format!("已读 {p:.1}%")>
                <span style:width=format!("{}%",p.clamp(0.0,100.0))></span>
            </div>
        })}
    }
}

#[component]
pub(super) fn LibraryCover(item: LibraryItem) -> impl IntoView {
    let failed = RwSignal::new(false);
    let kind = item.kind.clone();
    let class = format!("library-cover {}-cover", kind);
    if item.kind == "audio" || revaro_core::classify::is_editable(&item.file) {
        return view! {
            <div class=class>
                <div class="cover-placeholder">{file_icon(&item.file)}</div>
                <BookProgressBar percent=item.reading_progress />
            </div>
        }
        .into_any();
    }
    let is_video = item.kind == "video";
    let duration = RwSignal::new(
        item.duration_ms
            .filter(|ms| *ms > 0)
            .map(|ms| format_media_time(ms as f64 / 1000.0)),
    );
    let thumbnail_url = crate::components::resource_url::thumbnail_url(&item.file);
    let retry = RwSignal::new(0_u8);
    let retry_thumbnail = move |_| {
        failed.set(true);
        if is_video
            && retry.get_untracked() < 4
            && let Some(window) = web_sys::window()
        {
            use wasm_bindgen::{JsCast, closure::Closure};
            let callback = Closure::once_into_js(move || {
                if let Some(attempt) = retry.try_get_untracked() {
                    retry.set(attempt + 1);
                    failed.set(false);
                }
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                1500,
            );
        }
    };
    if is_video && duration.get_untracked().is_none() {
        let id = item.file.id.clone();
        leptos::task::spawn_local(async move {
            if let Ok(metadata) = api::fetch_video_media(&id).await
                && metadata.duration_ms > 0
            {
                let _ = duration.try_set(Some(format_media_time(
                    metadata.duration_ms as f64 / 1000.0,
                )));
            }
        });
    }
    view! {
        <div class=class>
            <Show when=move || !failed.get() fallback=move || view! {
                <div class="cover-placeholder">{file_icon(&item.file)}</div>
            }>
                <img loading="lazy" src={let url = thumbnail_url.clone(); move || format!("{}&retry={}", url, retry.get())} alt="" on:error=retry_thumbnail />
            </Show>
            <BookProgressBar percent=item.reading_progress />
            {is_video.then(|| view! { <span class="video-cover-play">{icons::play()}</span><span class="video-duration">{move || duration.get().unwrap_or_else(|| "—:—".to_owned())}</span> })}
        </div>
    }.into_any()
}

/// Render up to three real covers in the explicit member order.
#[component]
pub(super) fn StackCover(item: LibraryItem) -> impl IntoView {
    if let Some(stack) = &item.stack {
        let layers = stack.files.iter().take(3).enumerate().map(|(index, file)| {
            let mut cover = item.clone();
            cover.file = file.clone();
            cover.stack = None;
            if index>0 { cover.reading_progress=None; }
            view! { <div class="stack-cover-layer" style:z-index=(3-index).to_string()><LibraryCover item=cover /></div> }
        }).collect_view();
        view! { <div class="stack-covers">{layers}</div> }.into_any()
    } else {
        view! { <LibraryCover item=item /> }.into_any()
    }
}
