use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::checkpoint::{
    edit_narrative, promote_inbox, read_narrative_json, submit_provider_narrative,
};
use crate::cli::{CheckpointFormat, Cli, Command};
use crate::error::{Error, Result, io};
use crate::git::Git;
use crate::model::{CheckpointAuthor, ContentRef, EventKind, Provider, RunId, SessionId};
use crate::provider::hook::{
    HookEvent, HookOutput, NormalizedHook, capture_failure_output, normalize, session_start_output,
};
use crate::provider::{LaunchContext, adapter};
use crate::runtime::Runtime;
use crate::store::atomic::{create_private, read_private, sync_directory};
use crate::store::blob::BlobStore;
use crate::store::journal::{AppendOutcome, EventJournal, PendingEvent, PendingEventMeta};
use crate::store::lease::{LeaseStore, ProcessIdentity, RunLease, SessionOperationLock};
use crate::store::{Environment, SessionStore, StateLayout};
use crate::supervisor::{ExitFacts, Supervisor};

const MAX_HOOK_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HANDOFF_BYTES: usize = 65_536;

pub fn run(cli: Cli, environment: &Environment, runtime: &dyn Runtime) -> Result<i32> {
    match cli.command {
        Command::Run {
            provider,
            provider_args,
        } => run_command(provider, provider_args, environment, runtime),
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
    let run_paths = prepare_run_directory(&store, &run_id)?;
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
        if cwd_relative != store.meta().worktree.cwd_relative {
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

fn prepare_run_directory(store: &SessionStore, run_id: &RunId) -> Result<RunPaths> {
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
    create_private(
        &handoff,
        b"# Sesh handoff\n\nThis is the first provider run in this session. Continue from the current Git worktree and user prompt.\n",
    )?;
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
