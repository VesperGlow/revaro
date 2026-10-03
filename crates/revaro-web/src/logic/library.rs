//! Routes for the content shell, independent of browser APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibraryPage {
    #[default]
    Home,
    Books,
    Music,
    Gallery,
    Files,
}

impl LibraryPage {
    pub const ALL: [Self; 5] = [
        Self::Home,
        Self::Books,
        Self::Music,
        Self::Gallery,
        Self::Files,
    ];
    pub fn path(self) -> &'static str {
        match self {
            Self::Home => "/",
            Self::Books => "/library",
            Self::Music => "/music",
            Self::Gallery => "/gallery",
            Self::Files => "/files",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "首页",
            Self::Books => "阅读",
            Self::Music => "音乐",
            Self::Gallery => "图库",
            Self::Files => "文件",
        }
    }
    pub fn kind(self) -> &'static str {
        match self {
            Self::Books => "book",
            Self::Music => "audio",
            Self::Gallery => "image",
            _ => "",
        }
    }
    pub fn collection_label(self) -> &'static str {
        match self {
            Self::Books => "书架",
            Self::Music => "歌单",
            _ => "相册",
        }
    }
    pub fn from_path(path: &str) -> Self {
        match path.trim_end_matches('/') {
            "/library" => Self::Books,
            "/music" => Self::Music,
            "/gallery" => Self::Gallery,
            "/files" => Self::Files,
            p if p.starts_with("/f/") => Self::Files,
            p if p.starts_with("/read/") => Self::Books,
            _ => Self::Home,
        }
    }
}

/// Queue boundaries do not depend on the HTML media element.
pub fn next_index(index: usize, len: usize, direction: i32, repeat: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let next = index as i64 + i64::from(direction);
    if repeat {
        Some(next.rem_euclid(len as i64) as usize)
    } else {
        usize::try_from(next).ok().filter(|n| *n < len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restores_content_and_legacy_routes() {
        for (path, page) in [
            ("/", LibraryPage::Home),
            ("/library/", LibraryPage::Books),
            ("/read/book", LibraryPage::Books),
            ("/f/folder", LibraryPage::Files),
            ("/music", LibraryPage::Music),
            ("/gallery", LibraryPage::Gallery),
        ] {
            assert_eq!(LibraryPage::from_path(path), page);
        }
    }
    #[test]
    fn queue_stops_or_wraps_at_boundaries() {
        assert_eq!(next_index(2, 3, 1, false), None);
        assert_eq!(next_index(2, 3, 1, true), Some(0));
        assert_eq!(next_index(0, 3, -1, true), Some(2));
        assert_eq!(next_index(0, 0, 1, true), None);
    }
}
