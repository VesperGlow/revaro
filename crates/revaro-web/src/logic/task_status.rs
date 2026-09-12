//! Task status semantics shared by the top bar and the task centre.
//!
//! Rust home of `web/src/taskStatus.ts`, extended with the label/tone mapping
//! that the Vue code kept inline in `AppTopbar.vue`, `TaskCenter.vue` and
//! `StatusBadge.vue`. `AppTopbar` counts "active" tasks for its badge and the
//! task centre groups by the same predicate, so the two must never drift; that
//! is why the predicate lives here as the single definition.
//!
//! The status type itself is shared: [`TaskStatus`] comes from `revaro-core`,
//! the same enum the server serializes, so a new status cannot appear on the
//! wire without the client knowing about it.

use revaro_core::model::{TaskStatus, task_type};

/// True for statuses that still expect work to happen.
///
/// Exactly `queued | running | waiting_input | retrying`, matching
/// `web/src/taskStatus.test.ts`. Terminal states deliberately excluded:
/// `completed`, `failed`, `cancelled`.
#[must_use]
pub fn is_active_task_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::WaitingInput | TaskStatus::Retrying
    )
}

/// The visual tone a status maps to, mirroring `StatusBadge`'s tone set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTone {
    /// No activity and no problem (cancelled).
    Neutral,
    /// Work in progress.
    Info,
    /// Finished successfully.
    Success,
    /// Needs attention but is not broken (queued work that is retrying).
    Warning,
    /// Finished unsuccessfully.
    Danger,
}

impl TaskTone {
    /// The class suffix `StatusBadge` renders, e.g. `tone-danger`.
    #[must_use]
    pub const fn as_class(self) -> &'static str {
        match self {
            TaskTone::Neutral => "tone-neutral",
            TaskTone::Info => "tone-info",
            TaskTone::Success => "tone-success",
            TaskTone::Warning => "tone-warning",
            TaskTone::Danger => "tone-danger",
        }
    }
}

/// Map a status to the badge tone the UI shows for it.
#[must_use]
pub const fn task_status_tone(status: TaskStatus) -> TaskTone {
    match status {
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::WaitingInput => TaskTone::Info,
        // A retry is a soft failure: it is still moving, so it warns rather than
        // reporting an error.
        TaskStatus::Retrying => TaskTone::Warning,
        TaskStatus::Completed => TaskTone::Success,
        TaskStatus::Failed => TaskTone::Danger,
        TaskStatus::Cancelled => TaskTone::Neutral,
    }
}

/// The Chinese label for a task type, falling back to the raw type string.
///
/// `TaskCenter`'s `labels` map covered upload/archive/subtitle; unknown types
/// (a newer server) are displayed verbatim rather than hidden.
#[must_use]
pub fn task_type_label(value: &str) -> String {
    match value {
        task_type::UPLOAD => "上传".to_owned(),
        task_type::ARCHIVE_EXTRACT => "解压".to_owned(),
        task_type::SUBTITLE => "字幕处理".to_owned(),
        other => other.to_owned(),
    }
}

/// The Chinese status line for a task row.
///
/// Faithful port of `TaskCenter.vue`'s `status(task)` helper: `running` shows
/// the task's own `phase` (which the server localises), a failure shows the
/// server error when there is one, and an empty error falls back to `失败`.
#[must_use]
pub fn task_status_label(status: TaskStatus, kind: &str, phase: &str, error: &str) -> String {
    match status {
        TaskStatus::WaitingInput => {
            if kind == task_type::ARCHIVE_EXTRACT {
                "等待输入密码".to_owned()
            } else {
                "等待输入".to_owned()
            }
        }
        TaskStatus::Retrying => "等待重试".to_owned(),
        TaskStatus::Queued => "排队中".to_owned(),
        TaskStatus::Running => phase.to_owned(),
        TaskStatus::Completed => "已完成".to_owned(),
        TaskStatus::Cancelled => "已取消".to_owned(),
        TaskStatus::Failed => {
            if error.is_empty() {
                "失败".to_owned()
            } else {
                error.to_owned()
            }
        }
    }
}

/// Convert a server progress value into a safe percentage for CSS and text.
///
/// The server normally emits a finite value in the inclusive `0..=100` range,
/// but a browser must still render a malformed or partially migrated response
/// without producing invalid CSS. Non-finite values are treated as unknown
/// progress and numeric values are rounded after clamping.
#[must_use]
pub fn task_progress_percent(progress: f64) -> u8 {
    if progress.is_finite() {
        progress.clamp(0.0, 100.0).round() as u8
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_task_center_and_topbar_activity_semantics_aligned() {
        // Ported from `web/src/taskStatus.test.ts`: filtering all seven statuses
        // must leave exactly the four active ones, in declaration order.
        let active: Vec<&str> = TaskStatus::ALL
            .iter()
            .filter(|status| is_active_task_status(**status))
            .map(|status| status.as_str())
            .collect();
        assert_eq!(active, ["queued", "running", "waiting_input", "retrying"]);
    }

    #[test]
    fn terminal_statuses_are_never_active() {
        for status in TaskStatus::ALL.iter().filter(|status| status.is_terminal()) {
            assert!(!is_active_task_status(*status), "{status} is terminal");
        }
    }

    #[test]
    fn maps_statuses_to_badge_tone_classes() {
        assert_eq!(task_status_tone(TaskStatus::Queued).as_class(), "tone-info");
        assert_eq!(
            task_status_tone(TaskStatus::Running).as_class(),
            "tone-info"
        );
        assert_eq!(
            task_status_tone(TaskStatus::WaitingInput).as_class(),
            "tone-info"
        );
        assert_eq!(
            task_status_tone(TaskStatus::Retrying).as_class(),
            "tone-warning"
        );
        assert_eq!(
            task_status_tone(TaskStatus::Completed).as_class(),
            "tone-success"
        );
        assert_eq!(
            task_status_tone(TaskStatus::Failed).as_class(),
            "tone-danger"
        );
        assert_eq!(
            task_status_tone(TaskStatus::Cancelled).as_class(),
            "tone-neutral"
        );
    }

    #[test]
    fn labels_known_task_types_and_passes_unknown_ones_through() {
        assert_eq!(task_type_label("upload"), "上传");
        assert_eq!(task_type_label("archive_extract"), "解压");
        assert_eq!(task_type_label("subtitle"), "字幕处理");
        assert_eq!(task_type_label("future_type"), "future_type");
    }

    #[test]
    fn renders_status_lines_like_the_task_center() {
        assert_eq!(
            task_status_label(TaskStatus::WaitingInput, "archive_extract", "", ""),
            "等待输入密码"
        );
        assert_eq!(
            task_status_label(TaskStatus::WaitingInput, "upload", "", ""),
            "等待输入"
        );
        assert_eq!(
            task_status_label(TaskStatus::Retrying, "upload", "", ""),
            "等待重试"
        );
        assert_eq!(
            task_status_label(TaskStatus::Queued, "upload", "", ""),
            "排队中"
        );
        assert_eq!(
            task_status_label(TaskStatus::Running, "upload", "传输中", ""),
            "传输中"
        );
        assert_eq!(
            task_status_label(TaskStatus::Completed, "upload", "", ""),
            "已完成"
        );
        assert_eq!(
            task_status_label(TaskStatus::Cancelled, "upload", "", ""),
            "已取消"
        );
        assert_eq!(
            task_status_label(TaskStatus::Failed, "upload", "", "磁盘已满"),
            "磁盘已满"
        );
        assert_eq!(
            task_status_label(TaskStatus::Failed, "upload", "", ""),
            "失败"
        );
    }

    #[test]
    fn sanitizes_progress_before_rendering() {
        assert_eq!(task_progress_percent(f64::NAN), 0);
        assert_eq!(task_progress_percent(f64::NEG_INFINITY), 0);
        assert_eq!(task_progress_percent(f64::INFINITY), 0);
        assert_eq!(task_progress_percent(-10.0), 0);
        assert_eq!(task_progress_percent(12.5), 13);
        assert_eq!(task_progress_percent(150.0), 100);
    }
}
