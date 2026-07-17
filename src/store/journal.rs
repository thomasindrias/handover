use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::error::{Error, Result, io};
use crate::model::{Event, EventEnvelope, EventKind, Provider, RunId, SessionId};

#[derive(Clone, Debug)]
pub struct PendingEvent {
    pub occurred_at: String,
    pub recorded_at: String,
    pub run_id: Option<RunId>,
    pub provider: Option<Provider>,
    pub idempotency_key: Option<String>,
    pub kind: EventKind,
}

#[derive(Clone, Debug)]
pub struct PendingEventMeta {
    pub occurred_at: String,
    pub recorded_at: String,
    pub run_id: Option<RunId>,
    pub provider: Option<Provider>,
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppendOutcome {
    Appended(Event),
    Existing(Event),
}

#[derive(Clone, Debug)]
pub struct EventJournal {
    session_id: SessionId,
    path: PathBuf,
    lock_path: PathBuf,
}

impl EventJournal {
    pub fn new(session_dir: &Path, session_id: SessionId) -> Self {
        Self {
            session_id,
            path: session_dir.join("events.jsonl"),
            lock_path: session_dir.join("lock"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, pending: PendingEvent) -> Result<Event> {
        let PendingEvent {
            occurred_at,
            recorded_at,
            run_id,
            provider,
            idempotency_key,
            kind,
        } = pending;
        if idempotency_key.is_some() {
            return Err(Error::InvalidState(
                "idempotent events must use append_idempotent".into(),
            ));
        }
        self.append_with(
            PendingEventMeta {
                occurred_at,
                recorded_at,
                run_id,
                provider,
                idempotency_key: None,
            },
            move |_, _| Ok(kind),
        )
    }

    pub fn append_with(
        &self,
        pending: PendingEventMeta,
        build_kind: impl FnOnce(u64, &[EventEnvelope]) -> Result<EventKind>,
    ) -> Result<Event> {
        self.append_optional(pending, |sequence, events| {
            build_kind(sequence, events).map(Some)
        })?
        .ok_or_else(|| Error::InvalidState("event builder did not produce an event".into()))
    }

    pub fn append_optional(
        &self,
        pending: PendingEventMeta,
        build_kind: impl FnOnce(u64, &[EventEnvelope]) -> Result<Option<EventKind>>,
    ) -> Result<Option<Event>> {
        self.with_lock(|file| {
            let events = repair_and_read(file, &self.path, &self.session_id)?;
            let sequence = events
                .last()
                .map_or(Some(1), |item| item.event.sequence.checked_add(1))
                .ok_or_else(|| Error::InvalidState("event sequence overflow".into()))?;
            let Some(kind) = build_kind(sequence, &events)? else {
                return Ok(None);
            };
            let event = Event {
                schema_version: 1,
                sequence,
                occurred_at: pending.occurred_at,
                recorded_at: pending.recorded_at,
                session_id: self.session_id.clone(),
                run_id: pending.run_id,
                provider: pending.provider,
                idempotency_key: pending.idempotency_key,
                kind,
            };
            write_event(file, &self.path, &event)?;
            Ok(Some(event))
        })
    }

    pub fn append_idempotent(&self, pending: PendingEvent) -> Result<AppendOutcome> {
        let key = pending.idempotency_key.clone().ok_or_else(|| {
            Error::InvalidState("idempotent append requires an idempotency key".into())
        })?;
        self.with_lock(|file| {
            let events = repair_and_read(file, &self.path, &self.session_id)?;
            if let Some(existing) = events
                .iter()
                .find(|item| item.event.idempotency_key.as_deref() == Some(&key))
            {
                if existing.event.run_id == pending.run_id
                    && existing.event.provider == pending.provider
                    && existing.event.kind == pending.kind
                {
                    return Ok(AppendOutcome::Existing(existing.event.clone()));
                }
                return Err(Error::InvalidState(format!(
                    "idempotency key {key:?} conflicts with an existing event"
                )));
            }
            let sequence = events
                .last()
                .map_or(Some(1), |item| item.event.sequence.checked_add(1))
                .ok_or_else(|| Error::InvalidState("event sequence overflow".into()))?;
            let event = Event {
                schema_version: 1,
                sequence,
                occurred_at: pending.occurred_at,
                recorded_at: pending.recorded_at,
                session_id: self.session_id.clone(),
                run_id: pending.run_id,
                provider: pending.provider,
                idempotency_key: Some(key),
                kind: pending.kind,
            };
            write_event(file, &self.path, &event)?;
            Ok(AppendOutcome::Appended(event))
        })
    }

    pub fn read_repair(&self) -> Result<Vec<EventEnvelope>> {
        self.with_lock(|file| repair_and_read(file, &self.path, &self.session_id))
    }

    fn with_lock<T>(&self, operation: impl FnOnce(&mut std::fs::File) -> Result<T>) -> Result<T> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Error::InvalidState(format!("{} has no parent", self.path.display())))?;
        super::ensure_private_dir(parent)?;

        let lock = open_private_rw(&self.lock_path, parent)?;
        lock.lock_exclusive()
            .map_err(|source| io(&self.lock_path, source))?;
        let mut file = open_private_rw(&self.path, parent)?;
        let result = operation(&mut file);
        let unlock = FileExt::unlock(&lock).map_err(|source| io(&self.lock_path, source));
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }
}

fn write_event(file: &mut std::fs::File, path: &Path, event: &Event) -> Result<()> {
    let envelope = EventEnvelope::seal(event.clone())?;
    file.seek(SeekFrom::End(0))
        .map_err(|source| io(path, source))?;
    file.write_all(&envelope.line()?)
        .map_err(|source| io(path, source))?;
    file.sync_data().map_err(|source| io(path, source))
}

fn open_private_rw(path: &Path, parent: &Path) -> Result<std::fs::File> {
    let create = || {
        std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    };
    let existing = || {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    };

    let (file, created) = match create() {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            (existing().map_err(|source| io(path, source))?, false)
        }
        Err(source) => return Err(io(path, source)),
    };
    if created {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| io(path, source))?;
    }
    validate_private_file(&file, path)?;
    if created {
        file.sync_all().map_err(|source| io(path, source))?;
        sync_directory(parent)?;
    }
    Ok(file)
}

fn validate_private_file(file: &std::fs::File, path: &Path) -> Result<()> {
    let metadata = file.metadata().map_err(|source| io(path, source))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(Error::InvalidState(format!(
            "refusing insecure journal file {}",
            path.display(),
        )));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    let directory = std::fs::File::open(path).map_err(|source| io(path, source))?;
    directory.sync_all().map_err(|source| io(path, source))
}

fn repair_and_read(
    file: &mut std::fs::File,
    path: &Path,
    expected_session_id: &SessionId,
) -> Result<Vec<EventEnvelope>> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io(path, source))?;
    let mut events = Vec::new();
    let mut committed_end = 0usize;
    let lines: Vec<&[u8]> = bytes.split_inclusive(|byte| *byte == b'\n').collect();

    for (index, line) in lines.iter().enumerate() {
        let complete = line.ends_with(b"\n");
        if !complete {
            if index + 1 != lines.len() {
                return Err(Error::InvalidState(
                    "incomplete event before journal tail".into(),
                ));
            }
            file.set_len(committed_end as u64)
                .map_err(|source| io(path, source))?;
            file.sync_data().map_err(|source| io(path, source))?;
            break;
        }

        let payload = line.strip_suffix(b"\n").expect("complete line has newline");
        let envelope: EventEnvelope = serde_json::from_slice(payload)
            .map_err(|error| Error::InvalidState(format!("invalid event JSON: {error}")))?;
        envelope.verify()?;
        if envelope.line()?.as_slice() != *line {
            return Err(Error::InvalidState(
                "event line is not canonical JSON".into(),
            ));
        }
        let expected_sequence = u64::try_from(events.len())
            .ok()
            .and_then(|length| length.checked_add(1))
            .ok_or_else(|| Error::InvalidState("event sequence overflow".into()))?;
        if envelope.event.schema_version != 1
            || envelope.event.sequence != expected_sequence
            || &envelope.event.session_id != expected_session_id
        {
            return Err(Error::InvalidState(format!(
                "journal expected session {expected_session_id} schema 1 sequence {expected_sequence}, found session {} schema {} sequence {}",
                envelope.event.session_id, envelope.event.schema_version, envelope.event.sequence,
            )));
        }
        committed_end += line.len();
        events.push(envelope);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::{Arc, Barrier};

    use tempfile::TempDir;

    use super::{EventJournal, PendingEvent, PendingEventMeta};
    use crate::error::Error;
    use crate::model::{ContentRef, EventKind, Provider, SessionId};

    fn private_temp() -> TempDir {
        let temp = TempDir::new().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        temp
    }

    fn pending(prompt: &str) -> PendingEvent {
        PendingEvent {
            occurred_at: "2026-07-16T10:00:00Z".into(),
            recorded_at: "2026-07-16T10:00:01Z".into(),
            run_id: None,
            provider: Some(Provider::Claude),
            idempotency_key: None,
            kind: EventKind::ProviderPromptSubmitted {
                prompt: ContentRef::Inline {
                    text: prompt.into(),
                },
            },
        }
    }

    #[test]
    fn appends_monotonic_verified_events() {
        let temp = private_temp();
        let journal = EventJournal::new(
            temp.path(),
            SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        );

        assert_eq!(journal.append(pending("one")).unwrap().sequence, 1);
        assert_eq!(journal.append(pending("two")).unwrap().sequence, 2);
        assert_eq!(journal.read_repair().unwrap().len(), 2);
        for path in [journal.path().to_path_buf(), temp.path().join("lock")] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn append_with_allocates_under_lock_and_does_not_append_on_builder_failure() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        let meta = PendingEventMeta {
            occurred_at: "2026-07-16T10:00:02Z".into(),
            recorded_at: "2026-07-16T10:00:02Z".into(),
            run_id: None,
            provider: Some(Provider::Claude),
            idempotency_key: None,
        };

        let event = journal
            .append_with(meta.clone(), |sequence, committed| {
                assert_eq!(sequence, 2);
                assert_eq!(committed.len(), 1);
                Ok(EventKind::ProviderPromptSubmitted {
                    prompt: ContentRef::Inline { text: "two".into() },
                })
            })
            .unwrap();
        assert_eq!(event.sequence, 2);

        let error = journal.append_with(meta, |_, _| {
            Err(Error::InvalidState("do not append".into()))
        });
        assert!(error.is_err());
        assert_eq!(journal.read_repair().unwrap().len(), 2);
    }

    #[test]
    fn idempotent_append_reuses_exact_events_and_rejects_conflicts() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        let mut event = pending("one");
        event.idempotency_key = Some("prompt:stable".into());

        let first = journal.append_idempotent(event.clone()).unwrap();
        let second = journal.append_idempotent(event.clone()).unwrap();

        assert!(matches!(first, super::AppendOutcome::Appended(_)));
        assert!(matches!(second, super::AppendOutcome::Existing(_)));
        assert_eq!(journal.read_repair().unwrap().len(), 1);

        event.kind = EventKind::ProviderPromptSubmitted {
            prompt: ContentRef::Inline {
                text: "different".into(),
            },
        };
        assert!(journal.append_idempotent(event).is_err());
        assert_eq!(journal.read_repair().unwrap().len(), 1);
    }

    #[test]
    fn optional_append_suppresses_concurrent_duplicate_facts() {
        let temp = private_temp();
        let journal = Arc::new(EventJournal::new(temp.path(), SessionId::new()));
        let barrier = Arc::new(Barrier::new(8));
        let meta = PendingEventMeta {
            occurred_at: "2026-07-16T10:00:00Z".into(),
            recorded_at: "2026-07-16T10:00:00Z".into(),
            run_id: None,
            provider: Some(Provider::Claude),
            idempotency_key: None,
        };
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let journal = Arc::clone(&journal);
                let barrier = Arc::clone(&barrier);
                let meta = meta.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    journal.append_optional(meta, |_, committed| {
                        let exists = committed.iter().any(|item| {
                            matches!(
                                &item.event.kind,
                                EventKind::CwdChanged { cwd_relative }
                                    if cwd_relative == std::path::Path::new("apps/web")
                            )
                        });
                        Ok((!exists).then_some(EventKind::CwdChanged {
                            cwd_relative: std::path::PathBuf::from("apps/web"),
                        }))
                    })
                })
            })
            .collect();

        let appended = threads
            .into_iter()
            .map(|thread| thread.join().unwrap().unwrap().is_some())
            .filter(|appended| *appended)
            .count();
        assert_eq!(appended, 1);
        assert_eq!(journal.read_repair().unwrap().len(), 1);
    }

    #[test]
    fn removes_only_an_incomplete_final_line() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.path())
            .unwrap();
        file.write_all(b"{partial").unwrap();
        drop(file);

        let events = journal.read_repair().unwrap();

        assert_eq!(events.len(), 1);
        assert!(std::fs::read(journal.path()).unwrap().ends_with(b"\n"));
    }

    #[test]
    fn a_valid_json_tail_without_a_newline_is_still_uncommitted() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        let mut bytes = std::fs::read(journal.path()).unwrap();
        assert_eq!(bytes.pop(), Some(b'\n'));
        std::fs::write(journal.path(), bytes).unwrap();

        assert!(journal.read_repair().unwrap().is_empty());
        assert!(std::fs::read(journal.path()).unwrap().is_empty());
    }

    #[test]
    fn refuses_corruption_before_the_tail() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        journal.append(pending("two")).unwrap();
        let bytes = std::fs::read(journal.path()).unwrap();
        let second = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let mut corrupt = b"{}\n".to_vec();
        corrupt.extend_from_slice(&bytes[second..]);
        std::fs::write(journal.path(), corrupt).unwrap();

        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn refuses_a_complete_invalid_tail_line() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.path())
            .unwrap();
        file.write_all(b"{}\n").unwrap();

        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn refuses_a_checksum_valid_line_with_unknown_or_noncanonical_fields() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        let bytes = std::fs::read(journal.path()).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["event"]["unexpected"] = serde_json::json!(true);
        let mut replacement = serde_json::to_vec(&value).unwrap();
        replacement.push(b'\n');
        std::fs::write(journal.path(), replacement).unwrap();

        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn refuses_a_validly_sealed_non_monotonic_sequence() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        let bytes = std::fs::read(journal.path()).unwrap();
        let mut envelope: crate::model::EventEnvelope = serde_json::from_slice(&bytes).unwrap();
        envelope.event.sequence = 9;
        let replacement = crate::model::EventEnvelope::seal(envelope.event)
            .unwrap()
            .line()
            .unwrap();
        std::fs::write(journal.path(), replacement).unwrap();

        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn refuses_a_symlinked_journal() {
        let temp = private_temp();
        let outside = temp.path().join("outside.jsonl");
        std::fs::write(&outside, b"").unwrap();
        let session = temp.path().join("session");
        std::fs::create_dir(&session).unwrap();
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&outside, session.join("events.jsonl")).unwrap();
        let journal = EventJournal::new(&session, SessionId::new());

        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn refuses_a_symlinked_lock() {
        let temp = private_temp();
        let outside = temp.path().join("outside.lock");
        std::fs::write(&outside, b"").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&outside, temp.path().join("lock")).unwrap();
        let journal = EventJournal::new(temp.path(), SessionId::new());

        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn refuses_insecure_journal_and_lock_permissions() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();

        std::fs::set_permissions(journal.path(), std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(journal.read_repair().is_err());
        std::fs::set_permissions(journal.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

        std::fs::set_permissions(
            temp.path().join("lock"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn concurrent_appends_are_serialized_without_gaps() {
        let temp = private_temp();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        let barrier = Arc::new(Barrier::new(8));
        let mut threads = Vec::new();

        for index in 0..8 {
            let journal = journal.clone();
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                journal.append(pending(&format!("prompt-{index}"))).unwrap()
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }

        let events = journal.read_repair().unwrap();
        assert_eq!(events.len(), 8);
        assert_eq!(
            events
                .iter()
                .map(|event| event.event.sequence)
                .collect::<Vec<_>>(),
            (1..=8).collect::<Vec<_>>()
        );
    }
}
