use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::checkpoint::{
    edit_narrative, load_verified_checkpoint, promote_inbox, read_narrative_json,
    submit_provider_narrative,
};
use crate::cli::{CheckpointFormat, Cli, Command};
use crate::error::{Error, Result, io};
use crate::git::Git;
use crate::handoff::{
    BOOTSTRAP, CaptureGap, CommandFact, HandoffInput, is_recognized_test_command,
    render_with_selection,
};
use crate::model::{
    Checkpoint, CheckpointAuthor, CheckpointKind, ContentRef, Event, EventEnvelope, EventKind,
    GitSnapshot, Provider, RunId, SessionId,
};
use crate::provider::hook::{
    HookEvent, HookOutput, NormalizedHook, capture_failure_output, normalize, session_start_output,
};
use crate::provider::{LaunchContext, adapter};
use crate::runtime::Runtime;
use crate::store::atomic::{create_private, read_private, sync_directory};
use crate::store::blob::BlobStore;
use crate::store::journal::{AppendOutcome, EventJournal, PendingEvent, PendingEventMeta};
use crate::store::lease::{LeaseStore, ProcessIdentity, RunLease, SessionOperationLock, host_name};
use crate::store::refs::read_json;
use crate::store::{Environment, SessionStore, StateLayout};
use crate::supervisor::{ExitFacts, Supervisor};

const MAX_HOOK_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HANDOFF_BYTES: usize = 65_536;

pub fn run(cli: Cli, environment: &Environment, runtime: &dyn Runtime) -> Result<i32> {
    if environment.get("SESH_RUN_ID").is_some() && !provider_command_allowed(&cli.command) {
        return Err(Error::InvalidState(
            "an attached provider may only invoke Sesh hooks or submit provider checkpoints".into(),
        ));
    }
    match cli.command {
        Command::Run {
            provider,
            provider_args,
        } => run_command(provider, provider_args, environment, runtime),
        Command::Switch {
            provider,
            provider_args,
        } => switch_command(provider, provider_args, environment, runtime),
        Command::Checkpoint {
            format,
            from_provider,
        } => {
            let stdin = std::io::stdin();
            let input_is_terminal = stdin.is_terminal();
            checkpoint_command(
                format,
                from_provider,
                environment,
                runtime,
                stdin.lock(),
                input_is_terminal,
            )
        }
        Command::Status { json } => status_command(json, environment),
        Command::Log { from, json } => log_command(from, json, environment),
        Command::Inspect { json } => inspect_command(json, environment),
        Command::Delete { yes } => {
            let stdin = std::io::stdin();
            let input_is_terminal = stdin.is_terminal();
            delete_command(yes, environment, stdin.lock(), input_is_terminal)
        }
        Command::Hook { provider } => {
            let output = ingest_hook(provider, environment, runtime, std::io::stdin().lock())?;
            std::io::stdout()
                .write_all(output.stdout.as_bytes())
                .map_err(|source| io("stdout", source))?;
            std::io::stderr()
                .write_all(output.stderr.as_bytes())
                .map_err(|source| io("stderr", source))?;
            Ok(output.exit_code)
        }
    }
}

fn provider_command_allowed(command: &Command) -> bool {
    matches!(
        command,
        Command::Hook { .. }
            | Command::Checkpoint {
                from_provider: true,
                ..
            }
    )
}

pub fn run_command(
    provider: Provider,
    provider_args: Vec<OsString>,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<i32> {
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &cwd)?;
    let snapshot = Git::new().snapshot(&cwd)?;
    if let Some(existing) = SessionStore::find_for_worktree(&layout, &snapshot.identity)? {
        return Err(Error::InvalidState(format!(
            "worktree already belongs to session {}; use sesh switch",
            existing.id()
        )));
    }
    let store = SessionStore::create(&layout, runtime, snapshot.clone())?;
    let run_id = runtime.run_id();
    let provider_adapter = adapter(provider);

    let operation = SessionOperationLock::acquire(&store.session_dir())?;
    let leases = LeaseStore::new(&store.session_dir());
    if leases.read()?.is_some() {
        return Err(Error::InvalidState(
            "session already has an active or stale provider lease".into(),
        ));
    }
    provider_adapter.setup(&layout.integrations())?;
    let provider_version = provider_adapter.probe()?;
    let run_paths = prepare_run_directory(
        &store,
        &run_id,
        b"# Sesh handoff\n\nThis is the first provider run in this session. Continue from the current Git worktree and user prompt.\n",
        b"",
    )?;
    let hook_bin = std::env::current_exe()
        .map_err(|source| io("current executable", source))?
        .canonicalize()
        .map_err(|source| io("current executable", source))?;
    let args_for_event = provider_args.iter().map(|arg| encode_arg(arg)).collect();
    store.append(
        runtime,
        Some(run_id.clone()),
        Some(provider),
        EventKind::RunStarted {
            cwd: snapshot
                .identity
                .worktree
                .join(&snapshot.identity.cwd_relative)
                .to_str()
                .expect("validated Git identity is UTF-8")
                .to_owned(),
            args: args_for_event,
            supervisor_pid: std::process::id(),
        },
    )?;
    let lease = RunLease::new(
        store.id().clone(),
        run_id.clone(),
        provider,
        ProcessIdentity::capture(std::process::id())?,
    )?;
    leases.create(&lease)?;
    let mut spec = provider_adapter.launch_spec(LaunchContext {
        cwd: &cwd,
        inbox: &run_paths.inbox,
        integration_root: &layout.integrations(),
        hook_bin: &hook_bin,
        provider_args: &provider_args,
        bootstrap: None,
    })?;
    for (key, value) in [
        ("SESH_HOME", layout.root().as_os_str()),
        ("SESH_SESSION_ID", OsStr::new(&store.id().to_string())),
        ("SESH_RUN_ID", OsStr::new(&run_id.to_string())),
        ("SESH_PROVIDER", OsStr::new(provider.executable())),
        ("SESH_PROVIDER_VERSION", OsStr::new(&provider_version)),
        ("SESH_HOOK_BIN", hook_bin.as_os_str()),
        ("SESH_HANDOFF_PATH", run_paths.handoff.as_os_str()),
        ("SESH_CHECKPOINT_INBOX", run_paths.checkpoints.as_os_str()),
    ] {
        spec.env.insert(OsString::from(key), value.to_owned());
    }
    drop(operation);

    let supervised = Supervisor::launch(spec, &store, &run_id, Duration::from_secs(60));
    let operation = SessionOperationLock::acquire(&store.session_dir())?;
    let (facts, supervision_error) = match supervised {
        Ok(outcome) => (outcome.facts.clone(), outcome.startup_failure.clone()),
        Err(error) => (
            ExitFacts {
                exit_code: None,
                signal: None,
            },
            Some(error.to_string()),
        ),
    };
    promote_inbox(&store, runtime, &run_id, provider, &run_paths.checkpoints)?;
    store.append(
        runtime,
        Some(run_id.clone()),
        Some(provider),
        EventKind::RunStopped {
            exit_code: facts.exit_code,
            signal: facts.signal,
        },
    )?;
    let post = Git::new().snapshot(&cwd)?;
    store.append(
        runtime,
        Some(run_id.clone()),
        Some(provider),
        EventKind::GitSnapshot { snapshot: post },
    )?;
    leases.clear(&run_id)?;
    drop(operation);

    if let Some(message) = supervision_error {
        return Err(Error::Command(message));
    }
    Ok(facts
        .exit_code
        .unwrap_or_else(|| 128 + facts.signal.unwrap_or(1)))
}

pub fn switch_command(
    provider: Provider,
    provider_args: Vec<OsString>,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<i32> {
    let invocation_cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &invocation_cwd)?;
    let invocation_snapshot = Git::new().snapshot(&invocation_cwd)?;
    let store = SessionStore::find_for_worktree(&layout, &invocation_snapshot.identity)?
        .ok_or_else(|| Error::InvalidState("this worktree has no Sesh session".into()))?;
    let operation = SessionOperationLock::acquire(&store.session_dir())?;
    let leases = LeaseStore::new(&store.session_dir());
    recover_stale_lease(&store, &leases, runtime)?;

    let saved_cwd_relative = store.saved_cwd_relative()?;
    let saved_cwd_path = store.meta().worktree.worktree.join(&saved_cwd_relative);
    let saved_cwd = saved_cwd_path
        .canonicalize()
        .map_err(|source| io(&saved_cwd_path, source))?;
    if !saved_cwd.is_dir() || !saved_cwd.starts_with(&store.meta().worktree.worktree) {
        return Err(Error::InvalidState(format!(
            "saved cwd {} is not an existing directory in the session worktree",
            saved_cwd_path.display()
        )));
    }
    let switch_snapshot = Git::new().snapshot(&saved_cwd)?;
    verify_switch_snapshot(
        &invocation_snapshot,
        &switch_snapshot,
        &store.meta().worktree,
        &saved_cwd_relative,
    )?;
    store.append(
        runtime,
        None,
        None,
        EventKind::GitSnapshot {
            snapshot: switch_snapshot.clone(),
        },
    )?;

    let previous_provider = previous_provider(&store.events()?)?;
    store.append(
        runtime,
        None,
        None,
        EventKind::SwitchRequested {
            from: previous_provider,
            to: provider,
        },
    )?;
    let events_before_transition = store.events()?;
    let narrative_checkpoint = latest_narrative_checkpoint(&store, &events_before_transition)?;
    let narrative_sequence = narrative_checkpoint.as_ref().map(|item| item.0);
    let (transition_event, transition_stored) =
        store.create_transition_checkpoint(runtime, None, None, narrative_sequence)?;

    let envelopes = EventJournal::new(&store.session_dir(), store.id().clone()).read_repair()?;
    let recent_boundary = narrative_sequence.unwrap_or(0);
    let mut recent_events = Vec::new();
    for envelope in envelopes
        .iter()
        .filter(|item| item.event.sequence > recent_boundary)
    {
        let mut line = envelope.line()?;
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        recent_events.push((
            envelope.event.sequence,
            String::from_utf8(line).map_err(|_| {
                Error::InvalidState("canonical event envelope is not valid UTF-8".into())
            })?,
        ));
    }
    let events: Vec<_> = envelopes.iter().map(|item| item.event.clone()).collect();
    let (recent_commands, latest_test, latest_failure, capture_gaps) =
        command_facts(&store, &events)?;
    let rendered = render_with_selection(
        HandoffInput {
            session_id: store.id().clone(),
            from_provider: previous_provider,
            to_provider: provider,
            transition_sequence: transition_event.sequence,
            transition_checkpoint: transition_stored.checkpoint,
            narrative_checkpoint,
            snapshot: switch_snapshot.clone(),
            recent_events,
            recent_commands,
            latest_test,
            latest_failure,
            capture_gaps,
        },
        MAX_HANDOFF_BYTES,
    )?;
    let recent_events_jsonl = selected_event_lines(&envelopes, &rendered.recent_event_sequences)?;
    if recent_events_jsonl.len() > MAX_HANDOFF_BYTES {
        return Err(Error::InvalidState(
            "selected recent events exceed 64 KiB".into(),
        ));
    }

    let run_id = runtime.run_id();
    let run_paths = prepare_run_directory(
        &store,
        &run_id,
        rendered.markdown.as_bytes(),
        &recent_events_jsonl,
    )?;
    let provider_adapter = adapter(provider);
    provider_adapter.setup(&layout.integrations())?;
    let provider_version = provider_adapter.probe()?;
    let hook_bin = std::env::current_exe()
        .map_err(|source| io("current executable", source))?
        .canonicalize()
        .map_err(|source| io("current executable", source))?;
    store.append(
        runtime,
        Some(run_id.clone()),
        Some(provider),
        EventKind::RunStarted {
            cwd: saved_cwd
                .to_str()
                .ok_or_else(|| Error::InvalidState("saved cwd must be valid UTF-8".into()))?
                .to_owned(),
            args: provider_args.iter().map(|arg| encode_arg(arg)).collect(),
            supervisor_pid: std::process::id(),
        },
    )?;
    let lease = RunLease::new(
        store.id().clone(),
        run_id.clone(),
        provider,
        ProcessIdentity::capture(std::process::id())?,
    )?;
    leases.create(&lease)?;
    let mut spec = provider_adapter.launch_spec(LaunchContext {
        cwd: &saved_cwd,
        inbox: &run_paths.inbox,
        integration_root: &layout.integrations(),
        hook_bin: &hook_bin,
        provider_args: &provider_args,
        bootstrap: Some(BOOTSTRAP),
    })?;
    add_run_environment(
        &mut spec.env,
        &layout,
        &store,
        &run_id,
        provider,
        &provider_version,
        &hook_bin,
        &run_paths,
    );
    drop(operation);

    let supervised = Supervisor::launch(spec, &store, &run_id, Duration::from_secs(60));
    let operation = SessionOperationLock::acquire(&store.session_dir())?;
    let (facts, supervision_error) = match supervised {
        Ok(outcome) => (outcome.facts.clone(), outcome.startup_failure.clone()),
        Err(error) => (
            ExitFacts {
                exit_code: None,
                signal: None,
            },
            Some(error.to_string()),
        ),
    };
    promote_inbox(&store, runtime, &run_id, provider, &run_paths.checkpoints)?;
    store.append(
        runtime,
        Some(run_id.clone()),
        Some(provider),
        EventKind::RunStopped {
            exit_code: facts.exit_code,
            signal: facts.signal,
        },
    )?;
    let post = Git::new().snapshot(&saved_cwd)?;
    store.append(
        runtime,
        Some(run_id.clone()),
        Some(provider),
        EventKind::GitSnapshot { snapshot: post },
    )?;
    leases.clear(&run_id)?;
    drop(operation);

    if let Some(message) = supervision_error {
        return Err(Error::Command(message));
    }
    Ok(facts
        .exit_code
        .unwrap_or_else(|| 128 + facts.signal.unwrap_or(1)))
}

fn recover_stale_lease(
    store: &SessionStore,
    leases: &LeaseStore,
    runtime: &dyn Runtime,
) -> Result<()> {
    let Some(lease) = leases.read()? else {
        return Ok(());
    };
    if lease.host != host_name()? {
        return Err(Error::InvalidState(format!(
            "session lease {} belongs to host {}; explicit recovery is required",
            lease.run_id, lease.host
        )));
    }
    let supervisor_live = lease.supervisor.is_live()?;
    let child_live = lease
        .child
        .as_ref()
        .map(ProcessIdentity::is_live)
        .transpose()?
        .unwrap_or(false);
    if supervisor_live || child_live {
        return Err(Error::InvalidState(format!(
            "session already has active provider {}",
            lease.run_id
        )));
    }
    store.append(
        runtime,
        Some(lease.run_id.clone()),
        Some(lease.provider),
        EventKind::RunRecovered {
            reason: "same-host supervisor and child processes are no longer live".into(),
        },
    )?;
    leases.clear(&lease.run_id)
}

fn verify_switch_snapshot(
    invocation: &GitSnapshot,
    saved: &GitSnapshot,
    expected: &crate::model::WorktreeIdentity,
    saved_cwd_relative: &Path,
) -> Result<()> {
    let same_facts = invocation.identity.same_worktree_as(expected)
        && saved.identity.same_worktree_as(expected)
        && saved.identity.cwd_relative == saved_cwd_relative
        && invocation.branch == saved.branch
        && invocation.head == saved.head
        && invocation.staged == saved.staged
        && invocation.unstaged == saved.unstaged
        && invocation.untracked == saved.untracked
        && invocation.dirty_submodules == saved.dirty_submodules;
    if !same_facts {
        return Err(Error::InvalidState(
            "switch observations do not describe one unchanged source worktree".into(),
        ));
    }
    Ok(())
}

fn previous_provider(events: &[Event]) -> Result<Option<Provider>> {
    let Some(event) = events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, EventKind::RunStarted { .. }))
    else {
        return Ok(None);
    };
    event.provider.map(Some).ok_or_else(|| {
        Error::InvalidState(format!(
            "run.started event {} has no provider",
            event.sequence
        ))
    })
}

fn latest_narrative_checkpoint(
    store: &SessionStore,
    events: &[Event],
) -> Result<Option<(u64, Checkpoint)>> {
    let reference = store.session_dir().join("refs/latest-narrative-checkpoint");
    let newest_narrative = events.iter().rev().find_map(|event| {
        matches!(
            event.kind,
            EventKind::CheckpointCreated {
                checkpoint_kind: CheckpointKind::Narrative,
                ..
            }
        )
        .then_some(event.sequence)
    });
    let sequence = match std::fs::symlink_metadata(&reference) {
        Ok(_) => read_json::<u64>(&reference)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return if newest_narrative.is_none() {
                Ok(None)
            } else {
                Err(Error::InvalidState(
                    "latest narrative checkpoint ref is missing".into(),
                ))
            };
        }
        Err(source) => return Err(io(&reference, source)),
    };
    if newest_narrative != Some(sequence) {
        return Err(Error::InvalidState(format!(
            "latest narrative checkpoint ref is stale; expected {newest_narrative:?}, found {sequence}"
        )));
    }
    let expected_path = format!("checkpoints/{sequence:012}.json");
    let event = events
        .iter()
        .find(|event| event.sequence == sequence)
        .ok_or_else(|| {
            Error::InvalidState(format!(
                "latest narrative checkpoint ref names missing event {sequence}"
            ))
        })?;
    match &event.kind {
        EventKind::CheckpointCreated {
            checkpoint_kind: CheckpointKind::Narrative,
            through_sequence,
            path,
        } if *through_sequence == sequence.saturating_sub(1) && path == &expected_path => {}
        _ => {
            return Err(Error::InvalidState(format!(
                "latest narrative checkpoint ref does not name a matching event {sequence}"
            )));
        }
    }
    let checkpoint = load_verified_checkpoint(&store.session_dir(), sequence)?;
    if checkpoint.checkpoint_kind != CheckpointKind::Narrative {
        return Err(Error::InvalidState(format!(
            "latest narrative checkpoint {sequence} has the wrong kind"
        )));
    }
    Ok(Some((sequence, checkpoint)))
}

type HandoffFacts = (
    Vec<CommandFact>,
    Option<CommandFact>,
    Option<CommandFact>,
    Vec<CaptureGap>,
);

fn command_facts(store: &SessionStore, events: &[Event]) -> Result<HandoffFacts> {
    let blobs = BlobStore::new(&store.session_dir());
    let mut requests = BTreeMap::new();
    let mut commands = Vec::new();
    let mut latest_test = None;
    let mut latest_failure = None;
    let mut gaps = Vec::new();
    for event in events {
        match &event.kind {
            EventKind::ProviderToolRequested {
                tool_use_id,
                command: Some(command),
                ..
            } => {
                requests.insert(tool_use_id.clone(), command.clone());
            }
            EventKind::ProviderToolCompleted {
                tool_name,
                tool_use_id,
                response,
                stdout,
                stderr,
                exit_code,
                ..
            } => {
                let command = requests.get(tool_use_id).cloned().unwrap_or_else(|| {
                    format!("{tool_name} tool {tool_use_id} (command unavailable)")
                });
                let opaque_response = resolve_content(&blobs, response.as_ref())?;
                let stdout = match (resolve_content(&blobs, stdout.as_ref())?, &opaque_response) {
                    (Some(stdout), _) => Some(stdout),
                    (None, Some(response)) => Some(format!("provider response:\n{response}")),
                    (None, None) => None,
                };
                let fact = CommandFact {
                    sequence: event.sequence,
                    command,
                    exit_code: *exit_code,
                    stdout,
                    stderr: resolve_content(&blobs, stderr.as_ref())?,
                };
                if is_recognized_test_command(&fact.command) {
                    latest_test = Some(fact.clone());
                }
                if fact.exit_code.is_some_and(|code| code != 0) {
                    latest_failure = Some(fact.clone());
                }
                if exit_code.is_none() {
                    gaps.push(CaptureGap {
                        sequence: event.sequence,
                        phase: "provider.tool.completed".into(),
                        message:
                            "provider response had no structured exit code; status remains unknown"
                                .into(),
                    });
                }
                commands.push(fact);
            }
            EventKind::ProviderToolFailed {
                tool_name,
                tool_use_id,
                error,
            } => {
                let fact = CommandFact {
                    sequence: event.sequence,
                    command: requests.get(tool_use_id).cloned().unwrap_or_else(|| {
                        format!("{tool_name} tool {tool_use_id} (command unavailable)")
                    }),
                    exit_code: None,
                    stdout: None,
                    stderr: Some(error.clone()),
                };
                if is_recognized_test_command(&fact.command) {
                    latest_test = Some(fact.clone());
                }
                latest_failure = Some(fact.clone());
                gaps.push(CaptureGap {
                    sequence: event.sequence,
                    phase: "provider.tool.failed".into(),
                    message: "provider reported a tool failure without a structured exit code"
                        .into(),
                });
                commands.push(fact);
            }
            EventKind::CaptureFailed { phase, message } => gaps.push(CaptureGap {
                sequence: event.sequence,
                phase: phase.clone(),
                message: message.clone(),
            }),
            _ => {}
        }
    }
    Ok((commands, latest_test, latest_failure, gaps))
}

fn resolve_content(blobs: &BlobStore, content: Option<&ContentRef>) -> Result<Option<String>> {
    content
        .map(|content| {
            String::from_utf8(blobs.resolve(content)?)
                .map_err(|_| Error::InvalidState("captured provider text is not UTF-8".into()))
        })
        .transpose()
}

fn selected_event_lines(envelopes: &[EventEnvelope], sequences: &[u64]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut selected = sequences.iter().copied().peekable();
    for envelope in envelopes {
        if selected.peek() == Some(&envelope.event.sequence) {
            output.extend(envelope.line()?);
            selected.next();
        }
    }
    if selected.next().is_some() {
        return Err(Error::InvalidState(
            "handoff selected an event absent from the committed journal".into(),
        ));
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn add_run_environment(
    target: &mut BTreeMap<OsString, OsString>,
    layout: &StateLayout,
    store: &SessionStore,
    run_id: &RunId,
    provider: Provider,
    provider_version: &str,
    hook_bin: &Path,
    run_paths: &RunPaths,
) {
    for (key, value) in [
        ("SESH_HOME", layout.root().as_os_str()),
        ("SESH_SESSION_ID", OsStr::new(&store.id().to_string())),
        ("SESH_RUN_ID", OsStr::new(&run_id.to_string())),
        ("SESH_PROVIDER", OsStr::new(provider.executable())),
        ("SESH_PROVIDER_VERSION", OsStr::new(provider_version)),
        ("SESH_HOOK_BIN", hook_bin.as_os_str()),
        ("SESH_HANDOFF_PATH", run_paths.handoff.as_os_str()),
        ("SESH_CHECKPOINT_INBOX", run_paths.checkpoints.as_os_str()),
    ] {
        target.insert(OsString::from(key), value.to_owned());
    }
}

fn status_command(json: bool, environment: &Environment) -> Result<i32> {
    let (_layout, snapshot, store) = current_session(environment)?;
    let events = store.events()?;
    let provider = previous_provider(&events)?;
    let saved_cwd_path = store
        .meta()
        .worktree
        .worktree
        .join(store.saved_cwd_relative()?);
    let saved_cwd = saved_cwd_path
        .canonicalize()
        .map_err(|source| io(&saved_cwd_path, source))?;
    let (_, _, _, gaps) = command_facts(&store, &events)?;
    let value = serde_json::json!({
        "schema_version": 1,
        "session_id": store.id(),
        "provider": provider,
        "worktree": snapshot.identity.worktree,
        "branch": snapshot.branch,
        "head": snapshot.head,
        "cwd": saved_cwd,
        "dirty": {
            "staged": snapshot.staged,
            "unstaged": snapshot.unstaged,
            "untracked": snapshot.untracked,
            "dirty_submodules": snapshot.dirty_submodules,
        },
        "latest_checkpoint": latest_checkpoint_value(&store, &events)?,
        "capture_gaps": gaps.into_iter().map(|gap| serde_json::json!({
            "sequence": gap.sequence,
            "phase": gap.phase,
            "message": gap.message,
        })).collect::<Vec<_>>(),
    });
    write_projection(&value, json)?;
    Ok(0)
}

fn log_command(from: Option<u64>, json: bool, environment: &Environment) -> Result<i32> {
    let (_layout, _snapshot, store) = current_session(environment)?;
    let mut stdout = std::io::stdout().lock();
    for envelope in store
        .envelopes()?
        .into_iter()
        .filter(|envelope| from.is_none_or(|sequence| envelope.event.sequence >= sequence))
    {
        if json {
            stdout
                .write_all(&envelope.line()?)
                .map_err(|source| io("stdout", source))?;
        } else {
            let event_value = serde_json::to_value(&envelope.event).map_err(|error| {
                Error::InvalidState(format!("cannot encode event projection: {error}"))
            })?;
            let kind = event_value["type"]
                .as_str()
                .ok_or_else(|| Error::InvalidState("event projection has no type field".into()))?;
            writeln!(stdout, "{} {kind}", envelope.event.sequence)
                .map_err(|source| io("stdout", source))?;
        }
    }
    Ok(0)
}

fn inspect_command(json: bool, environment: &Environment) -> Result<i32> {
    let (layout, _snapshot, store) = current_session(environment)?;
    let envelopes = store.envelopes()?;
    let mut checkpoint_files = Vec::new();
    let checkpoints = store.session_dir().join("checkpoints");
    for entry in std::fs::read_dir(&checkpoints).map_err(|source| io(&checkpoints, source))? {
        let path = entry.map_err(|source| io(&checkpoints, source))?.path();
        checkpoint_files.push(private_file_value(&path)?);
    }
    checkpoint_files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));

    let mut blob_references = BTreeMap::new();
    let blobs = BlobStore::new(&store.session_dir());
    for envelope in &envelopes {
        match &envelope.event.kind {
            EventKind::ProviderPromptSubmitted { prompt } => {
                collect_blob_reference(prompt, &mut blob_references, &blobs)?;
            }
            EventKind::ProviderToolCompleted {
                response,
                stdout,
                stderr,
                ..
            } => {
                for content in [response, stdout, stderr].into_iter().flatten() {
                    collect_blob_reference(content, &mut blob_references, &blobs)?;
                }
            }
            _ => {}
        }
    }
    let mut permissions = Vec::new();
    for path in [
        layout.root().to_path_buf(),
        store.session_dir(),
        store.session_dir().join("events.jsonl"),
        store.session_dir().join("lock"),
        store.session_dir().join("refs"),
        store.session_dir().join("checkpoints"),
        store.session_dir().join("blobs"),
        store.session_dir().join("runs"),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => permissions.push(permission_value(&path)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io(&path, source)),
        }
    }
    let active_lease = LeaseStore::new(&store.session_dir()).read()?;
    let value = serde_json::json!({
        "schema_version": 1,
        "state_root": layout.root(),
        "session_dir": store.session_dir(),
        "event_count": envelopes.len(),
        "checkpoint_files": checkpoint_files,
        "blob_references": blob_references.into_iter().map(|(sha256, bytes)| {
            serde_json::json!({"sha256": sha256, "bytes": bytes})
        }).collect::<Vec<_>>(),
        "permissions": permissions,
        "active_lease": active_lease,
    });
    write_projection(&value, json)?;
    Ok(0)
}

fn delete_command(
    yes: bool,
    environment: &Environment,
    mut input: impl Read,
    input_is_terminal: bool,
) -> Result<i32> {
    let (layout, _snapshot, store) = current_session(environment)?;
    let operation = SessionOperationLock::acquire(&store.session_dir())?;
    if let Some(lease) = LeaseStore::new(&store.session_dir()).read()? {
        if lease.host != host_name()? {
            return Err(Error::InvalidState(format!(
                "session lease {} belongs to host {}; explicit recovery is required",
                lease.run_id, lease.host
            )));
        }
        let supervisor_live = lease.supervisor.is_live()?;
        let child_live = lease
            .child
            .as_ref()
            .map(ProcessIdentity::is_live)
            .transpose()?
            .unwrap_or(false);
        if supervisor_live || child_live {
            return Err(Error::InvalidState(format!(
                "cannot delete session with live run {}",
                lease.run_id
            )));
        }
    }
    if !yes {
        if !input_is_terminal {
            return Err(Error::InvalidState(
                "deletion requires a terminal confirmation or `sesh delete --yes`".into(),
            ));
        }
        let short_id = store.id().to_string()[..8].to_owned();
        eprintln!("Type `delete session {short_id}` to confirm:");
        let mut bytes = Vec::new();
        input
            .by_ref()
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(|source| io("stdin", source))?;
        if bytes.len() > 4096 {
            return Err(Error::InvalidState(
                "deletion confirmation is too long".into(),
            ));
        }
        let confirmation = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidState("deletion confirmation is not UTF-8".into()))?
            .trim();
        if confirmation != format!("delete session {short_id}") {
            return Err(Error::InvalidState(
                "deletion confirmation did not match".into(),
            ));
        }
    }

    let session_dir = store.session_dir();
    validate_owned_private_directory(&session_dir)?;
    let deleting = layout.root().join(format!(".deleting-{}", store.id()));
    match std::fs::symlink_metadata(&deleting) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(io(&deleting, source)),
        Ok(_) => {
            return Err(Error::InvalidState(format!(
                "deletion staging path {} already exists",
                deleting.display()
            )));
        }
    }
    std::fs::rename(&session_dir, &deleting).map_err(|source| io(&deleting, source))?;
    sync_directory(&layout.sessions())?;
    sync_directory(layout.root())?;
    if let Err(error) = store.remove_binding() {
        let rollback = std::fs::rename(&deleting, &session_dir)
            .map_err(|source| io(&session_dir, source))
            .and_then(|()| sync_directory(&layout.sessions()));
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(Error::InvalidState(format!(
                "cannot remove worktree binding ({error}); rollback also failed ({rollback_error})"
            ))),
        };
    }
    remove_tree_without_following(&deleting)?;
    sync_directory(layout.root())?;
    drop(operation);
    Ok(0)
}

fn current_session(environment: &Environment) -> Result<(StateLayout, GitSnapshot, SessionStore)> {
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &cwd)?;
    let snapshot = Git::new().snapshot(&cwd)?;
    let store = SessionStore::find_for_worktree(&layout, &snapshot.identity)?
        .ok_or_else(|| Error::InvalidState("this worktree has no Sesh session".into()))?;
    Ok((layout, snapshot, store))
}

fn latest_checkpoint_value(store: &SessionStore, events: &[Event]) -> Result<serde_json::Value> {
    let newest = events.iter().rev().find_map(|event| {
        matches!(event.kind, EventKind::CheckpointCreated { .. }).then_some(event.sequence)
    });
    let reference = store.session_dir().join("refs/latest-checkpoint");
    let sequence = match std::fs::symlink_metadata(&reference) {
        Ok(_) => read_json::<u64>(&reference)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && newest.is_none() => {
            return Ok(serde_json::Value::Null);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::InvalidState(
                "latest checkpoint ref is missing".into(),
            ));
        }
        Err(source) => return Err(io(&reference, source)),
    };
    if newest != Some(sequence) {
        return Err(Error::InvalidState(format!(
            "latest checkpoint ref is stale; expected {newest:?}, found {sequence}"
        )));
    }
    let event = events
        .iter()
        .find(|event| event.sequence == sequence)
        .ok_or_else(|| Error::InvalidState("latest checkpoint event is missing".into()))?;
    let checkpoint = load_verified_checkpoint(&store.session_dir(), sequence)?;
    let expected_path = format!("checkpoints/{sequence:012}.json");
    match &event.kind {
        EventKind::CheckpointCreated {
            checkpoint_kind,
            through_sequence,
            path,
        } if checkpoint_kind == &checkpoint.checkpoint_kind
            && *through_sequence == checkpoint.through_sequence
            && path == &expected_path => {}
        _ => {
            return Err(Error::InvalidState(
                "latest checkpoint event and artifacts do not match".into(),
            ));
        }
    }
    Ok(serde_json::json!({
        "sequence": sequence,
        "kind": checkpoint.checkpoint_kind,
        "through_sequence": checkpoint.through_sequence,
        "path": expected_path,
    }))
}

fn collect_blob_reference(
    content: &ContentRef,
    references: &mut BTreeMap<String, usize>,
    blobs: &BlobStore,
) -> Result<()> {
    if let ContentRef::Blob { sha256, bytes } = content {
        blobs.resolve(content)?;
        if let Some(previous) = references.insert(sha256.clone(), *bytes) {
            if previous != *bytes {
                return Err(Error::InvalidState(format!(
                    "blob {sha256} has conflicting recorded sizes"
                )));
            }
        }
    }
    Ok(())
}

fn private_file_value(path: &Path) -> Result<serde_json::Value> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io(path, source))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != effective_uid
        || mode != 0o600
    {
        return Err(Error::InvalidState(format!(
            "refusing insecure private file {}",
            path.display()
        )));
    }
    Ok(serde_json::json!({
        "path": path,
        "bytes": metadata.len(),
        "mode": mode,
        "uid": metadata.uid(),
    }))
}

fn permission_value(path: &Path) -> Result<serde_json::Value> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io(path, source))?;
    if metadata.file_type().is_symlink() {
        return Err(Error::InvalidState(format!(
            "refusing symlinked state path {}",
            path.display()
        )));
    }
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(Error::InvalidState(format!(
            "state path {} has unexpected owner {}",
            path.display(),
            metadata.uid()
        )));
    }
    Ok(serde_json::json!({
        "path": path,
        "kind": if metadata.is_dir() { "directory" } else { "file" },
        "mode": metadata.permissions().mode() & 0o777,
        "uid": metadata.uid(),
    }))
}

fn validate_owned_private_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io(path, source))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(Error::InvalidState(format!(
            "refusing insecure session directory {}",
            path.display()
        )));
    }
    Ok(())
}

fn remove_tree_without_following(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io(path, source))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        for entry in std::fs::read_dir(path).map_err(|source| io(path, source))? {
            remove_tree_without_following(&entry.map_err(|source| io(path, source))?.path())?;
        }
        std::fs::remove_dir(path).map_err(|source| io(path, source))
    } else {
        std::fs::remove_file(path).map_err(|source| io(path, source))
    }
}

fn write_projection(value: &serde_json::Value, compact: bool) -> Result<()> {
    let mut bytes = if compact {
        serde_json::to_vec(value)
    } else {
        serde_json::to_vec_pretty(value)
    }
    .map_err(|error| Error::InvalidState(format!("cannot encode JSON projection: {error}")))?;
    bytes.push(b'\n');
    std::io::stdout()
        .write_all(&bytes)
        .map_err(|source| io("stdout", source))
}

fn checkpoint_command(
    _format: CheckpointFormat,
    from_provider: bool,
    environment: &Environment,
    runtime: &dyn Runtime,
    input: impl Read,
    input_is_terminal: bool,
) -> Result<i32> {
    if from_provider {
        let narrative = read_narrative_json(input)?;
        submit_provider_narrative(environment, &narrative)?;
        return Ok(0);
    }
    if environment.get("SESH_RUN_ID").is_some() {
        return Err(Error::InvalidState(
            "an attached provider must use `sesh checkpoint --format json --from-provider`".into(),
        ));
    }
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &cwd)?;
    let snapshot = Git::new().snapshot(&cwd)?;
    let store = SessionStore::find_for_worktree(&layout, &snapshot.identity)?
        .ok_or_else(|| Error::InvalidState("this worktree has no Sesh session".into()))?;
    let _operation = SessionOperationLock::acquire(&store.session_dir())?;
    let narrative = if input_is_terminal {
        edit_narrative(layout.root(), environment)?
    } else {
        read_narrative_json(input)?
    };
    store.create_narrative_checkpoint(runtime, None, None, CheckpointAuthor::Human, narrative)?;
    Ok(0)
}

pub fn ingest_hook(
    provider: Provider,
    environment: &Environment,
    runtime: &dyn Runtime,
    input: impl Read,
) -> Result<HookOutput> {
    let mut bytes = Vec::new();
    input
        .take(MAX_HOOK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io("hook stdin", source))?;
    let event_name = recover_event_name(&bytes);
    match ingest_hook_inner(provider, environment, runtime, &bytes) {
        Ok(output) => Ok(output),
        Err(error) => {
            let _ = record_capture_failure(environment, runtime, &error.to_string());
            match event_name {
                Some(event) => Ok(capture_failure_output(provider, &event, &error.to_string())),
                None => Err(error),
            }
        }
    }
}

fn ingest_hook_inner(
    provider: Provider,
    environment: &Environment,
    runtime: &dyn Runtime,
    bytes: &[u8],
) -> Result<HookOutput> {
    if bytes.len() as u64 > MAX_HOOK_BYTES {
        return Err(Error::InvalidState(
            "provider hook input exceeds 8 MiB".into(),
        ));
    }
    let normalized = normalize(provider, bytes)?;
    let cwd = normalized
        .cwd
        .canonicalize()
        .map_err(|source| io(&normalized.cwd, source))?;
    if !cwd.is_dir() {
        return Err(Error::InvalidState(
            "provider hook cwd is not a directory".into(),
        ));
    }
    let process_cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &process_cwd)?;
    let session_id = SessionId::parse(required_env_utf8(environment, "SESH_SESSION_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid SESH_SESSION_ID: {error}")))?;
    let run_id = RunId::parse(required_env_utf8(environment, "SESH_RUN_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid SESH_RUN_ID: {error}")))?;
    let store = SessionStore::open(&layout, session_id)?;
    let lease = LeaseStore::new(&store.session_dir())
        .read()?
        .ok_or_else(|| Error::InvalidState("provider hook has no active run lease".into()))?;
    if lease.session_id != *store.id() || lease.run_id != run_id || lease.provider != provider {
        return Err(Error::InvalidState(
            "provider hook does not match the active run lease".into(),
        ));
    }
    let checkpoint_inbox = store
        .session_dir()
        .join(format!("runs/{run_id}/inbox/checkpoints"));
    let checkpoint_operation = SessionOperationLock::acquire(&store.session_dir())?;
    promote_inbox(&store, runtime, &run_id, provider, &checkpoint_inbox)?;
    drop(checkpoint_operation);
    let cwd_relative = cwd
        .strip_prefix(&store.meta().worktree.worktree)
        .map_err(|_| Error::InvalidState("provider hook cwd is outside the bound worktree".into()))?
        .to_path_buf();
    if matches!(
        normalized.event_name.as_str(),
        "UserPromptSubmit" | "PreToolUse"
    ) && capture_failure_exists(&store, &run_id)?
    {
        return Err(Error::InvalidState(
            "a previous capture failure requires sesh doctor --repair".into(),
        ));
    }
    if matches!(normalized.event, HookEvent::SessionStarted { .. }) {
        if cwd_relative != store.saved_cwd_relative()? {
            return Err(Error::InvalidState(
                "SessionStart cwd does not match the launched cwd".into(),
            ));
        }
    } else {
        let handshook = store.events()?.iter().any(|event| {
            event.run_id.as_ref() == Some(&run_id)
                && matches!(event.kind, EventKind::RunHandshake { .. })
        });
        if !handshook {
            return Err(Error::InvalidState(
                "provider hook arrived before SessionStart handshake".into(),
            ));
        }
        append_cwd_change(&store, runtime, &run_id, provider, &cwd_relative)?;
    }
    let blobs = BlobStore::new(&store.session_dir());
    ensure_outcome_request(&store, runtime, &run_id, provider, &normalized.event)?;
    let provider_version = environment
        .get("SESH_PROVIDER_VERSION")
        .and_then(OsStr::to_str)
        .map(str::to_owned);
    let (outcome, follow_snapshot, handoff) = map_and_append_hook(
        &store,
        runtime,
        &run_id,
        provider,
        normalized,
        &blobs,
        provider_version,
    )?;
    if matches!(outcome, AppendOutcome::Appended(_)) && follow_snapshot {
        let snapshot = Git::new().snapshot(&cwd)?;
        store.append(
            runtime,
            Some(run_id.clone()),
            Some(provider),
            EventKind::GitSnapshot { snapshot },
        )?;
    }
    if handoff {
        let path = store
            .session_dir()
            .join(format!("runs/{run_id}/inbox/handoff.md"));
        let bytes = read_private(&path)?;
        if bytes.len() > MAX_HANDOFF_BYTES {
            return Err(Error::InvalidState("handoff exceeds 64 KiB".into()));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidState("handoff is not valid UTF-8".into()))?;
        return Ok(session_start_output(text));
    }
    Ok(HookOutput {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
    })
}

fn map_and_append_hook(
    store: &SessionStore,
    runtime: &dyn Runtime,
    run_id: &RunId,
    provider: Provider,
    normalized: NormalizedHook,
    blobs: &BlobStore,
    provider_version: Option<String>,
) -> Result<(AppendOutcome, bool, bool)> {
    let (key, kind, snapshot, handoff) = match normalized.event {
        HookEvent::SessionStarted { native_session_id } => (
            Some(format!("handshake:{native_session_id}")),
            EventKind::RunHandshake {
                native_session_id,
                provider_version,
            },
            false,
            true,
        ),
        HookEvent::UserPromptSubmitted { prompt, .. } => (
            None,
            EventKind::ProviderPromptSubmitted {
                prompt: blobs.put(prompt.as_bytes())?,
            },
            false,
            false,
        ),
        HookEvent::ToolRequested {
            tool_name,
            tool_use_id,
            command,
            file_path,
            ..
        } => (
            Some(format!("pre:{tool_use_id}")),
            EventKind::ProviderToolRequested {
                tool_name,
                tool_use_id,
                command,
                file_path,
            },
            false,
            false,
        ),
        HookEvent::ToolCompleted {
            tool_name,
            tool_use_id,
            command: _,
            file_path: _,
            response,
            stdout,
            stderr,
            exit_code,
            duration_ms,
            ..
        } => (
            Some(format!("post:{tool_use_id}")),
            EventKind::ProviderToolCompleted {
                tool_name,
                tool_use_id,
                response: put_optional(blobs, response)?,
                stdout: put_optional(blobs, stdout)?,
                stderr: put_optional(blobs, stderr)?,
                exit_code,
                duration_ms,
            },
            true,
            false,
        ),
        HookEvent::ToolFailed {
            tool_name,
            tool_use_id,
            error,
            ..
        } => (
            Some(format!("post:{tool_use_id}")),
            EventKind::ProviderToolFailed {
                tool_name,
                tool_use_id,
                error,
            },
            true,
            false,
        ),
        HookEvent::Stopped { native_session_id } => (
            Some(format!("stop:{native_session_id}")),
            EventKind::ProviderStopObserved { native_session_id },
            true,
            false,
        ),
    };
    let now = runtime.now()?;
    let pending = PendingEvent {
        occurred_at: now.clone(),
        recorded_at: now,
        run_id: Some(run_id.clone()),
        provider: Some(provider),
        idempotency_key: key.clone(),
        kind,
    };
    let outcome = if key.is_some() {
        EventJournal::new(&store.session_dir(), store.id().clone()).append_idempotent(pending)?
    } else {
        AppendOutcome::Appended(
            EventJournal::new(&store.session_dir(), store.id().clone()).append(pending)?,
        )
    };
    Ok((outcome, snapshot, handoff))
}

fn ensure_outcome_request(
    store: &SessionStore,
    runtime: &dyn Runtime,
    run_id: &RunId,
    provider: Provider,
    event: &HookEvent,
) -> Result<()> {
    let HookEvent::ToolCompleted {
        tool_name,
        tool_use_id,
        command,
        file_path,
        ..
    } = event
    else {
        return Ok(());
    };
    if command.is_none() && file_path.is_none() {
        return Ok(());
    }
    let now = runtime.now()?;
    EventJournal::new(&store.session_dir(), store.id().clone()).append_idempotent(
        PendingEvent {
            occurred_at: now.clone(),
            recorded_at: now,
            run_id: Some(run_id.clone()),
            provider: Some(provider),
            idempotency_key: Some(format!("pre:{tool_use_id}")),
            kind: EventKind::ProviderToolRequested {
                tool_name: tool_name.clone(),
                tool_use_id: tool_use_id.clone(),
                command: command.clone(),
                file_path: file_path.clone(),
            },
        },
    )?;
    Ok(())
}

fn append_cwd_change(
    store: &SessionStore,
    runtime: &dyn Runtime,
    run_id: &RunId,
    provider: Provider,
    cwd_relative: &Path,
) -> Result<()> {
    let now = runtime.now()?;
    let initial = store.meta().worktree.cwd_relative.clone();
    let desired = cwd_relative.to_path_buf();
    EventJournal::new(&store.session_dir(), store.id().clone()).append_optional(
        PendingEventMeta {
            occurred_at: now.clone(),
            recorded_at: now,
            run_id: Some(run_id.clone()),
            provider: Some(provider),
            idempotency_key: None,
        },
        move |_, events| {
            let current = events
                .iter()
                .fold(initial, |cwd, item| match &item.event.kind {
                    EventKind::CwdChanged { cwd_relative } => cwd_relative.clone(),
                    _ => cwd,
                });
            Ok((current != desired).then_some(EventKind::CwdChanged {
                cwd_relative: desired,
            }))
        },
    )?;
    Ok(())
}

fn put_optional(store: &BlobStore, value: Option<String>) -> Result<Option<ContentRef>> {
    value.map(|value| store.put(value.as_bytes())).transpose()
}

struct RunPaths {
    inbox: PathBuf,
    checkpoints: PathBuf,
    handoff: PathBuf,
}

fn prepare_run_directory(
    store: &SessionStore,
    run_id: &RunId,
    handoff_contents: &[u8],
    recent_events_contents: &[u8],
) -> Result<RunPaths> {
    let runs = store.session_dir().join("runs");
    let temporary = runs.join(format!(".{run_id}.tmp"));
    let final_path = runs.join(run_id.to_string());
    for path in [&temporary, &final_path] {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io(path, source)),
            Ok(_) => {
                return Err(Error::InvalidState(format!(
                    "run path {} already exists",
                    path.display()
                )));
            }
        }
    }
    let inbox = temporary.join("inbox");
    let checkpoints = inbox.join("checkpoints");
    crate::store::ensure_private_dir(&checkpoints)?;
    let handoff = inbox.join("handoff.md");
    create_private(&handoff, handoff_contents)?;
    create_private(&inbox.join("recent-events.jsonl"), recent_events_contents)?;
    sync_directory(&checkpoints)?;
    sync_directory(&inbox)?;
    sync_directory(&temporary)?;
    std::fs::rename(&temporary, &final_path).map_err(|source| io(&final_path, source))?;
    sync_directory(&runs)?;
    let final_inbox = final_path.join("inbox");
    let final_checkpoints = final_inbox.join("checkpoints");
    for directory in [&final_inbox, &final_checkpoints] {
        crate::store::ensure_private_dir(directory)?;
    }
    Ok(RunPaths {
        inbox: final_inbox.clone(),
        checkpoints: final_checkpoints,
        handoff: final_inbox.join("handoff.md"),
    })
}

fn resolve_layout(environment: &Environment, cwd: &Path) -> Result<StateLayout> {
    let layout = StateLayout::from_environment_at(environment, cwd)?;
    layout.ensure()?;
    layout.canonicalized()
}

fn required_env_utf8<'a>(environment: &'a Environment, key: &str) -> Result<&'a str> {
    environment
        .get(key)
        .ok_or_else(|| Error::InvalidState(format!("{key} is required")))?
        .to_str()
        .ok_or_else(|| Error::InvalidState(format!("{key} must be valid UTF-8")))
}

fn recover_event_name(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()?
        .get("hook_event_name")?
        .as_str()
        .map(str::to_owned)
}

#[derive(Serialize)]
struct CaptureFailure<'a> {
    schema_version: u32,
    occurred_at: String,
    message: &'a str,
}

fn record_capture_failure(
    environment: &Environment,
    runtime: &dyn Runtime,
    message: &str,
) -> Result<()> {
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &cwd)?;
    let session = SessionId::parse(required_env_utf8(environment, "SESH_SESSION_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid SESH_SESSION_ID: {error}")))?;
    let run = RunId::parse(required_env_utf8(environment, "SESH_RUN_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid SESH_RUN_ID: {error}")))?;
    let path = layout
        .sessions()
        .join(session.to_string())
        .join("runs")
        .join(run.to_string())
        .join("capture-failed.json");
    crate::store::refs::write_json(
        &path,
        &CaptureFailure {
            schema_version: 1,
            occurred_at: runtime.now()?,
            message,
        },
    )
}

fn capture_failure_exists(store: &SessionStore, run_id: &RunId) -> Result<bool> {
    let path = store
        .session_dir()
        .join(format!("runs/{run_id}/capture-failed.json"));
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io(path, source)),
    }
}

fn encode_arg(arg: &OsStr) -> String {
    match arg.to_str() {
        Some(value) => value.to_owned(),
        None => format!("unix-hex:{}", hex::encode(arg.as_bytes())),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::{command_facts, latest_narrative_checkpoint, recover_stale_lease};
    use crate::model::{
        CheckpointAuthor, ContentRef, EventKind, GitSnapshot, NarrativeInput, Provider, RunId,
        SessionId, WorktreeIdentity,
    };
    use crate::runtime::Runtime;
    use crate::store::blob::BlobStore;
    use crate::store::lease::{LeaseStore, ProcessIdentity, RunLease};
    use crate::store::refs::write_json;
    use crate::store::{SessionStore, StateLayout};

    struct FixedRuntime;

    impl Runtime for FixedRuntime {
        fn now(&self) -> crate::error::Result<String> {
            Ok("2026-07-19T10:00:00Z".into())
        }

        fn session_id(&self) -> SessionId {
            SessionId::new()
        }

        fn run_id(&self) -> RunId {
            RunId::new()
        }
    }

    #[test]
    fn stale_narrative_ref_is_rejected_even_when_it_names_valid_artifacts() {
        let (_temp, store) = store_fixture();
        assert!(
            latest_narrative_checkpoint(&store, &store.events().unwrap())
                .unwrap()
                .is_none()
        );
        let (first, _) = store
            .create_narrative_checkpoint(
                &FixedRuntime,
                None,
                None,
                CheckpointAuthor::Human,
                NarrativeInput::minimal("First", "First summary", "First next"),
            )
            .unwrap();
        let (latest, _) = store
            .create_narrative_checkpoint(
                &FixedRuntime,
                None,
                None,
                CheckpointAuthor::Human,
                NarrativeInput::minimal("Latest", "Latest summary", "Latest next"),
            )
            .unwrap();
        assert!(latest.sequence > first.sequence);
        write_json(
            &store.session_dir().join("refs/latest-narrative-checkpoint"),
            &first.sequence,
        )
        .unwrap();

        assert!(latest_narrative_checkpoint(&store, &store.events().unwrap()).is_err());
        std::fs::remove_file(store.session_dir().join("refs/latest-narrative-checkpoint")).unwrap();
        assert!(latest_narrative_checkpoint(&store, &store.events().unwrap()).is_err());
    }

    #[test]
    fn only_same_host_fully_dead_leases_are_recovered() {
        let (_temp, store) = store_fixture();
        let leases = LeaseStore::new(&store.session_dir());
        let stale_run = RunId::new();
        let stale = RunLease::new(
            store.id().clone(),
            stale_run.clone(),
            Provider::Claude,
            ProcessIdentity {
                pid: u32::MAX,
                start_token: "gone".into(),
            },
        )
        .unwrap();
        leases.create(&stale).unwrap();

        recover_stale_lease(&store, &leases, &FixedRuntime).unwrap();

        assert!(leases.read().unwrap().is_none());
        assert!(store.events().unwrap().iter().any(|event| {
            event.run_id.as_ref() == Some(&stale_run)
                && matches!(event.kind, EventKind::RunRecovered { .. })
        }));

        let mut foreign = RunLease::new(
            store.id().clone(),
            RunId::new(),
            Provider::Claude,
            ProcessIdentity {
                pid: u32::MAX,
                start_token: "gone".into(),
            },
        )
        .unwrap();
        foreign.host = "different-host".into();
        leases.create(&foreign).unwrap();
        assert!(recover_stale_lease(&store, &leases, &FixedRuntime).is_err());
        assert_eq!(leases.read().unwrap().unwrap().run_id, foreign.run_id);
        leases.clear(&foreign.run_id).unwrap();

        let live = RunLease::new(
            store.id().clone(),
            RunId::new(),
            Provider::Codex,
            ProcessIdentity::capture(std::process::id()).unwrap(),
        )
        .unwrap();
        leases.create(&live).unwrap();
        assert!(recover_stale_lease(&store, &leases, &FixedRuntime).is_err());
        assert_eq!(leases.read().unwrap().unwrap().run_id, live.run_id);
    }

    #[test]
    fn opaque_provider_response_remains_unknown_and_creates_a_capture_gap() {
        let (_temp, store) = store_fixture();
        let run_id = RunId::new();
        store
            .append(
                &FixedRuntime,
                Some(run_id.clone()),
                Some(Provider::Codex),
                EventKind::ProviderToolRequested {
                    tool_name: "Bash".into(),
                    tool_use_id: "tool-1".into(),
                    command: Some("cargo test".into()),
                    file_path: None,
                },
            )
            .unwrap();
        let response = BlobStore::new(&store.session_dir())
            .put(b"Process exited with code 0\nFinal output:\nok")
            .unwrap();
        assert!(matches!(response, ContentRef::Inline { .. }));
        store
            .append(
                &FixedRuntime,
                Some(run_id),
                Some(Provider::Codex),
                EventKind::ProviderToolCompleted {
                    tool_name: "Bash".into(),
                    tool_use_id: "tool-1".into(),
                    response: Some(response),
                    stdout: None,
                    stderr: None,
                    exit_code: None,
                    duration_ms: None,
                },
            )
            .unwrap();

        let (commands, latest_test, latest_failure, gaps) =
            command_facts(&store, &store.events().unwrap()).unwrap();

        assert_eq!(commands.len(), 1);
        assert_eq!(latest_test.unwrap().exit_code, None);
        assert!(latest_failure.is_none());
        assert!(
            commands[0]
                .stdout
                .as_ref()
                .unwrap()
                .contains("Final output")
        );
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].sequence, commands[0].sequence);
    }

    fn store_fixture() -> (TempDir, SessionStore) {
        let temp = TempDir::new().unwrap();
        let worktree = temp.path().join("repo");
        let common_git_dir = worktree.join(".git");
        let snapshot = GitSnapshot {
            identity: WorktreeIdentity {
                key: WorktreeIdentity::derive_key(&common_git_dir, &common_git_dir),
                common_git_dir: common_git_dir.clone(),
                git_dir: common_git_dir,
                worktree,
                cwd_relative: PathBuf::new(),
            },
            branch: Some("main".into()),
            head: "deadbeef".into(),
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
            dirty_submodules: Vec::new(),
        };
        let store = SessionStore::create(
            &StateLayout::new(temp.path().join("state")),
            &FixedRuntime,
            snapshot,
        )
        .unwrap();
        (temp, store)
    }
}
