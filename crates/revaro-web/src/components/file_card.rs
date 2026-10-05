//! Shared card content for files, home sections and content libraries.

use leptos::prelude::*;
use revaro_core::{
    classify,
    model::{File, FileKind},
};

#[component]
pub(super) fn FileCard(
    name: String,
    detail: Signal<String>,
    #[prop(optional)] preview_title: Option<Signal<String>>,
    #[prop(optional)] cannot_open: Option<Signal<bool>>,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="file-card-content">
            <div class="card-preview"
                class:cannot-open=move || cannot_open.is_some_and(|value| value.get())
                title=move || preview_title.map(|value| value.get())>
                {children()}
            </div>
            <CardInfo name=name detail=detail />
        </div>
    }
}

#[component]
fn CardInfo(name: String, detail: Signal<String>) -> impl IntoView {
    view! {
        <div class="card-info">
            <strong title=name.clone()>{name.clone()}</strong>
            <small>{move || detail.get()}</small>
        </div>
    }
}

pub(super) fn file_icon(file: &File) -> AnyView {
    if file.kind == FileKind::Directory {
        view! {
            <svg class="file-type-icon folder-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="folder-back" d="M10 23c0-4 3-7 7-7h21l10 11h31c4 0 7 3 7 7v9H10Z"></path>
                <path class="folder-front" d="M8 38c0-4 3-7 7-7h66c5 0 8 4 7 9l-7 35c-1 4-4 6-8 6H16c-4 0-7-3-7-7Z"></path>
                <path class="folder-highlight" d="M17 38h62l-1 6H16Z"></path>
            </svg>
        }
        .into_any()
    } else if classify::is_video(file) {
        view! {
            <span class="large-video" aria-hidden="true">
                <svg viewBox="0 0 24 24"><path d="m9 7 8 5-8 5Z"></path></svg>
            </span>
        }
        .into_any()
    } else if file.kind == FileKind::File && classify::is_epub_name(&file.name) {
        view! {
            <svg class="file-type-icon book-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="icon-base" d="M48 24c-9-6-20-8-34-8v57c14 0 25 2 34 8 9-6 20-8 34-8V16c-14 0-25 2-34 8Z"></path>
                <path class="icon-detail" d="M48 24v57M23 31c7 0 13 1 18 4M23 44c7 0 13 1 18 4M73 31c-7 0-13 1-18 4M73 44c-7 0-13 1-18 4"></path>
            </svg>
        }
        .into_any()
    } else if classify::is_editable(file) {
        view! {
            <svg class="file-type-icon document-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="icon-base" d="M22 10h38l17 17v58H22Z"></path>
                <path class="icon-fold" d="M60 10v17h17Z"></path>
                <path class="icon-detail" d="M34 45h31M34 57h31M34 69h22"></path>
            </svg>
        }
        .into_any()
    } else if classify::is_audio(file) {
        view! {
            <svg class="file-type-icon audio-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="icon-base" d="M22 10h38l17 17v58H22Z"></path>
                <path class="icon-fold" d="M60 10v17h17Z"></path>
                <path class="icon-detail audio-note" d="M62 42v27m0-27-20 5v27"></path>
                <ellipse class="icon-accent" cx="35" cy="75" rx="9" ry="7"></ellipse>
                <ellipse class="icon-accent" cx="55" cy="70" rx="9" ry="7"></ellipse>
            </svg>
        }
        .into_any()
    } else {
        view! {
            <svg class="file-type-icon generic-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="icon-base" d="M22 10h38l17 17v58H22Z"></path>
                <path class="icon-fold" d="M60 10v17h17Z"></path>
                <circle class="icon-accent" cx="49" cy="58" r="5"></circle>
            </svg>
        }
        .into_any()
    }
}
