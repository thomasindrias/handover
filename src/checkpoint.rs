use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{Checkpoint, CheckpointAuthor, CheckpointKind, Decision, NarrativeInput};
use crate::store::atomic::{create_private, sync_directory};
use crate::store::refs::{write_json, write_json_create};

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
        "# Sesh checkpoint {event_sequence:012}\n\nKind: {}\n\nThrough event sequence: {}\n\n",
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::CheckpointService;
    use crate::model::{CheckpointAuthor, NarrativeInput, Provider};

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
}
