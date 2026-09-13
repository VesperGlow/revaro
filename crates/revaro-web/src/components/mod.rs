//! Stateful browser views.
//!
//! The client is being migrated feature by feature. Components own only view
//! state and event wiring; HTTP payloads stay in [`crate::api`] and reusable
//! formatting stays in [`crate::logic`].

pub mod icons;

mod account;
mod audio;
mod dialogs;
mod directory_picker;
mod editor;
mod file_browser;
mod file_browser_header;
mod library;
mod login;
mod media;
mod reader;
mod reader_cache;
mod selection_toolbar;
mod share;
mod sidebar;
mod system_status;
mod tasks;
mod topbar;
mod transfer;
mod uploads;
mod video;

pub use file_browser::FileBrowser;
pub use login::LoginView;
