use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::Provider;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    Narrative,
    Transition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "provider", rename_all = "snake_case")]
pub enum CheckpointAuthor {
    Human,
    Provider(Provider),
    System,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub statement: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NarrativeInput {
    pub objective: String,
    pub summary: String,
    pub decisions: Vec<Decision>,
    pub assumptions: Vec<String>,
    pub constraints: Vec<String>,
    pub completed: Vec<String>,
    pub in_progress: Vec<String>,
    pub blockers: Vec<String>,
    pub next_steps: Vec<String>,
    pub related_event_sequences: Vec<u64>,
}

impl NarrativeInput {
    pub const MAX_OBJECTIVE_BYTES: usize = 4 * 1024;
    pub const MAX_SUMMARY_BYTES: usize = 16 * 1024;
    pub const MAX_ITEM_BYTES: usize = 4 * 1024;
    pub const MAX_ITEMS: usize = 128;
    pub const MAX_TOTAL_BYTES: usize = 32 * 1024;

    pub fn minimal(objective: &str, summary: &str, next_step: &str) -> Self {
        Self {
            objective: objective.into(),
            summary: summary.into(),
            decisions: Vec::new(),
            assumptions: Vec::new(),
            constraints: Vec::new(),
            completed: Vec::new(),
            in_progress: Vec::new(),
            blockers: Vec::new(),
            next_steps: vec![next_step.into()],
            related_event_sequences: Vec::new(),
        }
    }

    pub fn validate(&self, through_sequence: u64) -> Result<()> {
        if self.objective.trim().is_empty() || self.summary.trim().is_empty() {
            return Err(Error::InvalidState(
                "checkpoint objective and summary are required".into(),
            ));
        }
        if self.next_steps.is_empty() || self.next_steps.iter().all(|item| item.trim().is_empty()) {
            return Err(Error::InvalidState(
                "checkpoint requires at least one next step".into(),
            ));
        }
        if self
            .related_event_sequences
            .iter()
            .any(|sequence| *sequence > through_sequence)
        {
            return Err(Error::InvalidState(
                "checkpoint references a future event".into(),
            ));
        }
        validate_field("objective", &self.objective, Self::MAX_OBJECTIVE_BYTES)?;
        validate_field("summary", &self.summary, Self::MAX_SUMMARY_BYTES)?;
        let item_count = self.decisions.len()
            + self.assumptions.len()
            + self.constraints.len()
            + self.completed.len()
            + self.in_progress.len()
            + self.blockers.len()
            + self.next_steps.len();
        if item_count > Self::MAX_ITEMS {
            return Err(Error::InvalidState(
                "checkpoint has more than 128 list items".into(),
            ));
        }
        for decision in &self.decisions {
            validate_field(
                "decision statement",
                &decision.statement,
                Self::MAX_ITEM_BYTES,
            )?;
            if let Some(reason) = &decision.reason {
                validate_field("decision reason", reason, Self::MAX_ITEM_BYTES)?;
            }
        }
        for (label, items) in [
            ("assumption", &self.assumptions),
            ("constraint", &self.constraints),
            ("completed item", &self.completed),
            ("in-progress item", &self.in_progress),
            ("blocker", &self.blockers),
            ("next step", &self.next_steps),
        ] {
            for item in items {
                validate_field(label, item, Self::MAX_ITEM_BYTES)?;
            }
        }
        if self
            .related_event_sequences
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(Error::InvalidState(
                "related event sequences must be sorted and unique".into(),
            ));
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|error| Error::InvalidState(format!("cannot encode checkpoint: {error}")))?;
        if bytes.len() > Self::MAX_TOTAL_BYTES {
            return Err(Error::InvalidState("checkpoint exceeds 32 KiB".into()));
        }
        Ok(())
    }
}

fn validate_field(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::InvalidState(format!("checkpoint {label} is empty")));
    }
    if value.len() > max_bytes {
        return Err(Error::InvalidState(format!(
            "checkpoint {label} exceeds {max_bytes} UTF-8 bytes",
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema_version: u32,
    pub checkpoint_kind: CheckpointKind,
    pub through_sequence: u64,
    pub author: CheckpointAuthor,
    pub narrative: Option<NarrativeInput>,
    pub narrative_checkpoint_sequence: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::{Decision, NarrativeInput};

    #[test]
    fn related_sequences_are_past_sorted_and_unique() {
        let mut narrative = NarrativeInput::minimal("Objective", "Summary", "Next");
        narrative.related_event_sequences = vec![1, 3, 3];
        assert!(narrative.validate(3).is_err());

        narrative.related_event_sequences = vec![1, 4];
        assert!(narrative.validate(3).is_err());

        narrative.related_event_sequences = vec![1, 3];
        assert!(narrative.validate(3).is_ok());
    }

    #[test]
    fn every_narrative_item_is_nonempty_and_bounded() {
        let mut narrative = NarrativeInput::minimal("Objective", "Summary", "Next");
        narrative.decisions.push(Decision {
            statement: "Decision".into(),
            reason: Some("  ".into()),
        });
        assert!(narrative.validate(1).is_err());

        narrative.decisions.clear();
        narrative.assumptions = vec!["item".into(); NarrativeInput::MAX_ITEMS];
        assert!(narrative.validate(1).is_err());
    }
}
