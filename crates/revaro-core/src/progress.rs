//! Causal ordering of media progress, independent of device clocks.

use serde::{Deserialize, Serialize};

use crate::{ApiError, model::MediaProgress};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressWrite {
    pub writer: String,
    pub sequence: u64,
    pub base_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveMediaProgress {
    pub position: f64,
    pub duration: f64,
    pub completed: bool,
    pub sync: ProgressWrite,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listening: Option<ListeningWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListeningQueue {
    pub collection: Option<(String, String)>,
    pub tracks: Vec<String>,
    pub track: Option<String>,
    #[serde(default = "sequential")]
    pub mode: String,
}

fn sequential() -> String {
    "sequential".into()
}

impl Default for ListeningQueue {
    fn default() -> Self {
        Self {
            collection: None,
            tracks: Vec::new(),
            track: None,
            mode: sequential(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListeningSession {
    pub queue: ListeningQueue,
    pub revision: u64,
    pub writer: Option<String>,
    pub sequence: u64,
}

impl ListeningSession {
    pub fn ordering(&self) -> MediaProgress {
        MediaProgress {
            revision: self.revision,
            writer: self.writer.clone(),
            sequence: self.sequence,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListeningWrite {
    pub queue: ListeningQueue,
    pub sync: ProgressWrite,
}

pub fn validate_listening(write: &ListeningWrite, file_id: &str) -> Result<(), ApiError> {
    validate_write(&write.sync)?;
    let queue = &write.queue;
    if queue.tracks.len() > 1000
        || queue.tracks.iter().any(|id| {
            id.is_empty()
                || id.len() > 128
                || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
        || queue
            .track
            .as_ref()
            .is_some_and(|id| id != file_id || !queue.tracks.contains(id))
        || (queue.track.is_none() && !queue.tracks.is_empty())
        || !matches!(
            queue.mode.as_str(),
            "sequential" | "repeat-one" | "repeat-all" | "shuffle"
        )
        || queue
            .collection
            .as_ref()
            .is_some_and(|(id, name)| id.len() > 128 || name.len() > 1024)
    {
        return Err(ApiError::bad_request("listening queue is invalid"));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum WriteDecision {
    Accept,
    Duplicate,
    Conflict,
}

/// Same-session writes are monotonic; a new session must have observed the
/// current version. A delayed packet cannot reacquire a superseded session.
pub fn decide_write(current: &MediaProgress, write: &ProgressWrite) -> WriteDecision {
    if current.writer.as_deref() == Some(&write.writer) {
        if write.sequence > current.sequence {
            WriteDecision::Accept
        } else {
            WriteDecision::Duplicate
        }
    } else if write.base_revision == current.revision {
        WriteDecision::Accept
    } else {
        WriteDecision::Conflict
    }
}

pub fn validate_write(write: &ProgressWrite) -> Result<(), ApiError> {
    if write.writer.is_empty()
        || write.writer.len() > 128
        || !write
            .writer
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        || write.sequence == 0
        || write.sequence > i64::MAX as u64
        || write.base_revision > i64::MAX as u64
    {
        return Err(ApiError::bad_request("progress sync values are invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_superseded_session_cannot_reclaim_with_delayed_or_retried_writes() {
        let current = MediaProgress {
            revision: 12,
            writer: Some("device-b".into()),
            sequence: 2,
            ..Default::default()
        };
        for sequence in [1, 2, 99, i64::MAX as u64] {
            assert_eq!(
                decide_write(
                    &current,
                    &ProgressWrite {
                        writer: "device-a".into(),
                        sequence,
                        base_revision: 10,
                    }
                ),
                WriteDecision::Conflict
            );
        }
        assert_eq!(
            decide_write(
                &current,
                &ProgressWrite {
                    writer: "device-c".into(),
                    sequence: 1,
                    base_revision: 12,
                }
            ),
            WriteDecision::Accept
        );
    }

    #[test]
    fn retransmissions_and_reordered_packets_never_rewind_the_same_session() {
        let current = MediaProgress {
            revision: 12,
            writer: Some("a".into()),
            sequence: 9,
            ..Default::default()
        };
        for sequence in 1..=9 {
            assert_eq!(
                decide_write(
                    &current,
                    &ProgressWrite {
                        writer: "a".into(),
                        sequence,
                        base_revision: 0,
                    }
                ),
                WriteDecision::Duplicate
            );
        }
        assert_eq!(
            decide_write(
                &current,
                &ProgressWrite {
                    writer: "a".into(),
                    sequence: 10,
                    base_revision: 0,
                }
            ),
            WriteDecision::Accept
        );
    }
}
