use std::path::PathBuf;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::checkpoint::{CheckpointService, StoredCheckpoint};
use crate::error::{Error, Result, io};
use crate::model::{
    CheckpointAuthor, CheckpointKind, Event, EventEnvelope, EventKind, GitSnapshot, NarrativeInput,
    Provider, RunId, SessionId, SessionMeta, WorktreeIdentity, WorktreeRef,
};
use crate::runtime::Runtime;
use crate::store::StateLayout;
use crate::store::journal::{EventJournal, PendingEvent, PendingEventMeta};
use crate::store::refs::{read_json, write_json_create};

#[derive(Clone, Debug)]
pub struct SessionStore {
    layout: StateLayout,
    meta: SessionMeta,
}

impl SessionStore {
    pub fn create(
        layout: &StateLayout,
        runtime: &dyn Runtime,
        snapshot: GitSnapshot,
    ) -> Result<Self> {
        snapshot.identity.validate()?;
        layout.ensure()?;
        let layout = layout.canonicalized()?;
        let reference_path = layout
            .worktree_refs()
            .join(format!("{}.json", snapshot.identity.key));
        match std::fs::symlink_metadata(&reference_path) {
            Ok(_) => {
                let existing: WorktreeRef = read_json(&reference_path)?;
                validate_worktree_ref(&existing)?;
                if existing.identity.same_worktree_as(&snapshot.identity) {
                    return Err(already_bound(&existing));
                }
                return Err(Error::InvalidState(
                    "worktree ref key collision or identity mismatch".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io(&reference_path, source)),
        }

        let meta = SessionMeta {
            schema_version: 1,
            id: runtime.session_id(),
            created_at: runtime.now()?,
            worktree: snapshot.identity.clone(),
            parent_session_id: None,
            parent_checkpoint_sequence: None,
        };
        validate_session_meta(&meta, &meta.id)?;
        let store = Self { layout, meta };
        store.ensure_directories()?;
        write_json_create(&store.session_dir().join("meta.json"), &store.meta)?;
        store.append(
            runtime,
            None,
            None,
            EventKind::SessionCreated {
                worktree: snapshot.identity.clone(),
            },
        )?;
        store.append(runtime, None, None, EventKind::GitSnapshot { snapshot })?;
        let reference = WorktreeRef {
            schema_version: 1,
            key: store.meta.worktree.key.clone(),
            session_id: store.meta.id.clone(),
            identity: store.meta.worktree.clone(),
        };
        if let Err(error) = write_json_create(&reference_path, &reference) {
            if let Ok(existing) = read_json::<WorktreeRef>(&reference_path) {
                if validate_worktree_ref(&existing).is_ok()
                    && existing.identity.same_worktree_as(&store.meta.worktree)
                {
                    return Err(already_bound(&existing));
                }
            }
            return Err(error);
        }
        Ok(store)
    }

    pub fn open(layout: &StateLayout, id: SessionId) -> Result<Self> {
        let layout = layout.canonicalized()?;
        let meta: SessionMeta =
            read_json(&layout.sessions().join(id.to_string()).join("meta.json"))?;
        validate_session_meta(&meta, &id)?;
        Ok(Self { layout, meta })
    }

    pub fn find_for_worktree(
        layout: &StateLayout,
        identity: &WorktreeIdentity,
    ) -> Result<Option<Self>> {
        identity.validate()?;
        let path = layout
            .worktree_refs()
            .join(format!("{}.json", identity.key));
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io(&path, source)),
            Ok(_) => {}
        }
        let reference: WorktreeRef = read_json(&path)?;
        validate_worktree_ref(&reference)?;
        if !reference.identity.same_worktree_as(identity) {
            return Err(Error::InvalidState("worktree ref identity mismatch".into()));
        }
        let store = Self::open(layout, reference.session_id.clone())?;
        if store.meta.id != reference.session_id || store.meta.worktree != reference.identity {
            return Err(Error::InvalidState(
                "worktree ref does not match immutable session metadata".into(),
            ));
        }
        Ok(Some(store))
    }

    pub fn id(&self) -> &SessionId {
        &self.meta.id
    }

    pub fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    pub fn session_dir(&self) -> PathBuf {
        self.layout.sessions().join(self.meta.id.to_string())
    }

    pub fn layout(&self) -> &StateLayout {
        &self.layout
    }

    pub fn append(
        &self,
        runtime: &dyn Runtime,
        run_id: Option<RunId>,
        provider: Option<Provider>,
        kind: EventKind,
    ) -> Result<Event> {
        let now = runtime.now()?;
        EventJournal::new(&self.session_dir(), self.meta.id.clone()).append(PendingEvent {
            occurred_at: now.clone(),
            recorded_at: now,
            run_id,
            provider,
            idempotency_key: None,
            kind,
        })
    }

    pub fn events(&self) -> Result<Vec<Event>> {
        self.envelopes()
            .map(|items| items.into_iter().map(|item| item.event).collect())
    }

    pub fn envelopes(&self) -> Result<Vec<EventEnvelope>> {
        EventJournal::new(&self.session_dir(), self.meta.id.clone()).read_repair()
    }

    pub fn saved_cwd_relative(&self) -> Result<PathBuf> {
        let mut cwd = self.meta.worktree.cwd_relative.clone();
        for event in self.events()? {
            if let EventKind::CwdChanged { cwd_relative } = event.kind {
                cwd = cwd_relative;
            }
        }
        Ok(cwd)
    }

    pub fn create_narrative_checkpoint(
        &self,
        runtime: &dyn Runtime,
        run_id: Option<RunId>,
        provider: Option<Provider>,
        author: CheckpointAuthor,
        narrative: NarrativeInput,
    ) -> Result<(Event, StoredCheckpoint)> {
        validate_checkpoint_author(author.clone(), provider)?;
        let now = runtime.now()?;
        let meta = PendingEventMeta {
            occurred_at: now.clone(),
            recorded_at: now,
            run_id,
            provider,
            idempotency_key: None,
        };
        let service = CheckpointService::new(&self.session_dir());
        let mut staged = None;
        let journal = EventJournal::new(&self.session_dir(), self.meta.id.clone());
        let event = journal.append_with(meta, |sequence, committed_events| {
            let known_sequences = committed_events
                .iter()
                .map(|item| item.event.sequence)
                .collect::<std::collections::BTreeSet<_>>();
            if narrative
                .related_event_sequences
                .iter()
                .any(|item| !known_sequences.contains(item))
            {
                return Err(Error::InvalidState(
                    "checkpoint references an event not committed in this session".into(),
                ));
            }
            let stored = service.stage_narrative(sequence, author, narrative)?;
            let relative = format!("checkpoints/{sequence:012}.json");
            let through_sequence = stored.checkpoint.through_sequence;
            staged = Some(stored);
            Ok(EventKind::CheckpointCreated {
                checkpoint_kind: CheckpointKind::Narrative,
                through_sequence,
                path: relative,
            })
        })?;
        let stored =
            staged.ok_or_else(|| Error::InvalidState("checkpoint was not staged".into()))?;
        service.commit_refs(&stored)?;
        Ok((event, stored))
    }

    pub fn create_transition_checkpoint(
        &self,
        runtime: &dyn Runtime,
        run_id: Option<RunId>,
        provider: Option<Provider>,
        narrative_checkpoint_sequence: Option<u64>,
    ) -> Result<(Event, StoredCheckpoint)> {
        let now = runtime.now()?;
        let meta = PendingEventMeta {
            occurred_at: now.clone(),
            recorded_at: now,
            run_id,
            provider,
            idempotency_key: None,
        };
        let service = CheckpointService::new(&self.session_dir());
        let mut staged = None;
        let journal = EventJournal::new(&self.session_dir(), self.meta.id.clone());
        let event = journal.append_with(meta, |sequence, committed_events| {
            if let Some(narrative_sequence) = narrative_checkpoint_sequence {
                let points_to_narrative = committed_events.iter().any(|item| {
                    item.event.sequence == narrative_sequence
                        && matches!(
                            item.event.kind,
                            EventKind::CheckpointCreated {
                                checkpoint_kind: CheckpointKind::Narrative,
                                ..
                            }
                        )
                });
                if !points_to_narrative {
                    return Err(Error::InvalidState(
                        "transition does not reference a committed narrative checkpoint".into(),
                    ));
                }
            }
            let stored = service.stage_transition(sequence, narrative_checkpoint_sequence)?;
            let relative = format!("checkpoints/{sequence:012}.json");
            let through_sequence = stored.checkpoint.through_sequence;
            staged = Some(stored);
            Ok(EventKind::CheckpointCreated {
                checkpoint_kind: CheckpointKind::Transition,
                through_sequence,
                path: relative,
            })
        })?;
        let stored =
            staged.ok_or_else(|| Error::InvalidState("checkpoint was not staged".into()))?;
        service.commit_refs(&stored)?;
        Ok((event, stored))
    }

    pub fn remove_binding(&self) -> Result<()> {
        let path = self
            .layout
            .worktree_refs()
            .join(format!("{}.json", self.meta.worktree.key));
        let reference: WorktreeRef = read_json(&path)?;
        validate_worktree_ref(&reference)?;
        if reference.session_id != self.meta.id || reference.identity != self.meta.worktree {
            return Err(Error::InvalidState(
                "refusing to remove a worktree binding owned by another session".into(),
            ));
        }
        std::fs::remove_file(&path).map_err(|source| io(&path, source))?;
        crate::store::atomic::sync_directory(self.layout.worktree_refs().as_path())
    }

    fn ensure_directories(&self) -> Result<()> {
        for suffix in ["", "refs", "checkpoints", "blobs", "blobs/sha256", "runs"] {
            super::ensure_private_dir(&self.session_dir().join(suffix))?;
        }
        Ok(())
    }
}

fn validate_session_meta(meta: &SessionMeta, expected_id: &SessionId) -> Result<()> {
    if meta.schema_version != 1 || &meta.id != expected_id {
        return Err(Error::InvalidState(
            "session metadata identity or schema mismatch".into(),
        ));
    }
    meta.worktree.validate()?;
    OffsetDateTime::parse(&meta.created_at, &Rfc3339)
        .map_err(|error| Error::InvalidState(format!("invalid session creation time: {error}")))?;
    if meta.parent_session_id.is_some() != meta.parent_checkpoint_sequence.is_some()
        || meta.parent_session_id.as_ref() == Some(&meta.id)
        || meta.parent_checkpoint_sequence == Some(0)
    {
        return Err(Error::InvalidState(
            "session lineage is incomplete or invalid".into(),
        ));
    }
    Ok(())
}

fn validate_worktree_ref(reference: &WorktreeRef) -> Result<()> {
    reference.identity.validate()?;
    if reference.schema_version != 1
        || reference.key != reference.identity.key
        || reference.key.is_empty()
    {
        return Err(Error::InvalidState(
            "worktree ref identity or schema mismatch".into(),
        ));
    }
    Ok(())
}

fn already_bound(reference: &WorktreeRef) -> Error {
    Error::InvalidState(format!(
        "worktree is already bound to session {}",
        reference.session_id
    ))
}

fn validate_checkpoint_author(author: CheckpointAuthor, provider: Option<Provider>) -> Result<()> {
    if let CheckpointAuthor::Provider(author_provider) = author {
        if provider != Some(author_provider) {
            return Err(Error::InvalidState(
                "provider checkpoint author does not match event provider".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};

    use tempfile::TempDir;

    use super::SessionStore;
    use crate::model::{
        CheckpointAuthor, CheckpointKind, EventKind, GitSnapshot, NarrativeInput, Provider,
        SessionId, WorktreeIdentity,
    };
    use crate::runtime::Runtime;
    use crate::store::StateLayout;

    struct FixedRuntime;

    impl Runtime for FixedRuntime {
        fn now(&self) -> crate::error::Result<String> {
            Ok("2026-07-16T10:00:00Z".into())
        }

        fn session_id(&self) -> SessionId {
            SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap()
        }

        fn run_id(&self) -> crate::model::RunId {
            crate::model::RunId::new()
        }
    }

    fn snapshot() -> GitSnapshot {
        let common_git_dir = PathBuf::from("/repo/.git");
        let git_dir = PathBuf::from("/repo/.git/worktrees/oauth");
        GitSnapshot {
            identity: WorktreeIdentity {
                key: WorktreeIdentity::derive_key(&common_git_dir, &git_dir),
                common_git_dir,
                git_dir,
                worktree: PathBuf::from("/work/oauth"),
                cwd_relative: PathBuf::from("apps/web"),
            },
            branch: Some("feat/oauth".into()),
            head: "deadbeef".into(),
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
            dirty_submodules: Vec::new(),
        }
    }

    #[test]
    fn create_binds_and_lookup_returns_the_same_session() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        let created = SessionStore::create(&layout, &FixedRuntime, snapshot()).unwrap();

        let found = SessionStore::find_for_worktree(&layout, &snapshot().identity)
            .unwrap()
            .unwrap();

        assert_eq!(found.id(), created.id());
        let events = created.events().unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].kind,
            crate::model::EventKind::SessionCreated { .. }
        ));
        assert!(matches!(
            events[1].kind,
            crate::model::EventKind::GitSnapshot { .. }
        ));
        let reference_path = layout
            .worktree_refs()
            .join(format!("{}.json", snapshot().identity.key));
        for path in [created.session_dir().join("meta.json"), reference_path] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn checkpoint_artifacts_precede_the_event_and_refs_follow_it() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        let store = SessionStore::create(&layout, &FixedRuntime, snapshot()).unwrap();
        let mut narrative =
            NarrativeInput::minimal("Implement OAuth", "PKCE done", "Fix callback test");
        narrative.related_event_sequences = vec![2];

        let (narrative_event, narrative_stored) = store
            .create_narrative_checkpoint(
                &FixedRuntime,
                None,
                Some(Provider::Claude),
                CheckpointAuthor::Provider(Provider::Claude),
                narrative,
            )
            .unwrap();
        assert_eq!(narrative_event.sequence, 3);
        assert!(matches!(
            narrative_event.kind,
            EventKind::CheckpointCreated {
                checkpoint_kind: CheckpointKind::Narrative,
                through_sequence: 2,
                ..
            }
        ));
        assert!(narrative_stored.json_path.exists());
        assert!(narrative_stored.markdown_path.exists());
        assert_eq!(
            crate::store::refs::read_json::<u64>(
                &store.session_dir().join("refs/latest-checkpoint")
            )
            .unwrap(),
            3
        );
        assert_eq!(
            crate::store::refs::read_json::<u64>(
                &store.session_dir().join("refs/latest-narrative-checkpoint")
            )
            .unwrap(),
            3
        );

        let (transition_event, transition_stored) = store
            .create_transition_checkpoint(&FixedRuntime, None, None, Some(3))
            .unwrap();
        assert_eq!(transition_event.sequence, 4);
        assert_eq!(
            transition_stored.checkpoint.narrative_checkpoint_sequence,
            Some(3)
        );
        assert!(transition_stored.checkpoint.narrative.is_none());
        assert_eq!(
            crate::store::refs::read_json::<u64>(
                &store.session_dir().join("refs/latest-checkpoint")
            )
            .unwrap(),
            4
        );
        assert_eq!(
            crate::store::refs::read_json::<u64>(
                &store.session_dir().join("refs/latest-narrative-checkpoint")
            )
            .unwrap(),
            3
        );
    }

    #[test]
    fn invalid_checkpoint_references_do_not_append_or_stage() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        let store = SessionStore::create(&layout, &FixedRuntime, snapshot()).unwrap();
        let mut narrative = NarrativeInput::minimal("Objective", "Summary", "Next");
        narrative.related_event_sequences = vec![99];

        assert!(
            store
                .create_narrative_checkpoint(
                    &FixedRuntime,
                    None,
                    None,
                    CheckpointAuthor::Human,
                    narrative,
                )
                .is_err()
        );
        assert_eq!(store.events().unwrap().len(), 2);
        assert_eq!(
            std::fs::read_dir(store.session_dir().join("checkpoints"))
                .unwrap()
                .count(),
            0
        );
        assert!(
            store
                .create_transition_checkpoint(&FixedRuntime, None, None, Some(1))
                .is_err()
        );
        assert_eq!(store.events().unwrap().len(), 2);
    }

    #[test]
    fn saved_cwd_is_derived_from_verified_events_without_mutating_metadata() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        let store = SessionStore::create(&layout, &FixedRuntime, snapshot()).unwrap();
        assert_eq!(
            store.saved_cwd_relative().unwrap(),
            PathBuf::from("apps/web")
        );

        store
            .append(
                &FixedRuntime,
                None,
                None,
                EventKind::CwdChanged {
                    cwd_relative: PathBuf::from("crates/api"),
                },
            )
            .unwrap();

        assert_eq!(
            store.saved_cwd_relative().unwrap(),
            PathBuf::from("crates/api")
        );
        assert_eq!(
            store.meta().worktree.cwd_relative,
            PathBuf::from("apps/web")
        );
    }

    #[test]
    fn second_session_for_the_same_worktree_is_rejected() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        SessionStore::create(&layout, &FixedRuntime, snapshot()).unwrap();

        let error = SessionStore::create(&layout, &FixedRuntime, snapshot()).unwrap_err();

        assert!(error.to_string().contains("already bound"));
    }

    #[test]
    fn open_rejects_unknown_metadata_schema() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        let created = SessionStore::create(&layout, &FixedRuntime, snapshot()).unwrap();
        let path = created.session_dir().join("meta.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(999);
        crate::store::refs::write_json(&path, &value).unwrap();

        assert!(SessionStore::open(&layout, created.id().clone()).is_err());
    }

    #[test]
    fn lookup_allows_the_current_cwd_to_change_within_the_same_worktree() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        let created = SessionStore::create(&layout, &FixedRuntime, snapshot()).unwrap();
        let mut current = snapshot().identity;
        current.cwd_relative = PathBuf::from("crates/api");

        let found = SessionStore::find_for_worktree(&layout, &current)
            .unwrap()
            .unwrap();

        assert_eq!(found.id(), created.id());
    }

    #[test]
    fn lookup_rejects_a_ref_redirected_to_different_metadata() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        SessionStore::create(&layout, &FixedRuntime, snapshot()).unwrap();
        let path = layout
            .worktree_refs()
            .join(format!("{}.json", snapshot().identity.key));
        let mut reference: crate::model::WorktreeRef =
            crate::store::refs::read_json(&path).unwrap();
        reference.identity.worktree = PathBuf::from("/work/other");
        crate::store::refs::write_json(&path, &reference).unwrap();

        assert!(SessionStore::find_for_worktree(&layout, &snapshot().identity).is_err());
    }

    struct RuntimeWithId(SessionId);

    impl Runtime for RuntimeWithId {
        fn now(&self) -> crate::error::Result<String> {
            Ok("2026-07-16T10:00:00Z".into())
        }

        fn session_id(&self) -> SessionId {
            self.0.clone()
        }

        fn run_id(&self) -> crate::model::RunId {
            crate::model::RunId::new()
        }
    }

    #[test]
    fn concurrent_creates_produce_exactly_one_worktree_binding() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        let barrier = Arc::new(Barrier::new(2));
        let mut threads = Vec::new();
        for id in [
            "22222222-2222-4222-8222-222222222222",
            "33333333-3333-4333-8333-333333333333",
        ] {
            let layout = layout.clone();
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let runtime = RuntimeWithId(SessionId::parse(id).unwrap());
                barrier.wait();
                SessionStore::create(&layout, &runtime, snapshot())
            }));
        }

        let results = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert_eq!(
            std::fs::read_dir(layout.worktree_refs()).unwrap().count(),
            1
        );
        assert!(
            SessionStore::find_for_worktree(&layout, &snapshot().identity)
                .unwrap()
                .is_some()
        );
    }
}
