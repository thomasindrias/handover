use std::fmt::Write as _;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};
use crate::model::{
    Checkpoint, CheckpointAuthor, CheckpointKind, Decision, NarrativeInput, Provider, RunId,
    SessionId,
};
use crate::runtime::Runtime;
use crate::store::atomic::{create_private, read_private, sync_directory};
use crate::store::refs::{read_json, write_json, write_json_create};
use crate::store::{Environment, SessionStore};

const MAX_CHECKPOINT_TRANSPORT_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub struct StoredCheckpoint {
    pub event_sequence: u64,
    pub checkpoint: Checkpoint,
    pub json_path: PathBuf,
    pub markdown_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct CheckpointService {
    checkpoints: PathBuf,
    refs: PathBuf,
}

impl CheckpointService {
    pub fn new(session_dir: &Path) -> Self {
        Self {
            checkpoints: session_dir.join("checkpoints"),
            refs: session_dir.join("refs"),
        }
    }

    #[cfg(test)]
    pub fn for_test(session_dir: &Path) -> Self {
        Self::new(session_dir)
    }

    pub fn stage_narrative(
        &self,
        event_sequence: u64,
        author: CheckpointAuthor,
        narrative: NarrativeInput,
    ) -> Result<StoredCheckpoint> {
        let through_sequence = event_sequence.checked_sub(1).ok_or_else(|| {
            Error::InvalidState("checkpoint event sequence must be positive".into())
        })?;
        narrative.validate(through_sequence)?;
        self.stage(
            event_sequence,
            Checkpoint {
                schema_version: 1,
                checkpoint_kind: CheckpointKind::Narrative,
                through_sequence,
                author,
                narrative: Some(narrative),
                narrative_checkpoint_sequence: None,
            },
        )
    }

    pub fn stage_transition(
        &self,
        event_sequence: u64,
        narrative_checkpoint_sequence: Option<u64>,
    ) -> Result<StoredCheckpoint> {
        let through_sequence = event_sequence.checked_sub(1).ok_or_else(|| {
            Error::InvalidState("checkpoint event sequence must be positive".into())
        })?;
        if narrative_checkpoint_sequence
            .is_some_and(|sequence| sequence == 0 || sequence > through_sequence)
        {
            return Err(Error::InvalidState(
                "transition references an invalid narrative checkpoint sequence".into(),
            ));
        }
        self.stage(
            event_sequence,
            Checkpoint {
                schema_version: 1,
                checkpoint_kind: CheckpointKind::Transition,
                through_sequence,
                author: CheckpointAuthor::System,
                narrative: None,
                narrative_checkpoint_sequence,
            },
        )
    }

    pub fn commit_refs(&self, stored: &StoredCheckpoint) -> Result<()> {
        write_json(&self.refs.join("latest-checkpoint"), &stored.event_sequence)?;
        if stored.checkpoint.checkpoint_kind == CheckpointKind::Narrative {
            write_json(
                &self.refs.join("latest-narrative-checkpoint"),
                &stored.event_sequence,
            )?;
        }
        Ok(())
    }

    fn stage(&self, event_sequence: u64, checkpoint: Checkpoint) -> Result<StoredCheckpoint> {
        validate_checkpoint_shape(event_sequence, &checkpoint)?;
        let stem = format!("{event_sequence:012}");
        let json_path = self.checkpoints.join(format!("{stem}.json"));
        let markdown_path = self.checkpoints.join(format!("{stem}.md"));
        let markdown = render_markdown(event_sequence, &checkpoint)?;

        write_json_create(&json_path, &checkpoint)?;
        if let Err(original) = create_private(&markdown_path, markdown.as_bytes()) {
            if std::fs::remove_file(&json_path).is_ok() {
                let _ = sync_directory(&self.checkpoints);
            }
            return Err(original);
        }

        Ok(StoredCheckpoint {
            event_sequence,
            checkpoint,
            json_path,
            markdown_path,
        })
    }
}

pub fn load_verified_checkpoint(session_dir: &Path, event_sequence: u64) -> Result<Checkpoint> {
    let stem = format!("{event_sequence:012}");
    let directory = session_dir.join("checkpoints");
    let json_path = directory.join(format!("{stem}.json"));
    let markdown_path = directory.join(format!("{stem}.md"));
    let checkpoint: Checkpoint = read_json(&json_path)?;
    validate_checkpoint_shape(event_sequence, &checkpoint)?;
    let markdown = read_private(&markdown_path)?;
    if markdown != render_markdown(event_sequence, &checkpoint)?.as_bytes() {
        return Err(Error::InvalidState(format!(
            "checkpoint Markdown {} does not match its canonical JSON",
            markdown_path.display()
        )));
    }
    Ok(checkpoint)
}

fn validate_checkpoint_shape(event_sequence: u64, checkpoint: &Checkpoint) -> Result<()> {
    if checkpoint.schema_version != 1
        || event_sequence == 0
        || checkpoint.through_sequence != event_sequence - 1
    {
        return Err(Error::InvalidState(
            "checkpoint schema or event boundary is invalid".into(),
        ));
    }
    match checkpoint.checkpoint_kind {
        CheckpointKind::Narrative
            if checkpoint.narrative.is_some()
                && checkpoint.narrative_checkpoint_sequence.is_none() =>
        {
            Ok(())
        }
        CheckpointKind::Transition
            if checkpoint.narrative.is_none()
                && checkpoint.author == CheckpointAuthor::System
                && checkpoint
                    .narrative_checkpoint_sequence
                    .is_none_or(|sequence| sequence > 0 && sequence < event_sequence) =>
        {
            Ok(())
        }
        _ => Err(Error::InvalidState(
            "checkpoint kind and contents are inconsistent".into(),
        )),
    }
}

fn render_markdown(event_sequence: u64, checkpoint: &Checkpoint) -> Result<String> {
    let mut output = format!(
        "# Handover checkpoint {event_sequence:012}\n\nKind: {}\n\nThrough event sequence: {}\n\n",
        match checkpoint.checkpoint_kind {
            CheckpointKind::Narrative => "narrative",
            CheckpointKind::Transition => "transition",
        },
        checkpoint.through_sequence
    );

    match &checkpoint.narrative {
        Some(narrative) => render_narrative(&mut output, narrative),
        None => {
            output.push_str("## Narrative checkpoint\n\n");
            if let Some(sequence) = checkpoint.narrative_checkpoint_sequence {
                writeln!(output, "[Checkpoint {sequence:012}]({sequence:012}.md)\n")
                    .expect("writing to a string cannot fail");
            } else {
                output.push_str("No narrative checkpoint recorded.\n");
            }
        }
    }
    Ok(output)
}

fn render_narrative(output: &mut String, narrative: &NarrativeInput) {
    section_text(output, "Objective", &narrative.objective);
    section_text(output, "Summary", &narrative.summary);
    section_decisions(output, &narrative.decisions);
    section_list(output, "Assumptions", &narrative.assumptions);
    section_list(output, "Constraints", &narrative.constraints);
    section_list(output, "Completed", &narrative.completed);
    section_list(output, "In progress", &narrative.in_progress);
    section_list(output, "Blockers", &narrative.blockers);
    section_list(output, "Next steps", &narrative.next_steps);
}

fn section_text(output: &mut String, heading: &str, text: &str) {
    writeln!(output, "## {heading}\n\n{text}\n").expect("writing to a string cannot fail");
}

fn section_decisions(output: &mut String, decisions: &[Decision]) {
    output.push_str("## Decisions\n\n");
    if decisions.is_empty() {
        output.push_str("- None\n\n");
        return;
    }
    for decision in decisions {
        match &decision.reason {
            Some(reason) => writeln!(output, "- {} — {}", decision.statement, reason),
            None => writeln!(output, "- {}", decision.statement),
        }
        .expect("writing to a string cannot fail");
    }
    output.push('\n');
}

fn section_list(output: &mut String, heading: &str, items: &[String]) {
    writeln!(output, "## {heading}\n").expect("writing to a string cannot fail");
    if items.is_empty() {
        output.push_str("- None\n\n");
        return;
    }
    for item in items {
        writeln!(output, "- {item}").expect("writing to a string cannot fail");
    }
    output.push('\n');
}

pub fn read_narrative_json(mut input: impl Read) -> Result<NarrativeInput> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(MAX_CHECKPOINT_TRANSPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| crate::error::io("checkpoint input", source))?;
    if bytes.len() as u64 > MAX_CHECKPOINT_TRANSPORT_BYTES {
        return Err(Error::InvalidState(
            "checkpoint input exceeds the 64 KiB transport limit".into(),
        ));
    }
    let narrative: NarrativeInput = serde_json::from_slice(&bytes)
        .map_err(|error| Error::InvalidState(format!("invalid checkpoint JSON: {error}")))?;
    narrative.validate(u64::MAX)?;
    Ok(narrative)
}

pub fn submit_provider_narrative(
    environment: &Environment,
    narrative: &NarrativeInput,
) -> Result<PathBuf> {
    narrative.validate(u64::MAX)?;
    let root = required_path(environment, "HANDOVER_HOME")?;
    let root = resolve_from_current_dir(root)?;
    validate_private_directory(&root)?;
    let root = root
        .canonicalize()
        .map_err(|source| crate::error::io("HANDOVER_HOME", source))?;
    let session_id = SessionId::parse(required_utf8(environment, "HANDOVER_SESSION_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid HANDOVER_SESSION_ID: {error}")))?;
    let run_id = RunId::parse(required_utf8(environment, "HANDOVER_RUN_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid HANDOVER_RUN_ID: {error}")))?;
    let expected = root
        .join("sessions")
        .join(session_id.to_string())
        .join("runs")
        .join(run_id.to_string())
        .join("inbox/checkpoints");
    validate_private_directory_chain(&root, &expected)?;
    let expected = expected
        .canonicalize()
        .map_err(|source| crate::error::io(&expected, source))?;
    let supplied =
        resolve_from_current_dir(required_path(environment, "HANDOVER_CHECKPOINT_INBOX")?)?
            .canonicalize()
            .map_err(|source| crate::error::io("HANDOVER_CHECKPOINT_INBOX", source))?;
    if supplied != expected {
        return Err(Error::InvalidState(
            "HANDOVER_CHECKPOINT_INBOX does not match the active run inbox".into(),
        ));
    }

    let stem = uuid::Uuid::new_v4().to_string();
    let temporary = expected.join(format!("{stem}.json.tmp"));
    let target = expected.join(format!("{stem}.json"));
    let bytes = encode_narrative(narrative)?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|source| crate::error::io(&temporary, source))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| crate::error::io(&temporary, source))?;
        file.write_all(&bytes)
            .map_err(|source| crate::error::io(&temporary, source))?;
        file.sync_all()
            .map_err(|source| crate::error::io(&temporary, source))?;
        std::fs::rename(&temporary, &target).map_err(|source| crate::error::io(&target, source))?;
        sync_directory(&expected)?;
        Ok(target.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub fn edit_narrative(state_root: &Path, environment: &Environment) -> Result<NarrativeInput> {
    let editor = environment
        .get("VISUAL")
        .filter(|value| !value.is_empty())
        .or_else(|| environment.get("EDITOR").filter(|value| !value.is_empty()))
        .ok_or_else(|| {
            Error::InvalidState(
                "no editor configured; pipe JSON to `handover checkpoint --format json`".into(),
            )
        })?;
    let editor = editor
        .to_str()
        .ok_or_else(|| Error::InvalidState("VISUAL or EDITOR must be valid UTF-8".into()))?;
    let parts = shell_words::split(editor)
        .map_err(|error| Error::InvalidState(format!("cannot parse editor command: {error}")))?;
    let (program, arguments) = parts
        .split_first()
        .ok_or_else(|| Error::InvalidState("editor command is empty".into()))?;
    let path = state_root.join(format!(".checkpoint.{}.json", uuid::Uuid::new_v4()));
    let template = serde_json::json!({
        "objective": "",
        "summary": "",
        "decisions": [],
        "assumptions": [],
        "constraints": [],
        "completed": [],
        "in_progress": [],
        "blockers": [],
        "next_steps": [""],
        "related_event_sequences": []
    });
    let mut bytes = serde_json::to_vec_pretty(&template).map_err(|error| {
        Error::InvalidState(format!("cannot encode checkpoint template: {error}"))
    })?;
    bytes.push(b'\n');
    create_private(&path, &bytes)?;

    let result = (|| {
        let status = Command::new(program)
            .args(arguments)
            .arg(&path)
            .status()
            .map_err(|error| {
                Error::Command(format!("cannot launch editor {program:?}: {error}"))
            })?;
        if !status.success() {
            return Err(Error::Command(format!("editor exited with {status}")));
        }
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|source| crate::error::io(&path, source))?;
        if metadata.len() > MAX_CHECKPOINT_TRANSPORT_BYTES {
            return Err(Error::InvalidState(
                "checkpoint input exceeds the 64 KiB transport limit".into(),
            ));
        }
        read_narrative_json(std::io::Cursor::new(read_private(&path)?))
    })();
    let removal = std::fs::remove_file(&path)
        .map_err(|source| crate::error::io(&path, source))
        .and_then(|()| sync_directory(state_root));
    match (result, removal) {
        (Ok(narrative), Ok(())) => Ok(narrative),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

pub fn promote_inbox(
    store: &SessionStore,
    runtime: &dyn Runtime,
    run_id: &RunId,
    provider: Provider,
    inbox: &Path,
) -> Result<usize> {
    validate_private_directory(inbox)?;
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(inbox).map_err(|source| crate::error::io(inbox, source))? {
        let entry = entry.map_err(|source| crate::error::io(inbox, source))?;
        let path = entry.path();
        if path.extension() == Some(std::ffi::OsStr::new("json")) {
            paths.push(path);
        }
    }
    paths.sort();

    let mut promoted = 0usize;
    for path in paths {
        validate_private_regular_file(&path)?;
        let narrative: NarrativeInput = read_json(&path)?;
        let through_sequence = store.events()?.last().map_or(0, |event| event.sequence);
        narrative.validate(through_sequence)?;
        store.create_narrative_checkpoint(
            runtime,
            Some(run_id.clone()),
            Some(provider),
            CheckpointAuthor::Provider(provider),
            narrative,
        )?;
        std::fs::remove_file(&path).map_err(|source| crate::error::io(&path, source))?;
        sync_directory(inbox)?;
        promoted += 1;
    }
    Ok(promoted)
}

fn encode_narrative(narrative: &NarrativeInput) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(narrative)
        .map_err(|error| Error::InvalidState(format!("cannot encode checkpoint: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn required_path(environment: &Environment, key: &str) -> Result<PathBuf> {
    let value = environment
        .get(key)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::InvalidState(format!("{key} is required")))?;
    Ok(PathBuf::from(value))
}

fn required_utf8<'a>(environment: &'a Environment, key: &str) -> Result<&'a str> {
    environment
        .get(key)
        .ok_or_else(|| Error::InvalidState(format!("{key} is required")))?
        .to_str()
        .ok_or_else(|| Error::InvalidState(format!("{key} must be valid UTF-8")))
}

fn resolve_from_current_dir(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .map_err(|source| crate::error::io(".", source))?
            .join(path))
    }
}

fn validate_private_directory(path: &Path) -> Result<()> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| crate::error::io(path, source))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(Error::InvalidState(format!(
            "refusing insecure checkpoint inbox {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_private_directory_chain(root: &Path, leaf: &Path) -> Result<()> {
    let relative = leaf
        .strip_prefix(root)
        .map_err(|_| Error::InvalidState("checkpoint inbox is outside HANDOVER_HOME".into()))?;
    validate_private_directory(root)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(Error::InvalidState(
                "checkpoint inbox path is not normalized".into(),
            ));
        }
        current.push(component.as_os_str());
        validate_private_directory(&current)?;
    }
    Ok(())
}

fn validate_private_regular_file(path: &Path) -> Result<()> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| crate::error::io(path, source))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(Error::InvalidState(format!(
            "refusing insecure checkpoint submission {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::{CheckpointService, edit_narrative, load_verified_checkpoint, read_narrative_json};
    use crate::model::{CheckpointAuthor, NarrativeInput, Provider};
    use crate::store::Environment;

    #[test]
    fn rejects_a_narrative_without_next_steps() {
        let input = NarrativeInput {
            objective: "Implement OAuth".into(),
            summary: "PKCE is done".into(),
            decisions: Vec::new(),
            assumptions: Vec::new(),
            constraints: Vec::new(),
            completed: vec!["PKCE".into()],
            in_progress: Vec::new(),
            blockers: Vec::new(),
            next_steps: Vec::new(),
            related_event_sequences: Vec::new(),
        };

        assert!(input.validate(10).is_err());
    }

    #[test]
    fn rejects_an_oversized_checkpoint_field() {
        let input = NarrativeInput::minimal(
            &"x".repeat(4 * 1024 + 1),
            "PKCE is done",
            "Fix callback test",
        );

        assert!(input.validate(10).is_err());
    }

    #[test]
    fn transition_points_to_latest_narrative_without_copying_it() {
        let temp = TempDir::new().unwrap();
        let service = CheckpointService::for_test(temp.path());
        let narrative = service
            .stage_narrative(
                10,
                CheckpointAuthor::Provider(Provider::Claude),
                NarrativeInput::minimal("Implement OAuth", "PKCE done", "Fix callback test"),
            )
            .unwrap();

        let transition = service
            .stage_transition(12, Some(narrative.event_sequence))
            .unwrap();

        assert_eq!(
            transition.checkpoint.narrative_checkpoint_sequence,
            Some(narrative.event_sequence)
        );
        assert!(transition.checkpoint.narrative.is_none());
        assert!(narrative.json_path.exists());
        assert!(narrative.markdown_path.exists());
        assert!(!temp.path().join("refs/latest-checkpoint").exists());
    }

    #[test]
    fn immutable_checkpoint_files_are_private_and_never_replaced() {
        let temp = TempDir::new().unwrap();
        let service = CheckpointService::for_test(temp.path());
        let input = NarrativeInput::minimal("Objective", "Summary", "Next");
        let stored = service
            .stage_narrative(5, CheckpointAuthor::Human, input.clone())
            .unwrap();

        assert!(
            service
                .stage_narrative(5, CheckpointAuthor::Human, input)
                .is_err()
        );
        for path in [&stored.json_path, &stored.markdown_path] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn verified_checkpoint_requires_matching_immutable_json_and_markdown() {
        let temp = TempDir::new().unwrap();
        let service = CheckpointService::for_test(temp.path());
        let stored = service
            .stage_narrative(
                2,
                CheckpointAuthor::Human,
                NarrativeInput::minimal("Objective", "Summary", "Next"),
            )
            .unwrap();

        assert_eq!(
            load_verified_checkpoint(temp.path(), 2).unwrap(),
            stored.checkpoint
        );
        std::fs::write(&stored.markdown_path, b"forged\n").unwrap();
        assert!(load_verified_checkpoint(temp.path(), 2).is_err());
    }

    #[test]
    fn narrative_markdown_has_a_stable_section_order() {
        let temp = TempDir::new().unwrap();
        let service = CheckpointService::for_test(temp.path());
        let stored = service
            .stage_narrative(
                2,
                CheckpointAuthor::Human,
                NarrativeInput::minimal("Objective", "Summary", "Next"),
            )
            .unwrap();
        let markdown = std::fs::read_to_string(stored.markdown_path).unwrap();
        let headings = [
            "## Objective",
            "## Summary",
            "## Decisions",
            "## Assumptions",
            "## Constraints",
            "## Completed",
            "## In progress",
            "## Blockers",
            "## Next steps",
        ];

        let positions: Vec<_> = headings
            .iter()
            .map(|heading| markdown.find(heading).unwrap())
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn editor_flow_parses_output_and_always_removes_the_private_template() {
        let temp = TempDir::new().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let editor = temp.path().join("editor.sh");
        std::fs::write(
            &editor,
            format!("#!/bin/sh\nprintf '%s' '{}' > \"$1\"\n", valid_json()),
        )
        .unwrap();
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
        let environment =
            Environment::from_pairs(HashMap::from([("VISUAL", editor.as_os_str().to_owned())]));

        let narrative = edit_narrative(temp.path(), &environment).unwrap();

        assert_eq!(narrative.objective, "Objective");
        assert!(!std::fs::read_dir(temp.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".checkpoint.")
        }));
    }

    #[test]
    fn editor_launch_failure_removes_the_template() {
        let temp = TempDir::new().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let environment = Environment::from_pairs(HashMap::from([(
            "EDITOR",
            OsString::from("/usr/bin/false"),
        )]));

        assert!(edit_narrative(temp.path(), &environment).is_err());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn missing_editor_reports_the_exact_pipe_path() {
        let temp = TempDir::new().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let error = edit_narrative(temp.path(), &Environment::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("pipe JSON to `handover checkpoint --format json`"));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn transport_rejects_unknown_fields_and_oversized_input() {
        let mut value: serde_json::Value = serde_json::from_str(valid_json()).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(read_narrative_json(serde_json::to_vec(&value).unwrap().as_slice()).is_err());

        let oversized = vec![b' '; 64 * 1024 + 1];
        assert!(read_narrative_json(oversized.as_slice()).is_err());
    }

    fn valid_json() -> &'static str {
        r#"{"objective":"Objective","summary":"Summary","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Next"],"related_event_sequences":[]}"#
    }
}
