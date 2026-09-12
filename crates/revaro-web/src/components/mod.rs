//! Stateful browser views.
//!
//! The client is being migrated feature by feature. Components own only view
//! state and event wiring; HTTP payloads stay in [`crate::api`] and reusable
//! formatting stays in [`crate::logic`].

pub mod icons;

mod audio;
mod dialogs;
mod directory_picker;
mod file_browser;
mod login;
mod media;
mod selection_toolbar;
mod tasks;
mod transfer;
mod uploads;
mod video;

pub use file_browser::FileBrowser;
pub use login::LoginView;
