use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::{CheckpointKind, GitSnapshot, Provider, RunId, SessionId, WorktreeIdentity};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "storage", rename_all = "snake_case")]
pub enum ContentRef {
    Inline { text: String },
    Blob { sha256: String, bytes: usize },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", deny_unknown_fields)]
pub enum EventKind {
    #[serde(rename = "session.created")]
    SessionCreated { worktree: WorktreeIdentity },
    #[serde(rename = "switch.requested")]
    SwitchRequested {
        from: Option<Provider>,
        to: Provider,
    },
    #[serde(rename = "run.started")]
    RunStarted {
        cwd: String,
        args: Vec<String>,
        supervisor_pid: u32,
    },
    #[serde(rename = "run.handshake")]
    RunHandshake {
        native_session_id: String,
        provider_version: Option<String>,
    },
    #[serde(rename = "run.stopped")]
    RunStopped {
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    #[serde(rename = "run.recovered")]
    RunRecovered { reason: String },
    #[serde(rename = "cwd.changed")]
    CwdChanged { cwd_relative: std::path::PathBuf },
    #[serde(rename = "provider.prompt.submitted")]
    ProviderPromptSubmitted { prompt: ContentRef },
    #[serde(rename = "provider.tool.requested")]
    ProviderToolRequested {
        tool_name: String,
        tool_use_id: String,
        command: Option<String>,
        file_path: Option<String>,
    },
    #[serde(rename = "provider.tool.completed")]
    ProviderToolCompleted {
        tool_name: String,
        tool_use_id: String,
        response: Option<ContentRef>,
        stdout: Option<ContentRef>,
        stderr: Option<ContentRef>,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
    #[serde(rename = "provider.tool.failed")]
    ProviderToolFailed {
        tool_name: String,
        tool_use_id: String,
        error: String,
    },
    #[serde(rename = "provider.stop.observed")]
    ProviderStopObserved { native_session_id: String },
    #[serde(rename = "git.snapshot")]
    GitSnapshot { snapshot: GitSnapshot },
    #[serde(rename = "checkpoint.created")]
    CheckpointCreated {
        checkpoint_kind: CheckpointKind,
        through_sequence: u64,
        path: String,
    },
    #[serde(rename = "capture.failed")]
    CaptureFailed { phase: String, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub sequence: u64,
    pub occurred_at: String,
    pub recorded_at: String,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub provider: Option<Provider>,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub checksum: String,
    pub event: Event,
}

impl EventEnvelope {
    pub fn seal(event: Event) -> Result<Self> {
        let bytes = serde_json::to_vec(&event)
            .map_err(|error| Error::InvalidState(format!("cannot encode event: {error}")))?;
        let checksum = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        Ok(Self { checksum, event })
    }

    pub fn verify(&self) -> Result<()> {
        let expected = Self::seal(self.event.clone())?.checksum;
        if self.checksum == expected {
            Ok(())
        } else {
            Err(Error::InvalidState(format!(
                "event {} checksum mismatch",
                self.event.sequence
            )))
        }
    }

    pub fn line(&self) -> Result<Vec<u8>> {
        let mut line = serde_json::to_vec(self)
            .map_err(|error| Error::InvalidState(format!("cannot encode envelope: {error}")))?;
        line.push(b'\n');
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentRef, Event, EventEnvelope, EventKind};
    use crate::model::{Provider, RunId, SessionId};

    fn event() -> Event {
        Event {
            schema_version: 1,
            sequence: 7,
            occurred_at: "2026-07-16T10:00:00Z".into(),
            recorded_at: "2026-07-16T10:00:01Z".into(),
            session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            run_id: Some(RunId::parse("22222222-2222-4222-8222-222222222222").unwrap()),
            provider: Some(Provider::Claude),
            kind: EventKind::ProviderPromptSubmitted {
                prompt: ContentRef::Inline {
                    text: "fix oauth".into(),
                },
            },
        }
    }

    #[test]
    fn sealed_event_verifies() {
        let envelope = EventEnvelope::seal(event()).unwrap();
        envelope.verify().unwrap();
    }

    #[test]
    fn mutation_breaks_the_checksum() {
        let mut envelope = EventEnvelope::seal(event()).unwrap();
        envelope.event.sequence = 8;
        assert!(envelope.verify().is_err());
    }

    #[test]
    fn encoding_is_stable() {
        let left = EventEnvelope::seal(event()).unwrap().line().unwrap();
        let right = EventEnvelope::seal(event()).unwrap().line().unwrap();
        assert_eq!(left, right);
        assert!(left.ends_with(b"\n"));
    }

    #[test]
    fn event_json_is_provider_neutral_and_explicitly_versioned() {
        let value = serde_json::to_value(event()).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "schema_version": 1,
                "sequence": 7,
                "occurred_at": "2026-07-16T10:00:00Z",
                "recorded_at": "2026-07-16T10:00:01Z",
                "session_id": "11111111-1111-4111-8111-111111111111",
                "run_id": "22222222-2222-4222-8222-222222222222",
                "provider": "claude",
                "type": "provider.prompt.submitted",
                "payload": {
                    "prompt": {
                        "storage": "inline",
                        "text": "fix oauth"
                    }
                }
            })
        );
    }

    #[test]
    fn envelope_and_payload_reject_unknown_fields() {
        let envelope = EventEnvelope::seal(event()).unwrap();
        let mut outer = serde_json::to_value(&envelope).unwrap();
        outer["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<EventEnvelope>(outer).is_err());

        let mut payload = serde_json::to_value(envelope).unwrap();
        payload["event"]["payload"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<EventEnvelope>(payload).is_err());
    }
}
