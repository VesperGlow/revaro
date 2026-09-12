//! File classification shared by the server's library views and the browser UI.
//!
//! Before the migration these rules existed twice: once in Go
//! (`isImageSource`, `isVideoSource`, `isAudioSource`, `isBookSource`,
//! `responseMime`, `safeDeliveryMime`) and once in TypeScript
//! (`web/src/fileTypes.ts`). Their drift is exactly the kind of bug a unified
//! Rust workspace removes, so they now live here and both ends call the same
//! functions.

use crate::model::{File, FileKind, FileStatus};

/// The five buckets the sidebar groups the library into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LibraryKind {
    /// EPUB and plain-text books.
    Book,
    /// Still images.
    Image,
    /// Video files.
    Video,
    /// Audio files.
    Audio,
    /// Everything else.
    #[default]
    File,
}

impl LibraryKind {
    /// Every bucket, in the order the sidebar presents them.
    pub const ALL: [LibraryKind; 5] = [
        LibraryKind::Book,
        LibraryKind::Image,
        LibraryKind::Video,
        LibraryKind::Audio,
        LibraryKind::File,
    ];
    /// The wire and query-string name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LibraryKind::Book => "book",
            LibraryKind::Image => "image",
            LibraryKind::Video => "video",
            LibraryKind::Audio => "audio",
            LibraryKind::File => "file",
        }
    }
}

impl std::str::FromStr for LibraryKind {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "book" => Ok(LibraryKind::Book),
            "image" => Ok(LibraryKind::Image),
            "video" => Ok(LibraryKind::Video),
            "audio" => Ok(LibraryKind::Audio),
            "file" => Ok(LibraryKind::File),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for LibraryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for LibraryKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for LibraryKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <String as serde::Deserialize>::deserialize(deserializer)?;
        raw.parse()
            .map_err(|()| serde::de::Error::custom(format!("unknown library type {raw:?}")))
    }
}

/// Extensions treated as video regardless of the stored MIME type.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "webm", "mov", "m4v", "mkv", "avi", "ogv", "mpg", "mpeg", "wmv", "flv", "ts", "m2ts",
    "mts",
];

/// Extensions treated as audio regardless of the stored MIME type.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "flac", "m4a", "aac", "ogg", "oga", "opus", "wma", "aif", "aiff", "ape",
];

/// Extensions treated as images regardless of the stored MIME type.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "avif", "bmp"];

/// Extensions the reader can open.
pub const BOOK_EXTENSIONS: &[&str] = &["epub", "txt"];

/// Extensions the built-in text editor can open.
pub const EDITABLE_EXTENSIONS: &[&str] = &[
    "md", "markdown", "txt", "yaml", "yml", "json", "toml", "ini", "conf", "log", "csv",
];

/// Archive suffixes, longest first so `.tar.gz` wins over `.gz`.
pub const ARCHIVE_SUFFIXES: &[&str] = &[
    "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "tgz", "tbz2", "tbz", "txz", "tzst", "zip", "7z",
    "rar", "tar", "gz", "bz2", "xz", "zst",
];

/// The lowercase extension of `name`, without the dot.
///
/// A leading dot is not an extension (`.bashrc` has none), matching Go's
/// `filepath.Ext` semantics.
#[must_use]
pub fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(index) if index > 0 && index + 1 < name.len() => &name[index + 1..],
        _ => "",
    }
}

fn has_extension(name: &str, allowed: &[&str]) -> bool {
    let ext = extension(name);
    !ext.is_empty()
        && allowed
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext))
}

/// The MIME type to serve a file with, inferring one from the name when the
/// stored type is missing or the generic octet-stream.
#[must_use]
pub fn response_mime(file: &File) -> String {
    if !file.mime_type.is_empty() && file.mime_type != "application/octet-stream" {
        return file.mime_type.clone();
    }
    match extension(&file.name).to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "m4v" => "video/x-m4v",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "flv" => "video/x-flv",
        "wmv" => "video/x-ms-wmv",
        "mpg" | "mpeg" => "video/mpeg",
        "ts" | "m2ts" | "mts" => "video/mp2t",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "yaml" | "yml" => "application/yaml; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "toml" => "application/toml; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "txt" | "conf" | "ini" | "log" => "text/plain; charset=utf-8",
        _ => return file.mime_type.clone(),
    }
    .to_owned()
}

/// The MIME type a text document is stored with.
#[must_use]
pub fn document_mime(name: &str) -> &'static str {
    match extension(name).to_ascii_lowercase().as_str() {
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "yaml" | "yml" => "application/yaml; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "toml" => "application/toml; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    }
}

/// True when `name` is a file the built-in editor may open.
#[must_use]
pub fn is_editable_name(name: &str) -> bool {
    has_extension(name, EDITABLE_EXTENSIONS)
}

/// True when `name` is a book the reader may open.
#[must_use]
pub fn is_book_name(name: &str) -> bool {
    has_extension(name, BOOK_EXTENSIONS)
}

/// True when `name` is an EPUB specifically.
#[must_use]
pub fn is_epub_name(name: &str) -> bool {
    extension(name).eq_ignore_ascii_case("epub")
}

/// True when `name` looks like an archive.
#[must_use]
pub fn is_archive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ARCHIVE_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(&format!(".{suffix}")))
}

/// True when the file is a ready, editable text document within the size cap.
#[must_use]
pub fn is_editable(file: &File) -> bool {
    is_ready_file(file)
        && file.size <= crate::limits::MAX_DOCUMENT_BYTES as i64
        && is_editable_name(&file.name)
}

/// True when the file is a ready, openable book within the size cap.
#[must_use]
pub fn is_book(file: &File) -> bool {
    is_ready_file(file) && is_book_name(&file.name)
}

/// True when the file is a ready image.
#[must_use]
pub fn is_image(file: &File) -> bool {
    if !is_ready_file(file) {
        return false;
    }
    let mime = response_mime(file).to_ascii_lowercase();
    if matches!(
        mime.as_str(),
        "image/jpeg" | "image/png" | "image/webp" | "image/gif" | "image/avif"
    ) {
        return true;
    }
    has_extension(&file.name, IMAGE_EXTENSIONS)
}

/// True when the file is a ready video.
#[must_use]
pub fn is_video(file: &File) -> bool {
    if !is_ready_file(file) {
        return false;
    }
    response_mime(file)
        .to_ascii_lowercase()
        .starts_with("video/")
        || has_extension(&file.name, VIDEO_EXTENSIONS)
}

/// True when the file is a ready audio file.
#[must_use]
pub fn is_audio(file: &File) -> bool {
    if !is_ready_file(file) {
        return false;
    }
    response_mime(file)
        .to_ascii_lowercase()
        .starts_with("audio/")
        || has_extension(&file.name, AUDIO_EXTENSIONS)
}

/// True when the file is a ready archive.
#[must_use]
pub fn is_archive(file: &File) -> bool {
    is_ready_file(file) && is_archive_name(&file.name)
}

/// True when the file belongs to `kind`.
#[must_use]
pub fn matches_library_kind(file: &File, kind: LibraryKind) -> bool {
    match kind {
        LibraryKind::Book => is_book(file),
        LibraryKind::Image => is_image(file),
        LibraryKind::Video => is_video(file),
        LibraryKind::Audio => is_audio(file),
        LibraryKind::File => is_ready_file(file),
    }
}

/// True when a ready file's bytes can be rendered inline by the browser.
#[must_use]
pub fn is_previewable(file: &File) -> bool {
    let mime = response_mime(file);
    if mime.starts_with("video/") || mime.starts_with("audio/") {
        return true;
    }
    matches!(
        mime.as_str(),
        "image/jpeg" | "image/png" | "image/webp" | "image/gif" | "image/avif"
    )
}

fn is_ready_file(file: &File) -> bool {
    file.kind == FileKind::File && file.status == FileStatus::Ready
}

/// MIME types that must never be served with an executable interpretation from
/// the application origin, even if a user uploaded a file claiming that type.
///
/// Active web formats stay downloadable, but only as opaque bytes.
const UNSAFE_DELIVERY_TYPES: &[&str] = &[
    "text/html",
    "application/xhtml+xml",
    "image/svg+xml",
    "application/xml",
    "text/xml",
    "application/javascript",
    "text/javascript",
    "application/ecmascript",
    "text/ecmascript",
];

/// Downgrade an active web MIME type to `application/octet-stream`.
///
/// A value that is not a media type at all is also downgraded, so an
/// unparseable `Content-Type` can never be echoed back to the browser.
#[must_use]
pub fn safe_delivery_mime(value: &str) -> String {
    let Some((media_type, _)) = crate::validate::parse_media_type(value) else {
        return "application/octet-stream".to_owned();
    };
    if UNSAFE_DELIVERY_TYPES
        .iter()
        .any(|unsafe_type| unsafe_type.eq_ignore_ascii_case(&media_type))
    {
        "application/octet-stream".to_owned()
    } else {
        value.to_owned()
    }
}

/// Strip a trailing reading-format suffix from a book name for display.
///
/// Only the reader's title is normalized; stored names never change.
#[must_use]
pub fn reader_display_title(name: &str) -> String {
    const READING_SUFFIXES: &[&str] = &[
        "epub", "txt", "pdf", "md", "markdown", "mobi", "azw", "azw3", "fb2", "djvu", "cbz", "cbr",
    ];
    let mut title = name;
    loop {
        let ext = extension(title);
        if ext.is_empty()
            || !READING_SUFFIXES
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
        {
            break;
        }
        title = &title[..title.len() - ext.len() - 1];
    }
    if title.is_empty() {
        name.to_owned()
    } else {
        title.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileKind, FileStatus};

    fn file(name: &str, mime: &str) -> File {
        File {
            name: name.to_owned(),
            mime_type: mime.to_owned(),
            kind: FileKind::File,
            status: FileStatus::Ready,
            ..File::default()
        }
    }

    #[test]
    fn extension_matches_go_filepath_ext() {
        assert_eq!(extension("movie.mp4"), "mp4");
        assert_eq!(extension("archive.tar.gz"), "gz");
        assert_eq!(extension("MYFILE.MP4"), "MP4");
        assert_eq!(extension(".bashrc"), "");
        assert_eq!(extension("noextension"), "");
        assert_eq!(extension("trailing."), "");
    }

    #[test]
    fn library_kinds_round_trip_through_their_names() {
        for kind in LibraryKind::ALL {
            assert_eq!(kind.as_str().parse::<LibraryKind>(), Ok(kind));
        }
        assert!("nope".parse::<LibraryKind>().is_err());
    }

    #[test]
    fn classification_prefers_the_stored_mime_type() {
        assert!(is_image(&file("photo.bin", "image/png")));
        assert!(is_video(&file("clip.bin", "video/mp4")));
        assert!(is_audio(&file("song.bin", "audio/flac")));
    }

    #[test]
    fn classification_falls_back_to_the_extension() {
        assert!(is_image(&file("photo.JPG", "")));
        assert!(is_video(&file("clip.MKV", "application/octet-stream")));
        assert!(is_audio(&file("song.flac", "")));
        assert!(is_book(&file("novel.epub", "")));
        assert!(is_archive(&file("bundle.tar.gz", "")));
        assert!(!is_image(&file("notes.md", "")));
    }

    #[test]
    fn only_ready_regular_files_are_classified() {
        let mut directory = file("photos", "");
        directory.kind = FileKind::Directory;
        assert!(!is_image(&directory));

        let mut pending = file("photo.png", "");
        pending.status = FileStatus::Pending;
        assert!(!is_image(&pending));
        assert!(!matches_library_kind(&pending, LibraryKind::File));
    }

    #[test]
    fn response_mime_infers_and_preserves() {
        assert_eq!(response_mime(&file("a.mp4", "")), "video/mp4");
        assert_eq!(
            response_mime(&file("a.epub", "application/epub+zip")),
            "application/epub+zip"
        );
        // oe/octet-stream is treated as "unknown" and re-inferred.
        assert_eq!(
            response_mime(&file("a.png", "application/octet-stream")),
            "image/png"
        );
    }

    #[test]
    fn safe_delivery_downgrades_active_formats_only() {
        assert_eq!(safe_delivery_mime("text/html"), "application/octet-stream");
        assert_eq!(
            safe_delivery_mime("text/html; charset=utf-8"),
            "application/octet-stream"
        );
        assert_eq!(
            safe_delivery_mime("image/svg+xml"),
            "application/octet-stream"
        );
        assert_eq!(safe_delivery_mime("video/mp4"), "video/mp4");
        assert_eq!(safe_delivery_mime("not a mime"), "application/octet-stream");
    }

    #[test]
    fn editable_documents_are_size_capped() {
        let small = File {
            size: 10,
            ..file("notes.md", "")
        };
        assert!(is_editable(&small));
        let large = File {
            size: crate::limits::MAX_DOCUMENT_BYTES as i64 + 1,
            ..file("notes.md", "")
        };
        assert!(!is_editable(&large));
        assert!(!is_editable(&file("photo.png", "")));
    }

    #[test]
    fn archive_suffixes_prefer_the_longest_match() {
        assert!(is_archive_name("a.tar.gz"));
        assert!(is_archive_name("a.TGZ"));
        assert!(is_archive_name("a.zip"));
        assert!(!is_archive_name("a.gz.txt"));
        assert!(!is_archive_name("a.txt"));
    }

    #[test]
    fn reader_titles_drop_trailing_format_suffixes() {
        assert_eq!(reader_display_title("三体.epub"), "三体");
        assert_eq!(reader_display_title("book.txt"), "book");
        assert_eq!(reader_display_title("book.epub.txt"), "book");
        assert_eq!(reader_display_title("plain"), "plain");
        // A name that is nothing but a suffix is left alone rather than emptied.
        assert_eq!(reader_display_title(".epub"), ".epub");
    }
}
