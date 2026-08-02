use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::checkpoint::{
    ActiveRun, StoredCheckpoint, active_run, edit_narrative, load_verified_checkpoint,
    promote_inbox, read_narrative_json, submit_provider_narrative,
};
use crate::cli::{CheckpointFormat, Cli, Command};
use crate::doctor;
use crate::error::{Error, Result, io};
use crate::fork::{
    ForkOperationStore, StagedChildProof, capture_fork_artifacts,
    recover_fork_failure_with_live_child,
};
use crate::git::Git;
use crate::git::fork::{ForkRequest, materialize};
use crate::handover::{
    BOOTSTRAP, CaptureGap, CommandFact, HandoverInput, ParentLineage, is_recognized_test_command,
    render_with_selection,
};
use crate::model::{
    Checkpoint, CheckpointAuthor, CheckpointKind, ContentRef, Event, EventEnvelope, EventKind,
    ForkOperation, ForkPhase, GitSnapshot, Provider, RunId, SessionId, Surface,
};
use crate::provider::hook::{
    HookEvent, HookOutput, NormalizedHook, capture_failure_output, normalize, session_start_output,
    stale_narrative_output,
};
use crate::provider::{LaunchContext, adapter};
use crate::runtime::Runtime;
use crate::store::atomic::{create_private, read_private, sync_directory};
use crate::store::blob::BlobStore;
use crate::store::journal::{AppendOutcome, EventJournal, PendingEvent, PendingEventMeta};
use crate::store::lease::{
    LeaseStore, ProcessIdentity, RunLease, SessionOperationLock, host_name, live_holder,
};
use crate::store::refs::read_json;
use crate::store::{Environment, SessionStore, StateLayout};
use crate::supervisor::{ExitFacts, Supervisor};

const MAX_HOOK_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HANDOVER_BYTES: usize = 65_536;
const STALE_NARRATIVE_EVENT_THRESHOLD: u64 = 20;

pub fn run(cli: Cli, environment: &Environment, runtime: &dyn Runtime) -> Result<i32> {
    if environment.get("HANDOVER_RUN_ID").is_some() && !provider_command_allowed(&cli.command) {
        return Err(Error::InvalidState(
            "an attached provider may only invoke Handover hooks, or submit provider checkpoints \
             and arm and claim a switch through `--from-provider`; to record a checkpoint, \
             pipe its JSON into `handover checkpoint --format json --from-provider`, and to hand \
             this session over, run `handover arm <provider> --from-provider`"
                .into(),
        ));
    }
    match cli.command {
        Command::Run {
            provider,
            provider_args,
        } => run_command(provider, provider_args, environment, runtime),
        Command::Switch {
            provider,
            recover_lease,
            provider_args,
        } => {
            let stdin = std::io::stdin();
            let input_is_terminal = stdin.is_terminal();
            switch_command(
                provider,
                provider_args,
                recover_lease,
                environment,
                runtime,
                stdin.lock(),
                input_is_terminal,
            )
        }
        Command::Arm {
            provider,
            surface,
            ttl,
            from_provider,
            json,
        } => arm_command(
            provider,
            surface,
            &ttl,
            from_provider,
            json,
            environment,
            runtime,
        ),
        Command::Claim {
            arm,
            from_provider,
            json,
        } => claim_command(arm, from_provider, json, environment, runtime),
        Command::Attach { provider, json } => attach_command(provider, json, environment, runtime),
        Command::Preview { provider, json } => handover_command(provider, json, environment),
        Command::Fork {
            provider,
            branch,
            worktree,
            provider_args,
        } => fork_command(
            ForkRequest {
                provider,
                branch,
                worktree,
                provider_args,
            },
            environment,
            runtime,
        ),
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
        Command::List { json } => {
            let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
            let layout = resolve_layout(environment, &cwd)?;
            crate::list::list_command(json, &layout)
        }
        Command::Status { json } => status_command(json, environment, runtime),
        Command::Log { from, json } => log_command(from, json, environment),
        Command::Inspect { json } => inspect_command(json, environment),
        Command::Delete { yes } => {
            let stdin = std::io::stdin();
            let input_is_terminal = stdin.is_terminal();
            delete_command(yes, environment, stdin.lock(), input_is_terminal)
        }
        Command::Setup { provider } => {
            let stdin = std::io::stdin();
            setup_command(provider, environment, stdin.is_terminal())
        }
        Command::Doctor { json, repair } => doctor_command(json, repair, environment),
        Command::McpServer => crate::mcp::mcp_server_command(environment, runtime),
        Command::Hook { provider } => {
            if ["HANDOVER_HOME", "HANDOVER_SESSION_ID", "HANDOVER_RUN_ID"]
                .into_iter()
                .all(|key| environment.get(key).is_none())
            {
                return Ok(0);
            }
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

/// Which commands a launched provider process may still invoke.
///
/// Hooks and `mcp-server` are the process-level exceptions. Everything else
/// here is a *write* the provider is trusted to make about its own run, and
/// each one re-proves that claim through `authorize_run_scoped_write`. The
/// flag is what makes the intent explicit at the call site; it is not itself
/// the authorization.
fn provider_command_allowed(command: &Command) -> bool {
    matches!(
        command,
        Command::Hook { .. }
            | Command::Checkpoint {
                from_provider: true,
                ..
            }
            | Command::Arm {
                from_provider: true,
                ..
            }
            | Command::Claim {
                from_provider: true,
                ..
            }
            | Command::McpServer
    )
}

fn fork_command(
    request: ForkRequest,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<i32> {
    let caller_cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let (layout, _snapshot, parent) = current_session(environment)?;
    let parent_operation_lock = SessionOperationLock::acquire(&parent.session_dir())?;
    let saved_cwd_relative = parent.saved_cwd_relative()?;
    let source_cwd_path = parent.meta().worktree.worktree.join(&saved_cwd_relative);
    let source_cwd = source_cwd_path
        .canonicalize()
        .map_err(|source| io(&source_cwd_path, source))?;
    if !source_cwd.is_dir() || !source_cwd.starts_with(&parent.meta().worktree.worktree) {
        return Err(Error::InvalidState(
            "saved cwd is not a real directory in the parent worktree".into(),
        ));
    }
    let locked_snapshot = Git::new().snapshot(&source_cwd)?;
    let parent_leases = LeaseStore::new(&parent.session_dir());
    recover_stale_lease(&parent, &parent_leases, runtime, &locked_snapshot)?;

    let operation_id = runtime.operation_id();
    let preflight = Git::new().preflight_fork(
        &source_cwd,
        &caller_cwd,
        &request,
        &operation_id.to_string(),
    )?;
    let target_branch = preflight.target.branch.clone();
    let target_worktree = preflight.target.worktree.clone();
    let operation = ForkOperation {
        schema_version: 1,
        id: operation_id.clone(),
        phase: ForkPhase::Prepared,
        source_session_id: parent.id().clone(),
        source_worktree: preflight.source.identity.clone(),
        source_checkpoint_sequence: None,
        source_fingerprint: None,
        target_branch: target_branch.clone(),
        target_worktree: target_worktree.clone(),
        target_head: preflight.source_head,
        child_session_id: None,
        target_fingerprint: None,
        target_cleanup_inventory_sha256: None,
        branch_created: false,
        target_created: false,
        error: None,
        updated_at: runtime.now()?,
    };
    let fork_store = ForkOperationStore::create(&layout, &operation)?;
    let mut live_child_proof = None;
    let prepared = (|| -> Result<_> {
        capture_fork_artifacts(&fork_store, &source_cwd, || Ok(()))?;
        materialize(&fork_store, &source_cwd, |_| Ok(()))?;
        let captured_source = fork_store
            .operation()?
            .source_fingerprint
            .ok_or_else(|| Error::InvalidState("fork lost its source fingerprint".into()))?;
        if Git::new().fingerprint(&source_cwd)? != captured_source {
            return Err(Error::InvalidState(
                "source changed before child lineage commit".into(),
            ));
        }

        let parent_events_before_transition = parent.events()?;
        let previous_provider = previous_provider(&parent_events_before_transition)?;
        let narrative_checkpoint =
            latest_narrative_checkpoint(&parent, &parent_events_before_transition)?;
        let narrative_sequence = narrative_checkpoint.as_ref().map(|item| item.0);
        let (transition_event, transition_stored) =
            parent.create_transition_checkpoint(runtime, None, None, narrative_sequence)?;
        let child_id = runtime.session_id();
        let target_cwd_path = target_worktree.join(&saved_cwd_relative);
        let target_cwd = target_cwd_path
            .canonicalize()
            .map_err(|source| io(&target_cwd_path, source))?;
        let child_snapshot = Git::new().snapshot(&target_cwd)?;
        let child = SessionStore::stage_child(
            &layout,
            runtime,
            child_snapshot.clone(),
            parent.id().clone(),
            transition_event.sequence,
            child_id.clone(),
        )?;
        live_child_proof = Some(StagedChildProof {
            child_session_id: child_id.clone(),
            source_checkpoint_sequence: transition_event.sequence,
        });
        let child_operation_lock = SessionOperationLock::acquire(&child.session_dir())?;
        fork_store.transition(ForkPhase::Verified, ForkPhase::ChildStaged, |record| {
            record.child_session_id = Some(child_id.clone());
            record.source_checkpoint_sequence = Some(transition_event.sequence);
        })?;
        parent.append(
            runtime,
            None,
            None,
            EventKind::SessionForked {
                operation_id: operation_id.clone(),
                child_session_id: child_id.clone(),
                parent_checkpoint_sequence: transition_event.sequence,
                target_worktree: target_worktree.clone(),
                target_branch: target_branch.clone(),
            },
        )?;
        fork_store.transition(ForkPhase::ChildStaged, ForkPhase::LineageCommitted, |_| {})?;
        child.bind_worktree()?;
        fork_store.transition(ForkPhase::LineageCommitted, ForkPhase::ChildBound, |_| {})?;

        let envelopes = parent.envelopes()?;
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
                    Error::InvalidState("canonical parent event envelope is not UTF-8".into())
                })?,
            ));
        }
        let parent_events = envelopes
            .iter()
            .map(|item| item.event.clone())
            .collect::<Vec<_>>();
        let (recent_commands, latest_test, latest_failure, capture_gaps) =
            command_facts(&parent, &parent_events)?;
        let rendered = render_with_selection(
            HandoverInput {
                session_id: child_id.clone(),
                parent_lineage: Some(ParentLineage {
                    session_id: parent.id().clone(),
                    transition_sequence: transition_event.sequence,
                    narrative_sequence,
                }),
                from_provider: previous_provider,
                to_provider: request.provider,
                transition_sequence: transition_event.sequence,
                transition_checkpoint: transition_stored.checkpoint,
                narrative_checkpoint,
                snapshot: child_snapshot,
                recent_events,
                recent_commands,
                latest_test,
                latest_failure,
                capture_gaps,
            },
            MAX_HANDOVER_BYTES,
        )?;
        let recent_events_jsonl =
            selected_event_lines(&envelopes, &rendered.recent_event_sequences)?;
        if recent_events_jsonl.len() > MAX_HANDOVER_BYTES {
            return Err(Error::InvalidState(
                "selected parent events exceed 64 KiB".into(),
            ));
        }

        let run_id = runtime.run_id();
        let run_paths = prepare_run_directory(
            &child,
            &run_id,
            rendered.markdown.as_bytes(),
            &recent_events_jsonl,
        )?;
        let provider_adapter = adapter(request.provider);
        provider_adapter.setup(&layout.integrations())?;
        let provider_version = provider_adapter.probe()?;
        let hook_bin = std::env::current_exe()
            .map_err(|source| io("current executable", source))?
            .canonicalize()
            .map_err(|source| io("current executable", source))?;
        child.append(
            runtime,
            Some(run_id.clone()),
            Some(request.provider),
            EventKind::RunStarted {
                cwd: target_cwd
                    .to_str()
                    .ok_or_else(|| Error::InvalidState("child cwd must be valid UTF-8".into()))?
                    .to_owned(),
                args: request
                    .provider_args
                    .iter()
                    .map(|arg| encode_arg(arg))
                    .collect(),
                supervisor_pid: std::process::id(),
            },
        )?;
        let child_leases = LeaseStore::new(&child.session_dir());
        child_leases.create(&RunLease::new(
            child.id().clone(),
            run_id.clone(),
            request.provider,
            ProcessIdentity::capture(std::process::id())?,
        )?)?;
        let provider_home = resolve_provider_home(request.provider, environment);
        let mut spec = provider_adapter.launch_spec(LaunchContext {
            cwd: &target_cwd,
            inbox: &run_paths.inbox,
            integration_root: &layout.integrations(),
            hook_bin: &hook_bin,
            provider_args: &request.provider_args,
            bootstrap: Some(BOOTSTRAP),
            run_dir: &run_paths.root,
            provider_home: provider_home.as_deref(),
        })?;
        add_run_environment(
            &mut spec.env,
            &layout,
            &child,
            &run_id,
            request.provider,
            &provider_version,
            &hook_bin,
            &run_paths,
        );
        fork_store.transition(ForkPhase::ChildBound, ForkPhase::RunLeased, |_| {})?;
        fork_store.transition(ForkPhase::RunLeased, ForkPhase::Complete, |_| {})?;
        drop(child_operation_lock);
        Ok((child, run_id, run_paths, spec, target_cwd))
    })();
    let (child, run_id, run_paths, spec, target_cwd) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let message = error.to_string();
            return match recover_fork_failure_with_live_child(
                &fork_store,
                &message,
                None,
                live_child_proof.as_ref(),
            ) {
                Ok(_) => Err(error),
                Err(recovery_error) => Err(Error::InvalidState(format!(
                    "{message}; fork recovery failed: {recovery_error}"
                ))),
            };
        }
    };
    let child_leases = LeaseStore::new(&child.session_dir());
    drop(parent_operation_lock);

    let supervised = Supervisor::launch(spec, &child, &run_id, Duration::from_secs(60));
    let child_operation_lock = SessionOperationLock::acquire(&child.session_dir())?;
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
    promote_inbox(
        &child,
        runtime,
        &run_id,
        request.provider,
        &run_paths.checkpoints,
    )?;
    child.append(
        runtime,
        Some(run_id.clone()),
        Some(request.provider),
        EventKind::RunStopped {
            exit_code: facts.exit_code,
            signal: facts.signal,
        },
    )?;
    child.append(
        runtime,
        Some(run_id.clone()),
        Some(request.provider),
        EventKind::GitSnapshot {
            snapshot: Git::new().snapshot(&target_cwd)?,
        },
    )?;
    child_leases.clear(&run_id)?;
    drop(child_operation_lock);

    if let Some(message) = supervision_error {
        return Err(Error::Command(message));
    }
    Ok(facts
        .exit_code
        .unwrap_or_else(|| 128 + facts.signal.unwrap_or(1)))
}

fn setup_command(
    provider: Provider,
    environment: &Environment,
    input_is_terminal: bool,
) -> Result<i32> {
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &cwd)?;
    let provider_adapter = adapter(provider);
    provider_adapter.setup(&layout.integrations())?;
    provider_adapter.verify(&layout.integrations())?;
    let diagnostics = doctor::check_provider(provider);
    if let Some(diagnostic) = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity == "error")
    {
        return Err(Error::Command(diagnostic.message.clone()));
    }
    let hook_bin = std::env::current_exe()
        .map_err(|source| io("current executable", source))?
        .canonicalize()
        .map_err(|source| io("current executable", source))?;
    let mut arguments = Vec::<OsString>::new();
    let mut extra_env = Vec::<(&str, OsString)>::new();
    match provider {
        Provider::Claude => {
            arguments.push("--plugin-dir".into());
            arguments.push(layout.integrations().join("claude/1").into_os_string());
        }
        Provider::Codex => {
            let review_dir = layout.integrations().join("codex/1/review");
            let provider_home = resolve_provider_home(provider, environment);
            crate::provider::codex::materialize_codex_home(
                &review_dir,
                &layout.integrations().join("codex/1"),
                provider_home.as_deref(),
            )?;
            extra_env.push(("CODEX_HOME", review_dir.into_os_string()));
        }
    }
    let command = std::iter::once(OsString::from(provider.executable()))
        .chain(arguments.iter().cloned())
        .map(|argument| shell_words::quote(&argument.to_string_lossy()).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let mut equivalent = format!(
        "HANDOVER_HOOK_BIN={} {command}",
        shell_words::quote(&hook_bin.to_string_lossy())
    );
    for (key, value) in &extra_env {
        equivalent = format!(
            "{key}={} {equivalent}",
            shell_words::quote(&value.to_string_lossy())
        );
    }
    if !input_is_terminal {
        println!("{equivalent}");
        return Ok(2);
    }
    match provider {
        Provider::Claude => println!(
            "Review the Handover plugin and hook commands, then exit without submitting a prompt."
        ),
        Provider::Codex => println!(
            "Open /hooks, review commands equal to '\"$HANDOVER_HOOK_BIN\" __hook codex', trust them, then exit."
        ),
    }
    let mut command = std::process::Command::new(provider.executable());
    command
        .args(&arguments)
        .env("HANDOVER_HOOK_BIN", &hook_bin)
        .env_remove("HANDOVER_HOME")
        .env_remove("HANDOVER_SESSION_ID")
        .env_remove("HANDOVER_RUN_ID")
        .env_remove("HANDOVER_PROVIDER")
        .env_remove("HANDOVER_PROVIDER_VERSION")
        .env_remove("HANDOVER_DOCUMENT_PATH")
        .env_remove("HANDOVER_CHECKPOINT_INBOX");
    for (key, value) in &extra_env {
        command.env(key, value);
    }
    let status = command
        .status()
        .map_err(|error| Error::Command(format!("cannot launch setup TUI: {error}")))?;
    Ok(status.code().unwrap_or(1))
}

fn doctor_command(json: bool, repair: bool, environment: &Environment) -> Result<i32> {
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = StateLayout::from_environment_at(environment, &cwd)?;
    let mut diagnostics = Vec::new();
    diagnostics.extend(doctor::check_format(&layout));
    diagnostics.extend(doctor::check_permissions(&layout));
    diagnostics.extend(doctor::check_git(&cwd));
    diagnostics.extend(doctor::check_provider(Provider::Claude));
    diagnostics.extend(doctor::check_provider(Provider::Codex));
    diagnostics.extend(doctor::check_integrations(&layout));
    diagnostics.extend(doctor::check_sessions(&layout));
    diagnostics.extend(doctor::check_forks(&layout));
    if repair {
        diagnostics.extend(doctor::repair(&layout));
    }
    if json {
        let value = serde_json::to_value(&diagnostics).map_err(|error| {
            Error::InvalidState(format!("cannot encode doctor diagnostics: {error}"))
        })?;
        write_projection(&value, true)?;
    } else {
        let mut stdout = std::io::stdout().lock();
        for diagnostic in &diagnostics {
            writeln!(
                stdout,
                "{} {}: {}",
                diagnostic.severity, diagnostic.code, diagnostic.message
            )
            .map_err(|source| io("stdout", source))?;
        }
    }
    Ok(
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == "error")
        {
            1
        } else {
            0
        },
    )
}

/// What one launch needs. `run` and `switch` differ only in what this holds;
/// everything after the launch is identical, which is why they share a tail.
///
/// `provider_args` belongs here rather than to the loop that launches: they are
/// the flags the user typed for *this* provider, and a successor launched by a
/// claimed arm must not inherit them.
struct LaunchRequest {
    provider: Provider,
    provider_args: Vec<OsString>,
    document: Vec<u8>,
    recent_events_jsonl: Vec<u8>,
    bootstrap: Option<&'static str>,
    cwd: PathBuf,
}

/// Launch `provider`, supervise it, and record its stop.
///
/// When the provider exits and an arm is pending, claim it and launch its
/// target in the same terminal rather than returning. That loop is the
/// in-session handover: arm while a provider runs, quit it, and its successor
/// comes up where it was. Each hop consumes the arm it acts on, so a further
/// hop needs a further arm — but that is not a bound. A provider that arms a
/// switch every time it starts keeps this loop going indefinitely; nothing
/// here counts hops or refuses one. Termination rests on providers not doing
/// that, not on the construction of this function.
///
/// Takes ownership of `operation` — the lock is dropped before the child
/// launches and re-acquired for teardown, exactly as the two call sites did.
fn launch_supervised_run(
    store: &SessionStore,
    layout: &StateLayout,
    leases: &LeaseStore,
    runtime: &dyn Runtime,
    environment: &Environment,
    request: LaunchRequest,
    mut operation: SessionOperationLock,
) -> Result<i32> {
    let mut request = request;
    loop {
        let provider = request.provider;
        let saved_cwd = request.cwd.clone();
        let run_id = runtime.run_id();
        let run_paths = prepare_run_directory(
            store,
            &run_id,
            &request.document,
            &request.recent_events_jsonl,
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
                args: request
                    .provider_args
                    .iter()
                    .map(|arg| encode_arg(arg))
                    .collect(),
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
        let provider_home = resolve_provider_home(provider, environment);
        let mut spec = provider_adapter.launch_spec(LaunchContext {
            cwd: &saved_cwd,
            inbox: &run_paths.inbox,
            integration_root: &layout.integrations(),
            hook_bin: &hook_bin,
            provider_args: &request.provider_args,
            bootstrap: request.bootstrap,
            run_dir: &run_paths.root,
            provider_home: provider_home.as_deref(),
        })?;
        add_run_environment(
            &mut spec.env,
            layout,
            store,
            &run_id,
            provider,
            &provider_version,
            &hook_bin,
            &run_paths,
        );
        drop(operation);

        let supervised = Supervisor::launch(spec, store, &run_id, Duration::from_secs(60));
        operation = SessionOperationLock::acquire(&store.session_dir())?;
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
        promote_inbox(store, runtime, &run_id, provider, &run_paths.checkpoints)?;
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
            EventKind::GitSnapshot {
                snapshot: post.clone(),
            },
        )?;
        leases.clear(&run_id)?;

        if let Some(message) = supervision_error {
            drop(operation);
            return Err(Error::Command(message));
        }

        let exit_code = facts
            .exit_code
            .unwrap_or_else(|| 128 + facts.signal.unwrap_or(1));

        // The lease is clear, so a pending arm can be claimed now. Anything
        // that stops the handover -- an expired arm, a handover that will not
        // render, a saved cwd that vanished -- leaves this run's exit as the
        // result rather than failing a session that already did its work. The
        // arm survives, so the user can claim it once the cause is fixed.
        match next_launch_from_pending_arm(store, layout, leases, runtime, &post) {
            Ok(Some(next)) => request = next,
            Ok(None) => {
                drop(operation);
                return Ok(exit_code);
            }
            Err(error) => {
                eprintln!(
                    "Handover did not complete: {error}. {} exited with {exit_code}; \
                     any armed switch is still pending, so fix the cause and run \
                     `handover claim`.",
                    provider.executable()
                );
                drop(operation);
                return Ok(exit_code);
            }
        }
    }
}

/// Claim a pending arm and describe the launch that succeeds it, or `None` when
/// nothing is armed.
///
/// Called only after the finished run's lease is cleared, and only with
/// `SessionOperationLock` held. Every failure here is recoverable by
/// construction: everything that can fail runs before the claim, so the caller
/// can report it and keep the finished run's exit code, and the arm survives
/// for `handover claim` to take once the cause is fixed.
fn next_launch_from_pending_arm(
    store: &SessionStore,
    layout: &StateLayout,
    leases: &LeaseStore,
    runtime: &dyn Runtime,
    post: &GitSnapshot,
) -> Result<Option<LaunchRequest>> {
    let Some(pending) = crate::arm::pending(store, runtime, &store.events()?)? else {
        return Ok(None);
    };
    // Prove the handover renders before anything is released or appended, for
    // the reason `claim_command` states: "nothing happened" is a state the user
    // can retry; "half happened" is not.
    preview_handover(store, post, pending.to)?;
    // Prove the successor can actually start, for the same reason. `arm` never
    // probes, so an arm for a provider that is not installed is accepted --
    // and the claim that consumes it must not be spent on a launch that is
    // about to fail. Both calls repeat verbatim at the top of the caller's
    // next iteration; both are idempotent.
    let successor = adapter(pending.to);
    successor.setup(&layout.integrations())?;
    successor.probe()?;
    // A pure read, and so it belongs above the claim with the rest of the
    // gate. The successor is a switch, so it gets the bootstrap and the saved
    // cwd even when this hop began as a plain `run` -- and, like a `handover
    // switch <provider>` typed with no trailing args, none of the finished
    // run's.
    let (_, next_cwd) = resolve_saved_cwd(store)?;
    eprintln!(
        "Handing over to {} (armed at sequence {}).",
        pending.to.executable(),
        pending.sequence
    );
    let built = claim_pending(store, leases, runtime, post, &pending)?;
    Ok(Some(LaunchRequest {
        provider: pending.to,
        provider_args: Vec::new(),
        document: built.rendered.markdown.into_bytes(),
        recent_events_jsonl: built.recent_events_jsonl,
        bootstrap: Some(BOOTSTRAP),
        cwd: next_cwd,
    }))
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
            "worktree already belongs to session {}; use handover switch",
            existing.id()
        )));
    }
    let store = SessionStore::create(&layout, runtime, snapshot.clone())?;

    let operation = SessionOperationLock::acquire(&store.session_dir())?;
    let leases = LeaseStore::new(&store.session_dir());
    if leases.read()?.is_some() {
        return Err(Error::InvalidState(
            "session already has an active or stale provider lease".into(),
        ));
    }

    launch_supervised_run(
        &store,
        &layout,
        &leases,
        runtime,
        environment,
        LaunchRequest {
            provider,
            provider_args,
            document: b"# Handover\n\nThis is the first provider run in this session. Continue from the current Git worktree and user prompt.\n".to_vec(),
            recent_events_jsonl: Vec::new(),
            bootstrap: None,
            cwd: cwd.clone(),
        },
        operation,
    )
}

pub fn switch_command(
    provider: Provider,
    provider_args: Vec<OsString>,
    recover_lease: bool,
    environment: &Environment,
    runtime: &dyn Runtime,
    input: impl Read,
    input_is_terminal: bool,
) -> Result<i32> {
    let invocation_cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &invocation_cwd)?;
    let invocation_snapshot = Git::new().snapshot(&invocation_cwd)?;
    let store = SessionStore::find_for_worktree(&layout, &invocation_snapshot.identity)?
        .ok_or_else(|| Error::InvalidState("this worktree has no Handover session".into()))?;
    let operation = SessionOperationLock::acquire(&store.session_dir())?;
    let leases = LeaseStore::new(&store.session_dir());
    let locked_snapshot = Git::new().snapshot(&invocation_cwd)?;
    recover_stale_lease_for_switch(
        &store,
        &leases,
        runtime,
        &locked_snapshot,
        provider,
        recover_lease,
        input,
        input_is_terminal,
    )?;

    let (saved_cwd, switch_snapshot) = resolve_switch_snapshot(&store, &invocation_snapshot)?;
    store.append(
        runtime,
        None,
        None,
        EventKind::GitSnapshot {
            snapshot: switch_snapshot.clone(),
        },
    )?;

    // Prove the handover renders before recording any intent. `arm`, `claim`,
    // and the supervisor's claim path all gate on this before they mutate; the
    // porcelain must not be the one command that leaves a capability the user
    // never asked for behind when it fails.
    preview_handover(&store, &invocation_snapshot, provider)?;

    // Reuse a matching arm rather than recording a second intent for the same
    // switch. An arm for a *different* provider is a conflict this refuses
    // rather than displaces: a one-shot capability is not overwritten by the
    // command that trips over it.
    let pending = match crate::arm::pending(&store, runtime, &store.events()?)? {
        Some(existing) if existing.to == provider => existing,
        Some(existing) => {
            return Err(Error::InvalidState(format!(
                "a switch to {} is already armed at sequence {} (expires {}); \
                 claim it with `handover claim`, or wait for it to expire",
                existing.to.executable(),
                existing.sequence,
                existing.expires_at
            )));
        }
        None => arm_for_switch(
            &store,
            runtime,
            &leases,
            // `switch` recovers a stale lease with consent *before* arming, so
            // there is no lease left to adopt and nothing to narrow.
            None,
            provider,
            Surface::Auto,
            crate::arm::parse_ttl(crate::arm::DEFAULT_TTL)?,
        )?,
    };
    let built = claim_pending(&store, &leases, runtime, &invocation_snapshot, &pending)?;

    launch_supervised_run(
        &store,
        &layout,
        &leases,
        runtime,
        environment,
        LaunchRequest {
            provider,
            provider_args,
            document: built.rendered.markdown.into_bytes(),
            recent_events_jsonl: built.recent_events_jsonl,
            bootstrap: Some(BOOTSTRAP),
            cwd: saved_cwd,
        },
        operation,
    )
}

fn resolve_saved_cwd(store: &SessionStore) -> Result<(PathBuf, PathBuf)> {
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
    Ok((saved_cwd_relative, saved_cwd))
}

fn collect_recent_events(
    envelopes: &[EventEnvelope],
    recent_boundary: u64,
) -> Result<Vec<(u64, String)>> {
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
    Ok(recent_events)
}

/// The transition checkpoint a handover is rendered against.
///
/// `switch` and `claim` supply one they have just committed to the journal;
/// `preview` and `status` supply one that only describes what a switch
/// *would* commit, because a dry run must append nothing.
struct Transition {
    sequence: u64,
    checkpoint: Checkpoint,
}

impl Transition {
    /// The transition `SessionStore::create_transition_checkpoint` just
    /// committed: `sequence` names a `checkpoint.created` event that exists.
    fn committed(event: &Event, stored: StoredCheckpoint) -> Self {
        Self {
            sequence: event.sequence,
            checkpoint: stored.checkpoint,
        }
    }

    /// The transition a switch would commit next. Never journaled — callers
    /// use it to prove the document renders without mutating anything.
    fn hypothetical(events: &[Event], narrative_sequence: Option<u64>) -> Self {
        let through_sequence = events.last().map(|event| event.sequence).unwrap_or(0);
        Self {
            sequence: through_sequence + 1,
            checkpoint: Checkpoint {
                schema_version: 1,
                checkpoint_kind: CheckpointKind::Transition,
                through_sequence,
                author: CheckpointAuthor::System,
                narrative: None,
                narrative_checkpoint_sequence: narrative_sequence,
            },
        }
    }
}

/// One rendered handover plus the facts its callers report alongside it.
struct BuiltHandover {
    events: Vec<Event>,
    from_provider: Option<Provider>,
    transition_sequence: u64,
    through_sequence: u64,
    narrative_checkpoint: Option<(u64, Checkpoint)>,
    capture_gaps: Vec<CaptureGap>,
    rendered: crate::handover::RenderedHandover,
    recent_events_jsonl: Vec<u8>,
}

/// Render the handover for a switch to `to_provider` against the journal
/// exactly as it stands when called.
///
/// Every command that emits a handover — `switch`, `claim`, `preview`,
/// `status`'s readiness check, the MCP server — renders through here, so no
/// two of them can describe the same session differently. The one thing
/// callers vary is the `transition`, because only they know whether it is
/// committed or hypothetical.
fn build_handover(
    store: &SessionStore,
    switch_snapshot: GitSnapshot,
    to_provider: Provider,
    transition: Transition,
    narrative_checkpoint: Option<(u64, Checkpoint)>,
) -> Result<BuiltHandover> {
    let envelopes = store.envelopes()?;
    let events: Vec<Event> = envelopes.iter().map(|item| item.event.clone()).collect();
    let from_provider = previous_provider(&events)?;
    let recent_boundary = narrative_checkpoint
        .as_ref()
        .map(|item| item.0)
        .unwrap_or(0);
    let recent_events = collect_recent_events(&envelopes, recent_boundary)?;
    let (recent_commands, latest_test, latest_failure, capture_gaps) =
        command_facts(store, &events)?;

    let transition_sequence = transition.sequence;
    let through_sequence = transition.checkpoint.through_sequence;
    let rendered = render_with_selection(
        HandoverInput {
            session_id: store.id().clone(),
            parent_lineage: None,
            from_provider,
            to_provider,
            transition_sequence,
            transition_checkpoint: transition.checkpoint,
            narrative_checkpoint: narrative_checkpoint.clone(),
            snapshot: switch_snapshot,
            recent_events,
            recent_commands,
            latest_test,
            latest_failure,
            capture_gaps: capture_gaps.clone(),
        },
        MAX_HANDOVER_BYTES,
    )?;
    let recent_events_jsonl = selected_event_lines(&envelopes, &rendered.recent_event_sequences)?;
    if recent_events_jsonl.len() > MAX_HANDOVER_BYTES {
        return Err(Error::InvalidState(
            "selected recent events exceed 64 KiB".into(),
        ));
    }

    Ok(BuiltHandover {
        events,
        from_provider,
        transition_sequence,
        through_sequence,
        narrative_checkpoint,
        capture_gaps,
        rendered,
        recent_events_jsonl,
    })
}

/// Resolve the saved cwd and verify it still describes the same unchanged
/// worktree the command was invoked from, returning that cwd and the
/// snapshot the handover must be rendered against.
fn resolve_switch_snapshot(
    store: &SessionStore,
    invocation_snapshot: &GitSnapshot,
) -> Result<(PathBuf, GitSnapshot)> {
    let (saved_cwd_relative, saved_cwd) = resolve_saved_cwd(store)?;
    let switch_snapshot = Git::new().snapshot(&saved_cwd)?;
    verify_switch_snapshot(
        invocation_snapshot,
        &switch_snapshot,
        &store.meta().worktree,
        &saved_cwd_relative,
    )?;
    Ok((saved_cwd, switch_snapshot))
}

/// Dry-run of the same handover a `switch` to `to_provider` would build:
/// resolves the saved cwd, verifies it against `invocation_snapshot`, and
/// renders — a pure read with no mutation, no lease/journal writes. Its
/// transition is hypothetical, so the document it produces must never be
/// handed to a provider as a record of a switch that happened. Used by
/// `handover_command` (`handover preview`), by `status_command`'s
/// `switch_readiness` check, and by `arm_command` and `claim_command` as the
/// gate that proves the real handover will render.
fn preview_handover(
    store: &SessionStore,
    invocation_snapshot: &GitSnapshot,
    to_provider: Provider,
) -> Result<BuiltHandover> {
    let (_, snapshot) = resolve_switch_snapshot(store, invocation_snapshot)?;
    let events = store.events()?;
    let narrative_checkpoint = latest_narrative_checkpoint(store, &events)?;
    let transition =
        Transition::hypothetical(&events, narrative_checkpoint.as_ref().map(|item| item.0));
    build_handover(
        store,
        snapshot,
        to_provider,
        transition,
        narrative_checkpoint,
    )
}

/// Commit a transition checkpoint and render the handover against it.
///
/// `switch` and `claim` are both provider boundaries, and a boundary leaves a
/// real `checkpoint.created` event behind so the document it emits names a
/// checkpoint that exists in the journal. Callers must already hold
/// `SessionOperationLock`, and must already have proven the document renders
/// (see `preview_handover`) if they mutated anything first.
fn commit_transition_handover(
    store: &SessionStore,
    runtime: &dyn Runtime,
    snapshot: GitSnapshot,
    to_provider: Provider,
) -> Result<BuiltHandover> {
    let events = store.events()?;
    let narrative_checkpoint = latest_narrative_checkpoint(store, &events)?;
    let narrative_sequence = narrative_checkpoint.as_ref().map(|item| item.0);
    let (event, stored) =
        store.create_transition_checkpoint(runtime, None, None, narrative_sequence)?;
    build_handover(
        store,
        snapshot,
        to_provider,
        Transition::committed(&event, stored),
        narrative_checkpoint,
    )
}

fn handover_command(provider: Provider, json: bool, environment: &Environment) -> Result<i32> {
    let invocation_cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &invocation_cwd)?;
    let invocation_snapshot = Git::new().snapshot(&invocation_cwd)?;
    let store = SessionStore::find_for_worktree(&layout, &invocation_snapshot.identity)?
        .ok_or_else(|| Error::InvalidState("this worktree has no Handover session".into()))?;

    let preview = preview_handover(&store, &invocation_snapshot, provider)?;

    if json {
        write_handover_projection(
            &store,
            preview.from_provider,
            provider,
            preview.transition_sequence,
            preview.through_sequence,
            &preview.events,
            preview.narrative_checkpoint,
            preview.capture_gaps,
            &preview.rendered,
        )
    } else {
        std::io::stdout()
            .write_all(preview.rendered.markdown.as_bytes())
            .map_err(|source| io("stdout", source))?;
        Ok(0)
    }
}

/// Prove a `--from-provider` write belongs to the session it is aimed at, and
/// hand back the run it proved.
///
/// `checkpoint::active_run` proves which run this process was launched for. It
/// does not prove which session the command resolved from the cwd, and a
/// provider process can `cd` anywhere — so the two are compared here. Without
/// this, a provider attached to one session could walk into another worktree
/// and arm the session it found. It then requires that run to still hold the
/// session's lease, because a run directory outlives its run; liveness is
/// deliberately not required.
///
/// The `ActiveRun` it returns is how a caller says *which* run it is, which is
/// what lets `arm_for_switch` record `armed_run` only for the run that owns the
/// lease.
///
/// Like the checkpoint path it reuses, this is a guardrail against accidental
/// misuse, not an authorization boundary: a provider running as the same Unix
/// user can already write `$HANDOVER_HOME` directly.
fn authorize_run_scoped_write(
    store: &SessionStore,
    environment: &Environment,
) -> Result<ActiveRun> {
    let run = active_run(environment)?;
    if run.session_id != *store.id() {
        return Err(Error::InvalidState(format!(
            "this provider is attached to session {}, but this directory belongs to session {}; \
             run the command from your own session's worktree",
            run.session_id,
            store.id()
        )));
    }

    // `active_run` proves a run by this name existed, not that it still does:
    // run directories outlive their run. Requiring the lease closes that gap —
    // a finished run's lease was cleared at teardown, so its leftover
    // environment can no longer arm a switch the user did not ask for.
    //
    // Liveness is deliberately not required. A supervisor killed while its
    // child still runs leaves a dead-but-owned lease, and that provider is
    // exactly who should still be able to hand its session over.
    let holds_lease = LeaseStore::new(&store.session_dir())
        .read()?
        .is_some_and(|lease| lease.run_id == run.run_id);
    if !holds_lease {
        return Err(Error::InvalidState(
            "this provider's run no longer holds this session's lease; the run it belongs to \
             has ended, so it can no longer arm or claim a switch for this session"
                .into(),
        ));
    }
    Ok(run)
}

/// Append the intent and the capability, and return the arm just created.
///
/// `armed_run` is recorded only when `caller_run` *is* the run holding the
/// lease. That is the entire authority an arm carries: a later claim may
/// release that run's dead lease without the `[y/N]` prompt
/// `switch --recover-lease` otherwise requires. An arm recorded from outside a
/// run adopts nothing, so it can never become an unprompted recovery of a lease
/// its caller never owned.
///
/// Callers must hold `SessionOperationLock`.
fn arm_for_switch(
    store: &SessionStore,
    runtime: &dyn Runtime,
    leases: &LeaseStore,
    caller_run: Option<&RunId>,
    provider: Provider,
    surface: Surface,
    ttl: std::time::Duration,
) -> Result<crate::arm::PendingArm> {
    let events = store.events()?;
    let armed_run = match (caller_run, leases.read()?) {
        (Some(caller), Some(lease)) if lease.run_id == *caller => Some(lease.run_id),
        _ => None,
    };
    let expires_at = crate::arm::expires_at(runtime, ttl)?;
    store.append(
        runtime,
        armed_run.clone(),
        None,
        EventKind::SwitchRequested {
            from: previous_provider(&events)?,
            to: provider,
        },
    )?;
    let event = store.append(
        runtime,
        armed_run.clone(),
        None,
        EventKind::SwitchArmed {
            to: provider,
            surface,
            expires_at: expires_at.clone(),
        },
    )?;
    Ok(crate::arm::PendingArm {
        sequence: event.sequence,
        to: provider,
        surface,
        expires_at,
        armed_run,
    })
}

/// The arm projection, shared by `handover arm` and the MCP `arm` tool.
///
/// `from_provider` selects the authorization rule, never the behaviour. Both
/// callers run this same function, which is the layering contract: the
/// experience layer holds no logic.
fn arm_value(
    provider: Provider,
    surface: Surface,
    ttl: &str,
    from_provider: bool,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<serde_json::Value> {
    let ttl = crate::arm::parse_ttl(ttl)?;
    let (_layout, snapshot, store) = current_session(environment)?;
    let caller_run = if from_provider {
        Some(authorize_run_scoped_write(&store, environment)?.run_id)
    } else {
        None
    };
    let _operation = SessionOperationLock::acquire(&store.session_dir())?;
    let events = store.events()?;
    if let Some(existing) = crate::arm::pending(&store, runtime, &events)? {
        return Err(Error::InvalidState(format!(
            "a switch to {} is already armed at sequence {}; claim it or wait until {}",
            existing.to.executable(),
            existing.sequence,
            existing.expires_at
        )));
    }

    // Gate on exactly what `switch_readiness.ready` means, minus its lease
    // term: arming while a provider is still running is the point.
    preview_handover(&store, &snapshot, provider)?;

    let events = store.events()?;
    let (_, events_since) = crate::list::narrative_freshness(&events);
    let checkpoint_fresh = events_since < STALE_NARRATIVE_EVENT_THRESHOLD;
    if !checkpoint_fresh {
        eprintln!(
            "warning: {events_since} events since the last narrative checkpoint; \
             the handover will be thin. Write one with `handover checkpoint`."
        );
    }

    let leases = LeaseStore::new(&store.session_dir());
    let arm = arm_for_switch(
        &store,
        runtime,
        &leases,
        caller_run.as_ref(),
        provider,
        surface,
        ttl,
    )?;

    Ok(serde_json::json!({
        "schema_version": 1,
        "armed_sequence": arm.sequence,
        "to": provider,
        "surface": surface,
        "expires_at": arm.expires_at,
        "checkpoint_fresh": checkpoint_fresh,
    }))
}

fn arm_command(
    provider: Provider,
    surface: Surface,
    ttl: &str,
    from_provider: bool,
    json: bool,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<i32> {
    let value = arm_value(provider, surface, ttl, from_provider, environment, runtime)?;
    write_projection(&value, json)?;
    Ok(0)
}

pub(crate) fn mcp_arm_value(
    provider: Provider,
    surface: Surface,
    ttl: &str,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<serde_json::Value> {
    arm_value(provider, surface, ttl, true, environment, runtime)
}

/// Release the lease the arm authorises us to release, if any.
///
/// The privilege is narrow: a *dead* lease belonging to the arming run may be
/// cleared without the recovery prompt. Nothing else may be touched.
///
/// Clearing it is a recovery, so it is journaled as `run.recovered` naming
/// the claim as the reason, exactly as the `switch` and `fork` recovery paths
/// record theirs. No lease leaves this session's history unexplained.
fn release_for_claim(
    store: &SessionStore,
    leases: &LeaseStore,
    runtime: &dyn Runtime,
    recovery_snapshot: &GitSnapshot,
    pending: &crate::arm::PendingArm,
) -> Result<()> {
    let Some(lease) = leases.read()? else {
        return Ok(());
    };
    // Three separate refusals: host, then liveness, then ownership. A
    // foreign-host lease may well have been created by the arming run -- it
    // just cannot be checked from here -- so it must not be told that
    // something else created it.
    if lease.host != host_name()? {
        let (_state, reason) = classify_lease(leases)?;
        return Err(Error::InvalidState(format!(
            "cannot release this session's lease; {}",
            reason.unwrap_or_else(|| "inspect it with `handover status`".into())
        )));
    }
    // Liveness before ownership: "quit the running provider" is true and
    // actionable whoever armed the switch, and nobody may release a live lease
    // anyway. Who owns it only becomes the interesting question once it is
    // dead and could actually be released.
    if let Some(holder) = live_holder(&lease)? {
        return Err(Error::InvalidState(format!(
            "{} is still running this session ({}); quit it to complete the armed switch",
            lease.provider.executable(),
            holder.describe()
        )));
    }
    if pending.armed_run.as_ref() != Some(&lease.run_id) {
        let (_state, reason) = classify_lease(leases)?;
        return Err(Error::InvalidState(format!(
            "cannot release this session's lease; it was not created by the run that armed the switch; {}",
            reason.unwrap_or_else(|| "inspect it with `handover status`".into())
        )));
    }
    store.append(
        runtime,
        Some(lease.run_id.clone()),
        Some(lease.provider),
        EventKind::RunRecovered {
            supervisor_pid: lease.supervisor.pid,
            supervisor_start_token: lease.supervisor.start_token.clone(),
            child_pid: lease.child.as_ref().map(|child| child.pid),
            child_start_token: lease.child.as_ref().map(|child| child.start_token.clone()),
            host: lease.host.clone(),
            reason: format!(
                "released by claim of the switch armed at sequence {}",
                pending.sequence
            ),
        },
    )?;
    store.append(
        runtime,
        Some(lease.run_id.clone()),
        Some(lease.provider),
        EventKind::GitSnapshot {
            snapshot: recovery_snapshot.clone(),
        },
    )?;
    leases.clear(&lease.run_id)?;
    eprintln!(
        "Released the stale {} lease left by the arming run ({}); completing the armed switch.",
        lease.provider.executable(),
        lease.supervisor.describe()
    );
    Ok(())
}

/// Consume `pending`: release the arming run's dead lease if there is one,
/// commit the transition checkpoint, and record the claim.
///
/// Every command that completes a switch goes through here, so no two of them
/// can hand a provider a different document. Callers must hold
/// `SessionOperationLock`, and must already have proven the handover renders.
fn claim_pending(
    store: &SessionStore,
    leases: &LeaseStore,
    runtime: &dyn Runtime,
    invocation_snapshot: &GitSnapshot,
    pending: &crate::arm::PendingArm,
) -> Result<BuiltHandover> {
    release_for_claim(store, leases, runtime, invocation_snapshot, pending)?;
    let (_, switch_snapshot) = resolve_switch_snapshot(store, invocation_snapshot)?;
    let built = commit_transition_handover(store, runtime, switch_snapshot, pending.to)?;
    store.append(
        runtime,
        None,
        None,
        EventKind::SwitchClaimed {
            armed_sequence: pending.sequence,
            to: pending.to,
            transition_checkpoint_sequence: built.transition_sequence,
        },
    )?;
    Ok(built)
}

/// Consume this session's pending arm and return everything either output form
/// needs.
///
/// Every command that completes a claim goes through here, so the CLI and the
/// MCP tool cannot diverge on what a claim does — only on how they print it.
///
/// Callers must not write through the returned `SessionStore`: the
/// `SessionOperationLock` that guarded the claim is released when this returns,
/// so any further append would be unserialized. Formatting the result is all
/// the store is for here.
fn claim_core(
    arm: Option<u64>,
    from_provider: bool,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<(SessionStore, Provider, BuiltHandover)> {
    let (_layout, snapshot, store) = current_session(environment)?;
    if from_provider {
        authorize_run_scoped_write(&store, environment)?;
    }
    let _operation = SessionOperationLock::acquire(&store.session_dir())?;
    let events = store.events()?;
    let pending = crate::arm::pending(&store, runtime, &events)?
        .ok_or_else(|| Error::InvalidState("no switch is armed for this session".into()))?;
    if let Some(expected) = arm
        && expected != pending.sequence
    {
        return Err(Error::InvalidState(format!(
            "the armed switch is at sequence {}, not {expected}",
            pending.sequence
        )));
    }

    // Prove the handover renders before anything is released or appended: a
    // missing blob, a vanished saved cwd, or an oversized document must leave
    // the arm pending and the lease untouched. "Nothing happened" is a state
    // the user can retry; "half happened" is not.
    preview_handover(&store, &snapshot, pending.to)?;

    let leases = LeaseStore::new(&store.session_dir());
    let built = claim_pending(&store, &leases, runtime, &snapshot, &pending)?;

    Ok((store, pending.to, built))
}

/// The projection a completed claim produces, for both `handover claim --json`
/// and the MCP `claim` tool.
///
/// One call site, so the two cannot drift into disagreeing about what a claim
/// returned. `build_handover_value` takes nine positional arguments, two pairs
/// of which are same-typed and therefore transposable in silence; keeping the
/// mapping from `BuiltHandover` in one place is what makes that unreachable
/// here.
fn claim_projection(
    store: &SessionStore,
    to_provider: Provider,
    built: BuiltHandover,
) -> serde_json::Value {
    build_handover_value(
        store,
        built.from_provider,
        to_provider,
        built.transition_sequence,
        built.through_sequence,
        &built.events,
        built.narrative_checkpoint,
        built.capture_gaps,
        &built.rendered,
    )
}

fn claim_command(
    arm: Option<u64>,
    from_provider: bool,
    json: bool,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<i32> {
    let (store, to_provider, built) = claim_core(arm, from_provider, environment, runtime)?;
    if json {
        write_projection(&claim_projection(&store, to_provider, built), true)?;
        Ok(0)
    } else {
        std::io::stdout()
            .write_all(built.rendered.markdown.as_bytes())
            .map_err(|source| io("stdout", source))?;
        Ok(0)
    }
}

pub(crate) fn mcp_claim_value(
    arm: Option<u64>,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<serde_json::Value> {
    let (store, to_provider, built) = claim_core(arm, true, environment, runtime)?;
    Ok(claim_projection(&store, to_provider, built))
}

/// The attach projection, shared by `handover attach` and the MCP `attach`
/// tool.
///
/// Deliberately **not** run-scoped. By definition no run exists yet — a desktop
/// app has no run environment at all — so this is scoped to the worktree its
/// cwd resolves to, exactly as `run` is.
fn attach_value(
    provider: Provider,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<serde_json::Value> {
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &cwd)?;
    let snapshot = Git::new().snapshot(&cwd)?;
    let (store, created, _operation) =
        match SessionStore::find_for_worktree(&layout, &snapshot.identity)? {
            Some(store) => {
                // Acquire the operation lock before checking the lease so no other
                // command can create a live lease between the check and the append
                // below (see `claim_core`, which follows the same ordering).
                let operation = SessionOperationLock::acquire(&store.session_dir())?;
                let (state, reason) = classify_lease(&LeaseStore::new(&store.session_dir()))?;
                if state == "blocked" {
                    return Err(Error::InvalidState(format!(
                        "cannot attach to this worktree's session; {}",
                        reason.unwrap_or_else(|| "it is already held".into())
                    )));
                }
                (store, false, operation)
            }
            None => {
                // No session dir exists yet, so there is nothing to lock until
                // after creation; the lock is acquired immediately afterward and
                // held through the append below.
                let store = SessionStore::create(&layout, runtime, snapshot.clone())?;
                let operation = SessionOperationLock::acquire(&store.session_dir())?;
                (store, true, operation)
            }
        };

    store.append(runtime, None, Some(provider), EventKind::SessionAttached {})?;

    Ok(serde_json::json!({
        "schema_version": 1,
        "session_id": store.id(),
        "provider": provider,
        "created": created,
        "worktree": snapshot.identity.worktree,
    }))
}

fn attach_command(
    provider: Provider,
    json: bool,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<i32> {
    let value = attach_value(provider, environment, runtime)?;
    write_projection(&value, json)?;
    Ok(0)
}

pub(crate) fn mcp_attach_value(
    provider: Provider,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<serde_json::Value> {
    attach_value(provider, environment, runtime)
}

#[allow(clippy::too_many_arguments)]
fn build_handover_value(
    store: &SessionStore,
    from_provider: Option<Provider>,
    to_provider: Provider,
    transition_sequence: u64,
    through_sequence: u64,
    events: &[Event],
    narrative_checkpoint: Option<(u64, Checkpoint)>,
    capture_gaps: Vec<CaptureGap>,
    rendered: &crate::handover::RenderedHandover,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "session_id": store.id(),
        "from_provider": from_provider,
        "to_provider": to_provider,
        "transition": {
            "sequence": transition_sequence,
            "through_sequence": through_sequence,
        },
        "narrative_checkpoint": narrative_checkpoint.map(|(sequence, checkpoint)| {
            serde_json::json!({
                "sequence": sequence,
                "through_sequence": checkpoint.through_sequence,
                "author": checkpoint.author,
                "events_since": events.iter().filter(|event| event.sequence > sequence).count() as u64,
            })
        }),
        "capture_gaps": capture_gaps.into_iter().map(|gap| serde_json::json!({
            "sequence": gap.sequence,
            "phase": gap.phase,
            "message": gap.message,
        })).collect::<Vec<_>>(),
        "omitted": rendered.omitted,
        "markdown_bytes": rendered.markdown.len(),
        "markdown": rendered.markdown,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_handover_projection(
    store: &SessionStore,
    from_provider: Option<Provider>,
    to_provider: Provider,
    transition_sequence: u64,
    through_sequence: u64,
    events: &[Event],
    narrative_checkpoint: Option<(u64, Checkpoint)>,
    capture_gaps: Vec<CaptureGap>,
    rendered: &crate::handover::RenderedHandover,
) -> Result<i32> {
    let value = build_handover_value(
        store,
        from_provider,
        to_provider,
        transition_sequence,
        through_sequence,
        events,
        narrative_checkpoint,
        capture_gaps,
        rendered,
    );
    write_projection(&value, true)?;
    Ok(0)
}

pub(crate) fn mcp_handover_value(
    provider: Provider,
    environment: &Environment,
) -> Result<serde_json::Value> {
    let (_layout, snapshot, store) = current_session(environment)?;
    let preview = preview_handover(&store, &snapshot, provider)?;
    Ok(build_handover_value(
        &store,
        preview.from_provider,
        provider,
        preview.transition_sequence,
        preview.through_sequence,
        &preview.events,
        preview.narrative_checkpoint,
        preview.capture_gaps,
        &preview.rendered,
    ))
}

fn confirm_lease_recovery(
    lease: &RunLease,
    target: Provider,
    recover_lease: bool,
    mut input: impl Read,
    input_is_terminal: bool,
) -> Result<&'static str> {
    if recover_lease {
        return Ok("recovery confirmed via --recover-lease");
    }
    let holder = lease.child.as_ref().unwrap_or(&lease.supervisor);
    if !input_is_terminal {
        return Err(Error::InvalidState(format!(
            "session has a stale {} lease ({}); rerun with `handover switch {} --recover-lease`, or run `handover switch {}` in a terminal to confirm",
            lease.provider.executable(),
            holder.describe(),
            target.executable(),
            target.executable()
        )));
    }
    eprintln!(
        "Stale lease from {} ({}) — the process is no longer running.",
        lease.provider.executable(),
        holder.describe()
    );
    eprint!("Recover this lease and continue switching? [y/N] ");
    std::io::stderr()
        .flush()
        .map_err(|source| io("stderr", source))?;
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|source| io("stdin", source))?;
    if bytes.len() > 4096 {
        return Err(Error::InvalidState(
            "lease recovery confirmation is too long".into(),
        ));
    }
    let answer = std::str::from_utf8(&bytes)
        .map_err(|_| Error::InvalidState("lease recovery confirmation is not UTF-8".into()))?
        .trim()
        .to_lowercase();
    if answer != "y" && answer != "yes" {
        return Err(Error::InvalidState(
            "switch cancelled: stale lease was not recovered".into(),
        ));
    }
    Ok("recovery confirmed interactively")
}

/// Stale-lease recovery for `handover switch`: refuses a live or foreign-host
/// lease with holder detail (provider, pid, started-when), and recovers a
/// same-host dead lease only after explicit consent (an interactive
/// `[y/N]` prompt or `--recover-lease`) via `confirm_lease_recovery`.
///
/// `handover fork` has its own, unrelated `recover_stale_lease` below: fork's
/// UX is out of scope for this consent gate and keeps its original,
/// unprompted, generically-worded behavior unchanged.
#[allow(clippy::too_many_arguments)]
fn recover_stale_lease_for_switch(
    store: &SessionStore,
    leases: &LeaseStore,
    runtime: &dyn Runtime,
    recovery_snapshot: &GitSnapshot,
    target: Provider,
    recover_lease: bool,
    input: impl Read,
    input_is_terminal: bool,
) -> Result<()> {
    let Some(lease) = leases.read()? else {
        return Ok(());
    };
    if lease.host != host_name()? {
        return Err(Error::InvalidState(format!(
            "cannot switch: session lease belongs to host {} ({}, {}); liveness cannot be checked from this host. If you're sure it's gone, remove refs/active-run.json for this session by hand after confirming.",
            lease.host,
            lease.provider.executable(),
            lease.supervisor.describe()
        )));
    }
    if let Some(holder) = live_holder(&lease)? {
        return Err(Error::InvalidState(format!(
            "cannot switch: {} is still running this session ({}). Finish or quit {}, then retry the switch.",
            lease.provider.executable(),
            holder.describe(),
            lease.provider.executable()
        )));
    }
    let reason_suffix =
        confirm_lease_recovery(&lease, target, recover_lease, input, input_is_terminal)?;
    store.append(
        runtime,
        Some(lease.run_id.clone()),
        Some(lease.provider),
        EventKind::RunRecovered {
            supervisor_pid: lease.supervisor.pid,
            supervisor_start_token: lease.supervisor.start_token.clone(),
            child_pid: lease.child.as_ref().map(|child| child.pid),
            child_start_token: lease.child.as_ref().map(|child| child.start_token.clone()),
            host: lease.host.clone(),
            reason: format!(
                "same-host supervisor and child processes are no longer live; {reason_suffix}"
            ),
        },
    )?;
    store.append(
        runtime,
        Some(lease.run_id.clone()),
        Some(lease.provider),
        EventKind::GitSnapshot {
            snapshot: recovery_snapshot.clone(),
        },
    )?;
    leases.clear(&lease.run_id)?;
    eprintln!(
        "Recovered stale {} lease ({}); continuing switch.",
        lease.provider.executable(),
        lease.supervisor.describe()
    );
    Ok(())
}

fn recover_stale_lease(
    store: &SessionStore,
    leases: &LeaseStore,
    runtime: &dyn Runtime,
    recovery_snapshot: &GitSnapshot,
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
    if live_holder(&lease)?.is_some() {
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
            supervisor_pid: lease.supervisor.pid,
            supervisor_start_token: lease.supervisor.start_token.clone(),
            child_pid: lease.child.as_ref().map(|child| child.pid),
            child_start_token: lease.child.as_ref().map(|child| child.start_token.clone()),
            host: lease.host.clone(),
            reason: "same-host supervisor and child processes are no longer live".into(),
        },
    )?;
    store.append(
        runtime,
        Some(lease.run_id.clone()),
        Some(lease.provider),
        EventKind::GitSnapshot {
            snapshot: recovery_snapshot.clone(),
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

type HandoverFacts = (
    Vec<CommandFact>,
    Option<CommandFact>,
    Option<CommandFact>,
    Vec<CaptureGap>,
);

fn command_facts(store: &SessionStore, events: &[Event]) -> Result<HandoverFacts> {
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
            "handover selected an event absent from the committed journal".into(),
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
        ("HANDOVER_HOME", layout.root().as_os_str()),
        ("HANDOVER_SESSION_ID", OsStr::new(&store.id().to_string())),
        ("HANDOVER_RUN_ID", OsStr::new(&run_id.to_string())),
        ("HANDOVER_PROVIDER", OsStr::new(provider.executable())),
        ("HANDOVER_PROVIDER_VERSION", OsStr::new(provider_version)),
        ("HANDOVER_HOOK_BIN", hook_bin.as_os_str()),
        ("HANDOVER_DOCUMENT_PATH", run_paths.handover.as_os_str()),
        (
            "HANDOVER_CHECKPOINT_INBOX",
            run_paths.checkpoints.as_os_str(),
        ),
    ] {
        target.insert(OsString::from(key), value.to_owned());
    }
}

fn classify_lease(leases: &LeaseStore) -> Result<(&'static str, Option<String>)> {
    let Some(lease) = leases.read()? else {
        return Ok(("free", None));
    };
    if lease.host != host_name()? {
        return Ok((
            "blocked",
            Some(format!(
                "lease belongs to host {} ({}, {}); liveness cannot be checked from this host",
                lease.host,
                lease.provider.executable(),
                lease.supervisor.describe()
            )),
        ));
    }
    if let Some(holder) = live_holder(&lease)? {
        return Ok((
            "blocked",
            Some(format!(
                "{} is still running this session ({})",
                lease.provider.executable(),
                holder.describe()
            )),
        ));
    }
    let holder = lease.child.as_ref().unwrap_or(&lease.supervisor);
    Ok((
        "recoverable",
        Some(format!(
            "stale {} lease ({}); switch will prompt to recover it, or pass --recover-lease",
            lease.provider.executable(),
            holder.describe()
        )),
    ))
}

pub(crate) fn build_status_value(
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<serde_json::Value> {
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
    let (latest_narrative, events_since) = crate::list::narrative_freshness(&events);
    let leases = LeaseStore::new(&store.session_dir());
    let (lease_state, lease_reason) = classify_lease(&leases)?;
    let checkpoint_fresh = events_since < STALE_NARRATIVE_EVENT_THRESHOLD;
    let target_provider = provider.map(Provider::other).unwrap_or(Provider::Claude);
    let (handover_renderable, handover_error) =
        match preview_handover(&store, &snapshot, target_provider) {
            Ok(_) => (true, None),
            Err(error) => (false, Some(error.to_string())),
        };
    // A pending arm is a refusal this block would otherwise not see: `switch`
    // rejects a target that is not the armed one, so reporting `ready: true`
    // and suggesting the other provider would send the user at a command that
    // cannot run. Read it without `crate::arm::pending`, which retires an
    // expired arm by appending `switch.expired` -- status holds no
    // `SessionOperationLock` and must never write.
    let now = runtime.now()?;
    let armed = match crate::arm::latest_unresolved(&events) {
        Some(arm) if crate::arm::is_live_at(&arm, &now)? => Some(arm),
        _ => None,
    };
    let ready = lease_state == "free" && handover_renderable && armed.is_none();
    // With an arm pending, the only switch that will be accepted is the one it
    // already names.
    let suggested_target = armed.as_ref().map(|arm| arm.to).unwrap_or(target_provider);
    Ok(serde_json::json!({
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
        "latest_narrative_checkpoint": latest_narrative,
        "events_since_narrative": events_since,
        "capture_gaps": gaps.into_iter().map(|gap| serde_json::json!({
            "sequence": gap.sequence,
            "phase": gap.phase,
            "message": gap.message,
        })).collect::<Vec<_>>(),
        "switch_readiness": {
            "ready": ready,
            "lease": lease_state,
            "lease_reason": lease_reason,
            "checkpoint_fresh": checkpoint_fresh,
            "handover_renderable": handover_renderable,
            "handover_error": handover_error,
            "armed": armed.as_ref().map(|arm| serde_json::json!({
                "to": arm.to,
                "sequence": arm.sequence,
                "expires_at": arm.expires_at,
            })),
            "suggested_switch_command": format!("handover switch {}", suggested_target.executable()),
        },
    }))
}

pub(crate) fn mcp_list_value(environment: &Environment) -> Result<serde_json::Value> {
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &cwd)?;
    crate::list::build_list_value(&layout)
}

fn status_command(json: bool, environment: &Environment, runtime: &dyn Runtime) -> Result<i32> {
    let value = build_status_value(environment, runtime)?;
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
                "deletion requires a terminal confirmation or `handover delete --yes`".into(),
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

    let child_ids = child_sessions(&layout, store.id())?;
    if !child_ids.is_empty() {
        return Err(Error::InvalidState(format!(
            "cannot delete parent session {} while child sessions remain; delete children first: {}",
            store.id(),
            child_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let terminal_operations = terminal_operations_for_deletion(&layout, store.id())?;
    let session_dir = store.session_dir();
    validate_owned_private_directory(&session_dir)?;
    let deletion_id = uuid::Uuid::new_v4();
    let mut renames = vec![(
        session_dir.clone(),
        layout
            .sessions()
            .join(format!(".deleting-{}-{deletion_id}", store.id())),
    )];
    for operation_dir in terminal_operations {
        let name = operation_dir
            .file_name()
            .expect("operation directory has a basename")
            .to_string_lossy()
            .into_owned();
        renames.push((
            operation_dir,
            layout
                .operations()
                .join(format!(".deleting-{name}-{deletion_id}")),
        ));
    }
    for (_, deleting) in &renames {
        match std::fs::symlink_metadata(deleting) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io(deleting, source)),
            Ok(_) => {
                return Err(Error::InvalidState(format!(
                    "deletion staging path {} already exists",
                    deleting.display()
                )));
            }
        }
    }
    let mut renamed = Vec::new();
    for (original, deleting) in &renames {
        if let Err(source) = std::fs::rename(original, deleting) {
            let error = io(deleting, source);
            return Err(with_rename_rollback(error, &renamed));
        }
        renamed.push((original.clone(), deleting.clone()));
        if let Some(parent) = original.parent()
            && let Err(error) = sync_directory(parent)
        {
            return Err(with_rename_rollback(error, &renamed));
        }
    }
    if let Err(error) = store.remove_binding() {
        return Err(with_rename_rollback(error, &renamed));
    }
    for (_, deleting) in &renamed {
        remove_tree_without_following(deleting)?;
        sync_directory(
            deleting
                .parent()
                .expect("deletion staging path has a parent"),
        )?;
    }
    drop(operation);
    Ok(0)
}

fn child_sessions(layout: &StateLayout, parent_id: &SessionId) -> Result<Vec<SessionId>> {
    let mut children = Vec::new();
    let entries =
        std::fs::read_dir(layout.sessions()).map_err(|source| io(layout.sessions(), source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io(layout.sessions(), source))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            Error::InvalidState(format!(
                "session directory name at {} is not UTF-8",
                entry.path().display()
            ))
        })?;
        let id = SessionId::parse(name).map_err(|error| {
            Error::InvalidState(format!("invalid session directory {name:?}: {error}"))
        })?;
        let session = SessionStore::open(layout, id.clone())?;
        if session.meta().parent_session_id.as_ref() == Some(parent_id) {
            children.push(id);
        }
    }
    children.sort_by_key(ToString::to_string);
    Ok(children)
}

fn terminal_operations_for_deletion(
    layout: &StateLayout,
    session_id: &SessionId,
) -> Result<Vec<PathBuf>> {
    let mut terminal = Vec::new();
    let mut blocking = Vec::new();
    let entries =
        std::fs::read_dir(layout.operations()).map_err(|source| io(layout.operations(), source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io(layout.operations(), source))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            Error::InvalidState(format!(
                "fork operation directory name at {} is not UTF-8",
                entry.path().display()
            ))
        })?;
        let id = crate::model::OperationId::parse(name).map_err(|error| {
            Error::InvalidState(format!(
                "invalid fork operation directory {name:?}: {error}"
            ))
        })?;
        let fork_store = ForkOperationStore::open(layout, id.clone())?;
        let fork = fork_store.operation()?;
        let names_session = &fork.source_session_id == session_id
            || fork.child_session_id.as_ref() == Some(session_id);
        let complete = matches!(
            fork.phase,
            ForkPhase::Complete | ForkPhase::RolledBack | ForkPhase::NeedsManualRecovery
        );
        if names_session && !complete {
            blocking.push(id);
        } else if &fork.source_session_id == session_id && complete {
            terminal.push(fork_store.operation_dir());
        }
    }
    if !blocking.is_empty() {
        blocking.sort_by_key(ToString::to_string);
        return Err(Error::InvalidState(format!(
            "cannot delete session {session_id} while fork operations are nonterminal: {}",
            blocking
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    terminal.sort();
    Ok(terminal)
}

fn with_rename_rollback(error: Error, renamed: &[(PathBuf, PathBuf)]) -> Error {
    let mut failures = Vec::new();
    for (original, deleting) in renamed.iter().rev() {
        if let Err(source) = std::fs::rename(deleting, original) {
            failures.push(io(original, source).to_string());
        }
    }
    for parent in renamed
        .iter()
        .filter_map(|(original, _)| original.parent())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if let Err(sync_error) = sync_directory(parent) {
            failures.push(sync_error.to_string());
        }
    }
    if failures.is_empty() {
        error
    } else {
        Error::InvalidState(format!(
            "deletion failed ({error}); rename rollback also failed ({})",
            failures.join("; ")
        ))
    }
}

fn current_session(environment: &Environment) -> Result<(StateLayout, GitSnapshot, SessionStore)> {
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &cwd)?;
    let snapshot = Git::new().snapshot(&cwd)?;
    let store = SessionStore::find_for_worktree(&layout, &snapshot.identity)?
        .ok_or_else(|| Error::InvalidState("this worktree has no Handover session".into()))?;
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
        if let Some(previous) = references.insert(sha256.clone(), *bytes)
            && previous != *bytes
        {
            return Err(Error::InvalidState(format!(
                "blob {sha256} has conflicting recorded sizes"
            )));
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

pub(crate) fn write_projection(value: &serde_json::Value, compact: bool) -> Result<()> {
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
    if environment.get("HANDOVER_RUN_ID").is_some() {
        return Err(Error::InvalidState(
            "an attached provider must use `handover checkpoint --format json --from-provider`"
                .into(),
        ));
    }
    let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
    let layout = resolve_layout(environment, &cwd)?;
    let snapshot = Git::new().snapshot(&cwd)?;
    let store = SessionStore::find_for_worktree(&layout, &snapshot.identity)?
        .ok_or_else(|| Error::InvalidState("this worktree has no Handover session".into()))?;
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
    let session_id = SessionId::parse(required_env_utf8(environment, "HANDOVER_SESSION_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid HANDOVER_SESSION_ID: {error}")))?;
    let run_id = RunId::parse(required_env_utf8(environment, "HANDOVER_RUN_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid HANDOVER_RUN_ID: {error}")))?;
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
            "a previous capture failure requires handover doctor --repair".into(),
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
        .get("HANDOVER_PROVIDER_VERSION")
        .and_then(OsStr::to_str)
        .map(str::to_owned);
    let is_stop = matches!(normalized.event, HookEvent::Stopped { .. });
    let (outcome, follow_snapshot, handover) = map_and_append_hook(
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
    if handover {
        let path = store
            .session_dir()
            .join(format!("runs/{run_id}/inbox/handover.md"));
        let bytes = read_private(&path)?;
        if bytes.len() > MAX_HANDOVER_BYTES {
            return Err(Error::InvalidState("handover exceeds 64 KiB".into()));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidState("handover is not valid UTF-8".into()))?;
        return Ok(session_start_output(text));
    }
    if is_stop
        && let Ok(events) = store.events()
        && let Some(output) = stop_nudge(&events)
    {
        return Ok(output);
    }
    Ok(HookOutput {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
    })
}

fn stop_nudge(events: &[Event]) -> Option<HookOutput> {
    let (latest_narrative, events_since) = crate::list::narrative_freshness(events);
    (events_since >= STALE_NARRATIVE_EVENT_THRESHOLD)
        .then(|| stale_narrative_output(events_since, latest_narrative.is_some()))
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
    let (key, kind, snapshot, handover) = match normalized.event {
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
    let key = key.map(|key| hook_idempotency_key(run_id, provider, &key));
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
    Ok((outcome, snapshot, handover))
}

/// Scope a hook idempotency key to the run and provider that reported it.
///
/// Hook keys are built from provider-supplied ids — `native_session_id` and
/// `tool_use_id` — which are only unique inside one provider attachment. A
/// resumed conversation reports the same native session id again (`claude
/// --resume`, `claude --session-id`), and nothing makes a tool use id unique
/// across separate provider sessions. The journal spans every run and provider
/// in the session, so an unscoped key would make the second run's SessionStart
/// collide with the first run's and hard-fail the whole run. Deduplication
/// still holds where it is needed — repeated hooks inside one run — because
/// the run id and provider are fixed for the life of a run.
fn hook_idempotency_key(run_id: &RunId, provider: Provider, key: &str) -> String {
    format!("{run_id}:{}:{key}", provider.executable())
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
            idempotency_key: Some(hook_idempotency_key(
                run_id,
                provider,
                &format!("pre:{tool_use_id}"),
            )),
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
    root: PathBuf,
    inbox: PathBuf,
    checkpoints: PathBuf,
    handover: PathBuf,
}

fn prepare_run_directory(
    store: &SessionStore,
    run_id: &RunId,
    handover_contents: &[u8],
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
    let handover = inbox.join("handover.md");
    create_private(&handover, handover_contents)?;
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
        root: final_path,
        inbox: final_inbox.clone(),
        checkpoints: final_checkpoints,
        handover: final_inbox.join("handover.md"),
    })
}

fn resolve_layout(environment: &Environment, cwd: &Path) -> Result<StateLayout> {
    let layout = StateLayout::from_environment_at(environment, cwd)?;
    layout.ensure()?;
    layout.canonicalized()
}

fn resolve_provider_home(provider: Provider, environment: &Environment) -> Option<PathBuf> {
    if provider != Provider::Codex {
        return None;
    }
    if let Some(home) = environment
        .get("CODEX_HOME")
        .filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(home));
    }
    let home = environment.get("HOME").filter(|value| !value.is_empty())?;
    Some(PathBuf::from(home).join(".codex"))
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
    let session = SessionId::parse(required_env_utf8(environment, "HANDOVER_SESSION_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid HANDOVER_SESSION_ID: {error}")))?;
    let run = RunId::parse(required_env_utf8(environment, "HANDOVER_RUN_ID")?)
        .map_err(|error| Error::InvalidState(format!("invalid HANDOVER_RUN_ID: {error}")))?;
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

    use super::{
        build_handover_value, classify_lease, command_facts, confirm_lease_recovery,
        latest_narrative_checkpoint, recover_stale_lease_for_switch, resolve_provider_home,
        stop_nudge, with_rename_rollback,
    };
    use crate::error::Error;
    use crate::handover::{CaptureGap, RenderedHandover};
    use crate::model::{
        Checkpoint, CheckpointAuthor, CheckpointKind, ContentRef, Event, EventKind, GitSnapshot,
        NarrativeInput, Provider, RunId, SessionId, WorktreeIdentity,
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

        fn operation_id(&self) -> crate::model::OperationId {
            crate::model::OperationId::parse("33333333-3333-4333-8333-333333333333").unwrap()
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
    fn build_handover_value_embeds_the_rendered_markdown_and_metadata() {
        let (_temp, store) = store_fixture();
        let events = vec![observed_event(
            1,
            EventKind::ProviderStopObserved {
                native_session_id: "native".into(),
            },
        )];
        let checkpoint = Checkpoint {
            schema_version: 1,
            checkpoint_kind: CheckpointKind::Narrative,
            through_sequence: 0,
            author: CheckpointAuthor::Human,
            narrative: None,
            narrative_checkpoint_sequence: None,
        };
        let rendered = RenderedHandover {
            markdown: "# Handover\n".into(),
            recent_event_sequences: vec![1],
            omitted: false,
        };
        let gaps = vec![CaptureGap {
            sequence: 1,
            phase: "capture".into(),
            message: "gap".into(),
        }];

        let value = build_handover_value(
            &store,
            Some(Provider::Claude),
            Provider::Codex,
            2,
            1,
            &events,
            Some((1, checkpoint)),
            gaps,
            &rendered,
        );

        assert_eq!(value["from_provider"], "claude");
        assert_eq!(value["to_provider"], "codex");
        assert_eq!(value["markdown"], "# Handover\n");
        assert_eq!(value["markdown_bytes"], "# Handover\n".len());
        assert_eq!(value["omitted"], false);
        assert_eq!(value["narrative_checkpoint"]["sequence"], 1);
        assert_eq!(value["narrative_checkpoint"]["events_since"], 0);
        assert_eq!(value["capture_gaps"][0]["message"], "gap");
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

        let recovery_snapshot = recorded_snapshot(&store);
        recover_stale_lease_for_switch(
            &store,
            &leases,
            &FixedRuntime,
            &recovery_snapshot,
            Provider::Codex,
            true,
            std::io::Cursor::new(Vec::new()),
            false,
        )
        .unwrap();

        assert!(leases.read().unwrap().is_none());
        let events = store.events().unwrap();
        let recovered_index = events
            .iter()
            .position(|event| {
                event.run_id.as_ref() == Some(&stale_run)
                    && matches!(
                        &event.kind,
                        EventKind::RunRecovered {
                            supervisor_pid,
                            supervisor_start_token,
                            child_pid: None,
                            child_start_token: None,
                            host,
                            reason,
                        } if *supervisor_pid == u32::MAX
                            && supervisor_start_token == "gone"
                            && host == &stale.host
                            && reason.contains("--recover-lease")
                    )
            })
            .unwrap();
        assert!(matches!(
            events[recovered_index + 1].kind,
            EventKind::GitSnapshot { .. }
        ));

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
        let foreign_error = recover_stale_lease_for_switch(
            &store,
            &leases,
            &FixedRuntime,
            &recovery_snapshot,
            Provider::Codex,
            true,
            std::io::Cursor::new(Vec::new()),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(foreign_error.contains("different-host"));
        assert!(foreign_error.contains("claude"));
        assert!(foreign_error.contains("pid 4294967295"));
        assert!(foreign_error.contains("liveness cannot be checked from this host"));
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
        let live_error = recover_stale_lease_for_switch(
            &store,
            &leases,
            &FixedRuntime,
            &recovery_snapshot,
            Provider::Codex,
            true,
            std::io::Cursor::new(Vec::new()),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(live_error.contains("codex"));
        assert!(live_error.contains(&format!("pid {}", std::process::id())));
        assert!(live_error.contains("retry the switch"));
        assert_eq!(leases.read().unwrap().unwrap().run_id, live.run_id);
    }

    #[test]
    fn confirm_lease_recovery_skips_the_prompt_with_the_flag() {
        let (_temp, store) = store_fixture();
        let lease = RunLease::new(
            store.id().clone(),
            RunId::new(),
            Provider::Claude,
            ProcessIdentity {
                pid: u32::MAX,
                start_token: "gone".into(),
            },
        )
        .unwrap();
        let reason = confirm_lease_recovery(
            &lease,
            Provider::Codex,
            true,
            std::io::Cursor::new(Vec::new()),
            false,
        )
        .unwrap();
        assert_eq!(reason, "recovery confirmed via --recover-lease");
    }

    #[test]
    fn confirm_lease_recovery_requires_a_terminal_without_the_flag() {
        let (_temp, store) = store_fixture();
        let lease = RunLease::new(
            store.id().clone(),
            RunId::new(),
            Provider::Claude,
            ProcessIdentity {
                pid: u32::MAX,
                start_token: "gone".into(),
            },
        )
        .unwrap();
        let error = confirm_lease_recovery(
            &lease,
            Provider::Codex,
            false,
            std::io::Cursor::new(Vec::new()),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("--recover-lease"));
        assert!(error.contains("handover switch codex --recover-lease"));
        assert!(error.contains("claude"));
    }

    #[test]
    fn confirm_lease_recovery_accepts_an_interactive_yes() {
        let (_temp, store) = store_fixture();
        let lease = RunLease::new(
            store.id().clone(),
            RunId::new(),
            Provider::Claude,
            ProcessIdentity {
                pid: u32::MAX,
                start_token: "gone".into(),
            },
        )
        .unwrap();
        for answer in ["y\n", "yes\n", "Y\n", " YES \n"] {
            let reason = confirm_lease_recovery(
                &lease,
                Provider::Codex,
                false,
                std::io::Cursor::new(answer.as_bytes().to_vec()),
                true,
            )
            .unwrap();
            assert_eq!(reason, "recovery confirmed interactively");
        }
    }

    #[test]
    fn confirm_lease_recovery_rejects_anything_else_interactively() {
        let (_temp, store) = store_fixture();
        let lease = RunLease::new(
            store.id().clone(),
            RunId::new(),
            Provider::Claude,
            ProcessIdentity {
                pid: u32::MAX,
                start_token: "gone".into(),
            },
        )
        .unwrap();
        for answer in ["n\n", "\n", "nope\n"] {
            let error = confirm_lease_recovery(
                &lease,
                Provider::Codex,
                false,
                std::io::Cursor::new(answer.as_bytes().to_vec()),
                true,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("not recovered"));
        }
    }

    #[test]
    fn classify_lease_distinguishes_free_recoverable_and_blocked() {
        let (_temp, store) = store_fixture();
        let leases = LeaseStore::new(&store.session_dir());

        let (state, reason) = classify_lease(&leases).unwrap();
        assert_eq!(state, "free");
        assert!(reason.is_none());

        let dead = RunLease::new(
            store.id().clone(),
            RunId::new(),
            Provider::Claude,
            ProcessIdentity {
                pid: u32::MAX,
                start_token: "gone".into(),
            },
        )
        .unwrap();
        leases.create(&dead).unwrap();
        let (state, reason) = classify_lease(&leases).unwrap();
        assert_eq!(state, "recoverable");
        assert!(reason.unwrap().contains("--recover-lease"));
        leases.clear(&dead.run_id).unwrap();

        let live = RunLease::new(
            store.id().clone(),
            RunId::new(),
            Provider::Claude,
            ProcessIdentity::capture(std::process::id()).unwrap(),
        )
        .unwrap();
        leases.create(&live).unwrap();
        let (state, reason) = classify_lease(&leases).unwrap();
        assert_eq!(state, "blocked");
        assert!(
            reason
                .unwrap()
                .contains(&format!("pid {}", std::process::id()))
        );
        leases.clear(&live.run_id).unwrap();

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
        let (state, reason) = classify_lease(&leases).unwrap();
        assert_eq!(state, "blocked");
        assert!(reason.unwrap().contains("different-host"));
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

    #[test]
    fn deletion_rename_rollback_restores_every_staged_directory() {
        let temp = TempDir::new().unwrap();
        let sessions = temp.path().join("sessions");
        let operations = temp.path().join("operations");
        std::fs::create_dir(&sessions).unwrap();
        std::fs::create_dir(&operations).unwrap();
        let originals = [
            sessions.join("session"),
            operations.join("one"),
            operations.join("two"),
        ];
        let renamed = originals
            .iter()
            .enumerate()
            .map(|(index, original)| {
                std::fs::create_dir(original).unwrap();
                let deleting = original
                    .parent()
                    .unwrap()
                    .join(format!(".deleting-{index}"));
                std::fs::rename(original, &deleting).unwrap();
                (original.clone(), deleting)
            })
            .collect::<Vec<_>>();

        let error =
            with_rename_rollback(Error::InvalidState("injected ref failure".into()), &renamed);

        assert!(error.to_string().contains("injected ref failure"));
        assert!(
            renamed
                .iter()
                .all(|(original, deleting)| original.exists() && !deleting.exists())
        );
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

    fn recorded_snapshot(store: &SessionStore) -> GitSnapshot {
        store
            .events()
            .unwrap()
            .into_iter()
            .find_map(|event| match event.kind {
                EventKind::GitSnapshot { snapshot } => Some(snapshot),
                _ => None,
            })
            .unwrap()
    }

    fn observed_event(sequence: u64, kind: EventKind) -> Event {
        Event {
            schema_version: 1,
            sequence,
            occurred_at: format!("2026-07-21T10:00:{:02}Z", sequence % 60),
            recorded_at: format!("2026-07-21T10:00:{:02}Z", sequence % 60),
            session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            run_id: None,
            provider: Some(Provider::Claude),
            idempotency_key: None,
            kind,
        }
    }

    fn stop_event(sequence: u64) -> Event {
        observed_event(
            sequence,
            EventKind::ProviderStopObserved {
                native_session_id: "native".into(),
            },
        )
    }

    fn narrative_checkpoint_event(sequence: u64) -> Event {
        observed_event(
            sequence,
            EventKind::CheckpointCreated {
                checkpoint_kind: CheckpointKind::Narrative,
                through_sequence: sequence - 1,
                path: format!("checkpoints/{sequence:012}.json"),
            },
        )
    }

    #[test]
    fn stop_nudge_fires_at_twenty_stale_events_and_not_below() {
        let below: Vec<Event> = (1..=19).map(stop_event).collect();
        assert!(stop_nudge(&below).is_none());

        let at: Vec<Event> = (1..=20).map(stop_event).collect();
        let nudge = stop_nudge(&at).unwrap();
        assert!(
            nudge
                .stdout
                .contains("20 events and no narrative checkpoint yet")
        );

        let mut checkpointed: Vec<Event> = vec![narrative_checkpoint_event(1)];
        checkpointed.extend((2..=20).map(stop_event));
        assert!(stop_nudge(&checkpointed).is_none());

        checkpointed.push(stop_event(21));
        let stale = stop_nudge(&checkpointed).unwrap();
        assert!(
            stale
                .stdout
                .contains("20 events since the last narrative checkpoint")
        );
    }

    #[test]
    fn resolve_provider_home_prefers_codex_home_over_derived_default() {
        let with_override =
            crate::store::Environment::from_pairs(std::collections::HashMap::from([
                ("CODEX_HOME", std::ffi::OsString::from("/custom/codex-home")),
                ("HOME", std::ffi::OsString::from("/home/dev")),
            ]));
        assert_eq!(
            resolve_provider_home(Provider::Codex, &with_override),
            Some(PathBuf::from("/custom/codex-home"))
        );

        let default_only = crate::store::Environment::from_pairs(std::collections::HashMap::from(
            [("HOME", std::ffi::OsString::from("/home/dev"))],
        ));
        assert_eq!(
            resolve_provider_home(Provider::Codex, &default_only),
            Some(PathBuf::from("/home/dev/.codex"))
        );

        let neither = crate::store::Environment::from_pairs(std::collections::HashMap::new());
        assert_eq!(resolve_provider_home(Provider::Codex, &neither), None);
    }

    #[test]
    fn resolve_provider_home_is_none_for_providers_other_than_codex() {
        let with_codex_home_set =
            crate::store::Environment::from_pairs(std::collections::HashMap::from([
                ("CODEX_HOME", std::ffi::OsString::from("/custom/codex-home")),
                ("HOME", std::ffi::OsString::from("/home/dev")),
            ]));
        assert_eq!(
            resolve_provider_home(Provider::Claude, &with_codex_home_set),
            None
        );
    }
}
