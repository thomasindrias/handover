use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::{
    Checkpoint, CheckpointAuthor, CheckpointKind, DirtyPath, GitSnapshot, NarrativeInput, Provider,
    SessionId,
};

const HEADING: &str = "# Sesh handoff\n\n";
pub const BOOTSTRAP: &str = "Continue the active Sesh session from its injected handoff. Verify the current worktree state, then proceed with the recorded next action.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandFact {
    pub sequence: u64,
    pub command: String,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureGap {
    pub sequence: u64,
    pub phase: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParentLineage {
    pub session_id: SessionId,
    pub transition_sequence: u64,
    pub narrative_sequence: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct HandoffInput {
    pub session_id: SessionId,
    pub parent_lineage: Option<ParentLineage>,
    pub from_provider: Option<Provider>,
    pub to_provider: Provider,
    pub transition_sequence: u64,
    pub transition_checkpoint: Checkpoint,
    pub narrative_checkpoint: Option<(u64, Checkpoint)>,
    pub snapshot: GitSnapshot,
    pub recent_events: Vec<(u64, String)>,
    pub recent_commands: Vec<CommandFact>,
    pub latest_test: Option<CommandFact>,
    pub latest_failure: Option<CommandFact>,
    pub capture_gaps: Vec<CaptureGap>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedHandoff {
    pub markdown: String,
    pub recent_event_sequences: Vec<u64>,
}

#[derive(Clone, Debug)]
struct GitSummary {
    staged: usize,
    unstaged: usize,
    untracked: usize,
    dirty_submodules: usize,
    fingerprint: String,
}

#[derive(Clone, Copy, Debug, Default)]
struct OmittedPaths {
    staged: usize,
    unstaged: usize,
    untracked: usize,
    dirty_submodules: usize,
}

pub fn render(input: HandoffInput, max_bytes: usize) -> Result<String> {
    render_with_selection(input, max_bytes).map(|rendered| rendered.markdown)
}

pub fn render_with_selection(input: HandoffInput, max_bytes: usize) -> Result<RenderedHandoff> {
    input.validate()?;
    let mut events = input.recent_events.clone();
    let mut commands = input.recent_commands.clone();
    let mut snapshot = input.snapshot.clone();
    sort_snapshot_paths(&mut snapshot)?;
    let git_summary = git_summary(&snapshot)?;
    let mut gaps = input.capture_gaps.clone();
    let mut omitted_events = Vec::new();
    let mut omitted_commands = Vec::new();
    let mut omitted_paths = OmittedPaths::default();
    let mut omitted_gaps = Vec::new();

    loop {
        let output = render_sections(
            &input,
            &git_summary,
            &snapshot,
            &events,
            &commands,
            &gaps,
            &omitted_events,
            &omitted_commands,
            omitted_paths,
            &omitted_gaps,
        )?;
        if output.len() <= max_bytes && recent_event_bytes(&events) <= max_bytes {
            return Ok(RenderedHandoff {
                markdown: output,
                recent_event_sequences: events.iter().map(|item| item.0).collect(),
            });
        }
        if !events.is_empty() {
            let count = removal_batch(events.len(), 0);
            omitted_events.extend(events.drain(..count).map(|(sequence, _)| sequence));
            continue;
        }
        if !commands.is_empty() {
            let count = removal_batch(commands.len(), 0);
            omitted_commands.extend(commands.drain(..count).map(|command| command.sequence));
            continue;
        }
        if !snapshot.untracked.is_empty() {
            let count = removal_batch(snapshot.untracked.len(), 0);
            snapshot
                .untracked
                .truncate(snapshot.untracked.len().saturating_sub(count));
            omitted_paths.untracked += count;
            continue;
        }
        if !snapshot.unstaged.is_empty() {
            let count = removal_batch(snapshot.unstaged.len(), 0);
            snapshot
                .unstaged
                .truncate(snapshot.unstaged.len().saturating_sub(count));
            omitted_paths.unstaged += count;
            continue;
        }
        if !snapshot.staged.is_empty() {
            let count = removal_batch(snapshot.staged.len(), 0);
            snapshot
                .staged
                .truncate(snapshot.staged.len().saturating_sub(count));
            omitted_paths.staged += count;
            continue;
        }
        if !snapshot.dirty_submodules.is_empty() {
            let count = removal_batch(snapshot.dirty_submodules.len(), 0);
            snapshot
                .dirty_submodules
                .truncate(snapshot.dirty_submodules.len().saturating_sub(count));
            omitted_paths.dirty_submodules += count;
            continue;
        }
        if gaps.len() > 1 {
            let count = removal_batch(gaps.len(), 1);
            omitted_gaps.extend(gaps.drain(..count).map(|gap| gap.sequence));
            continue;
        }
        return Err(Error::InvalidState(
            "required handoff facts exceed configured byte limit".into(),
        ));
    }
}

pub fn is_recognized_test_command(command: &str) -> bool {
    if command.bytes().any(|byte| {
        matches!(
            byte,
            b';' | b'|' | b'&' | b'<' | b'>' | b'\n' | b'\r' | b'`'
        )
    }) || command.contains("$(")
    {
        return false;
    }
    let Ok(tokens) = shell_words::split(command) else {
        return false;
    };
    let mut remaining = tokens.as_slice();
    loop {
        match remaining.first().map(String::as_str) {
            Some("command" | "rtk") => remaining = &remaining[1..],
            Some("env") => {
                remaining = &remaining[1..];
                while let Some(token) = remaining.first() {
                    if matches!(token.as_str(), "-i" | "--ignore-environment")
                        || is_environment_assignment(token)
                    {
                        remaining = &remaining[1..];
                    } else {
                        break;
                    }
                }
            }
            _ => break,
        }
    }
    matches_prefix(remaining, &["cargo", "test"])
        || matches_prefix(remaining, &["pytest"])
        || matches_prefix(remaining, &["python", "-m", "pytest"])
        || matches_prefix(remaining, &["go", "test"])
        || matches_prefix(remaining, &["npm", "test"])
        || matches_prefix(remaining, &["npm", "run", "test"])
        || matches_prefix(remaining, &["pnpm", "test"])
        || matches_prefix(remaining, &["yarn", "test"])
        || matches_prefix(remaining, &["bun", "test"])
}

fn recent_event_bytes(events: &[(u64, String)]) -> usize {
    events
        .iter()
        .map(|(_, line)| line.len().saturating_add(1))
        .sum()
}

fn matches_prefix(tokens: &[String], expected: &[&str]) -> bool {
    tokens.len() >= expected.len()
        && tokens
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual == expected)
}

fn is_environment_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

impl HandoffInput {
    fn validate(&self) -> Result<()> {
        if self.transition_sequence == 0
            || self.transition_checkpoint.schema_version != 1
            || self.transition_checkpoint.checkpoint_kind != CheckpointKind::Transition
            || self.transition_checkpoint.through_sequence != self.transition_sequence - 1
            || self.transition_checkpoint.author != CheckpointAuthor::System
            || self.transition_checkpoint.narrative.is_some()
        {
            return Err(Error::InvalidState(
                "handoff transition checkpoint is inconsistent".into(),
            ));
        }
        match &self.narrative_checkpoint {
            Some((sequence, checkpoint)) => {
                if *sequence == 0
                    || *sequence >= self.transition_sequence
                    || checkpoint.schema_version != 1
                    || checkpoint.checkpoint_kind != CheckpointKind::Narrative
                    || checkpoint.through_sequence != sequence - 1
                    || checkpoint.narrative.is_none()
                    || checkpoint.narrative_checkpoint_sequence.is_some()
                    || self.transition_checkpoint.narrative_checkpoint_sequence != Some(*sequence)
                {
                    return Err(Error::InvalidState(
                        "handoff narrative checkpoint is inconsistent".into(),
                    ));
                }
                checkpoint
                    .narrative
                    .as_ref()
                    .expect("validated narrative")
                    .validate(checkpoint.through_sequence)?;
            }
            None if self
                .transition_checkpoint
                .narrative_checkpoint_sequence
                .is_some() =>
            {
                return Err(Error::InvalidState(
                    "handoff is missing its referenced narrative checkpoint".into(),
                ));
            }
            None => {}
        }
        self.snapshot.identity.validate()?;
        if let Some(parent) = self.parent_lineage.as_ref() {
            if parent.session_id == self.session_id
                || parent.transition_sequence != self.transition_sequence
                || parent.narrative_sequence
                    != self.narrative_checkpoint.as_ref().map(|item| item.0)
            {
                return Err(Error::InvalidState(
                    "handoff parent lineage is inconsistent".into(),
                ));
            }
        }
        validate_snapshot_paths(&self.snapshot)?;
        validate_sequence_pairs("recent events", &self.recent_events)?;
        validate_fact_sequences(
            "recent commands",
            self.recent_commands.iter().map(|fact| fact.sequence),
        )?;
        validate_fact_sequences(
            "capture gaps",
            self.capture_gaps.iter().map(|gap| gap.sequence),
        )?;
        for sequence in self
            .recent_events
            .iter()
            .map(|item| item.0)
            .chain(self.recent_commands.iter().map(|item| item.sequence))
            .chain(self.capture_gaps.iter().map(|item| item.sequence))
            .chain(self.latest_test.iter().map(|item| item.sequence))
            .chain(self.latest_failure.iter().map(|item| item.sequence))
        {
            if sequence == 0 {
                return Err(Error::InvalidState(
                    "handoff fact sequence must be positive".into(),
                ));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn fixture() -> Self {
        use std::path::PathBuf;

        use crate::model::WorktreeIdentity;

        let common_git_dir = PathBuf::from("/repo/.git");
        let git_dir = PathBuf::from("/repo/.git/worktrees/oauth");
        let command = CommandFact {
            sequence: 18,
            command: "cargo test oauth_callback".into(),
            exit_code: Some(101),
            stdout: Some("1 passed; 1 failed".into()),
            stderr: Some("callback integration test failed".into()),
        };
        Self {
            session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            parent_lineage: None,
            from_provider: Some(Provider::Claude),
            to_provider: Provider::Codex,
            transition_sequence: 19,
            transition_checkpoint: Checkpoint {
                schema_version: 1,
                checkpoint_kind: CheckpointKind::Transition,
                through_sequence: 18,
                author: CheckpointAuthor::System,
                narrative: None,
                narrative_checkpoint_sequence: Some(10),
            },
            narrative_checkpoint: Some((
                10,
                Checkpoint {
                    schema_version: 1,
                    checkpoint_kind: CheckpointKind::Narrative,
                    through_sequence: 9,
                    author: CheckpointAuthor::Provider(Provider::Claude),
                    narrative: Some(NarrativeInput::minimal(
                        "Implement OAuth callback",
                        "PKCE support is complete; one integration test still fails.",
                        "Fix callback integration test",
                    )),
                    narrative_checkpoint_sequence: None,
                },
            )),
            snapshot: GitSnapshot {
                identity: WorktreeIdentity {
                    key: WorktreeIdentity::derive_key(&common_git_dir, &git_dir),
                    common_git_dir,
                    git_dir,
                    worktree: PathBuf::from("/work/oauth"),
                    cwd_relative: PathBuf::from("apps/web"),
                },
                branch: Some("feat/oauth".into()),
                head: "deadbeef".into(),
                staged: vec![DirtyPath {
                    path: PathBuf::from("apps/web/src/oauth.rs"),
                    sha256: Some("a".repeat(64)),
                    executable: false,
                    symlink_target: None,
                }],
                unstaged: Vec::new(),
                untracked: Vec::new(),
                dirty_submodules: Vec::new(),
            },
            recent_events: vec![(16, "provider prompt submitted".into())],
            recent_commands: vec![command.clone()],
            latest_test: Some(command.clone()),
            latest_failure: Some(command),
            capture_gaps: Vec::new(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_sections(
    input: &HandoffInput,
    git: &GitSummary,
    snapshot: &GitSnapshot,
    events: &[(u64, String)],
    commands: &[CommandFact],
    gaps: &[CaptureGap],
    omitted_events: &[u64],
    omitted_commands: &[u64],
    omitted_paths: OmittedPaths,
    omitted_gaps: &[u64],
) -> Result<String> {
    let mut output = String::from(HEADING);
    render_transition(input, &mut output);
    render_repository(snapshot, &mut output)?;
    render_checkpoint_boundaries(input, &mut output);
    render_git_facts(git, snapshot, &mut output)?;
    let event_scope = if input.parent_lineage.is_some() {
        "parent event"
    } else {
        "child event"
    };
    render_events(events, event_scope, &mut output);
    render_commands(commands, event_scope, &mut output);
    render_required_command(
        "Latest recognized test",
        input.latest_test.as_ref(),
        false,
        event_scope,
        &mut output,
    );
    render_required_command(
        "Latest failed command",
        input.latest_failure.as_ref(),
        true,
        event_scope,
        &mut output,
    );
    render_capture_gaps(gaps, input.parent_lineage.is_some(), &mut output);
    render_omissions(
        omitted_events,
        omitted_commands,
        omitted_paths,
        omitted_gaps,
        input.parent_lineage.is_some(),
        &mut output,
    );
    output.push_str("## Inspect the complete session\n\n- `sesh log --json`\n- `sesh inspect`\n");
    Ok(output)
}

fn render_transition(input: &HandoffInput, output: &mut String) {
    let from = input.from_provider.map_or("none", Provider::executable);
    writeln!(
        output,
        "## Provider transition\n\n- Session: `{}`\n- Provider: `{from}` → `{}`\n- Transition event sequence: {}\n",
        input.session_id,
        input.to_provider.executable(),
        input.transition_sequence
    )
    .expect("writing to a string cannot fail");
    if let Some(parent) = input.parent_lineage.as_ref() {
        writeln!(
            output,
            "Forked from session `{}` at parent checkpoint {}.\n",
            parent.session_id, parent.transition_sequence
        )
        .expect("writing to a string cannot fail");
    }
}

fn render_repository(snapshot: &GitSnapshot, output: &mut String) -> Result<()> {
    let identity = &snapshot.identity;
    writeln!(
        output,
        "## Repository identity\n\n- Worktree: `{}`\n- Saved cwd: `{}`\n- Branch: `{}`\n- HEAD: `{}`\n- Worktree key: `{}`\n",
        path_text(&identity.worktree)?,
        path_text(&identity.cwd_relative)?,
        snapshot.branch.as_deref().unwrap_or("detached HEAD"),
        snapshot.head,
        identity.key
    )
    .expect("writing to a string cannot fail");
    Ok(())
}

fn render_checkpoint_boundaries(input: &HandoffInput, output: &mut String) {
    writeln!(
        output,
        "## Transition checkpoint\n\n- Event sequence: {}\n- Includes committed facts through sequence: {}\n",
        input.transition_sequence, input.transition_checkpoint.through_sequence
    )
    .expect("writing to a string cannot fail");
    output.push_str("## Narrative checkpoint\n\n");
    let Some((sequence, checkpoint)) = &input.narrative_checkpoint else {
        output.push_str(
            "No narrative checkpoint exists. Objective, decisions, assumptions, and next steps were not checkpointed.\n\n",
        );
        return;
    };
    writeln!(
        output,
        "- Event sequence: {sequence}\n- Includes committed facts through sequence: {}\n- Author: {}\n",
        checkpoint.through_sequence,
        author_text(&checkpoint.author)
    )
    .expect("writing to a string cannot fail");
    render_narrative(
        output,
        checkpoint.narrative.as_ref().expect("validated narrative"),
    );
}

fn render_narrative(output: &mut String, narrative: &NarrativeInput) {
    writeln!(output, "### Objective\n\n{}\n", narrative.objective)
        .expect("writing to a string cannot fail");
    writeln!(output, "### Summary\n\n{}\n", narrative.summary)
        .expect("writing to a string cannot fail");
    output.push_str("### Decisions\n\n");
    if narrative.decisions.is_empty() {
        output.push_str("- None checkpointed.\n\n");
    } else {
        for decision in &narrative.decisions {
            match &decision.reason {
                Some(reason) => writeln!(output, "- {} — {}", decision.statement, reason),
                None => writeln!(output, "- {}", decision.statement),
            }
            .expect("writing to a string cannot fail");
        }
        output.push('\n');
    }
    render_narrative_list(output, "Assumptions", &narrative.assumptions);
    render_narrative_list(output, "Constraints", &narrative.constraints);
    render_narrative_list(output, "Completed", &narrative.completed);
    render_narrative_list(output, "In progress", &narrative.in_progress);
    render_narrative_list(output, "Blockers", &narrative.blockers);
    render_narrative_list(output, "Next steps", &narrative.next_steps);
}

fn render_narrative_list(output: &mut String, heading: &str, items: &[String]) {
    writeln!(output, "### {heading}\n").expect("writing to a string cannot fail");
    if items.is_empty() {
        output.push_str("- None checkpointed.\n\n");
    } else {
        for item in items {
            writeln!(output, "- {item}").expect("writing to a string cannot fail");
        }
        output.push('\n');
    }
}

fn render_git_facts(git: &GitSummary, snapshot: &GitSnapshot, output: &mut String) -> Result<()> {
    writeln!(
        output,
        "## Observed worktree facts\n\n- Staged paths: {}\n- Unstaged paths: {}\n- Untracked paths: {}\n- Dirty submodules: {}\n- Git snapshot fingerprint: `sha256:{}`\n",
        git.staged, git.unstaged, git.untracked, git.dirty_submodules, git.fingerprint
    )
    .expect("writing to a string cannot fail");
    render_dirty_paths("Staged path details", &snapshot.staged, output)?;
    render_dirty_paths("Unstaged path details", &snapshot.unstaged, output)?;
    render_dirty_paths("Untracked path details", &snapshot.untracked, output)?;
    output.push_str("### Dirty submodule details\n\n");
    if snapshot.dirty_submodules.is_empty() {
        output.push_str("- None selected.\n\n");
    } else {
        for path in &snapshot.dirty_submodules {
            writeln!(output, "- `{}`", path_text(path)?).expect("writing to a string cannot fail");
        }
        output.push('\n');
    }
    Ok(())
}

fn render_dirty_paths(heading: &str, paths: &[DirtyPath], output: &mut String) -> Result<()> {
    writeln!(output, "### {heading}\n").expect("writing to a string cannot fail");
    if paths.is_empty() {
        output.push_str("- None selected.\n\n");
        return Ok(());
    }
    for path in paths {
        write!(output, "- `{}`", path_text(&path.path)?).expect("writing to a string cannot fail");
        if let Some(sha256) = &path.sha256 {
            write!(output, " — `sha256:{sha256}`").expect("writing to a string cannot fail");
        }
        if path.executable {
            output.push_str(" — executable");
        }
        if let Some(target) = &path.symlink_target {
            write!(output, " — symlink to `{}`", path_text(target)?)
                .expect("writing to a string cannot fail");
        }
        output.push('\n');
    }
    output.push('\n');
    Ok(())
}

fn render_events(events: &[(u64, String)], scope: &str, output: &mut String) {
    output.push_str("## Recent normalized events\n\n");
    if events.is_empty() {
        output.push_str("- None selected.\n\n");
        return;
    }
    for (sequence, event) in events {
        writeln!(
            output,
            "- {scope} {sequence}: {}",
            bounded_head(event, 2 * 1024)
        )
        .expect("writing to a string cannot fail");
    }
    output.push('\n');
}

fn render_commands(commands: &[CommandFact], scope: &str, output: &mut String) {
    output.push_str("## Recent commands\n\n");
    if commands.is_empty() {
        output.push_str("- None selected.\n\n");
        return;
    }
    for command in commands {
        writeln!(
            output,
            "- {scope} {}: `{}` — status {}",
            command.sequence,
            bounded_head(&command.command, 2 * 1024),
            status_text(command.exit_code)
        )
        .expect("writing to a string cannot fail");
    }
    output.push('\n');
}

fn render_required_command(
    heading: &str,
    command: Option<&CommandFact>,
    failure_excerpt: bool,
    event_scope: &str,
    output: &mut String,
) {
    writeln!(output, "## {heading}\n").expect("writing to a string cannot fail");
    let Some(command) = command else {
        output.push_str("No matching command was captured.\n\n");
        return;
    };
    writeln!(
        output,
        "- {event_scope}: {}\n- Command: `{}`\n- Status: {}\n",
        command.sequence,
        bounded_head(&command.command, 2 * 1024),
        status_text(command.exit_code)
    )
    .expect("writing to a string cannot fail");
    let combined = command_output(command);
    let excerpt = if failure_excerpt {
        head_tail_excerpt(&combined, 2 * 1024, 6 * 1024)
    } else {
        bounded_head(&combined, 2 * 1024)
    };
    output.push_str("```text\n");
    output.push_str(&excerpt);
    if !excerpt.ends_with('\n') {
        output.push('\n');
    }
    output.push_str("```\n\n");
}

fn render_capture_gaps(gaps: &[CaptureGap], parent_scoped: bool, output: &mut String) {
    output.push_str("## Capture gaps\n\n");
    if gaps.is_empty() {
        output.push_str("- None recorded.\n\n");
        return;
    }
    for gap in gaps {
        let scope = if parent_scoped { "parent event " } else { "" };
        writeln!(
            output,
            "- {scope}{} [{}]: {}",
            gap.sequence,
            bounded_head(&gap.phase, 256),
            bounded_head(&gap.message, 1024)
        )
        .expect("writing to a string cannot fail");
    }
    output.push('\n');
}

fn render_omissions(
    events: &[u64],
    commands: &[u64],
    paths: OmittedPaths,
    gaps: &[u64],
    parent_scoped: bool,
    output: &mut String,
) {
    output.push_str("## Omitted details\n\n");
    let mut any = false;
    let scope = if parent_scoped { "parent " } else { "" };
    for (label, sequences) in [
        (format!("Omitted {scope}event sequences"), events),
        (format!("Omitted {scope}command event sequences"), commands),
        (format!("Omitted {scope}capture-gap event sequences"), gaps),
    ] {
        if !sequences.is_empty() {
            writeln!(output, "- {label}: {}", render_ranges(sequences))
                .expect("writing to a string cannot fail");
            any = true;
        }
    }
    for (label, count) in [
        ("Omitted staged path details", paths.staged),
        ("Omitted unstaged path details", paths.unstaged),
        ("Omitted untracked path details", paths.untracked),
        ("Omitted dirty submodule details", paths.dirty_submodules),
    ] {
        if count > 0 {
            writeln!(output, "- {label}: {count}").expect("writing to a string cannot fail");
            any = true;
        }
    }
    if !any {
        output.push_str("- None.\n");
    }
    output.push('\n');
}

fn git_summary(snapshot: &GitSnapshot) -> Result<GitSummary> {
    #[derive(Serialize)]
    struct Fingerprint<'a> {
        identity: &'a crate::model::WorktreeIdentity,
        branch: &'a Option<String>,
        head: &'a str,
        staged: &'a [DirtyPath],
        unstaged: &'a [DirtyPath],
        untracked: &'a [DirtyPath],
        dirty_submodules: &'a [std::path::PathBuf],
    }
    let bytes = serde_json::to_vec(&Fingerprint {
        identity: &snapshot.identity,
        branch: &snapshot.branch,
        head: &snapshot.head,
        staged: &snapshot.staged,
        unstaged: &snapshot.unstaged,
        untracked: &snapshot.untracked,
        dirty_submodules: &snapshot.dirty_submodules,
    })
    .map_err(|error| Error::InvalidState(format!("cannot fingerprint Git snapshot: {error}")))?;
    Ok(GitSummary {
        staged: snapshot.staged.len(),
        unstaged: snapshot.unstaged.len(),
        untracked: snapshot.untracked.len(),
        dirty_submodules: snapshot.dirty_submodules.len(),
        fingerprint: hex::encode(Sha256::digest(bytes)),
    })
}

fn sort_snapshot_paths(snapshot: &mut GitSnapshot) -> Result<()> {
    for paths in [
        &mut snapshot.staged,
        &mut snapshot.unstaged,
        &mut snapshot.untracked,
    ] {
        paths.sort_by(|left, right| left.path.cmp(&right.path));
        if paths.windows(2).any(|pair| pair[0].path == pair[1].path) {
            return Err(Error::InvalidState(
                "Git snapshot contains duplicate path facts".into(),
            ));
        }
    }
    snapshot.dirty_submodules.sort();
    if snapshot
        .dirty_submodules
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        return Err(Error::InvalidState(
            "Git snapshot contains duplicate dirty submodules".into(),
        ));
    }
    Ok(())
}

fn validate_snapshot_paths(snapshot: &GitSnapshot) -> Result<()> {
    for path in snapshot
        .staged
        .iter()
        .chain(&snapshot.unstaged)
        .chain(&snapshot.untracked)
    {
        path_text(&path.path)?;
        if let Some(target) = &path.symlink_target {
            path_text(target)?;
        }
    }
    for path in &snapshot.dirty_submodules {
        path_text(path)?;
    }
    Ok(())
}

fn validate_sequence_pairs(label: &str, values: &[(u64, String)]) -> Result<()> {
    validate_fact_sequences(label, values.iter().map(|item| item.0))
}

fn validate_fact_sequences(label: &str, values: impl Iterator<Item = u64>) -> Result<()> {
    let mut previous = None;
    for sequence in values {
        if previous.is_some_and(|prior| sequence <= prior) {
            return Err(Error::InvalidState(format!(
                "handoff {label} must be sorted with unique sequences"
            )));
        }
        previous = Some(sequence);
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::InvalidState("handoff paths must be valid UTF-8".into()))
}

fn author_text(author: &CheckpointAuthor) -> &'static str {
    match author {
        CheckpointAuthor::Human => "human",
        CheckpointAuthor::Provider(provider) => provider.executable(),
        CheckpointAuthor::System => "system",
    }
}

fn status_text(exit_code: Option<i32>) -> String {
    exit_code.map_or_else(|| "unknown".into(), |code| format!("exit {code}"))
}

fn command_output(command: &CommandFact) -> String {
    let mut output = String::new();
    if let Some(stdout) = &command.stdout {
        output.push_str("stdout:\n");
        output.push_str(stdout);
        output.push('\n');
    }
    if let Some(stderr) = &command.stderr {
        output.push_str("stderr:\n");
        output.push_str(stderr);
        output.push('\n');
    }
    if output.is_empty() {
        output.push_str("No output captured.\n");
    }
    output
}

fn bounded_head(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut keep = char_boundary_at_or_before(value, max_bytes.saturating_sub(32));
    loop {
        let omitted = value.len() - keep;
        let marker = format!("\n… [{omitted} bytes omitted]");
        let allowed = max_bytes.saturating_sub(marker.len());
        let adjusted = char_boundary_at_or_before(value, keep.min(allowed));
        if adjusted == keep {
            let mut output = value[..keep].to_owned();
            output.push_str(&marker);
            return output;
        }
        keep = adjusted;
    }
}

fn head_tail_excerpt(value: &str, head_bytes: usize, tail_bytes: usize) -> String {
    if value.len() <= head_bytes + tail_bytes {
        return value.to_owned();
    }
    let head_end = char_boundary_at_or_before(value, head_bytes);
    let tail_start = char_boundary_at_or_after(value, value.len() - tail_bytes);
    if head_end >= tail_start {
        return value.to_owned();
    }
    let omitted = tail_start - head_end;
    format!(
        "{}\n… [{omitted} bytes omitted] …\n{}",
        &value[..head_end],
        &value[tail_start..]
    )
}

fn char_boundary_at_or_before(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn char_boundary_at_or_after(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn render_ranges(sequences: &[u64]) -> String {
    let mut output = String::new();
    let mut start = sequences[0];
    let mut end = start;
    for sequence in sequences.iter().copied().skip(1) {
        if sequence == end.saturating_add(1) {
            end = sequence;
        } else {
            append_range(&mut output, start, end);
            start = sequence;
            end = sequence;
        }
    }
    append_range(&mut output, start, end);
    output
}

fn append_range(output: &mut String, start: u64, end: u64) {
    if !output.is_empty() {
        output.push_str(", ");
    }
    if start == end {
        write!(output, "{start}").expect("writing to a string cannot fail");
    } else {
        write!(output, "{start}..{end}").expect("writing to a string cannot fail");
    }
}

fn removal_batch(len: usize, retain: usize) -> usize {
    (len.saturating_sub(retain) / 2)
        .max(1)
        .min(len.saturating_sub(retain))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        HandoffInput, ParentLineage, head_tail_excerpt, is_recognized_test_command, render,
        render_ranges, render_with_selection,
    };
    use crate::model::{DirtyPath, Provider};

    #[test]
    fn facts_and_narrative_are_labeled_separately() {
        let output = render(HandoffInput::fixture(), 65_536).unwrap();
        let checkpoint = output.find("## Narrative checkpoint").unwrap();
        let worktree = output.find("## Observed worktree facts").unwrap();
        let failure = output.find("## Latest failed command").unwrap();
        assert!(checkpoint < worktree && worktree < failure);
        assert!(output.contains("Fix callback integration test"));
    }

    #[test]
    fn oversized_history_reports_exact_omitted_range() {
        let mut input = HandoffInput::fixture();
        input.recent_events = (20..=120)
            .map(|sequence| (sequence, "x".repeat(256)))
            .collect();

        let output = render(input, 4096).unwrap();

        assert!(output.len() <= 4096);
        assert!(output.contains("Omitted event sequences"));
        assert!(output.contains("20.."));
    }

    #[test]
    fn huge_dirty_path_set_keeps_counts_and_fingerprint_within_limit() {
        let mut input = HandoffInput::fixture();
        input.snapshot.untracked = (0..10_000)
            .map(|index| DirtyPath {
                path: PathBuf::from(format!("generated/{index:05}.txt")),
                sha256: Some("a".repeat(64)),
                executable: false,
                symlink_target: None,
            })
            .collect();

        let output = render(input, 65_536).unwrap();

        assert!(output.len() <= 65_536);
        assert!(output.contains("Untracked paths: 10000"));
        assert!(output.contains("Omitted untracked path details:"));
        assert!(output.contains("Git snapshot fingerprint:"));
    }

    #[test]
    fn no_narrative_checkpoint_is_explicit() {
        let mut input = HandoffInput::fixture();
        input.narrative_checkpoint = None;
        input.transition_checkpoint.narrative_checkpoint_sequence = None;

        let output = render(input, 65_536).unwrap();

        assert!(output.contains(
            "No narrative checkpoint exists. Objective, decisions, assumptions, and next steps were not checkpointed."
        ));
    }

    #[test]
    fn path_order_does_not_change_the_fingerprint_or_output() {
        let mut left = HandoffInput::fixture();
        left.snapshot.untracked = vec![
            DirtyPath {
                path: PathBuf::from("z-last"),
                sha256: None,
                executable: false,
                symlink_target: None,
            },
            DirtyPath {
                path: PathBuf::from("a-first"),
                sha256: None,
                executable: false,
                symlink_target: None,
            },
        ];
        let mut right = left.clone();
        right.snapshot.untracked.reverse();

        assert_eq!(
            render(left, 65_536).unwrap(),
            render(right, 65_536).unwrap()
        );
    }

    #[test]
    fn sequence_inputs_must_be_sorted_and_unique() {
        let mut input = HandoffInput::fixture();
        input.recent_events = vec![(2, "later".into()), (1, "earlier".into())];
        assert!(render(input, 65_536).is_err());

        let mut input = HandoffInput::fixture();
        input.recent_commands.push(input.recent_commands[0].clone());
        assert!(render(input, 65_536).is_err());

        let mut input = HandoffInput::fixture();
        input.capture_gaps = vec![
            super::CaptureGap {
                sequence: 5,
                phase: "write".into(),
                message: "first".into(),
            },
            super::CaptureGap {
                sequence: 5,
                phase: "write".into(),
                message: "duplicate".into(),
            },
        ];
        assert!(render(input, 65_536).is_err());
    }

    #[test]
    fn omissions_group_only_truly_contiguous_sequences() {
        assert_eq!(render_ranges(&[1, 2, 4, 6, 7]), "1..2, 4, 6..7");
    }

    #[test]
    fn latest_capture_gap_survives_bounded_selection() {
        let mut input = HandoffInput::fixture();
        input.capture_gaps = (1..=100)
            .map(|sequence| super::CaptureGap {
                sequence,
                phase: "journal".into(),
                message: "å".repeat(600),
            })
            .collect();

        let output = render(input, 5_000).unwrap();

        assert!(output.len() <= 5_000);
        assert!(output.contains("- 100 [journal]"));
        assert!(output.contains("Omitted capture-gap event sequences: 1.."));
        assert!(output.contains("bytes omitted"));
    }

    #[test]
    fn failure_excerpt_keeps_utf8_safe_head_and_tail_with_exact_omission() {
        let value = format!("HEAD{}TAIL", "å".repeat(10_000));
        let excerpt = head_tail_excerpt(&value, 2 * 1024, 6 * 1024);

        assert!(excerpt.starts_with("HEAD"));
        assert!(excerpt.ends_with("TAIL"));
        assert!(excerpt.contains("bytes omitted"));
        assert!(std::str::from_utf8(excerpt.as_bytes()).is_ok());
    }

    #[test]
    fn provider_transition_is_provider_neutral_data() {
        let mut input = HandoffInput::fixture();
        input.from_provider = Some(Provider::Codex);
        input.to_provider = Provider::Claude;
        let output = render(input, 65_536).unwrap();
        assert!(output.contains("`codex` → `claude`"));
    }

    #[test]
    fn fork_lineage_keeps_parent_facts_explicitly_scoped() {
        let mut input = HandoffInput::fixture();
        input.session_id =
            crate::model::SessionId::parse("22222222-2222-4222-8222-222222222222").unwrap();
        input.parent_lineage = Some(ParentLineage {
            session_id: crate::model::SessionId::parse("11111111-1111-4111-8111-111111111111")
                .unwrap(),
            transition_sequence: input.transition_sequence,
            narrative_sequence: Some(10),
        });

        let output = render(input, 65_536).unwrap();

        assert!(output.contains("Forked from session"));
        assert!(output.contains("parent event 16"));
        assert!(output.contains("parent event: 18"));
    }

    #[test]
    fn recognized_tests_are_tokenized_without_executing_shell_syntax() {
        for command in [
            "cargo test oauth",
            "pytest -q",
            "python -m pytest tests/oauth.py",
            "go test ./...",
            "npm test -- --runInBand",
            "npm run test -- oauth",
            "pnpm test",
            "yarn test",
            "bun test",
            "rtk cargo test",
            "command pytest",
            "env -i RUST_LOG=debug cargo test",
            "env --ignore-environment command rtk cargo test",
        ] {
            assert!(is_recognized_test_command(command), "missed {command:?}");
        }
        for command in [
            "cargo check",
            "pytestish",
            "env --unset=HOME cargo test",
            "cargo test ; touch sentinel",
            "touch sentinel; cargo test",
            "cargo test | tee output",
            "cargo test > output",
            "cargo $(printf test)",
        ] {
            assert!(
                !is_recognized_test_command(command),
                "misclassified {command:?}"
            );
        }
        let temp = tempfile::TempDir::new().unwrap();
        let sentinel = temp.path().join("sentinel");
        assert!(!is_recognized_test_command(&format!(
            "touch {}; cargo test",
            sentinel.display()
        )));
        assert!(!sentinel.exists());
    }

    #[test]
    fn renderer_reports_the_exact_events_selected_for_the_bounded_copy() {
        let mut input = HandoffInput::fixture();
        input.recent_events = (1..=20)
            .map(|sequence| (sequence, "x".repeat(512)))
            .collect();

        let rendered = render_with_selection(input, 4_096).unwrap();

        assert!(rendered.markdown.len() <= 4_096);
        assert!(
            rendered
                .recent_event_sequences
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
        assert!(rendered.recent_event_sequences.last() == Some(&20));
        assert!(rendered.markdown.contains("Omitted event sequences"));
    }
}
