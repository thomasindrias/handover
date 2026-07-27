use std::ffi::OsStr;

use crate::app::write_projection;
use crate::error::{Error, Result, io};
use crate::model::{
    CheckpointKind, Event, EventKind, Provider, SessionId, SessionMeta, WorktreeRef,
};
use crate::store::refs::read_json;
use crate::store::{SessionStore, StateLayout};

pub(crate) fn build_list_value(layout: &StateLayout) -> Result<serde_json::Value> {
    let sessions_dir = layout.sessions();
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&sessions_dir).map_err(|source| io(&sessions_dir, source))? {
        let entry = entry.map_err(|source| io(&sessions_dir, source))?;
        names.push(entry.file_name());
    }
    let mut rows = Vec::new();
    for name in &names {
        rows.push(session_row(layout, name));
    }
    rows.sort_by(|left, right| {
        let left_key = (
            std::cmp::Reverse(left["last_activity"].as_str()),
            left["session_id"].as_str(),
        );
        let right_key = (
            std::cmp::Reverse(right["last_activity"].as_str()),
            right["session_id"].as_str(),
        );
        left_key.cmp(&right_key)
    });
    Ok(serde_json::json!({
        "schema_version": 1,
        "sessions": rows,
    }))
}

pub fn list_command(json: bool, layout: &StateLayout) -> Result<i32> {
    let value = build_list_value(layout)?;
    write_projection(&value, json)?;
    Ok(0)
}

fn session_row(layout: &StateLayout, name: &OsStr) -> serde_json::Value {
    match fallible_row(layout, name) {
        Ok(row) => row,
        Err(error) => degraded_row(&name.to_string_lossy(), &error.to_string()),
    }
}

fn fallible_row(layout: &StateLayout, name: &OsStr) -> Result<serde_json::Value> {
    let text = name
        .to_str()
        .ok_or_else(|| Error::InvalidState("session directory name is not UTF-8".into()))?;
    let id = SessionId::parse(text)
        .map_err(|_| Error::InvalidState(format!("unrecognized sessions entry {text}")))?;
    let store = SessionStore::open(layout, id)?;
    let events = store.events()?;
    let meta = store.meta();
    let (bound, binding_diagnostic) = binding_state(layout, meta);
    let (latest_narrative, events_since) = narrative_freshness(&events);
    Ok(serde_json::json!({
        "session_id": meta.id,
        "degraded": false,
        "diagnostics": binding_diagnostic.into_iter().collect::<Vec<_>>(),
        "repository": meta.worktree.common_git_dir,
        "worktree": meta.worktree.worktree,
        "branch": last_branch(&events),
        "bound": bound,
        "last_provider": last_provider(&events),
        "last_activity": last_activity(&events),
        "latest_narrative_checkpoint": latest_narrative,
        "events_since_narrative": events_since,
    }))
}

fn degraded_row(name: &str, diagnostic: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": name,
        "degraded": true,
        "diagnostics": [format!("{diagnostic}; run handover doctor")],
        "repository": null,
        "worktree": null,
        "branch": null,
        "bound": false,
        "last_provider": null,
        "last_activity": null,
        "latest_narrative_checkpoint": null,
        "events_since_narrative": null,
    })
}

fn binding_state(layout: &StateLayout, meta: &SessionMeta) -> (bool, Option<String>) {
    let path = layout
        .worktree_refs()
        .join(format!("{}.json", meta.worktree.key));
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, None),
        Err(error) => (false, Some(format!("cannot read worktree ref: {error}"))),
        Ok(_) => match read_json::<WorktreeRef>(&path) {
            Ok(reference) if reference.session_id == meta.id => (true, None),
            Ok(_) => (false, Some("worktree ref names another session".into())),
            Err(error) => (false, Some(format!("worktree ref is unreadable: {error}"))),
        },
    }
}

pub(crate) fn last_provider(events: &[Event]) -> Option<Provider> {
    events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, EventKind::RunStarted { .. }))
        .and_then(|event| event.provider)
}

pub(crate) fn last_activity(events: &[Event]) -> Option<String> {
    events.last().map(|event| event.recorded_at.clone())
}

pub(crate) fn last_branch(events: &[Event]) -> Option<String> {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::GitSnapshot { snapshot } => Some(snapshot.branch.clone()),
            _ => None,
        })
        .flatten()
}

pub(crate) fn narrative_freshness(events: &[Event]) -> (Option<u64>, u64) {
    let latest = events.iter().rev().find_map(|event| {
        matches!(
            event.kind,
            EventKind::CheckpointCreated {
                checkpoint_kind: CheckpointKind::Narrative,
                ..
            }
        )
        .then_some(event.sequence)
    });
    let since = events
        .iter()
        .filter(|event| latest.is_none_or(|sequence| event.sequence > sequence))
        .count() as u64;
    (latest, since)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::model::{
        CheckpointKind, Event, EventKind, GitSnapshot, Provider, SessionId, SessionMeta,
        WorktreeIdentity, WorktreeRef,
    };
    use crate::store::StateLayout;
    use crate::store::refs::write_json_create;

    use super::{
        binding_state, build_list_value, last_activity, last_branch, last_provider,
        narrative_freshness,
    };

    fn event(sequence: u64, provider: Option<Provider>, kind: EventKind) -> Event {
        Event {
            schema_version: 1,
            sequence,
            occurred_at: format!("2026-07-21T10:00:{sequence:02}Z"),
            recorded_at: format!("2026-07-21T10:00:{sequence:02}Z"),
            session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            run_id: None,
            provider,
            idempotency_key: None,
            kind,
        }
    }

    fn run_started(sequence: u64, provider: Provider) -> Event {
        event(
            sequence,
            Some(provider),
            EventKind::RunStarted {
                cwd: "/work/repo".into(),
                args: Vec::new(),
                supervisor_pid: 42,
            },
        )
    }

    fn snapshot_event(sequence: u64, branch: Option<&str>) -> Event {
        let common_git_dir = PathBuf::from("/work/repo/.git");
        let git_dir = common_git_dir.clone();
        event(
            sequence,
            None,
            EventKind::GitSnapshot {
                snapshot: GitSnapshot {
                    identity: WorktreeIdentity {
                        key: WorktreeIdentity::derive_key(&common_git_dir, &git_dir),
                        common_git_dir,
                        git_dir,
                        worktree: PathBuf::from("/work/repo"),
                        cwd_relative: PathBuf::new(),
                    },
                    branch: branch.map(str::to_owned),
                    head: "0123456789012345678901234567890123456789".into(),
                    staged: Vec::new(),
                    unstaged: Vec::new(),
                    untracked: Vec::new(),
                    dirty_submodules: Vec::new(),
                },
            },
        )
    }

    fn checkpoint_event(sequence: u64, kind: CheckpointKind) -> Event {
        event(
            sequence,
            None,
            EventKind::CheckpointCreated {
                checkpoint_kind: kind,
                through_sequence: sequence.saturating_sub(1),
                path: format!("checkpoints/{sequence:012}.json"),
            },
        )
    }

    #[test]
    fn last_provider_comes_from_the_newest_run_started_event() {
        let events = [
            run_started(1, Provider::Claude),
            snapshot_event(2, Some("main")),
            run_started(3, Provider::Codex),
        ];
        assert_eq!(last_provider(&events), Some(Provider::Codex));
        assert_eq!(last_provider(&[]), None);
        assert_eq!(last_provider(&[snapshot_event(1, None)]), None);
    }

    #[test]
    fn last_activity_is_the_newest_recorded_timestamp() {
        let events = [run_started(1, Provider::Claude), snapshot_event(2, None)];
        assert_eq!(
            last_activity(&events).as_deref(),
            Some("2026-07-21T10:00:02Z")
        );
        assert_eq!(last_activity(&[]), None);
    }

    #[test]
    fn last_branch_reads_only_the_newest_git_snapshot() {
        let named = [snapshot_event(1, Some("main"))];
        assert_eq!(last_branch(&named).as_deref(), Some("main"));

        let detached_after_named = [snapshot_event(1, Some("main")), snapshot_event(2, None)];
        assert_eq!(last_branch(&detached_after_named), None);
        assert_eq!(last_branch(&[]), None);
    }

    #[test]
    fn narrative_freshness_counts_events_after_the_newest_narrative_checkpoint() {
        let events = [
            run_started(1, Provider::Claude),
            checkpoint_event(2, CheckpointKind::Narrative),
            snapshot_event(3, Some("main")),
            checkpoint_event(4, CheckpointKind::Transition),
        ];
        assert_eq!(narrative_freshness(&events), (Some(2), 2));

        let unsummarized = [run_started(1, Provider::Claude), snapshot_event(2, None)];
        assert_eq!(narrative_freshness(&unsummarized), (None, 2));
        assert_eq!(narrative_freshness(&[]), (None, 0));
    }

    fn worktree_identity() -> WorktreeIdentity {
        let common_git_dir = PathBuf::from("/work/repo/.git");
        let git_dir = common_git_dir.clone();
        WorktreeIdentity {
            key: WorktreeIdentity::derive_key(&common_git_dir, &git_dir),
            common_git_dir,
            git_dir,
            worktree: PathBuf::from("/work/repo"),
            cwd_relative: PathBuf::new(),
        }
    }

    fn session_meta(id: SessionId, worktree: WorktreeIdentity) -> SessionMeta {
        SessionMeta {
            schema_version: 1,
            id,
            created_at: "2026-07-21T10:00:00Z".into(),
            worktree,
            parent_session_id: None,
            parent_checkpoint_sequence: None,
        }
    }

    #[test]
    fn binding_state_flags_a_worktree_ref_that_names_another_session() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().to_path_buf());
        let identity = worktree_identity();
        let meta = session_meta(SessionId::new(), identity.clone());
        let ref_path = layout
            .worktree_refs()
            .join(format!("{}.json", identity.key));
        let reference = WorktreeRef {
            schema_version: 1,
            key: identity.key.clone(),
            session_id: SessionId::new(),
            identity,
        };
        write_json_create(&ref_path, &reference).unwrap();

        let (bound, diagnostic) = binding_state(&layout, &meta);
        assert!(!bound);
        assert_eq!(
            diagnostic.as_deref(),
            Some("worktree ref names another session")
        );
    }

    #[test]
    fn binding_state_flags_a_worktree_ref_that_is_not_valid_json() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().to_path_buf());
        let identity = worktree_identity();
        let meta = session_meta(SessionId::new(), identity.clone());
        let ref_path = layout
            .worktree_refs()
            .join(format!("{}.json", identity.key));
        let reference = WorktreeRef {
            schema_version: 1,
            key: identity.key.clone(),
            session_id: meta.id.clone(),
            identity,
        };
        write_json_create(&ref_path, &reference).unwrap();
        std::fs::write(&ref_path, b"not json").unwrap();

        let (bound, diagnostic) = binding_state(&layout, &meta);
        assert!(!bound);
        assert!(
            diagnostic
                .unwrap()
                .starts_with("worktree ref is unreadable")
        );
    }

    #[test]
    fn build_list_value_reports_schema_version_and_empty_sessions_for_a_fresh_layout() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().to_path_buf());
        std::fs::create_dir_all(layout.sessions()).unwrap();

        let value = build_list_value(&layout).unwrap();

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["sessions"].as_array().unwrap().len(), 0);
    }
}
