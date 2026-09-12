//! Stateful browser views.
//!
//! The client is being migrated feature by feature. Components own only view
//! state and event wiring; HTTP payloads stay in [`crate::api`] and reusable
//! formatting stays in [`crate::logic`].

pub mod icons;

mod dialogs;
mod file_browser;
mod login;
mod selection_toolbar;

pub use file_browser::FileBrowser;
pub use login::LoginView;
