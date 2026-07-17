# Sesh V1 Provider Switching Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the local Rust CLI that records a provider-neutral coding session and proves `sesh run claude` can hand the same worktree, cwd, facts, checkpoint, and failure to `sesh switch codex`.

**Architecture:** A single binary owns an append-only JSONL store outside the repository. Git observation, provider hook normalization, checkpoint promotion, handoff rendering, and child-process supervision are isolated behind narrow Rust modules. The first acceptance path uses deterministic Bash provider fixtures; real Claude and Codex adapters use the same interfaces and documented lifecycle hooks.

**Tech Stack:** Rust 2024, Clap, Serde/serde_json, SHA-256, UUIDs, `fs2` file locks, `time`, `signal-hook`, real Git subprocesses, Bash provider fixtures, `assert_cmd`, and `tempfile`.

---

## Scope and sequencing

This plan implements every approved V1 behavior except `sesh fork`. Forking is isolated in `2026-07-16-sesh-v1-worktree-fork.md` because it has a separate Git transaction and recovery model.

Work only in `/Users/thomasindrias/private/sesh-v1-foundation` on branch `feat/v1-foundation`. Read `docs/superpowers/specs/2026-07-16-sesh-v1-design.md` before Task 1. Use `rtk` for every shell command.

Do not call a real model in automated tests. Real-provider smoke tests are ignored and opt-in.

## File structure

The completed core plan uses this structure:

```text
Cargo.toml                         package and dependency contract
rust-toolchain.toml               stable Rust selection
.gitignore                        Rust and local test artifacts
.github/workflows/ci.yml          macOS/Linux verification
src/main.rs                       error-to-exit-code boundary
src/lib.rs                        module exports and command dispatch
src/cli.rs                        public and hidden CLI grammar
src/error.rs                      typed user-facing failures
src/runtime.rs                    injected clock, IDs, and process identity
src/model/
  mod.rs                          model exports
  ids.rs                          SessionId and RunId
  provider.rs                     Provider enum
  git.rs                          worktree and snapshot facts
  event.rs                        normalized append-only event schema
  checkpoint.rs                   narrative and transition checkpoints
src/store/
  mod.rs                          StateLayout and SessionStore facade
  atomic.rs                       private files and atomic replacement
  journal.rs                      locked JSONL append and recovery
  refs.rs                         worktree/checkpoint refs
  blob.rs                         session-scoped content-addressed output
  lease.rs                        active provider lease
src/git/
  mod.rs                          Git facade
  command.rs                      argument-vector Git runner
  observe.rs                      worktree discovery and dirty snapshot
src/provider/
  mod.rs                          adapter contract and registry
  hook.rs                         normalized hook ingestion/output
  claude.rs                       Claude launch/plugin adapter
  codex.rs                        Codex launch/config adapter
  assets/claude-plugin.json       embedded Claude plugin manifest
  assets/claude-hooks.json        embedded Claude hook definitions
src/checkpoint.rs                 validation and inbox promotion
src/handoff.rs                    deterministic bounded Markdown renderer
src/supervisor.rs                 lease, child process, handshake, and exit
src/app.rs                        run/switch/read/delete/setup/doctor orchestration
tests/support/mod.rs              temporary Git and fake-provider helpers
tests/cli_contract.rs             CLI surface
tests/git_observer.rs             real Git observation
tests/hook_contract.rs            sanitized provider hook fixtures
tests/north_star.rs               Claude-to-Codex acceptance path
tests/recovery.rs                 journal, lease, and process failures
tests/fixtures/hooks/             provider JSON fixtures
README.md                         install, trust, workflow, and privacy boundary
```

Keep provider JSON parsing out of `model`, Git commands out of `app`, and filesystem writes out of adapters.

### Task 1: Bootstrap a warning-clean Rust CLI

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `.gitignore`
- Create: `src/main.rs`
- Create: `src/lib.rs`
- Create: `src/cli.rs`
- Test: `tests/cli_contract.rs`

- [x] **Step 1: Create the manifest and failing CLI contract test**

Create `Cargo.toml`:

```toml
[package]
name = "sesh"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
description = "A local-first session layer for coding agents"

[dependencies]
clap = { version = "4.5", features = ["derive"] }
fs2 = "0.4"
hex = "0.4"
libc = "0.2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
shell-words = "1"
signal-hook = "0.3"
thiserror = "2"
time = { version = "0.3", features = ["formatting", "macros"] }
uuid = { version = "1", features = ["serde", "v4"] }

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
pretty_assertions = "1"
tempfile = "3"
```

Create `rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]
profile = "minimal"
```

Create `.gitignore`:

```gitignore
/target/
/.sesh-test/
```

Create `tests/cli_contract.rs`:

```rust
use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn help_identifies_the_product() {
    cargo_bin_cmd!("sesh")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Switch coding providers without losing your place",
        ));
}

#[test]
fn version_comes_from_the_package() {
    cargo_bin_cmd!("sesh")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("sesh 0.1.0"));
}
```

- [x] **Step 2: Run the test and verify the binary target is missing**

Run: `rtk cargo test --test cli_contract`

Expected: FAIL because `src/main.rs` and the `sesh` binary do not exist.

- [x] **Step 3: Add the minimal CLI implementation**

Create `src/cli.rs`:

```rust
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "sesh",
    version,
    about = "Switch coding providers without losing your place"
)]
pub struct Cli {}
```

Create `src/lib.rs`:

```rust
pub mod cli;
```

Create `src/main.rs`:

```rust
use clap::Parser;
use sesh::cli::Cli;

fn main() {
    let _ = Cli::parse();
}
```

- [x] **Step 4: Verify the bootstrap is clean**

Run:

```bash
rtk cargo fmt --check
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo test --test cli_contract
```

Expected: all commands PASS.

- [x] **Step 5: Commit the bootstrap**

```bash
rtk git add Cargo.toml Cargo.lock rust-toolchain.toml .gitignore src tests/cli_contract.rs
rtk git commit -m "chore: bootstrap sesh CLI"
```

### Task 2: Resolve private local storage deterministically

**Files:**
- Create: `src/error.rs`
- Create: `src/store/mod.rs`
- Create: `src/store/atomic.rs`
- Modify: `src/lib.rs`
- Test: `src/store/mod.rs`
- Test: `src/store/atomic.rs`

- [x] **Step 1: Write failing tests for state precedence and permissions**

Add these tests to `src/store/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::{Environment, StateLayout};

    #[test]
    fn sesh_home_wins_over_xdg_and_home() {
        let env = Environment::from_pairs(HashMap::from([
            ("SESH_HOME", OsString::from("/state/explicit")),
            ("XDG_STATE_HOME", OsString::from("/state/xdg")),
            ("HOME", OsString::from("/home/dev")),
        ]));

        assert_eq!(
            StateLayout::from_environment(&env).unwrap().root(),
            std::path::Path::new("/state/explicit")
        );
    }

    #[test]
    fn xdg_then_home_are_the_fallbacks() {
        let xdg = Environment::from_pairs(HashMap::from([
            ("XDG_STATE_HOME", OsString::from("/state/xdg")),
            ("HOME", OsString::from("/home/dev")),
        ]));
        let home = Environment::from_pairs(HashMap::from([(
            "HOME",
            OsString::from("/home/dev"),
        )]));

        assert_eq!(
            StateLayout::from_environment(&xdg).unwrap().root(),
            std::path::Path::new("/state/xdg/sesh")
        );
        assert_eq!(
            StateLayout::from_environment(&home).unwrap().root(),
            std::path::Path::new("/home/dev/.local/state/sesh")
        );
    }

    #[test]
    fn ensure_creates_a_user_only_root() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));

        layout.ensure().unwrap();

        let mode = std::fs::metadata(layout.root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn ensure_refuses_a_symlinked_state_root() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let root = temp.path().join("state");
        symlink(&target, &root).unwrap();

        assert!(StateLayout::new(root).ensure().is_err());
    }

    #[test]
    fn ensure_refuses_an_existing_group_readable_state_root() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o750)).unwrap();

        assert!(StateLayout::new(root).ensure().is_err());
    }

    #[test]
    fn relative_sesh_home_is_resolved_once_against_the_launch_cwd() {
        let temp = TempDir::new().unwrap();
        let cwd = temp.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let env = Environment::from_pairs(HashMap::from([(
            "SESH_HOME",
            OsString::from("../state"),
        )]));

        let layout = StateLayout::from_environment_at(&env, &cwd).unwrap();
        layout.ensure().unwrap();
        let canonical = layout.canonicalized().unwrap();

        assert_eq!(canonical.root(), temp.path().join("state"));
    }

    #[test]
    fn ensure_refuses_an_unknown_state_format() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        layout.ensure().unwrap();
        std::fs::write(layout.root().join("FORMAT"), b"sesh-state 999\n").unwrap();

        assert!(layout.ensure().is_err());
    }
}
```

Add this test to `src/store/atomic.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::{create_private, replace_private};

    #[test]
    fn replacement_is_complete_and_private() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("ref.json");

        replace_private(&path, b"first").unwrap();
        replace_private(&path, b"second").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn immutable_create_never_replaces_an_existing_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("checkpoint.json");

        create_private(&path, b"first").unwrap();
        assert!(create_private(&path, b"second").is_err());

        assert_eq!(std::fs::read(path).unwrap(), b"first");
    }
}
```

- [x] **Step 2: Run the focused tests and verify the modules are missing**

Run: `rtk cargo test store::`

Expected: FAIL because `store`, `Environment`, `StateLayout`, and `replace_private` do not exist.

- [x] **Step 3: Implement the state layout and typed errors**

Create `src/error.rs`:

```rust
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HOME is not set and neither SESH_HOME nor XDG_STATE_HOME is available")]
    StateHomeUnavailable,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid local state: {0}")]
    InvalidState(String),
    #[error("command failed: {0}")]
    Command(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}
```

Create `src/store/mod.rs`:

```rust
pub mod atomic;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Result, io};

#[derive(Clone, Debug, Default)]
pub struct Environment {
    values: HashMap<String, OsString>,
}

impl Environment {
    pub fn capture() -> Self {
        Self {
            values: std::env::vars_os()
                .map(|(key, value)| (key.to_string_lossy().into_owned(), value))
                .collect(),
        }
    }

    pub fn from_pairs(values: HashMap<&str, OsString>) -> Self {
        Self {
            values: values
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.values.get(key).map(OsString::as_os_str)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateLayout {
    root: PathBuf,
}

impl StateLayout {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_environment(env: &Environment) -> Result<Self> {
        let cwd = std::env::current_dir()
            .map_err(|source| io(".", source))?;
        Self::from_environment_at(env, &cwd)
    }

    pub fn from_environment_at(env: &Environment, cwd: &Path) -> Result<Self> {
        if let Some(root) = env.get("SESH_HOME") {
            return Ok(Self::new(resolve_from(cwd, PathBuf::from(root))));
        }
        if let Some(root) = env.get("XDG_STATE_HOME") {
            return Ok(Self::new(resolve_from(cwd, PathBuf::from(root)).join("sesh")));
        }
        let home = env
            .get("HOME")
            .ok_or(crate::error::Error::StateHomeUnavailable)?;
        Ok(Self::new(resolve_from(cwd, PathBuf::from(home)).join(".local/state/sesh")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn worktree_refs(&self) -> PathBuf {
        self.root.join("refs/worktrees")
    }

    pub fn ensure(&self) -> Result<()> {
        let paths = [
            self.root.clone(),
            self.sessions(),
            self.root.join("refs"),
            self.worktree_refs(),
        ];
        for path in &paths {
            ensure_private_dir(path)?;
        }
        let format = self.root.join("FORMAT");
        if format.exists() {
            let bytes = atomic::read_private(&format)?;
            if bytes != b"sesh-state 1\n" {
                return Err(crate::error::Error::InvalidState(
                    "unsupported Sesh state format; expected 1".into(),
                ));
            }
        } else {
            atomic::create_private(&format, b"sesh-state 1\n")?;
        }
        Ok(())
    }

    pub fn canonicalized(&self) -> Result<Self> {
        let root = self.root.canonicalize().map_err(|source| io(&self.root, source))?;
        Ok(Self::new(root))
    }
}

fn resolve_from(cwd: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() { path } else { cwd.join(path) }
}

pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    let existed = match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => return Err(io(path, source)),
    };
    if !existed {
        std::fs::create_dir_all(path).map_err(|source| io(path, source))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|source| io(path, source))?;
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(crate::error::Error::InvalidState(format!(
            "private state path {} is not a real directory",
            path.display(),
        )));
    }
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(crate::error::Error::InvalidState(format!(
            "private state path {} has unexpected owner {}",
            path.display(),
            metadata.uid(),
        )));
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(crate::error::Error::InvalidState(format!(
            "private state directory {} must have mode 0700",
            path.display(),
        )));
    }
    Ok(())
}
```

Create `src/store/atomic.rs`:

```rust
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::error::{Result, io};

pub fn replace_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        crate::error::Error::InvalidState(format!("{} has no parent", path.display()))
    })?;
    super::ensure_private_dir(parent)?;

    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|name| name.to_str()).unwrap_or("state"),
        uuid::Uuid::new_v4()
    ));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|source| io(&temp, source))?;
    file.write_all(contents).map_err(|source| io(&temp, source))?;
    file.sync_all().map_err(|source| io(&temp, source))?;
    drop(file);
    std::fs::rename(&temp, path).map_err(|source| io(path, source))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|source| io(path, source))?;
    let directory = std::fs::File::open(parent).map_err(|source| io(parent, source))?;
    directory.sync_all().map_err(|source| io(parent, source))?;
    Ok(())
}

pub fn create_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        crate::error::Error::InvalidState(format!("{} has no parent", path.display()))
    })?;
    super::ensure_private_dir(parent)?;
    let temp = parent.join(format!(".create.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|source| io(&temp, source))?;
    file.write_all(contents).map_err(|source| io(&temp, source))?;
    file.sync_all().map_err(|source| io(&temp, source))?;
    drop(file);
    let link_result = std::fs::hard_link(&temp, path).map_err(|source| io(path, source));
    let cleanup_result = std::fs::remove_file(&temp).map_err(|source| io(&temp, source));
    link_result?;
    cleanup_result?;
    let directory = std::fs::File::open(parent).map_err(|source| io(parent, source))?;
    directory.sync_all().map_err(|source| io(parent, source))?;
    Ok(())
}

pub fn read_private(path: &Path) -> Result<Vec<u8>> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io(path, source))?;
    let metadata = file.metadata().map_err(|source| io(path, source))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(crate::error::Error::InvalidState(format!(
            "refusing insecure private file {}",
            path.display(),
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|source| io(path, source))?;
    Ok(bytes)
}
```

Export the modules from `src/lib.rs`:

```rust
pub mod cli;
pub mod error;
pub mod store;
```

- [x] **Step 4: Run the tests and all static checks**

Run:

```bash
rtk cargo test store::
rtk cargo fmt --check
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. If Clippy flags the temporary-name construction, fix the code rather than allowing the lint.

- [x] **Step 5: Commit private storage layout**

```bash
rtk git add src
rtk git commit -m "feat: add private local state layout"
```

### Task 3: Define IDs, runtime inputs, and the normalized event schema

**Files:**
- Create: `src/runtime.rs`
- Create: `src/model/mod.rs`
- Create: `src/model/ids.rs`
- Create: `src/model/provider.rs`
- Create: `src/model/git.rs`
- Create: `src/model/event.rs`
- Modify: `src/lib.rs`
- Test: `src/model/event.rs`

- [x] **Step 1: Write the failing envelope-integrity tests**

Add this test module to the new `src/model/event.rs` before its implementation:

```rust
#[cfg(test)]
mod tests {
    use super::{Event, EventEnvelope, EventKind};
    use crate::model::{Provider, RunId, SessionId};

    fn event() -> Event {
        Event {
            schema_version: 1,
            sequence: 7,
            occurred_at: "2026-07-16T10:00:00Z".into(),
            recorded_at: "2026-07-16T10:00:01Z".into(),
            session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            run_id: Some(RunId::parse("22222222-2222-4222-8222-222222222222").unwrap()),
            provider: Some(Provider::Claude),
            kind: EventKind::ProviderPromptSubmitted {
                prompt: "fix oauth".into(),
            },
        }
    }

    #[test]
    fn sealed_event_verifies() {
        let envelope = EventEnvelope::seal(event()).unwrap();
        envelope.verify().unwrap();
    }

    #[test]
    fn mutation_breaks_the_checksum() {
        let mut envelope = EventEnvelope::seal(event()).unwrap();
        envelope.event.sequence = 8;
        assert!(envelope.verify().is_err());
    }

    #[test]
    fn encoding_is_stable() {
        let left = EventEnvelope::seal(event()).unwrap().line().unwrap();
        let right = EventEnvelope::seal(event()).unwrap().line().unwrap();
        assert_eq!(left, right);
        assert!(left.ends_with(b"\n"));
    }
}
```

- [x] **Step 2: Run the focused test and verify the model is absent**

Run: `rtk cargo test model::event::tests`

Expected: FAIL with unresolved model types.

- [x] **Step 3: Implement stable IDs and provider names**

Create `src/model/ids.rs`:

```rust
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn parse(value: &str) -> Result<Self, uuid::Error> {
                Uuid::parse_str(value).map(Self)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                Display::fmt(&self.0, formatter)
            }
        }
    };
}

id_type!(SessionId);
id_type!(RunId);
```

Create `src/model/provider.rs`:

```rust
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}
```

Create `src/runtime.rs`:

```rust
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::model::{RunId, SessionId};

pub trait Runtime: Send + Sync {
    fn now(&self) -> Result<String>;
    fn session_id(&self) -> SessionId;
    fn run_id(&self) -> RunId;
}

#[derive(Debug, Default)]
pub struct SystemRuntime;

impl Runtime for SystemRuntime {
    fn now(&self) -> Result<String> {
        OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| Error::InvalidState(format!("cannot format UTC time: {error}")))
    }

    fn session_id(&self) -> SessionId {
        SessionId::new()
    }

    fn run_id(&self) -> RunId {
        RunId::new()
    }
}
```

- [x] **Step 4: Implement Git fact types and the sealed event envelope**

Create `src/model/git.rs`:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorktreeIdentity {
    pub common_git_dir: PathBuf,
    pub git_dir: PathBuf,
    pub worktree: PathBuf,
    pub cwd_relative: PathBuf,
    pub key: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirtyPath {
    pub path: PathBuf,
    pub sha256: Option<String>,
    pub executable: bool,
    pub symlink_target: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitSnapshot {
    pub identity: WorktreeIdentity,
    pub branch: Option<String>,
    pub head: String,
    pub staged: Vec<DirtyPath>,
    pub unstaged: Vec<DirtyPath>,
    pub untracked: Vec<DirtyPath>,
    pub dirty_submodules: Vec<PathBuf>,
}
```

Create `src/model/event.rs`:

```rust
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::{GitSnapshot, Provider, RunId, SessionId, WorktreeIdentity};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum EventKind {
    #[serde(rename = "session.created")]
    SessionCreated { worktree: WorktreeIdentity },
    #[serde(rename = "switch.requested")]
    SwitchRequested {
        from: Option<Provider>,
        to: Provider,
    },
    #[serde(rename = "run.started")]
    RunStarted {
        cwd: String,
        args: Vec<String>,
        supervisor_pid: u32,
    },
    #[serde(rename = "run.handshake")]
    RunHandshake {
        native_session_id: String,
        provider_version: Option<String>,
    },
    #[serde(rename = "run.stopped")]
    RunStopped {
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    #[serde(rename = "run.recovered")]
    RunRecovered { reason: String },
    #[serde(rename = "cwd.changed")]
    CwdChanged { cwd_relative: std::path::PathBuf },
    #[serde(rename = "provider.prompt.submitted")]
    ProviderPromptSubmitted { prompt: String },
    #[serde(rename = "provider.tool.requested")]
    ProviderToolRequested {
        tool_name: String,
        tool_use_id: String,
        command: Option<String>,
        file_path: Option<String>,
    },
    #[serde(rename = "provider.tool.completed")]
    ProviderToolCompleted {
        tool_name: String,
        tool_use_id: String,
        response: Option<String>,
        stdout: Option<String>,
        stderr: Option<String>,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
    #[serde(rename = "provider.tool.failed")]
    ProviderToolFailed {
        tool_name: String,
        tool_use_id: String,
        error: String,
    },
    #[serde(rename = "provider.stop.observed")]
    ProviderStopObserved { native_session_id: String },
    #[serde(rename = "git.snapshot")]
    GitSnapshot { snapshot: GitSnapshot },
    #[serde(rename = "checkpoint.created")]
    CheckpointCreated {
        checkpoint_kind: String,
        through_sequence: u64,
        path: String,
    },
    #[serde(rename = "capture.failed")]
    CaptureFailed { phase: String, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub sequence: u64,
    pub occurred_at: String,
    pub recorded_at: String,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub provider: Option<Provider>,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub checksum: String,
    pub event: Event,
}

impl EventEnvelope {
    pub fn seal(event: Event) -> Result<Self> {
        let bytes = serde_json::to_vec(&event)
            .map_err(|error| Error::InvalidState(format!("cannot encode event: {error}")))?;
        let checksum = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        Ok(Self { checksum, event })
    }

    pub fn verify(&self) -> Result<()> {
        let expected = Self::seal(self.event.clone())?.checksum;
        if self.checksum == expected {
            Ok(())
        } else {
            Err(Error::InvalidState(format!(
                "event {} checksum mismatch",
                self.event.sequence
            )))
        }
    }

    pub fn line(&self) -> Result<Vec<u8>> {
        let mut line = serde_json::to_vec(self)
            .map_err(|error| Error::InvalidState(format!("cannot encode envelope: {error}")))?;
        line.push(b'\n');
        Ok(line)
    }
}
```

Create `src/model/mod.rs`:

```rust
mod event;
mod git;
mod ids;
mod provider;

pub use event::{Event, EventEnvelope, EventKind};
pub use git::{DirtyPath, GitSnapshot, WorktreeIdentity};
pub use ids::{RunId, SessionId};
pub use provider::Provider;
```

Add these exports to `src/lib.rs`:

```rust
pub mod model;
pub mod runtime;
```

- [x] **Step 5: Verify serialization and linting**

Run:

```bash
rtk cargo test model::event::tests
rtk cargo fmt --check
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [x] **Step 6: Commit the normalized event contract**

```bash
rtk git add src Cargo.lock
rtk git commit -m "feat: define normalized session events"
```

### Task 4: Append, verify, and recover the event journal

**Files:**
- Create: `src/store/journal.rs`
- Modify: `src/store/mod.rs`
- Test: `src/store/journal.rs`

- [x] **Step 1: Write failing journal tests**

Add to `src/store/journal.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::{EventJournal, PendingEvent};
    use crate::model::{EventKind, Provider, SessionId};

    fn pending(prompt: &str) -> PendingEvent {
        PendingEvent {
            occurred_at: "2026-07-16T10:00:00Z".into(),
            recorded_at: "2026-07-16T10:00:01Z".into(),
            run_id: None,
            provider: Some(Provider::Claude),
            kind: EventKind::ProviderPromptSubmitted {
                prompt: prompt.into(),
            },
        }
    }

    #[test]
    fn appends_monotonic_verified_events() {
        let temp = TempDir::new().unwrap();
        let journal = EventJournal::new(
            temp.path(),
            SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        );

        assert_eq!(journal.append(pending("one")).unwrap().sequence, 1);
        assert_eq!(journal.append(pending("two")).unwrap().sequence, 2);
        assert_eq!(journal.read_repair().unwrap().len(), 2);
    }

    #[test]
    fn removes_only_an_invalid_final_line() {
        let temp = TempDir::new().unwrap();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.path())
            .unwrap();
        file.write_all(b"{partial").unwrap();
        drop(file);

        let events = journal.read_repair().unwrap();

        assert_eq!(events.len(), 1);
        assert!(std::fs::read(journal.path()).unwrap().ends_with(b"\n"));
    }

    #[test]
    fn refuses_corruption_before_the_tail() {
        let temp = TempDir::new().unwrap();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        journal.append(pending("two")).unwrap();
        let bytes = std::fs::read(journal.path()).unwrap();
        let second = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let mut corrupt = b"{}\n".to_vec();
        corrupt.extend_from_slice(&bytes[second..]);
        std::fs::write(journal.path(), corrupt).unwrap();

        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn refuses_a_complete_invalid_tail_line() {
        let temp = TempDir::new().unwrap();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.path())
            .unwrap();
        file.write_all(b"{}\n").unwrap();

        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn refuses_a_validly_sealed_non_monotonic_sequence() {
        let temp = TempDir::new().unwrap();
        let journal = EventJournal::new(temp.path(), SessionId::new());
        journal.append(pending("one")).unwrap();
        let bytes = std::fs::read(journal.path()).unwrap();
        let mut envelope: crate::model::EventEnvelope =
            serde_json::from_slice(&bytes).unwrap();
        envelope.event.sequence = 9;
        let replacement = crate::model::EventEnvelope::seal(envelope.event)
            .unwrap()
            .line()
            .unwrap();
        std::fs::write(journal.path(), replacement).unwrap();

        assert!(journal.read_repair().is_err());
    }

    #[test]
    fn refuses_a_symlinked_journal() {
        let temp = TempDir::new().unwrap();
        let outside = temp.path().join("outside.jsonl");
        std::fs::write(&outside, b"").unwrap();
        let session = temp.path().join("session");
        std::fs::create_dir(&session).unwrap();
        symlink(&outside, session.join("events.jsonl")).unwrap();
        let journal = EventJournal::new(&session, SessionId::new());

        assert!(journal.read_repair().is_err());
    }
}
```

- [x] **Step 2: Run the journal tests and verify they fail**

Run: `rtk cargo test store::journal::tests`

Expected: FAIL because `EventJournal` and `PendingEvent` are undefined.

- [x] **Step 3: Implement locked append and tail-only recovery**

Create `src/store/journal.rs` with these public types and behavior:

```rust
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::error::{Error, Result, io};
use crate::model::{Event, EventEnvelope, EventKind, Provider, RunId, SessionId};

#[derive(Clone, Debug)]
pub struct PendingEvent {
    pub occurred_at: String,
    pub recorded_at: String,
    pub run_id: Option<RunId>,
    pub provider: Option<Provider>,
    pub kind: EventKind,
}

#[derive(Clone, Debug)]
pub struct EventJournal {
    session_id: SessionId,
    path: PathBuf,
    lock_path: PathBuf,
}

impl EventJournal {
    pub fn new(session_dir: &Path, session_id: SessionId) -> Self {
        Self {
            session_id,
            path: session_dir.join("events.jsonl"),
            lock_path: session_dir.join("lock"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, pending: PendingEvent) -> Result<Event> {
        self.with_lock(|file| {
            let events = repair_and_read(file, &self.path, &self.session_id)?;
            let event = Event {
                schema_version: 1,
                sequence: events.last().map_or(1, |item| item.event.sequence + 1),
                occurred_at: pending.occurred_at,
                recorded_at: pending.recorded_at,
                session_id: self.session_id.clone(),
                run_id: pending.run_id,
                provider: pending.provider,
                kind: pending.kind,
            };
            let envelope = EventEnvelope::seal(event.clone())?;
            file.seek(SeekFrom::End(0)).map_err(|source| io(&self.path, source))?;
            file.write_all(&envelope.line()?)
                .map_err(|source| io(&self.path, source))?;
            file.sync_data().map_err(|source| io(&self.path, source))?;
            Ok(event)
        })
    }

    pub fn read_repair(&self) -> Result<Vec<EventEnvelope>> {
        self.with_lock(|file| repair_and_read(file, &self.path, &self.session_id))
    }

    fn with_lock<T>(&self, operation: impl FnOnce(&mut std::fs::File) -> Result<T>) -> Result<T> {
        let parent = self.path.parent().ok_or_else(|| {
            Error::InvalidState(format!("{} has no parent", self.path.display()))
        })?;
        super::ensure_private_dir(parent)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.lock_path)
            .map_err(|source| io(&self.lock_path, source))?;
        lock.lock_exclusive()
            .map_err(|source| io(&self.lock_path, source))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
            .map_err(|source| io(&self.path, source))?;
        let result = operation(&mut file);
        let unlock = FileExt::unlock(&lock).map_err(|source| io(&self.lock_path, source));
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }
}

fn repair_and_read(
    file: &mut std::fs::File,
    path: &Path,
    expected_session_id: &SessionId,
) -> Result<Vec<EventEnvelope>> {
    file.seek(SeekFrom::Start(0)).map_err(|source| io(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|source| io(path, source))?;
    let mut events = Vec::new();
    let mut committed_end = 0usize;
    let lines: Vec<&[u8]> = bytes.split_inclusive(|byte| *byte == b'\n').collect();

    for (index, line) in lines.iter().enumerate() {
        let complete = line.ends_with(b"\n");
        let payload = line.strip_suffix(b"\n").unwrap_or(line);
        let parsed = serde_json::from_slice::<EventEnvelope>(payload)
            .map_err(|error| Error::InvalidState(format!("invalid event JSON: {error}")))
            .and_then(|envelope| {
                envelope.verify()?;
                Ok(envelope)
            });
        match parsed {
            Ok(envelope) if complete => {
                let expected_sequence = events.len() as u64 + 1;
                if envelope.event.schema_version != 1
                    || envelope.event.sequence != expected_sequence
                    || &envelope.event.session_id != expected_session_id
                {
                    return Err(Error::InvalidState(format!(
                        "journal expected session {expected_session_id} schema 1 sequence {expected_sequence}, found session {} schema {} sequence {}",
                        envelope.event.session_id,
                        envelope.event.schema_version,
                        envelope.event.sequence,
                    )));
                }
                committed_end += line.len();
                events.push(envelope);
            }
            Err(_) if !complete && index + 1 == lines.len() => {
                file.set_len(committed_end as u64)
                    .map_err(|source| io(path, source))?;
                file.sync_data().map_err(|source| io(path, source))?;
                break;
            }
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(Error::InvalidState(
                    "incomplete event before journal tail".into(),
                ));
            }
        }
    }
    Ok(events)
}
```

Immediately after opening the lock and journal descriptors, validate with `File::metadata` that each is a regular file owned by `geteuid()` with no group/other permission bits. Reject before locking or reading otherwise. The `O_NOFOLLOW` flag and descriptor metadata check must be covered by a test that replaces `events.jsonl` with a symlink.

Add `pub mod journal;` to `src/store/mod.rs`.

- [x] **Step 4: Verify recovery behavior and the full suite**

Run:

```bash
rtk cargo test store::journal::tests
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. Confirm the corruption test fails if the implementation is temporarily changed to discard the first invalid line.

- [x] **Step 5: Commit the journal**

```bash
rtk git add src/store
rtk git commit -m "feat: add recoverable event journal"
```

### Task 5: Observe exact Git worktree and dirty state

**Files:**
- Create: `src/git/mod.rs`
- Create: `src/git/command.rs`
- Create: `src/git/observe.rs`
- Create: `tests/support/mod.rs`
- Create: `tests/git_observer.rs`
- Modify: `src/lib.rs`

- [x] **Step 1: Add real-Git test helpers and a failing observation test**

Create `tests/support/mod.rs`:

```rust
use std::path::Path;
use std::process::Command;

pub fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.name", "Sesh Test"]);
    git(path, &["config", "user.email", "sesh@example.invalid"]);
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-m", "initial"]);
}
```

Create `tests/git_observer.rs`:

```rust
mod support;

use std::os::unix::fs::symlink;

use sesh::git::Git;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[test]
fn observes_linked_worktree_nested_cwd_and_all_dirty_classes() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("oauth worktree");
    support::init_repo(&repo);
    support::git(
        &repo,
        &["worktree", "add", "-b", "feat/oauth", worktree.to_str().unwrap()],
    );
    let cwd = worktree.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(worktree.join("README.md"), "staged\n").unwrap();
    support::git(&worktree, &["add", "README.md"]);
    std::fs::write(worktree.join("README.md"), "unstaged\n").unwrap();
    std::fs::write(cwd.join("new file.txt"), "untracked\n").unwrap();
    symlink("new file.txt", cwd.join("new-link")).unwrap();

    let snapshot = Git::new().snapshot(&cwd).unwrap();

    assert_eq!(snapshot.identity.worktree, worktree.canonicalize().unwrap());
    assert_eq!(snapshot.identity.cwd_relative, std::path::Path::new("apps/web"));
    assert_eq!(snapshot.branch.as_deref(), Some("feat/oauth"));
    assert!(snapshot.staged.iter().any(|path| path.path == std::path::Path::new("README.md")));
    assert!(snapshot.unstaged.iter().any(|path| path.path == std::path::Path::new("README.md")));
    assert_eq!(
        snapshot.staged.iter().find(|path| path.path == std::path::Path::new("README.md")).unwrap().sha256,
        Some(hex::encode(Sha256::digest(b"staged\n")))
    );
    assert_eq!(
        snapshot.unstaged.iter().find(|path| path.path == std::path::Path::new("README.md")).unwrap().sha256,
        Some(hex::encode(Sha256::digest(b"unstaged\n")))
    );
    assert!(snapshot.untracked.iter().any(|path| path.path == std::path::Path::new("apps/web/new file.txt")));
    assert!(snapshot.untracked.iter().any(|path| path.symlink_target.as_deref() == Some(std::path::Path::new("new file.txt"))));
}

#[test]
fn reports_a_dirty_submodule_explicitly() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    let dependency = temp.path().join("dependency");
    support::init_repo(&repo);
    support::init_repo(&dependency);
    support::git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            dependency.to_str().unwrap(),
            "vendor/dependency",
        ],
    );
    support::git(&repo, &["commit", "-m", "add dependency"]);
    std::fs::write(repo.join("vendor/dependency/README.md"), "dirty\n").unwrap();

    let snapshot = Git::new().snapshot(&repo).unwrap();

    assert_eq!(snapshot.dirty_submodules, [std::path::PathBuf::from("vendor/dependency")]);
}
```

- [x] **Step 2: Run the integration test and verify the Git facade is missing**

Run: `rtk cargo test --test git_observer`

Expected: FAIL because `sesh::git::Git` does not exist.

- [x] **Step 3: Implement argument-vector Git execution**

Create `src/git/command.rs`:

```rust
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

#[derive(Clone, Debug, Default)]
pub struct GitCommand;

impl GitCommand {
    pub fn output<I, S>(&self, cwd: &Path, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args
            .into_iter()
            .map(|arg| arg.as_ref().to_os_string())
            .collect();
        let output = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(&args)
            .output()
            .map_err(|error| Error::Command(format!("cannot run git: {error}")))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(Error::Command(format!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    pub fn text<I, S>(&self, cwd: &Path, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        String::from_utf8(self.output(cwd, args)?)
            .map(|text| text.trim_end().to_owned())
            .map_err(|error| Error::Command(format!("git emitted non-UTF-8 metadata: {error}")))
    }
}
```

- [x] **Step 4: Implement identity, NUL-delimited path lists, and hashing**

Create `src/git/observe.rs` with this interface and exact command set:

```rust
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::git::command::GitCommand;
use crate::model::{DirtyPath, GitSnapshot, WorktreeIdentity};

pub fn snapshot(command: &GitCommand, cwd: &Path) -> Result<GitSnapshot> {
    let worktree = canonical(command.text(cwd, ["rev-parse", "--show-toplevel"])?)?;
    let git_dir = canonical(command.text(cwd, ["rev-parse", "--path-format=absolute", "--git-dir"])?)?;
    let common_git_dir = canonical(command.text(
        cwd,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)?;
    let canonical_cwd = cwd
        .canonicalize()
        .map_err(|error| Error::Command(format!("cannot canonicalize {}: {error}", cwd.display())))?;
    require_utf8([&worktree, &git_dir, &common_git_dir, &canonical_cwd])?;
    let cwd_relative = canonical_cwd
        .strip_prefix(&worktree)
        .map_err(|_| Error::Command("cwd is outside discovered worktree".into()))?
        .to_path_buf();
    let key = {
        let mut digest = Sha256::new();
        digest.update(common_git_dir.as_os_str().as_encoded_bytes());
        digest.update([0]);
        digest.update(git_dir.as_os_str().as_encoded_bytes());
        hex::encode(digest.finalize())
    };
    let identity = WorktreeIdentity {
        common_git_dir,
        git_dir,
        worktree: worktree.clone(),
        cwd_relative,
        key,
    };
    let head = command.text(cwd, ["rev-parse", "HEAD"])?;
    let branch_text = command.text(cwd, ["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let branch = branch_text.ok().filter(|value| !value.is_empty());
    let staged = paths(command.output(
        cwd,
        ["diff", "--cached", "--name-only", "--no-renames", "-z"],
    )?)?
        .into_iter()
        .map(|path| staged_path(command, cwd, path))
        .collect::<Result<Vec<_>>>()?;
    let unstaged = paths(command.output(
        cwd,
        ["diff", "--name-only", "--no-renames", "-z"],
    )?)?
        .into_iter()
        .map(|path| worktree_path(&worktree, path))
        .collect::<Result<Vec<_>>>()?;
    let untracked = paths(command.output(
        cwd,
        ["ls-files", "--others", "--exclude-standard", "-z"],
    )?)?
    .into_iter()
    .map(|path| worktree_path(&worktree, path))
    .collect::<Result<Vec<_>>>()?;
    let dirty_submodules = dirty_submodules(command, cwd)?;

    Ok(GitSnapshot {
        identity,
        branch,
        head,
        staged,
        unstaged,
        untracked,
        dirty_submodules,
    })
}

fn canonical(value: String) -> Result<PathBuf> {
    PathBuf::from(value)
        .canonicalize()
        .map_err(|error| Error::Command(format!("cannot canonicalize Git path: {error}")))
}

fn require_utf8<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> Result<()> {
    if paths.into_iter().any(|path| path.to_str().is_none()) {
        return Err(Error::InvalidState(
            "Sesh V1 requires Git paths that are valid UTF-8; no path was recorded lossily".into(),
        ));
    }
    Ok(())
}

fn paths(bytes: Vec<u8>) -> Result<Vec<PathBuf>> {
    let mut paths = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| PathBuf::from(OsString::from_vec(part.to_vec())))
        .collect::<Vec<_>>();
    require_utf8(paths.iter())?;
    paths.sort();
    Ok(paths)
}

fn staged_path(command: &GitCommand, cwd: &Path, path: PathBuf) -> Result<DirtyPath> {
    let entry = command.output(
        cwd,
        [
            OsString::from("ls-files"),
            OsString::from("--stage"),
            OsString::from("-z"),
            OsString::from("--"),
            path.as_os_str().to_os_string(),
        ],
    )?;
    if entry.is_empty() {
        return Ok(DirtyPath {
            path,
            sha256: None,
            executable: false,
            symlink_target: None,
        });
    }
    let records = entry
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    if records.len() != 1 {
        return Err(Error::InvalidState(format!("unmerged index entry at {}", path.display())));
    }
    let tab = records[0]
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or_else(|| Error::InvalidState("malformed git index entry".into()))?;
    let header = std::str::from_utf8(&records[0][..tab])
        .map_err(|_| Error::InvalidState("non-ASCII git index header".into()))?;
    let mut fields = header.split_ascii_whitespace();
    let mode = fields
        .next()
        .ok_or_else(|| Error::InvalidState("missing git index mode".into()))?;
    let object = fields
        .next()
        .ok_or_else(|| Error::InvalidState("missing git object id".into()))?;
    let stage = fields
        .next()
        .ok_or_else(|| Error::InvalidState("missing git index stage".into()))?;
    if stage != "0" {
        return Err(Error::InvalidState(format!("unmerged index entry at {}", path.display())));
    }
    let content = if mode == "160000" {
        object.as_bytes().to_vec()
    } else {
        let mut spec = OsString::from(":");
        spec.push(path.as_os_str());
        command.output(cwd, [OsString::from("show"), spec])?
    };
    let symlink_target = (mode == "120000")
        .then(|| PathBuf::from(OsString::from_vec(content.clone())));
    if let Some(target) = symlink_target.as_ref() {
        require_utf8([target])?;
    }
    Ok(DirtyPath {
        path,
        sha256: Some(hex::encode(Sha256::digest(&content))),
        executable: mode == "100755",
        symlink_target,
    })
}

fn worktree_path(worktree: &Path, path: PathBuf) -> Result<DirtyPath> {
    let absolute = worktree.join(&path);
    let metadata = std::fs::symlink_metadata(&absolute);
    match metadata {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = std::fs::read_link(&absolute)
                .map_err(|error| Error::Command(format!("cannot read {}: {error}", absolute.display())))?;
            require_utf8([&target])?;
            let digest = Sha256::digest(target.as_os_str().as_encoded_bytes());
            Ok(DirtyPath {
                path,
                sha256: Some(hex::encode(digest)),
                executable: false,
                symlink_target: Some(target),
            })
        }
        Ok(metadata) if metadata.is_file() => {
            let bytes = std::fs::read(&absolute)
                .map_err(|error| Error::Command(format!("cannot read {}: {error}", absolute.display())))?;
            Ok(DirtyPath {
                path,
                sha256: Some(hex::encode(Sha256::digest(bytes))),
                executable: metadata.permissions().mode() & 0o111 != 0,
                symlink_target: None,
            })
        }
        Ok(metadata) => Err(Error::Command(format!(
            "unsupported dirty file type at {} (mode {:o})",
            absolute.display(),
            metadata.mode()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DirtyPath {
            path,
            sha256: None,
            executable: false,
            symlink_target: None,
        }),
        Err(error) => Err(Error::Command(format!(
            "cannot inspect {}: {error}",
            absolute.display()
        ))),
    }
}

fn dirty_submodules(command: &GitCommand, cwd: &Path) -> Result<Vec<PathBuf>> {
    let index = command.output(cwd, ["ls-files", "--stage", "-z"])?;
    let mut dirty = Vec::new();
    for record in index.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| Error::InvalidState("malformed git index entry".into()))?;
        if !record[..tab].starts_with(b"160000 ") {
            continue;
        }
        let path = PathBuf::from(OsString::from_vec(record[tab + 1..].to_vec()));
        require_utf8([&path])?;
        let status = command.output(
            cwd,
            [
                OsString::from("status"),
                OsString::from("--porcelain=v1"),
                OsString::from("-z"),
                OsString::from("--ignore-submodules=none"),
                OsString::from("--"),
                path.as_os_str().to_os_string(),
            ],
        )?;
        if !status.is_empty() {
            dirty.push(path);
        }
    }
    dirty.sort();
    Ok(dirty)
}
```

The UTF-8 check is an explicit V1 storage boundary, not a lossy conversion: canonical JSON remains human-inspectable and the refusal names the unsupported condition.

Create `src/git/mod.rs`:

```rust
mod command;
mod observe;

use std::path::Path;

use crate::error::Result;
use crate::model::GitSnapshot;

#[derive(Clone, Debug, Default)]
pub struct Git {
    command: command::GitCommand,
}

impl Git {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self, cwd: &Path) -> Result<GitSnapshot> {
        observe::snapshot(&self.command, cwd)
    }
}
```

Add `pub mod git;` to `src/lib.rs`.

- [x] **Step 5: Run the real-Git test and repair detached-HEAD handling if needed**

Run:

```bash
rtk cargo test --test git_observer
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. The intentionally non-zero `symbolic-ref --quiet` result must become `branch: None`; no stderr should reach the user for detached HEAD.

- [x] **Step 6: Commit Git observation**

```bash
rtk git add src tests
rtk git commit -m "feat: observe git worktree state"
```

### Task 6: Bind one canonical session to a worktree

**Files:**
- Create: `src/model/session.rs`
- Create: `src/store/refs.rs`
- Create: `src/store/session.rs`
- Modify: `src/model/mod.rs`
- Modify: `src/store/mod.rs`
- Test: `src/store/session.rs`

- [x] **Step 1: Write failing tests for create, lookup, and duplicate refusal**

Add to `src/store/session.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::SessionStore;
    use crate::model::{GitSnapshot, SessionId, WorktreeIdentity};
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
        GitSnapshot {
            identity: WorktreeIdentity {
                common_git_dir: PathBuf::from("/repo/.git"),
                git_dir: PathBuf::from("/repo/.git/worktrees/oauth"),
                worktree: PathBuf::from("/work/oauth"),
                cwd_relative: PathBuf::from("apps/web"),
                key: "abc123".into(),
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
        assert_eq!(created.events().unwrap().len(), 2);
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
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(999);
        crate::store::refs::write_json(&path, &value).unwrap();

        assert!(SessionStore::open(&layout, created.id().clone()).is_err());
    }
}
```

- [x] **Step 2: Run the tests and verify the session facade is missing**

Run: `rtk cargo test store::session::tests`

Expected: FAIL with unresolved `SessionStore` and `SessionMeta`.

- [x] **Step 3: Add session metadata and worktree refs**

Create `src/model/session.rs`:

```rust
use serde::{Deserialize, Serialize};

use crate::model::{SessionId, WorktreeIdentity};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub schema_version: u32,
    pub id: SessionId,
    pub created_at: String,
    pub worktree: WorktreeIdentity,
    pub parent_session_id: Option<SessionId>,
    pub parent_checkpoint_sequence: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorktreeRef {
    pub schema_version: u32,
    pub key: String,
    pub session_id: SessionId,
    pub identity: WorktreeIdentity,
}
```

Create `src/store/refs.rs`:

```rust
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
use crate::store::atomic::{create_private, read_private, replace_private};

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| Error::InvalidState(format!("cannot encode {}: {error}", path.display())))?;
    bytes.push(b'\n');
    replace_private(path, &bytes)
}

pub fn write_json_create<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| Error::InvalidState(format!("cannot encode {}: {error}", path.display())))?;
    bytes.push(b'\n');
    create_private(path, &bytes)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = read_private(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| Error::InvalidState(format!("cannot decode {}: {error}", path.display())))
}
```

Export `SessionMeta` and `WorktreeRef` from `src/model/mod.rs`:

```rust
mod session;
pub use session::{SessionMeta, WorktreeRef};
```

- [x] **Step 4: Implement the SessionStore transaction**

Create `src/store/session.rs`:

```rust
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{Event, EventKind, GitSnapshot, Provider, RunId, SessionId, SessionMeta, WorktreeIdentity, WorktreeRef};
use crate::runtime::Runtime;
use crate::store::journal::{EventJournal, PendingEvent};
use crate::store::refs::{read_json, write_json_create};
use crate::store::StateLayout;

#[derive(Clone, Debug)]
pub struct SessionStore {
    layout: StateLayout,
    meta: SessionMeta,
}

impl SessionStore {
    pub fn create(layout: &StateLayout, runtime: &dyn Runtime, snapshot: GitSnapshot) -> Result<Self> {
        layout.ensure()?;
        let reference_path = layout.worktree_refs().join(format!("{}.json", snapshot.identity.key));
        if reference_path.exists() {
            let existing: WorktreeRef = read_json(&reference_path)?;
            return Err(Error::InvalidState(format!(
                "worktree is already bound to session {}",
                existing.session_id
            )));
        }
        let meta = SessionMeta {
            schema_version: 1,
            id: runtime.session_id(),
            created_at: runtime.now()?,
            worktree: snapshot.identity.clone(),
            parent_session_id: None,
            parent_checkpoint_sequence: None,
        };
        let store = Self {
            layout: layout.clone(),
            meta,
        };
        store.ensure_directories()?;
        write_json_create(&store.session_dir().join("meta.json"), &store.meta)?;
        store.append(runtime, None, None, EventKind::SessionCreated {
            worktree: snapshot.identity.clone(),
        })?;
        store.append(runtime, None, None, EventKind::GitSnapshot { snapshot })?;
        write_json_create(
            &reference_path,
            &WorktreeRef {
                schema_version: 1,
                key: store.meta.worktree.key.clone(),
                session_id: store.meta.id.clone(),
                identity: store.meta.worktree.clone(),
            },
        )?;
        Ok(store)
    }

    pub fn open(layout: &StateLayout, id: SessionId) -> Result<Self> {
        let meta: SessionMeta = read_json(&layout.sessions().join(id.to_string()).join("meta.json"))?;
        if meta.schema_version != 1 || meta.id != id {
            return Err(Error::InvalidState("session metadata identity or schema mismatch".into()));
        }
        if meta.parent_session_id.is_some() != meta.parent_checkpoint_sequence.is_some() {
            return Err(Error::InvalidState("session lineage is incomplete".into()));
        }
        Ok(Self {
            layout: layout.clone(),
            meta,
        })
    }

    pub fn find_for_worktree(
        layout: &StateLayout,
        identity: &WorktreeIdentity,
    ) -> Result<Option<Self>> {
        let path = layout.worktree_refs().join(format!("{}.json", identity.key));
        if !path.exists() {
            return Ok(None);
        }
        let reference: WorktreeRef = read_json(&path)?;
        if reference.schema_version != 1
            || reference.key != identity.key
            || reference.identity.key != identity.key
            || reference.identity.common_git_dir != identity.common_git_dir
            || reference.identity.git_dir != identity.git_dir
            || reference.identity.worktree != identity.worktree
        {
            return Err(Error::InvalidState("worktree ref identity mismatch".into()));
        }
        Self::open(layout, reference.session_id).map(Some)
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
            kind,
        })
    }

    pub fn events(&self) -> Result<Vec<Event>> {
        EventJournal::new(&self.session_dir(), self.meta.id.clone())
            .read_repair()
            .map(|items| items.into_iter().map(|item| item.event).collect())
    }

    pub fn remove_binding(&self) -> Result<()> {
        let path = self
            .layout
            .worktree_refs()
            .join(format!("{}.json", self.meta.worktree.key));
        std::fs::remove_file(&path).map_err(|source| crate::error::io(path, source))
    }

    fn ensure_directories(&self) -> Result<()> {
        for suffix in ["", "refs", "checkpoints", "blobs", "blobs/sha256", "runs"] {
            let path = self.session_dir().join(suffix);
            super::ensure_private_dir(&path)?;
        }
        Ok(())
    }
}
```

Add to `src/store/mod.rs`:

```rust
pub mod refs;
pub mod session;
pub use session::SessionStore;
```

- [x] **Step 5: Verify session creation and ref lookup**

Run:

```bash
rtk cargo test store::session::tests
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. Inspect the temporary test state during a debug run and confirm no path under the fake worktree contains Sesh state.

- [x] **Step 6: Commit the session registry**

```bash
rtk git add src
rtk git commit -m "feat: bind sessions to git worktrees"
```

### Task 7: Normalize Claude and Codex hook payloads

**Files:**
- Create: `src/provider/mod.rs`
- Create: `src/provider/hook.rs`
- Create: `tests/hook_contract.rs`
- Create: `tests/fixtures/hooks/claude-session-start.json`
- Create: `tests/fixtures/hooks/claude-user-prompt.json`
- Create: `tests/fixtures/hooks/claude-post-tool.json`
- Create: `tests/fixtures/hooks/codex-pre-tool.json`
- Create: `tests/fixtures/hooks/codex-post-tool.json`
- Modify: `src/lib.rs`

- [x] **Step 1: Add sanitized hook fixtures**

Create `tests/fixtures/hooks/claude-session-start.json`:

```json
{"session_id":"claude-native-1","transcript_path":"/ignored/transcript.jsonl","cwd":"/work/oauth","permission_mode":"default","hook_event_name":"SessionStart","source":"startup"}
```

Create `tests/fixtures/hooks/claude-user-prompt.json`:

```json
{"session_id":"claude-native-1","transcript_path":"/ignored/transcript.jsonl","cwd":"/work/oauth","permission_mode":"default","hook_event_name":"UserPromptSubmit","prompt":"Implement the OAuth callback"}
```

Create `tests/fixtures/hooks/claude-post-tool.json`:

```json
{"session_id":"claude-native-1","transcript_path":"/ignored/transcript.jsonl","cwd":"/work/oauth","permission_mode":"default","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_callback"},"tool_response":{"stdout":"1 passed; 1 failed","stderr":"assertion failed","exit_code":101},"tool_use_id":"tool-1"}
```

Create `tests/fixtures/hooks/codex-pre-tool.json`:

```json
{"session_id":"codex-native-1","turn_id":"turn-1","transcript_path":null,"cwd":"/work/oauth","model":"gpt-test","permission_mode":"default","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_callback"},"tool_use_id":"tool-2"}
```

Create `tests/fixtures/hooks/codex-post-tool.json`:

```json
{"session_id":"codex-native-1","turn_id":"turn-1","transcript_path":null,"cwd":"/work/oauth","model":"gpt-test","permission_mode":"default","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_callback"},"tool_response":"Chunk ID: test\nProcess exited with code 0\nFinal output:\nok\n","tool_use_id":"tool-2"}
```

- [x] **Step 2: Write failing cross-provider contract tests**

Create `tests/hook_contract.rs`:

```rust
use sesh::model::Provider;
use sesh::provider::hook::{HookEvent, capture_failure_output, normalize};

#[test]
fn normalizes_claude_prompt_without_persisting_transcript_path() {
    let event = normalize(
        Provider::Claude,
        include_bytes!("fixtures/hooks/claude-user-prompt.json"),
    )
    .unwrap();

    assert_eq!(
        event,
        HookEvent::UserPromptSubmitted {
            native_session_id: "claude-native-1".into(),
            prompt: "Implement the OAuth callback".into(),
        }
    );
}

#[test]
fn normalizes_claude_session_start() {
    let event = normalize(
        Provider::Claude,
        include_bytes!("fixtures/hooks/claude-session-start.json"),
    )
    .unwrap();

    assert_eq!(
        event,
        HookEvent::SessionStarted {
            native_session_id: "claude-native-1".into(),
        }
    );
}

#[test]
fn normalizes_claude_tool_result() {
    let event = normalize(
        Provider::Claude,
        include_bytes!("fixtures/hooks/claude-post-tool.json"),
    )
    .unwrap();

    assert!(matches!(
        event,
        HookEvent::ToolCompleted {
            tool_name,
            exit_code: Some(101),
            ..
        } if tool_name == "Bash"
    ));
}

#[test]
fn normalizes_codex_tool_request() {
    let event = normalize(
        Provider::Codex,
        include_bytes!("fixtures/hooks/codex-pre-tool.json"),
    )
    .unwrap();

    assert!(matches!(
        event,
        HookEvent::ToolRequested {
            command: Some(command),
            tool_use_id,
            ..
        } if command == "cargo test oauth_callback" && tool_use_id == "tool-2"
    ));
}

#[test]
fn normalizes_codex_tool_result_with_unknown_fields_allowed() {
    let mut value: serde_json::Value = serde_json::from_slice(include_bytes!(
        "fixtures/hooks/codex-post-tool.json"
    ))
    .unwrap();
    value["future_field"] = serde_json::json!({"ignored": true});
    let bytes = serde_json::to_vec(&value).unwrap();

    let event = normalize(Provider::Codex, &bytes).unwrap();

    assert!(matches!(
        event,
        HookEvent::ToolCompleted {
            response: Some(response),
            exit_code: None,
            ..
        } if response.contains("Final output")
    ));
}

#[test]
fn missing_required_prompt_is_an_error() {
    let payload = br#"{"session_id":"native","cwd":"/work","hook_event_name":"UserPromptSubmit"}"#;
    assert!(normalize(Provider::Claude, payload).is_err());
}

#[test]
fn capture_failure_blocks_before_work_for_both_providers() {
    for provider in [Provider::Claude, Provider::Codex] {
        let pre_tool = capture_failure_output(provider, "PreToolUse", "disk full");
        assert_eq!(pre_tool.exit_code, 0);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pre_tool.stdout).unwrap(),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "Sesh capture failed: disk full"
                }
            })
        );

        let prompt = capture_failure_output(provider, "UserPromptSubmit", "disk full");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&prompt.stdout).unwrap(),
            serde_json::json!({
                "decision": "block",
                "reason": "Sesh capture failed: disk full"
            })
        );

        let post_tool = capture_failure_output(provider, "PostToolUse", "disk full");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&post_tool.stdout).unwrap(),
            serde_json::json!({
                "continue": false,
                "stopReason": "Sesh capture failed: disk full"
            })
        );
    }
}
```

- [x] **Step 3: Run the contract tests and verify the provider layer is missing**

Run: `rtk cargo test --test hook_contract`

Expected: FAIL because `provider::hook` does not exist.

- [x] **Step 4: Implement tolerant normalization and fail-closed output**

Create `src/provider/hook.rs`:

```rust
use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::Provider;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HookEvent {
    SessionStarted {
        native_session_id: String,
    },
    UserPromptSubmitted {
        native_session_id: String,
        prompt: String,
    },
    ToolRequested {
        native_session_id: String,
        tool_name: String,
        tool_use_id: String,
        command: Option<String>,
        file_path: Option<String>,
    },
    ToolCompleted {
        native_session_id: String,
        tool_name: String,
        tool_use_id: String,
        response: Option<String>,
        stdout: Option<String>,
        stderr: Option<String>,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
    ToolFailed {
        native_session_id: String,
        tool_name: String,
        tool_use_id: String,
        error: String,
    },
    Stopped {
        native_session_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn normalize(_provider: Provider, bytes: &[u8]) -> Result<HookEvent> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| Error::InvalidState(format!("invalid provider hook JSON: {error}")))?;
    let event = required(&value, "hook_event_name")?;
    let native_session_id = required(&value, "session_id")?.to_owned();
    match event {
        "SessionStart" => Ok(HookEvent::SessionStarted { native_session_id }),
        "UserPromptSubmit" => Ok(HookEvent::UserPromptSubmitted {
            native_session_id,
            prompt: required(&value, "prompt")?.to_owned(),
        }),
        "PreToolUse" => Ok(HookEvent::ToolRequested {
            native_session_id,
            tool_name: required(&value, "tool_name")?.to_owned(),
            tool_use_id: required(&value, "tool_use_id")?.to_owned(),
            command: pointer_string(&value, "/tool_input/command"),
            file_path: pointer_string(&value, "/tool_input/file_path"),
        }),
        "PostToolUse" => Ok(HookEvent::ToolCompleted {
            native_session_id,
            tool_name: required(&value, "tool_name")?.to_owned(),
            tool_use_id: required(&value, "tool_use_id")?.to_owned(),
            response: opaque_response(&value),
            stdout: pointer_string(&value, "/tool_response/stdout"),
            stderr: pointer_string(&value, "/tool_response/stderr"),
            exit_code: value
                .pointer("/tool_response/exit_code")
                .and_then(Value::as_i64)
                .map(i32::try_from)
                .transpose()
                .map_err(|_| Error::InvalidState("tool exit code is outside i32 range".into()))?,
            duration_ms: value.pointer("/tool_response/duration_ms").and_then(Value::as_u64),
        }),
        "PostToolUseFailure" => Ok(HookEvent::ToolFailed {
            native_session_id,
            tool_name: required(&value, "tool_name")?.to_owned(),
            tool_use_id: required(&value, "tool_use_id")?.to_owned(),
            error: required(&value, "error")?.to_owned(),
        }),
        "Stop" => Ok(HookEvent::Stopped { native_session_id }),
        other => Err(Error::InvalidState(format!("unsupported hook event {other}"))),
    }
}

pub fn session_start_output(handoff: &str) -> HookOutput {
    HookOutput {
        stdout: serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": handoff
            }
        })
        .to_string(),
        stderr: String::new(),
        exit_code: 0,
    }
}

pub fn capture_failure_output(_provider: Provider, event: &str, message: &str) -> HookOutput {
    let stdout = match event {
        "PreToolUse" => serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": format!("Sesh capture failed: {message}")
            }
        }),
        "UserPromptSubmit" => serde_json::json!({
            "decision": "block",
            "reason": format!("Sesh capture failed: {message}")
        }),
        "PostToolUse" | "PostToolUseFailure" | "Stop" => serde_json::json!({
            "continue": false,
            "stopReason": format!("Sesh capture failed: {message}")
        }),
        _ => serde_json::json!({
            "systemMessage": format!("Sesh capture failed: {message}")
        }),
    };
    HookOutput {
        stdout: stdout.to_string(),
        stderr: String::new(),
        exit_code: 0,
    }
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidState(format!("hook payload is missing string field {key}")))
}

fn pointer_string(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer).and_then(Value::as_str).map(str::to_owned)
}

fn opaque_response(value: &Value) -> Option<String> {
    let response = value.get("tool_response")?;
    let has_structured_command_fields = ["stdout", "stderr", "exit_code", "duration_ms"]
        .iter()
        .any(|field| response.get(field).is_some());
    if has_structured_command_fields {
        None
    } else if let Some(text) = response.as_str() {
        Some(text.to_owned())
    } else {
        serde_json::to_string(response).ok()
    }
}
```

Before constructing `HookEvent`, enforce byte limits: 256 for event/tool names, 512 for native session/tool-use IDs, 16 KiB for cwd, 1 MiB for command or file-tool input strings, and 4 MiB for a prompt or individual response string. Reject empty identifiers and any over-limit value; the outer app still enforces the 8 MiB payload cap. Add a contract test for each boundary class so an attacker-controlled ID cannot inflate an idempotency key or journal line without bound.

Create `src/provider/mod.rs`:

```rust
pub mod hook;
```

Add `pub mod provider;` to `src/lib.rs`.

- [x] **Step 5: Verify both provider fixtures and unknown-field compatibility**

Run:

```bash
rtk cargo test --test hook_contract
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. The five fixture-backed tests above are the drift sentinel: every fixture must be loaded by exactly one named test.

- [x] **Step 6: Commit normalized hook ingestion**

```bash
rtk git add src tests
rtk git commit -m "feat: normalize provider hook events"
```

### Task 8: Validate and promote immutable checkpoints

**Files:**
- Create: `src/model/checkpoint.rs`
- Create: `src/checkpoint.rs`
- Create: `src/store/blob.rs`
- Modify: `src/model/mod.rs`
- Modify: `src/store/mod.rs`
- Modify: `src/store/journal.rs`
- Test: `src/checkpoint.rs`

- [x] **Step 1: Write failing tests for narrative validation and transition inheritance**

Add to `src/checkpoint.rs`:

```rust
#[cfg(test)]
mod tests {
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

        let transition = service.stage_transition(12, Some(narrative.event_sequence)).unwrap();

        assert_eq!(transition.checkpoint.narrative_checkpoint_sequence, Some(narrative.event_sequence));
        assert!(transition.checkpoint.narrative.is_none());
        assert!(narrative.json_path.exists());
        assert!(narrative.markdown_path.exists());
        assert!(!temp.path().join("refs/latest-checkpoint").exists());
    }
}
```

- [x] **Step 2: Run the focused tests and verify checkpoint types are absent**

Run: `rtk cargo test checkpoint::tests`

Expected: FAIL with unresolved checkpoint types.

- [x] **Step 3: Implement the checkpoint model and validation**

Create `src/model/checkpoint.rs` with these exact public types:

```rust
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
pub struct Decision {
    pub statement: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
            return Err(Error::InvalidState("checkpoint objective and summary are required".into()));
        }
        if self.next_steps.is_empty() || self.next_steps.iter().all(|item| item.trim().is_empty()) {
            return Err(Error::InvalidState("checkpoint requires at least one next step".into()));
        }
        if self.related_event_sequences.iter().any(|sequence| *sequence > through_sequence) {
            return Err(Error::InvalidState("checkpoint references a future event".into()));
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
            return Err(Error::InvalidState("checkpoint has more than 128 list items".into()));
        }
        for decision in &self.decisions {
            validate_field("decision statement", &decision.statement, Self::MAX_ITEM_BYTES)?;
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
        if self.related_event_sequences.windows(2).any(|pair| pair[0] >= pair[1]) {
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
```

Export these types from `src/model/mod.rs`.

- [x] **Step 4: Add sequence-aware journal append and checkpoint promotion**

Introduce metadata that does not carry an event kind, then make the existing `append` delegate to one sequence-allocation path in `src/store/journal.rs`:

```rust
#[derive(Clone, Debug)]
pub struct PendingEventMeta {
    pub occurred_at: String,
    pub recorded_at: String,
    pub run_id: Option<RunId>,
    pub provider: Option<Provider>,
}

pub fn append(&self, pending: PendingEvent) -> Result<Event> {
    let PendingEvent {
        occurred_at,
        recorded_at,
        run_id,
        provider,
        kind,
    } = pending;
    self.append_with(
        PendingEventMeta {
            occurred_at,
            recorded_at,
            run_id,
            provider,
        },
        move |_, _| Ok(kind),
    )
}

pub fn append_with(
    &self,
    pending: PendingEventMeta,
    build_kind: impl FnOnce(u64, &[EventEnvelope]) -> Result<EventKind>,
) -> Result<Event> {
    self.with_lock(|file| {
        let events = repair_and_read(file, &self.path, &self.session_id)?;
        let sequence = events.last().map_or(1, |item| item.event.sequence + 1);
        let kind = build_kind(sequence, &events)?;
        let event = Event {
            schema_version: 1,
            sequence,
            occurred_at: pending.occurred_at,
            recorded_at: pending.recorded_at,
            session_id: self.session_id.clone(),
            run_id: pending.run_id,
            provider: pending.provider,
            kind,
        };
        let envelope = EventEnvelope::seal(event.clone())?;
        file.seek(SeekFrom::End(0)).map_err(|source| io(&self.path, source))?;
        file.write_all(&envelope.line()?)
            .map_err(|source| io(&self.path, source))?;
        file.sync_data().map_err(|source| io(&self.path, source))?;
        Ok(event)
    })
}
```

Use `create_private` from Task 2 and `write_json_create` from Task 6; unlike a mutable ref, an immutable checkpoint must never be replaced.

Create `src/checkpoint.rs` around this exact storage contract:

```rust
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{Checkpoint, CheckpointAuthor, CheckpointKind, NarrativeInput};
use crate::store::atomic::create_private;
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
        narrative.validate(event_sequence.saturating_sub(1))?;
        self.stage(event_sequence, Checkpoint {
            schema_version: 1,
            checkpoint_kind: CheckpointKind::Narrative,
            through_sequence: event_sequence.saturating_sub(1),
            author,
            narrative: Some(narrative),
            narrative_checkpoint_sequence: None,
        })
    }

    pub fn stage_transition(
        &self,
        event_sequence: u64,
        narrative_checkpoint_sequence: Option<u64>,
    ) -> Result<StoredCheckpoint> {
        self.stage(event_sequence, Checkpoint {
            schema_version: 1,
            checkpoint_kind: CheckpointKind::Transition,
            through_sequence: event_sequence.saturating_sub(1),
            author: CheckpointAuthor::System,
            narrative: None,
            narrative_checkpoint_sequence,
        })
    }

    pub fn commit_refs(&self, stored: &StoredCheckpoint) -> Result<()> {
        write_json(
            &self.refs.join("latest-checkpoint"),
            &stored.event_sequence,
        )?;
        if stored.checkpoint.checkpoint_kind == CheckpointKind::Narrative {
            write_json(
                &self.refs.join("latest-narrative-checkpoint"),
                &stored.event_sequence,
            )?;
        }
        Ok(())
    }
}
```

Implement the private `stage` method as follows:

1. Build `checkpoints/<sequence>.json` and `checkpoints/<sequence>.md` using the same zero-padded sequence.
2. Serialize the JSON once and render Markdown from the typed `Checkpoint`; the Markdown headings are `Objective`, `Summary`, `Decisions`, `Assumptions`, `Constraints`, `Completed`, `In progress`, `Blockers`, and `Next steps` in that order. Transition Markdown links to the narrative sequence instead of copying its prose.
3. Create both files with `create_private`; if the second create fails in the same process, remove the first and return the original error. Never overwrite either path.
4. Return `StoredCheckpoint` without changing refs.

Change `EventKind::CheckpointCreated.checkpoint_kind` from `String` to `CheckpointKind`. Wire `SessionStore::create_narrative_checkpoint` and `create_transition_checkpoint` using this order while the journal lock owns sequence allocation:

```rust
let mut staged = None;
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
let stored = staged.ok_or_else(|| Error::InvalidState("checkpoint was not staged".into()))?;
service.commit_refs(&stored)?;
Ok((event, stored))
```

The transition method uses the same shape with `stage_transition`. The artifact is durable before its journal event; the mutable refs advance only after the journal event is durable. A crash can therefore leave an unreferenced artifact or a stale ref, never a ref to an uncommitted event. Task 16 rebuilds refs and reports orphan artifacts during recovery.

- [x] **Step 5: Add session-scoped blob storage for large command output**

Create `src/store/blob.rs` with `BlobStore::put(bytes) -> ContentRef`. Keep strings up to 8 KiB inline; otherwise write once to `blobs/sha256/<first-two>/<rest>` with mode `0600` and return the hash and byte length. Add `ContentRef::{Inline, Blob}` to `src/model/event.rs`; change prompt content from `String` to `ContentRef` and tool response/stdout/stderr fields from `Option<String>` to `Option<ContentRef>`. Update the hook contract assertions and handoff blob resolution accordingly.

The complete content reference is:

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "storage", rename_all = "snake_case")]
pub enum ContentRef {
    Inline { text: String },
    Blob { sha256: String, bytes: usize },
}
```

On an existing blob path, open with `O_NOFOLLOW`, require a private regular file, and re-hash before reuse. A mismatched blob is corruption. If two hooks race to create the same blob, the losing immutable create re-opens and verifies the winner rather than reporting a false capture failure.

- [x] **Step 6: Verify checkpoint immutability and blob deduplication**

Run:

```bash
rtk cargo test checkpoint::tests
rtk cargo test store::blob::tests
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. A second write of identical blob bytes must reuse the same path; a second checkpoint at the same sequence must fail.

- [x] **Step 7: Commit checkpoints and blobs**

```bash
rtk git add src
rtk git commit -m "feat: add immutable session checkpoints"
```

### Task 9: Render a deterministic bounded handoff

**Files:**
- Create: `src/handoff.rs`
- Test: `src/handoff.rs`

- [x] **Step 1: Write failing ordering and truncation tests**

Add to `src/handoff.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{HandoffInput, render};
    use crate::model::DirtyPath;

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
}
```

- [x] **Step 2: Run the tests and verify the renderer is absent**

Run: `rtk cargo test handoff::tests`

Expected: FAIL because `HandoffInput` and `render` do not exist.

- [x] **Step 3: Implement the structural renderer**

Create these typed renderer inputs. Keep rendering independent of provider JSON and filesystem access:

```rust
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

#[derive(Clone, Debug)]
pub struct HandoffInput {
    pub session_id: SessionId,
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

pub fn render(input: HandoffInput, max_bytes: usize) -> crate::error::Result<String> {
    input.validate_sequence_order()?;
    let mut events = input.recent_events.clone();
    let mut commands = input.recent_commands.clone();
    let mut snapshot = input.snapshot.clone();
    let git_summary = input.git_summary()?;
    let mut gaps = input.capture_gaps.clone();
    let mut omitted_events = Vec::new();
    let mut omitted_commands = Vec::new();
    let mut omitted_staged = 0usize;
    let mut omitted_unstaged = 0usize;
    let mut omitted_untracked = 0usize;
    let mut omitted_gaps = Vec::new();

    loop {
        let output = input.render_sections(
            HEADING,
            &git_summary,
            &snapshot,
            &events,
            &commands,
            &gaps,
            &omitted_events,
            &omitted_commands,
            [omitted_staged, omitted_unstaged, omitted_untracked],
            &omitted_gaps,
            2 * 1024,
            6 * 1024,
        )?;
        if output.len() <= max_bytes {
            return Ok(output);
        }
        if let Some((sequence, _)) = events.first() {
            omitted_events.push(*sequence);
            events.remove(0);
            continue;
        }
        if let Some(command) = commands.first() {
            omitted_commands.push(command.sequence);
            commands.remove(0);
            continue;
        }
        if snapshot.untracked.pop().is_some() {
            omitted_untracked += 1;
            continue;
        }
        if snapshot.unstaged.pop().is_some() {
            omitted_unstaged += 1;
            continue;
        }
        if snapshot.staged.pop().is_some() {
            omitted_staged += 1;
            continue;
        }
        if gaps.len() > 1 {
            omitted_gaps.push(gaps.remove(0).sequence);
            continue;
        }
        return Err(crate::error::Error::InvalidState(
            "required handoff facts exceed configured 64 KiB limit".into(),
        ));
    }
}
```

`git_summary` is computed from the full snapshot before detail removal and contains counts plus a SHA-256 of the sorted typed facts. `render_sections` must emit the approved order: transition identity and repository facts; transition and narrative checkpoint boundaries; Git counts/fingerprint and selected path details; selected normalized events; selected recent commands; latest recognized test; latest failed command; capture gaps; omitted ranges/counts and the exact commands `sesh log --json` and `sesh inspect`. Sort paths lexically and reject unsorted or duplicate sequence inputs rather than silently reordering facts. Range rendering groups contiguous omitted sequences, so gaps are never represented as one misleading range.

Add a `#[cfg(test)] HandoffInput::fixture` using a transition at sequence 19, a Claude-authored narrative at sequence 10, a `feat/oauth` snapshot with one staged path, one recent `cargo test` failure, and next step `Fix callback integration test`. This makes the Step 1 assertions self-contained without production fixture constructors.

Do not add a summarizer. The latest test and failure sections, Git counts/fingerprint, and at least the latest capture gap are required. Selection drops oldest events, oldest recent commands, lexically last path details, then oldest gap details; every omission remains explicit. Cap each capture-gap message at 1 KiB with an omitted-byte count, cap the latest-test output excerpt at 2 KiB, and make the failure excerpt trim only at UTF-8 character boundaries, keeping the first 2 KiB and final 6 KiB where possible with the exact omitted byte count.

- [x] **Step 4: Add golden coverage for no narrative checkpoint**

Add a test whose `narrative_checkpoint` is `None`. Assert the handoff literally says:

```text
No narrative checkpoint exists. Objective, decisions, assumptions, and next steps were not checkpointed.
```

Run: `rtk cargo test handoff::tests`

Expected: PASS.

- [x] **Step 5: Verify determinism and commit**

Run:

```bash
rtk cargo test handoff::tests
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk git add src/handoff.rs
rtk git commit -m "feat: render deterministic provider handoffs"
```

### Task 10: Build session-scoped Claude and Codex launch adapters

**Files:**
- Create: `src/provider/claude.rs`
- Create: `src/provider/codex.rs`
- Create: `src/provider/assets/claude-plugin.json`
- Create: `src/provider/assets/claude-hooks.json`
- Modify: `src/provider/mod.rs`
- Test: `src/provider/claude.rs`
- Test: `src/provider/codex.rs`

- [x] **Step 1: Write failing launch-spec tests**

Add tests that build both adapters with cwd `/work/oauth/apps/web`, inbox `/state/run/inbox`, integration root `/state/integrations`, and provider flags `--model test`. Assert:

```rust
assert_eq!(claude.program, std::ffi::OsString::from("claude"));
assert!(claude.args.windows(2).any(|pair| {
    pair == [
        std::ffi::OsString::from("--plugin-dir"),
        std::ffi::OsString::from("/state/integrations/claude/1"),
    ]
}));
assert!(claude.args.windows(2).any(|pair| {
    pair == [
        std::ffi::OsString::from("--add-dir"),
        std::ffi::OsString::from("/state/run/inbox"),
    ]
}));
assert_eq!(codex.program, std::ffi::OsString::from("codex"));
assert!(codex.args.iter().any(|arg| arg.to_string_lossy().contains("hooks.SessionStart")));
assert!(codex.args.windows(2).any(|pair| {
    pair == [
        std::ffi::OsString::from("--add-dir"),
        std::ffi::OsString::from("/state/run/inbox"),
    ]
}));
assert!(codex.args.windows(2).any(|pair| {
    pair == [
        std::ffi::OsString::from("-C"),
        std::ffi::OsString::from("/work/oauth/apps/web"),
    ]
}));
```

Also assert neither argument vector contains a transcript path, prompt content, session ID, or dynamically interpolated session data. The only shell expansion allowed in hook definitions is the exact quoted static token `$SESH_HOOK_BIN`.

- [x] **Step 2: Run adapter tests and verify launch types are absent**

Run: `rtk cargo test provider::claude::tests provider::codex::tests`

Expected: FAIL with missing adapters.

- [x] **Step 3: Add the provider contract**

Add `StateLayout::integrations() -> PathBuf`, returning `<root>/integrations`; adapter setup creates versioned children with mode `0700`.

Replace `src/provider/mod.rs` with:

```rust
pub mod claude;
pub mod codex;
pub mod hook;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::model::Provider;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchContext<'a> {
    pub cwd: &'a Path,
    pub inbox: &'a Path,
    pub integration_root: &'a Path,
    pub hook_bin: &'a Path,
    pub provider_args: &'a [OsString],
    pub bootstrap: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: BTreeMap<OsString, OsString>,
    pub cwd: PathBuf,
}

pub trait ProviderAdapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn launch_spec(&self, context: LaunchContext<'_>) -> Result<LaunchSpec>;
    fn setup(&self, integration_root: &Path) -> Result<()>;
    fn probe(&self) -> Result<String>;
}

pub fn adapter(provider: Provider) -> Box<dyn ProviderAdapter> {
    match provider {
        Provider::Claude => Box::new(claude::ClaudeAdapter),
        Provider::Codex => Box::new(codex::CodexAdapter),
    }
}
```

- [x] **Step 4: Add static Claude plugin assets**

Create `src/provider/assets/claude-plugin.json`:

```json
{"name":"sesh","description":"Capture the active Sesh coding session","version":"1.0.0"}
```

Create `src/provider/assets/claude-hooks.json`:

```json
{
  "hooks": {
    "SessionStart": [{"matcher":"startup|resume|clear|compact","hooks":[{"type":"command","command":"\"$SESH_HOOK_BIN\" __hook claude","timeout":30}]}],
    "UserPromptSubmit": [{"hooks":[{"type":"command","command":"\"$SESH_HOOK_BIN\" __hook claude","timeout":30}]}],
    "PreToolUse": [{"hooks":[{"type":"command","command":"\"$SESH_HOOK_BIN\" __hook claude","timeout":30}]}],
    "PostToolUse": [{"hooks":[{"type":"command","command":"\"$SESH_HOOK_BIN\" __hook claude","timeout":120}]}],
    "PostToolUseFailure": [{"hooks":[{"type":"command","command":"\"$SESH_HOOK_BIN\" __hook claude","timeout":120}]}],
    "Stop": [{"hooks":[{"type":"command","command":"\"$SESH_HOOK_BIN\" __hook claude","timeout":120}]}]
  }
}
```

`ClaudeAdapter::setup` must atomically materialize these embedded strings at:

```text
<integration_root>/claude/1/.claude-plugin/plugin.json
<integration_root>/claude/1/hooks/hooks.json
```

Use immutable create for a missing versioned asset. If a path already exists, hash and compare it to the embedded bytes; accept an exact match and refuse a mismatch instead of overwriting trusted hook definitions in place. Hook changes require a new integration version. Both launch specs set `SESH_HOOK_BIN` to the canonical `context.hook_bin` and the static hook commands quote that one variable, preventing repository-local PATH shadowing. `launch_spec` prepends `--plugin-dir`, the versioned directory, `--add-dir`, and the inbox. It then appends user provider arguments. Append the bootstrap positional prompt only when `context.bootstrap` is `Some`.

- [x] **Step 5: Implement the Codex per-launch overlay**

Use repeated `-c` arguments with these exact static TOML values:

```text
hooks.SessionStart=[{matcher="startup|resume|clear|compact",hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=30}]}]
hooks.UserPromptSubmit=[{hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=30}]}]
hooks.PreToolUse=[{hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=30}]}]
hooks.PostToolUse=[{hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=120}]}]
hooks.Stop=[{hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=120}]}]
```

`CodexAdapter::launch_spec` appends `--add-dir <inbox>`, `-C <cwd>`, then user provider flags, then the optional bootstrap prompt. `setup` immutably writes the exact overlay lines to `<integration_root>/codex/1/hooks.txt` for inspection, accepts only an existing exact-byte match, and invokes no model. `probe` executes `<provider> --version` and returns trimmed stdout for both adapters.

- [x] **Step 6: Verify adapters preserve flags and contain no session content**

Run:

```bash
rtk cargo test provider::claude::tests provider::codex::tests
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. Use `OsString` comparisons; do not convert provider arguments through UTF-8 or a shell.

- [x] **Step 7: Commit provider launch adapters**

```bash
rtk git add src/provider
rtk git commit -m "feat: add claude and codex adapters"
```

### Task 11: Protect each session with a recoverable run lease

**Files:**
- Create: `src/store/lease.rs`
- Create: `src/supervisor.rs`
- Modify: `src/store/mod.rs`
- Modify: `src/lib.rs`
- Test: `src/store/lease.rs`
- Test: `src/supervisor.rs`

- [x] **Step 1: Write failing lease identity tests**

Add tests proving:

```rust
#[test]
fn current_process_identity_is_live() {
    let identity = ProcessIdentity::capture(std::process::id()).unwrap();
    assert!(identity.is_live().unwrap());
}

#[test]
fn pid_reuse_is_not_treated_as_the_same_process() {
    let identity = ProcessIdentity {
        pid: std::process::id(),
        start_token: "definitely-not-this-process".into(),
    };
    assert!(!identity.is_live().unwrap());
}

#[test]
fn live_lease_cannot_be_replaced() {
    let temp = tempfile::TempDir::new().unwrap();
    let store = LeaseStore::new(temp.path());
    let lease = RunLease::fixture(ProcessIdentity::capture(std::process::id()).unwrap());
    store.create(&lease).unwrap();
    assert!(store.create(&lease).unwrap_err().to_string().contains("active provider"));
}

#[test]
fn operation_lock_serializes_lease_check_and_create() {
    let temp = tempfile::TempDir::new().unwrap();
    let first = SessionOperationLock::acquire(temp.path()).unwrap();
    let path = temp.path().to_path_buf();
    let waiting = std::thread::spawn(move || {
        let _second = SessionOperationLock::acquire(&path).unwrap();
        42
    });
    std::thread::sleep(std::time::Duration::from_millis(25));
    assert!(!waiting.is_finished());
    drop(first);
    assert_eq!(waiting.join().unwrap(), 42);
}
```

- [x] **Step 2: Run the lease tests and verify they fail**

Run: `rtk cargo test store::lease::tests`

Expected: FAIL because lease types are undefined.

- [x] **Step 3: Implement process identity and atomic lease state**

Create `src/store/lease.rs` with:

```rust
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{Provider, RunId, SessionId};
use crate::store::refs::{read_json, write_json};

pub struct SessionOperationLock {
    file: std::fs::File,
}

impl SessionOperationLock {
    pub fn acquire(session_dir: &Path) -> Result<Self> {
        use fs2::FileExt;
        use std::os::unix::fs::OpenOptionsExt;

        let path = session_dir.join("operation.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|source| crate::error::io(&path, source))?;
        file.lock_exclusive()
            .map_err(|source| crate::error::io(&path, source))?;
        Ok(Self { file })
    }
}

impl Drop for SessionOperationLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_token: String,
}

impl ProcessIdentity {
    pub fn capture(pid: u32) -> Result<Self> {
        let output = Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .map_err(|error| Error::Command(format!("cannot inspect process {pid}: {error}")))?;
        if !output.status.success() {
            return Err(Error::Command(format!("process {pid} does not exist")));
        }
        let start_token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if start_token.is_empty() {
            return Err(Error::Command(format!("process {pid} has no start identity")));
        }
        Ok(Self { pid, start_token })
    }

    pub fn is_live(&self) -> Result<bool> {
        match Self::capture(self.pid) {
            Ok(current) => Ok(current == *self),
            Err(Error::Command(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunLease {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub provider: Provider,
    pub host: String,
    pub supervisor: ProcessIdentity,
    pub child: Option<ProcessIdentity>,
}

#[cfg(test)]
impl RunLease {
    fn fixture(supervisor: ProcessIdentity) -> Self {
        Self {
            schema_version: 1,
            session_id: SessionId::new(),
            run_id: RunId::new(),
            provider: Provider::Claude,
            host: "test-host".into(),
            supervisor,
            child: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LeaseStore {
    path: PathBuf,
}

impl LeaseStore {
    pub fn new(session_dir: &Path) -> Self {
        Self {
            path: session_dir.join("refs/active-run.json"),
        }
    }

    pub fn create(&self, lease: &RunLease) -> Result<()> {
        if let Some(existing) = self.read()? {
            let child_is_live = match existing.child.as_ref() {
                Some(child) => child.is_live()?,
                None => false,
            };
            if existing.supervisor.is_live()? || child_is_live {
                return Err(Error::InvalidState(format!(
                    "session already has active provider {}",
                    existing.run_id
                )));
            }
            return Err(Error::InvalidState(format!(
                "session has stale lease {}; recover it before launching",
                existing.run_id
            )));
        }
        write_json(&self.path, lease)
    }

    pub fn update_child(&self, expected: &RunId, child: ProcessIdentity) -> Result<()> {
        let mut lease = self.read()?.ok_or_else(|| Error::InvalidState("active run lease disappeared".into()))?;
        if &lease.run_id != expected {
            return Err(Error::InvalidState("active run lease changed".into()));
        }
        lease.child = Some(child);
        write_json(&self.path, &lease)
    }

    pub fn read(&self) -> Result<Option<RunLease>> {
        if self.path.exists() {
            read_json(&self.path).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn clear(&self, expected: &RunId) -> Result<()> {
        let lease = self.read()?.ok_or_else(|| Error::InvalidState("active run lease disappeared".into()))?;
        if &lease.run_id != expected {
            return Err(Error::InvalidState("refusing to clear a different run lease".into()));
        }
        std::fs::remove_file(&self.path).map_err(|source| crate::error::io(&self.path, source))
    }
}
```

Get the host with `hostname` once when creating a lease; trim it and fail if it is empty. Add `pub mod lease;` to `src/store/mod.rs`.

- [x] **Step 4: Write a failing supervisor test with a fake provider**

The fake provider must invoke the test's handshake callback, sleep briefly, and exit `23`. Assert `Supervisor::launch` returns `handshake_completed: true` and `facts.exit_code: Some(23)`, captures a child process identity, and leaves the lease intact. Then simulate the caller appending `run.stopped`, clear the expected lease, and assert it is absent. Add separate cases for exit-before-handshake and handshake timeout; both must return observed exit facts with `handshake_completed: false` so the caller can journal the failed run before clearing its lease.

- [x] **Step 5: Implement inherited-I/O launch and handshake polling**

Create `src/supervisor.rs` with:

```rust
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .envs(&spec.env)
            .current_dir(&spec.cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| Error::Command(format!("cannot launch {:?}: {error}", spec.program)))?;
        let child_identity = match ProcessIdentity::capture(child.id()) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if let Err(error) = LeaseStore::new(&store.session_dir())
            .update_child(run_id, child_identity.clone())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let _signals = match SignalForwarder::start(child_identity) {
            Ok(forwarder) => forwarder,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };

        let started = Instant::now();
        loop {
            let handshook = store.events()?.iter().any(|event| {
                event.run_id.as_ref() == Some(run_id)
                    && matches!(&event.kind, EventKind::RunHandshake { .. })
            });
            if handshook {
                break;
            }
            if let Some(status) = child.try_wait().map_err(|error| Error::Command(format!("cannot poll provider: {error}")))? {
                return Ok(SupervisionOutcome {
                    facts: ExitFacts {
                        exit_code: status.code(),
                        signal: status.signal(),
                    },
                    handshake_completed: false,
                    startup_failure: Some("provider exited before SessionStart handshake".into()),
                });
            }
            if started.elapsed() >= deadline {
                let _ = child.kill();
                let status = child.wait().map_err(|error| Error::Command(format!("cannot reap provider after handshake timeout: {error}")))?;
                return Ok(SupervisionOutcome {
                    facts: ExitFacts {
                        exit_code: status.code(),
                        signal: status.signal(),
                    },
                    handshake_completed: false,
                    startup_failure: Some("provider did not complete SessionStart within 60 seconds".into()),
                });
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        let status = child.wait().map_err(|error| Error::Command(format!("cannot wait for provider: {error}")))?;
        Ok(SupervisionOutcome {
            facts: ExitFacts {
                exit_code: status.code(),
                signal: status.signal(),
            },
            handshake_completed: true,
            startup_failure: None,
        })
    }
}
```

Wrap the spawned process immediately in a `ChildGuard` whose `Drop` kills and reaps unless a successful `wait` has marked it reaped. Route `try_wait`, `kill`, and `wait` through the guard. This covers journal-read errors, signal-forwarder setup errors, identity errors, and every future `?` path; add a test that injects a handshake journal read failure and proves the fake child is gone.

Implement `SignalForwarder` in the same file. `start` accepts the captured `ProcessIdentity`, registers `SIGTERM` and `SIGHUP` with `signal_hook::iterator::Signals`, saves its `Handle`, and spawns a thread. Before each `libc::kill`, re-capture the PID identity and forward only if it still equals the child identity; stop the thread when it does not. Its `Drop` implementation closes the handle and joins the thread, so all early returns above are covered. Add a safety comment immediately above `libc::kill` explaining the identity check and residual check-to-signal race. Do not register `SIGINT`; the provider and Sesh share the terminal foreground process group and both receive terminal Ctrl-C.

- [x] **Step 6: Verify process and lease behavior**

Run:

```bash
rtk cargo test store::lease::tests supervisor::tests
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. The test must fail if `ProcessIdentity::is_live` checks PID without comparing `start_token`.

- [x] **Step 7: Commit leases and supervision**

```bash
rtk git add src
rtk git commit -m "feat: supervise provider runs with leases"
```

### Task 12: Implement `sesh run` and internal hook ingestion

**Files:**
- Create: `src/app.rs`
- Modify: `src/cli.rs`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`
- Modify: `src/provider/hook.rs`
- Modify: `tests/support/mod.rs`
- Create: `tests/run_session.rs`

- [x] **Step 1: Add executable-fixture support and a failing run test**

Add to `tests/support/mod.rs`:

```rust
use std::os::unix::fs::PermissionsExt;

pub fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

```

Create `tests/run_session.rs`. It must initialize a repository, create `apps/web`, and place fake `claude` plus a decoy executable named `sesh` that exits `99` in a temporary `bin`. Invoke the real test binary through `cargo_bin_cmd!`; successful hooks prove `SESH_HOOK_BIN` prevents PATH shadowing. Run:

```rust
cargo_bin_cmd!("sesh")
    .current_dir(&cwd)
    .env("SESH_HOME", &state)
    .env("PATH", path_with_fixture_bin)
    .arg("run")
    .arg("claude")
    .assert()
    .code(23);
```

The fake `claude` script is:

```bash
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
printf '%s' '{"session_id":"native-claude","transcript_path":null,"cwd":"'"$PWD"'","permission_mode":"default","hook_event_name":"SessionStart","source":"startup"}' | "$SESH_HOOK_BIN" __hook claude >/dev/null
printf '%s' '{"session_id":"native-claude","transcript_path":null,"cwd":"'"$PWD"'","permission_mode":"default","hook_event_name":"UserPromptSubmit","prompt":"Implement OAuth"}' | "$SESH_HOOK_BIN" __hook claude >/dev/null
exit 23
```

After exit, invoke `sesh log --json` only after Task 15; for this task inspect `$SESH_HOME/sessions/*/events.jsonl` directly and assert the ordered event types include `session.created`, `git.snapshot`, `run.started`, `run.handshake`, `provider.prompt.submitted`, and `run.stopped`. Add a second fake provider that submits the identical `SessionStart` and `PostToolUse` payload twice; assert each idempotency key produces one event and one post-tool Git snapshot.

- [x] **Step 2: Run the test and verify `run` is not recognized**

Run: `rtk cargo test --test run_session`

Expected: FAIL because the CLI has no `run` or `__hook` commands.

- [x] **Step 3: Expand the CLI only with commands implemented in this task**

Replace `src/cli.rs` with:

```rust
use std::ffi::OsString;

use clap::{Parser, Subcommand};

use crate::model::Provider;

#[derive(Debug, Parser)]
#[command(name = "sesh", version, about = "Switch coding providers without losing your place")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Run {
        provider: Provider,
        #[arg(last = true, allow_hyphen_values = true)]
        provider_args: Vec<OsString>,
    },
    #[command(name = "__hook", hide = true)]
    Hook { provider: Provider },
}
```

- [x] **Step 4: Make normalized hooks carry and validate cwd**

Change `normalize` to return:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedHook {
    pub cwd: std::path::PathBuf,
    pub event_name: String,
    pub event: HookEvent,
}
```

Read required `cwd` and `hook_event_name` once, retain them, and update hook contract tests. Canonicalize hook cwd and require it to be a directory inside the canonical bound worktree. `SessionStart` must equal the saved cwd used for launch. For later hooks, add a journal `append_optional` primitive whose closure sees committed events under the journal lock; use it to append `cwd.changed` only when the relative cwd differs from the last committed value. This prevents concurrent hooks from producing duplicate cwd facts. Add `SessionStore::saved_cwd_relative()` that derives the latest value from verified events and falls back to the immutable initial value in `SessionMeta`; never rewrite `meta.json`.

- [x] **Step 5: Implement app dispatch, run creation, and hook mapping**

Create `src/app.rs` with `run(cli, environment, runtime) -> Result<i32>` and two focused functions:

```rust
pub fn run_command(
    provider: Provider,
    provider_args: Vec<std::ffi::OsString>,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> Result<i32>;

pub fn ingest_hook(
    provider: Provider,
    environment: &Environment,
    runtime: &dyn Runtime,
    input: impl std::io::Read,
) -> Result<crate::provider::hook::HookOutput>;
```

Centralize state resolution in a private `resolve_layout(environment, cwd)` helper that performs the resolve/ensure/canonicalize sequence once. Every later command reuses it; no command reconstructs `$SESH_HOME` independently.

Before implementing orchestration, add an optional idempotency field to the canonical event:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub idempotency_key: Option<String>,
```

Add the same field to `PendingEvent` and `PendingEventMeta`, setting it to `None` in all earlier constructors. Add `EventJournal::append_idempotent`. While holding the journal lock, it finds an existing event with the same key. An exact match of run ID, provider, and `EventKind` returns `AppendOutcome::Existing`; a different payload for the same key is a hard conflict; otherwise it appends and returns `AppendOutcome::Appended`. It must share the same locked append helper as `append` and `append_with`, not copy the sequence/checksum/write algorithm.

`run_command` must:

1. Resolve `StateLayout` against the current cwd, call `ensure`, replace it with `canonicalized()`, and use that one absolute root for every ref, environment value, and later hook.
2. Observe Git; refuse if the worktree already has a session, otherwise create `SessionStore`.
3. Acquire `SessionOperationLock`, prove no lease exists, and keep this guard through lease creation.
4. Materialize adapter setup assets.
5. Build `runs/.<run-id>.tmp/inbox/checkpoints`, write mode-`0600` protocol-only `handoff.md`, sync files/directories, then rename the complete run directory to `runs/<run-id>`.
6. Verify the resulting inbox and checkpoint directories are mode `0700` and not symlinks.
7. Append `run.started`.
8. Create `RunLease` with current supervisor identity and no child.
9. Release `SessionOperationLock` before spawning; the SessionStart hook must be able to acquire the journal lock during the handshake.
10. Build a launch spec with these environment values:

```text
SESH_HOME=<resolved root>
SESH_SESSION_ID=<session UUID>
SESH_RUN_ID=<run UUID>
SESH_PROVIDER=<claude|codex>
SESH_PROVIDER_VERSION=<adapter probe output>
SESH_HOOK_BIN=<canonical current Sesh executable>
SESH_HANDOFF_PATH=<run inbox>/handoff.md
SESH_CHECKPOINT_INBOX=<run inbox>/checkpoints
```

11. Launch through `Supervisor` with a 60-second handshake deadline.
12. On every spawn error or supervision outcome, reacquire `SessionOperationLock`, append `run.stopped` with all available facts, append a post-exit Git snapshot, then clear only the expected lease.
13. Return the provider exit code or `128 + signal`. If the handshake did not complete, return a typed startup error only after the failed run and lease cleanup are durable.

`ingest_hook` must read at most 8 MiB plus one sentinel byte, normalize the event, parse environment IDs, confirm the lease run/provider, validate cwd as above, and record any post-handshake cwd change before mapping:

```text
SessionStarted       -> run.handshake
UserPromptSubmitted  -> provider.prompt.submitted
ToolRequested        -> provider.tool.requested
ToolCompleted        -> provider.tool.completed, then git.snapshot
ToolFailed           -> provider.tool.failed, then git.snapshot
Stopped              -> provider.stop.observed, then git.snapshot
```

Use these idempotency keys when the provider supplied a stable identity:

```text
SessionStarted  handshake:<native-session-id>
ToolRequested   pre:<tool-use-id>
ToolCompleted   post:<tool-use-id>
ToolFailed      post:<tool-use-id>
Stopped         stop:<native-session-id>
```

Do not invent a key for Claude prompts, which have no stable prompt/turn ID. On `AppendOutcome::Existing`, return the same hook response without appending the follow-up Git snapshot. A conflicting payload for one key is a capture failure.

For SessionStart, return `session_start_output(contents_of_SESH_HANDOFF_PATH)`. Resolve that path from the run ID rather than trusting an arbitrary environment path, require a mode-`0600` regular file, and cap it at 65,536 bytes.

On any ingestion failure, atomically write `runs/<run-id>/capture-failed.json` with phase, timestamp, and error before returning `capture_failure_output(provider, event_name, error)`. A later `UserPromptSubmit` or `PreToolUse` must check this sentinel first and block until `sesh doctor --repair` proves storage healthy and clears it. If the input is too malformed to recover `hook_event_name`, write the sentinel, print a concise error to stderr, and exit non-zero so the provider treats the hook itself as failed.

- [x] **Step 6: Wire main without swallowing child exit codes**

`src/lib.rs` exports `app`, and exposes:

```rust
pub fn run_from<I, T>(args: I) -> crate::error::Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::Parser;
    let cli = crate::cli::Cli::parse_from(args);
    crate::app::run(
        cli,
        &crate::store::Environment::capture(),
        &crate::runtime::SystemRuntime,
    )
}
```

`src/main.rs` becomes:

```rust
use std::process::ExitCode;

fn main() -> ExitCode {
    match sesh::run_from(std::env::args_os()) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("sesh: {error}");
            ExitCode::FAILURE
        }
    }
}
```

- [x] **Step 7: Verify the first real vertical slice**

Run:

```bash
rtk cargo test --test run_session
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. Confirm the fake provider receives its terminal streams by inheriting them; no output-capture pipe may replace stdout/stderr.

- [x] **Step 8: Commit `sesh run`**

```bash
rtk git add src tests
rtk git commit -m "feat: run providers inside a sesh session"
```

### Task 13: Accept human and provider narrative checkpoints

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/app.rs`
- Modify: `src/checkpoint.rs`
- Create: `tests/checkpoint_cli.rs`

- [ ] **Step 1: Write failing provider-inbox and human-checkpoint tests**

The provider test sets `SESH_CHECKPOINT_INBOX`, sends valid `NarrativeInput` JSON to:

```bash
sesh checkpoint --format json --from-provider
```

Assert the command creates one mode-`0600` atomic submission in the inbox and does not change `events.jsonl`.

The human test creates a session, pipes the same JSON without `--from-provider`, and asserts a narrative checkpoint event, JSON artifact, Markdown artifact, and both latest refs are committed.

- [ ] **Step 2: Run the tests and verify checkpoint is not a CLI command**

Run: `rtk cargo test --test checkpoint_cli`

Expected: FAIL with an unrecognized subcommand.

- [ ] **Step 3: Add the checkpoint CLI grammar**

Add:

```rust
Checkpoint {
    #[arg(long, value_enum, default_value = "json")]
    format: CheckpointFormat,
    #[arg(long)]
    from_provider: bool,
}
```

and:

```rust
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum CheckpointFormat {
    Json,
}
```

Detect stdin with `std::io::IsTerminal`. The human no-stdin editor path is also required: write a complete JSON template to a private temporary file under `$SESH_HOME`, parse `$VISUAL` then `$EDITOR` with `shell_words::split`, invoke the resulting program and argument vector with the temporary path appended, parse on successful editor exit, and always remove the temporary file. If neither variable exists, report the exact pipe command accepted by the JSON path. If `SESH_RUN_ID` is present, refuse the human path and require `--from-provider`; an attached provider must not be able to select the canonical human-write path.

- [ ] **Step 4: Implement inbox submission and promotion**

`--from-provider` must require `SESH_HOME`, `SESH_SESSION_ID`, `SESH_RUN_ID`, and `SESH_CHECKPOINT_INBOX`; derive `sessions/<session>/runs/<run>/inbox/checkpoints` from those IDs and require exact equality after canonicalizing the existing directory. Cap transport input at 64 KiB, then enforce the typed 32 KiB narrative limit. Create `<uuid>.json.tmp` with `create_new`, sync it, rename it to `<uuid>.json`, and sync the directory. It must not open `SessionStore` or trust an arbitrary inbox path.

Add `promote_inbox(store, runtime, run_id, provider, inbox)` to `src/checkpoint.rs`. It must:

1. List only regular `*.json` files without following symlinks.
2. Sort by filename for deterministic promotion.
3. Revalidate every submission against the current journal high-water mark.
4. Use `SessionStore::create_narrative_checkpoint`.
5. Write the Markdown rendering.
6. Delete a submission only after the event, JSON, Markdown, and refs succeed.

Call `promote_inbox` at the beginning of every internal hook and again after child exit. Thus a provider Bash checkpoint is promoted by the following PostToolUse/Stop hook without adding the canonical store as an adapter writable root. Document that unrestricted same-user shell access is outside this boundary.

- [ ] **Step 5: Verify both author paths and immutable refs**

Run:

```bash
rtk cargo test --test checkpoint_cli
rtk cargo test checkpoint::tests
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. The provider checkpoint author is the active provider; the human author is `human`. A symlink submission must be refused and retained for inspection.

- [ ] **Step 6: Commit checkpoint commands**

```bash
rtk git add src tests
rtk git commit -m "feat: capture explicit session checkpoints"
```

### Task 14: Pass the North Star Claude-to-Codex acceptance test

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/app.rs`
- Modify: `src/handoff.rs`
- Modify: `tests/support/mod.rs`
- Create: `tests/north_star.rs`

- [ ] **Step 1: Write the complete failing acceptance test**

Create `tests/north_star.rs` that:

1. Creates a repository, linked worktree `oauth worktree`, branch `feat/oauth`, and cwd `apps/web`.
2. Installs fake `claude`, fake `codex`, and a failing decoy named `sesh` in a temporary `bin`; hooks must use `SESH_HOOK_BIN`.
3. Runs fake Claude from the nested cwd.
4. Invokes `sesh switch codex` from the worktree root.
5. Asserts fake Codex recorded the original nested cwd.
6. Asserts its SessionStart hook output contains objective, decision, dirty file, passing test, failing test output, and next step.
7. Asserts no `.sesh`, `.claude`, or `.codex` state was written to the application worktree.

Use this fake Claude body:

```bash
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
hook() { printf '%s' "$2" | "$SESH_HOOK_BIN" __hook claude >/dev/null; }
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook start '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"SessionStart","source":"startup"}'
hook prompt '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"UserPromptSubmit","prompt":"Implement OAuth callback with PKCE"}'
hook pre '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_unit"},"tool_use_id":"tool-pass"}'
hook post '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_unit"},"tool_response":{"stdout":"1 passed","stderr":"","exit_code":0},"tool_use_id":"tool-pass"}'
printf 'callback with pkce\n' > oauth_callback.rs
hook fail '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_integration"},"tool_response":{"stdout":"0 passed; 1 failed","stderr":"assertion failed: callback state","exit_code":101},"tool_use_id":"tool-fail"}'
printf '%s' '{"objective":"Implement OAuth callback with PKCE","summary":"Callback and PKCE are implemented","decisions":[{"statement":"Keep verifier in the session cookie","reason":"Avoid server-side state"}],"assumptions":[],"constraints":[],"completed":["OAuth callback","PKCE"],"in_progress":[],"blockers":["integration test failure"],"next_steps":["Fix callback integration test"],"related_event_sequences":[]}' | "$SESH_HOOK_BIN" checkpoint --format json --from-provider
hook stop '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"Stop"}'
exit 75
```

Use this fake Codex body:

```bash
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
printf '%s\n' "$PWD" > "$SESH_TEST_TRACE/codex.cwd"
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"codex-native","turn_id":"turn-1","transcript_path":null,"cwd":"'"$cwd_json"'","model":"gpt-test","permission_mode":"default","hook_event_name":"SessionStart","source":"startup"}' | "$SESH_HOOK_BIN" __hook codex > "$SESH_TEST_TRACE/codex.context.json"
exit 0
```

- [ ] **Step 2: Run the acceptance test and verify switch is absent**

Run: `rtk cargo test --test north_star -- --nocapture`

Expected: FAIL because `switch` is not implemented.

- [ ] **Step 3: Add switch grammar and transaction orchestration**

Add the same provider and trailing provider-flag fields as `run`:

```rust
Switch {
    provider: Provider,
    #[arg(last = true, allow_hyphen_values = true)]
    provider_args: Vec<std::ffi::OsString>,
}
```

Implement `switch_command` in this exact durable order:

1. Observe current worktree and resolve its `SessionStore`.
2. Acquire `SessionOperationLock`; hold it through new lease creation.
3. Refuse any live lease; recover only a same-host lease whose supervisor and child identities are both dead.
4. Resolve `meta.worktree.worktree.join(store.saved_cwd_relative())` and fail if it is not an existing directory.
5. Append a fresh Git snapshot and confirm its worktree key, Git directories, branch/HEAD, staged, unstaged, and untracked facts describe the same source worktree being switched.
6. Append `switch.requested` with previous provider derived from the provider field of the latest `run.started` event.
7. Resolve `latest-narrative-checkpoint`, then verify its sequence names a committed narrative checkpoint event and matching immutable JSON/Markdown before creating the transition checkpoint. A stale or forged ref is fatal.
8. Build `HandoffInput` only from committed events and immutable checkpoint/blob artifacts.
9. Build the entire run directory under `runs/.<run-id>.tmp`, write mode-`0600` `handoff.md` and bounded `recent-events.jsonl`, sync every file and directory, then rename to `runs/<run-id>`. Select newest complete event envelopes for `recent-events.jsonl` with the same omitted ranges as the Markdown handoff.
10. Run adapter setup, append `run.started`, and create the lease.
11. Release `SessionOperationLock`, then launch the adapter with the fixed bootstrap prompt from `handoff::BOOTSTRAP`.
12. On every launch result, reacquire the operation lock, promote the checkpoint inbox, append `run.stopped`, append Git state, and clear only the expected lease.

Do not modify, reset, stash, clean, or recreate the source worktree anywhere in this path. The acceptance test must fingerprint `.git`-excluded working files and the index before and after `switch` and prove they are unchanged apart from writes intentionally made by the fake providers.

The recognized-test classifier must tokenize without executing the command and recognize these initial exact forms, optionally preceded by `env`, `command`, or `rtk`:

```text
cargo test
pytest
python -m pytest
go test
npm test
npm run test
pnpm test
yarn test
bun test
```

Use `shell_words::split`; for `env`, skip leading `NAME=value` assignments and supported `-i`/`--ignore-environment` flags before matching. Do not expand variables, globs, substitutions, redirections, or compound shell syntax. Add a test with `touch sentinel; cargo test` that remains unrecognized and never creates `sentinel`.

Unrecognized commands remain commands. The renderer always includes the latest failed command independently of classification.

When a completed command has an opaque provider response but no structured exit code, retain the response through `ContentRef`, render the status as `unknown`, and add a handoff capture-gap line naming that event sequence. Never parse text such as `Process exited with code ...` to manufacture a structured status.

- [ ] **Step 4: Verify the North Star and repository separation**

Run:

```bash
rtk cargo test --test north_star -- --nocapture
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. Run the acceptance test twice to expose stale lease or ref cleanup bugs.

- [ ] **Step 5: Commit provider switching**

```bash
rtk git add src tests
rtk git commit -m "feat: switch providers without losing session state"
```

### Task 15: Add inspection and deletion commands

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/app.rs`
- Create: `tests/read_commands.rs`
- Create: `tests/delete_session.rs`

- [ ] **Step 1: Write failing JSON contracts for status, log, and inspect**

The tests create a session through the fake provider and assert:

```text
sesh status --json   -> session, provider, worktree, branch, HEAD, cwd, dirty paths, latest checkpoint, capture gaps
sesh log --json      -> one JSON event envelope per output line, in sequence order
sesh log --from 5    -> no event below sequence 5
sesh inspect --json  -> state root, session dir, event count, checkpoint files, blob references, permissions, active lease
```

Use `serde_json::Value` assertions rather than complete string snapshots so paths remain portable.

- [ ] **Step 2: Write a failing deletion safety test**

Assert `sesh delete` without a TTY or `--yes` refuses, `sesh delete --yes` refuses a live lease, and successful deletion removes the worktree ref and entire session directory but leaves every repository/worktree byte unchanged.

- [ ] **Step 3: Add CLI grammar and read-only projections**

Add:

```rust
Status { #[arg(long)] json: bool },
Log { #[arg(long)] from: Option<u64>, #[arg(long)] json: bool },
Inspect { #[arg(long)] json: bool },
Delete { #[arg(long)] yes: bool },
```

At the top of app dispatch, if `SESH_RUN_ID` is present, allow only the hidden hook and `checkpoint --from-provider`; refuse `run`, `switch`, human checkpoint, setup, doctor, status, log, inspect, and delete. This enforces the provider-facing CLI boundary even if an agent tries to invoke another Sesh command.

Build each output only from verified events, current Git observation, refs, and lease state. Add `SessionStore::envelopes()` so JSON log output preserves checksums rather than reconstructing envelopes from bare events. Human output may be formatted, but JSON field names are a stable V1 contract. `inspect` must never print blob contents unless the user separately asks `log`; it reports hashes and sizes.

- [ ] **Step 4: Implement complete-session deletion**

Deletion order:

1. Resolve and verify the session/worktree ref.
2. Refuse a live or foreign-host lease.
3. Require interactive `delete session <short-id>` confirmation unless `--yes`.
4. Reject a symlink or wrong-owner session directory, then rename it atomically to `$SESH_HOME/.deleting-<session-id>`.
5. Remove the worktree ref.
6. Recursively remove the renamed directory without following symlinks.
7. Sync the state root.

If step 5 fails, rename the session directory back. Document that deletion is not forensic erasure.

- [ ] **Step 5: Verify read and delete behavior**

Run:

```bash
rtk cargo test --test read_commands
rtk cargo test --test delete_session
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Commit inspection and deletion**

```bash
rtk git add src tests
rtk git commit -m "feat: inspect and delete local sessions"
```

### Task 16: Implement setup, doctor, and crash recovery

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/app.rs`
- Create: `src/doctor.rs`
- Create: `tests/recovery.rs`
- Create: `tests/doctor.rs`

- [ ] **Step 1: Write failing diagnostic tests**

Cover:

- Missing Git
- Missing provider executable
- Claude without `--plugin-dir` or `--add-dir`
- Codex without the stable enabled `hooks` feature in `codex features list`
- Integration asset checksum mismatch
- Insecure state permissions
- Invalid middle journal event
- Recoverable invalid final journal line
- Dead same-host supervisor and child lease
- Live orphan child lease
- Foreign-host lease
- SessionStart timeout

Every JSON diagnostic has `code`, `severity`, `message`, and optional `repair_command`. No diagnostic mutates state.

- [ ] **Step 2: Add setup and doctor grammar**

```rust
Setup { provider: Provider },
Doctor {
    #[arg(long)] json: bool,
    #[arg(long)] repair: bool,
},
```

`sesh setup claude` materializes assets and verifies `claude --help` contains `--plugin-dir` and `--add-dir`. In an interactive terminal, print `Review the Sesh plugin and hook commands, then exit without submitting a prompt.` and run `claude --plugin-dir <asset-dir>` with no prompt and no Sesh session environment. This opens the TUI but spends no model turn. In setup mode, `sesh __hook` exits successfully without capture only when all `SESH_SESSION_ID`, `SESH_RUN_ID`, and `SESH_HOME` are absent. A non-interactive terminal prints the exact equivalent command and exits with code 2.

`sesh setup codex` materializes the inspectable overlay, verifies `codex --help` contains `--config`, `--add-dir`, and `--cd`, and verifies `codex features list` reports `hooks stable true`. It then opens the Codex TUI with the exact static overlay and no prompt, with `SESH_HOOK_BIN` set to the canonical current executable. Before launch, print: `Open /hooks, review commands equal to '"$SESH_HOOK_BIN" __hook codex', trust them, then exit.` This spends no model turn. A non-interactive terminal prints the exact equivalent command and exits with code 2. Never add `--dangerously-bypass-hook-trust`.

- [ ] **Step 3: Implement read-only doctor checks**

Create `src/doctor.rs` with one function per diagnostic layer:

```rust
pub fn check_format(layout: &StateLayout) -> Vec<Diagnostic>;
pub fn check_permissions(layout: &StateLayout) -> Vec<Diagnostic>;
pub fn check_git(cwd: &std::path::Path) -> Vec<Diagnostic>;
pub fn check_provider(provider: Provider) -> Vec<Diagnostic>;
pub fn check_integrations(layout: &StateLayout) -> Vec<Diagnostic>;
pub fn check_sessions(layout: &StateLayout) -> Vec<Diagnostic>;
```

Plain `doctor` aggregates through a non-mutating journal scanner; it never opens a journal through the repairing read path. A partial final line is reported as repairable with the exact byte count, while complete-tail or middle corruption is fatal.

`doctor --repair` is the only diagnostic mutation path. For each session, acquire `SessionOperationLock`, repair only an incomplete final journal line, rebuild checkpoint refs solely from committed `checkpoint.created` events whose immutable artifacts validate, and remove `capture-failed.json` only after a private create/sync/remove probe in the journal directory succeeds. Report orphan checkpoint artifacts and temporary files but do not delete them automatically. Emit one diagnostic per mutation. Add focused tests proving plain `doctor` leaves all bytes and mtimes unchanged and `--repair` performs only the listed changes.

- [ ] **Step 4: Implement explicit stale-lease recovery on run/switch**

Before launch, while holding `SessionOperationLock`, if the lease host equals the current host and both process identities are dead:

1. Extend `run.recovered` to store the supervisor PID/start token, optional child PID/start token, host, and reason; append that complete fact.
2. Append a fresh Git snapshot.
3. Clear exactly that run ID.
4. Continue the requested command.

If either process is live or the host differs, refuse. Never send a signal from recovery.

- [ ] **Step 5: Add a fail-closed capture regression test**

Change `events.jsonl` to mode `0400` after SessionStart. Invoke a fake PreToolUse hook and assert the hook process exits `0` with `permissionDecision: deny`; assert `capture-failed.json` is written and the fake provider does not execute its intended file write. Restore mode `0600` during cleanup, run `sesh doctor --repair`, and prove the next hook is accepted.

- [ ] **Step 6: Verify recovery and diagnostics**

Run:

```bash
rtk cargo test --test recovery -- --nocapture
rtk cargo test --test doctor
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 7: Commit setup and recovery**

```bash
rtk git add src tests
rtk git commit -m "feat: diagnose and recover session capture"
```

### Task 17: Document, verify, and freeze the core V1 contract

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `README.md`
- Create: `tests/provider_smoke.rs`
- Modify: `tests/cli_contract.rs`

- [ ] **Step 1: Extend the CLI contract test**

Assert help lists exactly these public core commands before fork lands:

```text
run
switch
checkpoint
status
log
inspect
delete
setup
doctor
```

Assert `__hook` is absent from help.

- [ ] **Step 2: Add ignored real-provider smoke tests**

`tests/provider_smoke.rs` must mark tests `#[ignore = "requires the provider CLI to be installed"]`. Claude smoke materializes the plugin and runs `claude plugin validate <plugin-dir>`. Codex smoke runs `codex --strict-config` with every static `-c` hook overlay and the `features list` subcommand. Neither opens an agent session, requires authentication, submits a prompt, or consumes model quota. Both use a temporary `SESH_HOME` and clean it on success or failure.

- [ ] **Step 3: Write the user-facing README**

Document:

- Install from source
- `sesh setup claude` and `sesh setup codex`
- The North Star `run`/`switch` workflow
- Existing-worktree default
- Checkpoint JSON and editor flows
- `status`, `log`, and `inspect`
- Plaintext/session-secret warning
- Same-user unrestricted provider shells are not an OS security boundary
- Complete deletion and non-forensic-erasure warning
- No transcript parsing, embeddings, cloud, or source commits
- Current macOS/Linux and Git requirements
- Opt-in smoke-test commands

Include one exact end-to-end example and no future feature promises.

- [ ] **Step 4: Add macOS and Linux CI**

Create `.github/workflows/ci.yml`:

```yaml
name: ci

on:
  push:
  pull_request:

jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy,rustfmt
      - run: cargo fmt --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test --all-targets --all-features
      - run: cargo doc --no-deps
```

- [ ] **Step 5: Run the release gate**

Run:

```bash
rtk cargo fmt --check
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo test --all-targets --all-features
rtk cargo doc --no-deps
rtk git status --short
```

Expected: all commands PASS; status shows only the intended documentation/CI/test changes before commit.

- [ ] **Step 6: Audit the acceptance evidence**

Run:

```bash
rtk cargo test --test north_star -- --nocapture
rtk cargo test --test recovery -- --nocapture
rtk cargo test --test delete_session
```

Expected: PASS. Open the temporary state from a retained debug run with `jq` and verify it is understandable without provider-native files.

- [ ] **Step 7: Commit the core V1 foundation**

```bash
rtk git add .github README.md tests src Cargo.toml Cargo.lock rust-toolchain.toml .gitignore
rtk git commit -m "docs: document sesh provider switching"
```

## Core plan completion gate

Do not execute the fork plan until:

- `sesh run claude` and `sesh switch codex` pass the fake-provider acceptance test on macOS and Linux.
- A missing or failed hook cannot silently permit new unrecorded work.
- The exact worktree and saved cwd are preserved.
- Current Git facts and latest explicit narrative are visibly distinct.
- No Sesh file appears in the application repository.
- Real-provider setup uses documented extension points and no model quota in setup/smoke flows.
- The full release gate is warning-clean.
