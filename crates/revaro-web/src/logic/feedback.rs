//! User-facing transient notifications.
//!
//! The reference client gives every toast a severity and removes it after a
//! short fixed lifetime. Keeping the value separate from the DOM lets the
//! file browser, upload queue, task centre and account settings share exactly
//! the same notification sink.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackKind {
    Error,
    Success,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feedback {
    pub message: String,
    pub kind: FeedbackKind,
}

impl Feedback {
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: FeedbackKind::Error,
        }
    }

    #[must_use]
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: FeedbackKind::Success,
        }
    }
}
