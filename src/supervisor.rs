use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use signal_hook::iterator::{Handle, Signals};

use crate::error::{Error, Result};
use crate::model::{EventKind, RunId};
use crate::provider::LaunchSpec;
use crate::store::SessionStore;
use crate::store::lease::{LeaseStore, ProcessIdentity};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExitFacts {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisionOutcome {
    pub facts: ExitFacts,
    pub handshake_completed: bool,
    pub startup_failure: Option<String>,
}

pub struct Supervisor;

impl Supervisor {
    pub fn launch(
        spec: LaunchSpec,
        store: &SessionStore,
        run_id: &RunId,
        deadline: Duration,
    ) -> Result<SupervisionOutcome> {
        let child = Command::new(&spec.program)
            .args(&spec.args)
            .envs(&spec.env)
            .current_dir(&spec.cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| {
                Error::Command(format!("cannot launch {:?}: {error}", spec.program))
            })?;
        let mut child = ChildGuard::new(child);
        let child_identity = ProcessIdentity::capture(child.id())?;
        LeaseStore::new(&store.session_dir()).update_child(run_id, child_identity.clone())?;
        let _signals = SignalForwarder::start(child_identity)?;

        let started = Instant::now();
        loop {
            let handshook = store.events()?.iter().any(|event| {
                event.run_id.as_ref() == Some(run_id)
                    && matches!(&event.kind, EventKind::RunHandshake { .. })
            });
            if handshook {
                let status = child.wait().map_err(|error| {
                    Error::Command(format!("cannot wait for provider: {error}"))
                })?;
                return Ok(SupervisionOutcome {
                    facts: exit_facts(status),
                    handshake_completed: true,
                    startup_failure: None,
                });
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|error| Error::Command(format!("cannot poll provider: {error}")))?
            {
                return Ok(SupervisionOutcome {
                    facts: exit_facts(status),
                    handshake_completed: false,
                    startup_failure: Some("provider exited before SessionStart handshake".into()),
                });
            }
            if started.elapsed() >= deadline {
                let _ = child.kill();
                let status = child.wait().map_err(|error| {
                    Error::Command(format!(
                        "cannot reap provider after handshake timeout: {error}"
                    ))
                })?;
                return Ok(SupervisionOutcome {
                    facts: exit_facts(status),
                    handshake_completed: false,
                    startup_failure: Some(
                        "provider did not complete SessionStart within 60 seconds".into(),
                    ),
                });
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn id(&self) -> u32 {
        self.child.as_ref().expect("child has not been reaped").id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let status = self
            .child
            .as_mut()
            .expect("child has not been reaped")
            .try_wait()?;
        if status.is_some() {
            self.child.take();
        }
        Ok(status)
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.child
            .as_mut()
            .expect("child has not been reaped")
            .kill()
    }

    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        let status = self
            .child
            .as_mut()
            .expect("child has not been reaped")
            .wait();
        if status.is_ok() {
            self.child.take();
        }
        status
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        self.child.take();
    }
}

struct SignalForwarder {
    handle: Handle,
    thread: Option<JoinHandle<()>>,
}

impl SignalForwarder {
    fn start(child: ProcessIdentity) -> Result<Self> {
        let mut signals = Signals::new([libc::SIGTERM, libc::SIGHUP]).map_err(|error| {
            Error::Command(format!("cannot install signal forwarding: {error}"))
        })?;
        let handle = signals.handle();
        let thread = std::thread::spawn(move || {
            for signal in signals.forever() {
                match child.is_live() {
                    Ok(true) => {
                        // SAFETY: the PID was re-captured and matched the original process start
                        // identity immediately before signaling. A residual check-to-signal PID
                        // reuse race remains unavoidable with portable Unix process APIs.
                        let result = unsafe { libc::kill(child.pid as i32, signal) };
                        if result != 0 {
                            break;
                        }
                    }
                    Ok(false) | Err(_) => break,
                }
            }
        });
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
}

impl Drop for SignalForwarder {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn exit_facts(status: ExitStatus) -> ExitFacts {
    ExitFacts {
        exit_code: status.code(),
        signal: status.signal(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::Supervisor;
    use crate::model::{EventKind, GitSnapshot, Provider, RunId, SessionId, WorktreeIdentity};
    use crate::provider::LaunchSpec;
    use crate::runtime::Runtime;
    use crate::store::lease::{LeaseStore, ProcessIdentity, RunLease};
    use crate::store::{SessionStore, StateLayout};

    static SUPERVISOR_TEST_LOCK: Mutex<()> = Mutex::new(());
    const TEST_DEADLINE: Duration = Duration::from_secs(10);

    struct FixedRuntime;

    impl Runtime for FixedRuntime {
        fn now(&self) -> crate::error::Result<String> {
            Ok("2026-07-16T10:00:00Z".into())
        }

        fn session_id(&self) -> SessionId {
            SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap()
        }

        fn run_id(&self) -> RunId {
            RunId::new()
        }
    }

    #[test]
    fn provider_handshake_and_exit_facts_leave_lease_for_caller_commit() {
        let _serial = SUPERVISOR_TEST_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let sentinel = fixture.temp.path().join("handshake");
        let callback =
            fixture.write_executable("callback.sh", "#!/bin/sh\ntouch \"$HANDSHAKE_SENTINEL\"\n");
        let provider = fixture.write_executable(
            "provider.sh",
            "#!/bin/sh\n\"$CALLBACK_BIN\"\nsleep 0.15\nexit 23\n",
        );
        let store_for_callback = fixture.store.clone();
        let run_for_callback = fixture.run_id.clone();
        let sentinel_for_callback = sentinel.clone();
        let callback_thread = std::thread::spawn(move || {
            wait_for_path(&sentinel_for_callback);
            store_for_callback
                .append(
                    &FixedRuntime,
                    Some(run_for_callback),
                    Some(Provider::Claude),
                    EventKind::RunHandshake {
                        native_session_id: "native".into(),
                        provider_version: Some("fake 1.0".into()),
                    },
                )
                .unwrap();
        });
        let spec = fixture.spec(
            &provider,
            BTreeMap::from([
                (OsString::from("CALLBACK_BIN"), callback.into_os_string()),
                (
                    OsString::from("HANDSHAKE_SENTINEL"),
                    sentinel.into_os_string(),
                ),
            ]),
        );

        let outcome =
            Supervisor::launch(spec, &fixture.store, &fixture.run_id, TEST_DEADLINE).unwrap();
        callback_thread.join().unwrap();

        assert!(outcome.handshake_completed);
        assert_eq!(outcome.facts.exit_code, Some(23));
        assert_eq!(outcome.facts.signal, None);
        let lease_store = LeaseStore::new(&fixture.store.session_dir());
        assert!(lease_store.read().unwrap().unwrap().child.is_some());
        fixture
            .store
            .append(
                &FixedRuntime,
                Some(fixture.run_id.clone()),
                Some(Provider::Claude),
                EventKind::RunStopped {
                    exit_code: outcome.facts.exit_code,
                    signal: outcome.facts.signal,
                },
            )
            .unwrap();
        lease_store.clear(&fixture.run_id).unwrap();
        assert!(lease_store.read().unwrap().is_none());
    }

    #[test]
    fn exit_before_handshake_returns_observed_facts() {
        let _serial = SUPERVISOR_TEST_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let provider = fixture.write_executable("provider.sh", "#!/bin/sh\nsleep 0.05\nexit 17\n");

        let outcome = Supervisor::launch(
            fixture.spec(&provider, BTreeMap::new()),
            &fixture.store,
            &fixture.run_id,
            TEST_DEADLINE,
        )
        .unwrap();

        assert!(!outcome.handshake_completed);
        assert_eq!(outcome.facts.exit_code, Some(17));
        assert!(
            outcome
                .startup_failure
                .unwrap()
                .contains("before SessionStart")
        );
    }

    #[test]
    fn handshake_timeout_kills_and_reaps_with_exit_facts() {
        let _serial = SUPERVISOR_TEST_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let provider = fixture.write_executable("provider.sh", "#!/bin/sh\nsleep 5\nexit 0\n");

        let outcome = Supervisor::launch(
            fixture.spec(&provider, BTreeMap::new()),
            &fixture.store,
            &fixture.run_id,
            Duration::from_millis(50),
        )
        .unwrap();

        assert!(!outcome.handshake_completed);
        assert!(outcome.facts.exit_code.is_none());
        assert!(outcome.facts.signal.is_some());
        assert!(outcome.startup_failure.unwrap().contains("SessionStart"));
        let child = LeaseStore::new(&fixture.store.session_dir())
            .read()
            .unwrap()
            .unwrap()
            .child
            .unwrap();
        assert!(!child.is_live().unwrap());
    }

    #[test]
    fn journal_read_failure_still_kills_and_reaps_the_child() {
        let _serial = SUPERVISOR_TEST_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let sentinel = fixture.temp.path().join("break-journal");
        let provider = fixture.write_executable(
            "provider.sh",
            "#!/bin/sh\nsleep 0.05\ntouch \"$BREAK_SENTINEL\"\nsleep 5\n",
        );
        let events = fixture.store.session_dir().join("events.jsonl");
        let sentinel_for_thread = sentinel.clone();
        let breaker = std::thread::spawn(move || {
            wait_for_path(&sentinel_for_thread);
            std::fs::set_permissions(&events, std::fs::Permissions::from_mode(0o400)).unwrap();
        });

        let result = Supervisor::launch(
            fixture.spec(
                &provider,
                BTreeMap::from([(OsString::from("BREAK_SENTINEL"), sentinel.into_os_string())]),
            ),
            &fixture.store,
            &fixture.run_id,
            TEST_DEADLINE,
        );
        breaker.join().unwrap();
        std::fs::set_permissions(
            fixture.store.session_dir().join("events.jsonl"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        assert!(result.is_err());
        let child = LeaseStore::new(&fixture.store.session_dir())
            .read()
            .unwrap()
            .unwrap()
            .child
            .unwrap();
        assert!(!child.is_live().unwrap());
    }

    struct Fixture {
        temp: TempDir,
        store: SessionStore,
        run_id: RunId,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let layout = StateLayout::new(temp.path().join("state"));
            let common_git_dir = temp.path().join("repo/.git");
            let git_dir = common_git_dir.clone();
            let snapshot = GitSnapshot {
                identity: WorktreeIdentity {
                    key: WorktreeIdentity::derive_key(&common_git_dir, &git_dir),
                    common_git_dir,
                    git_dir,
                    worktree: temp.path().join("repo"),
                    cwd_relative: PathBuf::new(),
                },
                branch: Some("main".into()),
                head: "deadbeef".into(),
                staged: Vec::new(),
                unstaged: Vec::new(),
                untracked: Vec::new(),
                dirty_submodules: Vec::new(),
            };
            let store = SessionStore::create(&layout, &FixedRuntime, snapshot).unwrap();
            let run_id = RunId::new();
            let lease = RunLease::new(
                store.id().clone(),
                run_id.clone(),
                Provider::Claude,
                ProcessIdentity::capture(std::process::id()).unwrap(),
            )
            .unwrap();
            LeaseStore::new(&store.session_dir())
                .create(&lease)
                .unwrap();
            Self {
                temp,
                store,
                run_id,
            }
        }

        fn write_executable(&self, name: &str, body: &str) -> PathBuf {
            let path = self.temp.path().join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn spec(&self, program: &Path, env: BTreeMap<OsString, OsString>) -> LaunchSpec {
            LaunchSpec {
                program: program.as_os_str().to_owned(),
                args: Vec::new(),
                env,
                cwd: self.temp.path().to_path_buf(),
            }
        }
    }

    fn wait_for_path(path: &Path) {
        let started = Instant::now();
        while !path.exists() {
            assert!(started.elapsed() < TEST_DEADLINE);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
