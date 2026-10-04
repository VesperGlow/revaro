//! Stateful browser views.
//!
//! The client is being migrated feature by feature. Components own only view
//! state and event wiring; HTTP payloads stay in [`crate::api`] and reusable
//! formatting stays in [`crate::logic`].

pub mod icons;

mod account;
mod audio;
mod content_shell;
mod dialogs;
mod directory_picker;
mod editor;
mod file_browser;
mod file_browser_header;
mod login;
mod management;
mod media;
mod menu;
mod music_player;
mod playback;
mod reader;
pub(crate) mod reader_cache;
mod resource_url;
mod selection;
mod selection_toolbar;
mod share;
mod topbar;
mod transfer;
mod uploads;
mod version_history;
mod video;

pub use content_shell::ContentShell;
pub use file_browser::FileBrowser;
pub use login::LoginView;
