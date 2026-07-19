# Sesh V1 Worktree Fork Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an explicit `sesh fork <provider>` command that creates a new Git worktree and child Sesh session containing the source worktree's exact staged, unstaged, and untracked state, while the default `sesh switch` path continues in place.

**Architecture:** Fork is a durable, forward-recoverable transaction outside the source repository. A typed operation journal records source fingerprints, immutable binary patches, the untracked manifest, target identities, and every phase. Git materialization is isolated from session-lineage commit; before the lineage commit point Sesh may roll back only unchanged artifacts it created, while after that point recovery finishes forward and never guesses.

**Tech Stack:** The core Rust CLI and storage model from `2026-07-16-sesh-v1-provider-switching.md`, real Git subprocesses with argument vectors, SHA-256 fingerprints, Serde JSON, Unix file metadata, Bash provider fixtures, `assert_cmd`, and `tempfile`.

---

## Prerequisites and scope

Execute this plan only after the core plan completion gate passes. Work in `/Users/thomasindrias/private/sesh-v1-foundation` on branch `feat/v1-foundation`. Read `docs/superpowers/specs/2026-07-16-sesh-v1-design.md` and the completed core plan before Task 1. Use `rtk` for every shell command.

This command is deliberately separate from switching:

```text
sesh switch codex          continue in the exact current session worktree
sesh fork codex            create and continue in a verified duplicate worktree
```

V1 fork copies staged, unstaged, and non-ignored untracked state. It excludes ignored files and refuses unmerged index entries, intent-to-add entries, staged gitlink changes, dirty submodules, unsupported untracked file types, and sparse checkout. It never stashes, resets, cleans, commits, or changes the source branch.

## Additional file structure

```text
src/model/fork.rs                 operation, artifact, and fingerprint schema
src/git/fingerprint.rs           stable semantic source/target fingerprints
src/git/fork.rs                  preflight, capture, materialize, and compare
src/fork.rs                      transaction, lineage commit, and rollback
tests/fork_cli.rs                grammar, naming, and preflight
tests/fork_state.rs              real-Git state duplication matrix
tests/fork_transaction.rs        failure injection and rollback boundary
tests/fork_north_star.rs         child-session provider continuation
```

Keep operation records under `$SESH_HOME/operations`, never in either worktree.

### Task 1: Define the fork CLI, IDs, target naming, and preflight

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/model/ids.rs`
- Modify: `src/model/mod.rs`
- Modify: `src/runtime.rs`
- Modify: `src/store/mod.rs`
- Create: `src/git/fork.rs`
- Modify: `src/git/mod.rs`
- Create: `tests/fork_cli.rs`

- [x] **Step 1: Write failing CLI and deterministic naming tests**

Create `tests/fork_cli.rs` with a help test and unit-facing naming cases. The public grammar is:

```text
sesh fork <PROVIDER> [--branch <BRANCH>] [--worktree <PATH>] [-- <PROVIDER_ARGS>...]
```

Assert:

```rust
cargo_bin_cmd!("sesh")
    .arg("--help")
    .assert()
    .success()
    .stdout(predicate::str::contains("fork"));
```

Add table-driven cases for `default_target`:

```rust
#[test]
fn default_target_is_stable_and_git_like() {
    let target = default_target(
        std::path::Path::new("/work/acme platform"),
        "12345678-1234-4234-8234-123456789abc",
    )
    .unwrap();

    assert_eq!(target.branch, "sesh/acme-platform-12345678");
    assert_eq!(target.worktree, std::path::Path::new("/work/acme platform-sesh-12345678"));
}

#[test]
fn repository_name_sanitization_never_creates_an_invalid_component() {
    let target = default_target(
        std::path::Path::new("/work/@@@"),
        "aaaaaaaa-1234-4234-8234-123456789abc",
    )
    .unwrap();

    assert_eq!(target.branch, "sesh/repo-aaaaaaaa");
    assert_eq!(target.worktree, std::path::Path::new("/work/@@@-sesh-aaaaaaaa"));
}
```

The target worktree basename preserves the repository basename because it is a filesystem path; only the branch component is sanitized.

- [x] **Step 2: Run the focused test and verify fork is absent**

Run: `rtk cargo test --test fork_cli`

Expected: FAIL because `fork`, `OperationId`, and target naming are not defined.

- [x] **Step 3: Add an operation ID to the injected runtime**

Extend the existing ID macro:

```rust
id_type!(SessionId);
id_type!(RunId);
id_type!(OperationId);
```

Extend `Runtime` and both system/test implementations:

```rust
fn operation_id(&self) -> OperationId;
```

`SystemRuntime` returns `OperationId::new()`. Fixed test runtimes return a parsed UUID so default names and golden JSON do not depend on randomness.

Add `StateLayout::operations() -> PathBuf`, returning `<root>/operations`, and include it in the private-directory validation performed by `StateLayout::ensure`.

- [x] **Step 4: Add fork grammar without changing switch semantics**

Add this `Command` variant:

```rust
Fork {
    provider: Provider,
    #[arg(long)]
    branch: Option<String>,
    #[arg(long)]
    worktree: Option<std::path::PathBuf>,
    #[arg(last = true, allow_hyphen_values = true)]
    provider_args: Vec<std::ffi::OsString>,
},
```

Do not add `--clone` to `run` or `switch`. `fork` is the one discoverable operation, analogous to a distinct Git subcommand.

- [x] **Step 5: Implement target naming and read-only preflight**

Create these types in `src/git/fork.rs`:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkRequest {
    pub provider: Provider,
    pub branch: Option<String>,
    pub worktree: Option<PathBuf>,
    pub provider_args: Vec<OsString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkTarget {
    pub branch: String,
    pub worktree: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkPreflight {
    pub source: GitSnapshot,
    pub target: ForkTarget,
    pub source_head: String,
}
```

`default_target(source_worktree, operation_id)` must:

1. Use the first eight lowercase hexadecimal UUID characters.
2. Replace each run of branch-invalid repository-name characters with one `-`.
3. Trim leading/trailing `-` and use `repo` if empty.
4. Return branch `sesh/<sanitized>-<short-id>`.
5. Return sibling worktree `<source-basename>-sesh-<short-id>`.

Resolve an explicit relative `--worktree` against the caller's cwd, then canonicalize its existing parent without following a target symlink. Require the target path itself not to exist. Validate the final branch by executing:

```text
git check-ref-format --branch <branch>
git show-ref --verify --quiet refs/heads/<branch>
```

The first command must succeed and the second must report absent. Use a Git command API that distinguishes expected exit code `1` from execution failure.

Preflight observes from `source_worktree.join(store.saved_cwd_relative())`, not from the caller's current directory, so the child preserves the session's latest recorded cwd. It must then refuse, before creating an operation directory, when any condition is true:

```text
active or foreign/stale-unrecovered Sesh lease
core.sparseCheckout or core.sparseCheckoutCone is true
git ls-files --unmerged -z is non-empty
git status --porcelain=v2 -z --untracked-files=no contains an intent-to-add `.A` XY record
git diff --cached --raw -z contains mode 160000
GitSnapshot.dirty_submodules is non-empty
an untracked path is not a regular file or symlink
source or target metadata contains a non-UTF-8 path
target is inside the source worktree
target is nested inside any other registered worktree
target branch or path already exists
```

`git ls-files --others --exclude-standard -z` remains the only inclusion source for copied untracked files. Git does not enumerate FIFOs, sockets, or devices there, so fork preflight must also perform a read-only metadata walk rooted at the source worktree. Never follow symlinks; prune the root `.git` administration entry and recorded gitlink directories. Batch candidate directories and special nodes through `git check-ignore -z --stdin`: prune ignored directories, ignore an ignored special node, and refuse every unignored special node. This fork-only walk discovers unsupported state without adding ignored content to the copy manifest.

- [x] **Step 6: Add preflight refusal tests**

Using real temporary Git repositories, add one named test for each refusal above. For FIFO coverage on Unix:

```rust
let status = std::process::Command::new("mkfifo")
    .arg(worktree.join("agent.pipe"))
    .status()
    .unwrap();
assert!(status.success());
```

After every refusal, assert the target path and target branch are absent, `$SESH_HOME/operations` contains no operation record, and no child worktree ref exists. The dirty-submodule test may use a local file-protocol submodule fixture; it must not contact a network.

- [x] **Step 7: Verify and commit preflight**

Run:

```bash
rtk cargo test --test fork_cli
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

Commit:

```bash
rtk git add src tests/fork_cli.rs
rtk git commit -m "feat: add safe worktree fork preflight"
```

### Task 2: Capture immutable fork artifacts behind a durable operation journal

**Files:**
- Create: `src/model/fork.rs`
- Modify: `src/model/mod.rs`
- Create: `src/git/fingerprint.rs`
- Modify: `src/git/mod.rs`
- Create: `src/fork.rs`
- Modify: `src/lib.rs`
- Create: `tests/fork_transaction.rs`

- [x] **Step 1: Write failing operation-schema and mutation-detection tests**

Add serialization tests proving an operation record round-trips and rejects an unknown schema version. Add a capture test with a test-only boundary callback that changes a staged-and-unstaged file after artifacts are written but before the second fingerprint. Assert capture returns `source changed during fork capture`, no target exists, and the operation phase is `rolled_back` with `target_created: false`; retained operation artifacts are diagnostic data outside the repository.

The operation phases are intentionally finite:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPhase {
    Prepared,
    ArtifactsCaptured,
    WorktreeCreated,
    StagedApplied,
    UnstagedApplied,
    UntrackedCopied,
    Verified,
    ChildStaged,
    LineageCommitted,
    ChildBound,
    RunLeased,
    Complete,
    RolledBack,
    NeedsManualRecovery,
}
```

- [x] **Step 2: Run tests and verify operation types are absent**

Run: `rtk cargo test --test fork_transaction operation`

Expected: FAIL with unresolved operation and fingerprint types.

- [x] **Step 3: Define the inspectable operation model**

Create `src/model/fork.rs`:

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForkFingerprint {
    pub head: String,
    pub branch: Option<String>,
    pub index_entries_sha256: String,
    pub staged_patch_sha256: String,
    pub unstaged_patch_sha256: String,
    pub untracked_manifest_sha256: String,
    pub submodule_manifest_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UntrackedKind {
    Regular,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UntrackedEntry {
    pub path: PathBuf,
    pub kind: UntrackedKind,
    pub sha256: String,
    pub bytes: u64,
    pub executable: bool,
    pub symlink_target: Option<PathBuf>,
    pub artifact: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForkOperation {
    pub schema_version: u32,
    pub id: OperationId,
    pub phase: ForkPhase,
    pub source_session_id: SessionId,
    pub source_worktree: WorktreeIdentity,
    pub source_checkpoint_sequence: Option<u64>,
    pub source_fingerprint: Option<ForkFingerprint>,
    pub target_branch: String,
    pub target_worktree: PathBuf,
    pub target_head: String,
    pub child_session_id: Option<SessionId>,
    pub target_fingerprint: Option<ForkFingerprint>,
    pub target_cleanup_inventory_sha256: Option<String>,
    pub branch_created: bool,
    pub target_created: bool,
    pub error: Option<String>,
    pub updated_at: String,
}
```

Paths in JSON are repository-relative where possible and validated as UTF-8 by core V1. The untracked manifest is a separate sorted `Vec<UntrackedEntry>` at `operations/<id>/untracked/manifest.json`.

- [x] **Step 4: Implement atomic operation transitions**

Create `ForkOperationStore` in `src/fork.rs` with:

```rust
pub fn create(layout: &StateLayout, operation: &ForkOperation) -> Result<Self>;
pub fn read(layout: &StateLayout, id: OperationId) -> Result<ForkOperation>;
pub fn transition(
    &self,
    expected: ForkPhase,
    next: ForkPhase,
    update: impl FnOnce(&mut ForkOperation),
) -> Result<ForkOperation>;
```

`create` uses immutable private create for `operation.json`; `transition` reads securely, requires the exact expected phase, updates `updated_at`, and atomically replaces the record. Validate every decoded record:

```text
schema_version == 1
operation directory basename == operation ID
source/target paths are absolute
phase flags are monotonic and compatible
child_session_id is present at and after child_staged
source_checkpoint_sequence is present at and after child_staged
source_fingerprint is absent only in prepared and present at and after artifacts_captured
target_fingerprint is present whenever target_created is true
target_cleanup_inventory_sha256 is present whenever target_created is true
```

Sync the operation directory after every transition. Never infer a phase from which files happen to exist.

- [x] **Step 5: Implement semantic fingerprints and immutable capture**

In `src/git/fingerprint.rs`, use these exact argument-vector commands:

```text
git rev-parse HEAD
git symbolic-ref --quiet --short HEAD
git ls-files --stage -z
git diff --cached --binary --full-index --no-ext-diff --no-textconv --no-renames
git diff --binary --full-index --no-ext-diff --no-textconv --no-renames
git ls-files --others --exclude-standard -z
```

The expected detached-HEAD exit from `symbolic-ref` maps to `None`; all other unexpected failures are fatal. Hash raw command bytes, not rendered text. Build each untracked entry with `symlink_metadata`, hash regular-file bytes or raw symlink-target bytes, and sort by path before canonical JSON serialization and hashing.

Capture writes:

```text
operations/<id>/staged.patch
operations/<id>/unstaged.patch
operations/<id>/untracked/manifest.json
operations/<id>/untracked/blobs/sha256/<first-two>/<remaining>
operations/<id>/submodules.json
```

Every artifact uses immutable private create and is fsynced. Regular untracked content is stored once by SHA-256; symlink targets live only in the manifest and are never followed. Build `submodules.json` without shell evaluation: enumerate mode-`160000` entries from each initialized repository index, inspect each path with `symlink_metadata`, and recurse only into a submodule whose own Git identity and checked-out HEAD prove it is initialized and clean. Record the repository-relative path, expected gitlink object ID, initialized flag, and parent path; never record absolute Git-directory paths. After writing artifacts, fingerprint the source again from fresh commands and file reads. Only an exact semantic match advances `Prepared -> ArtifactsCaptured` and stores `source_fingerprint`.

Do not hash the raw `.git/index` file: Git may legitimately refresh stat-cache bytes without changing staged content. `git ls-files --stage -z` is the stable index semantic fingerprint.

- [x] **Step 6: Verify capture determinism and source invariance**

Add tests that capture twice from equivalent repositories at different absolute paths and assert patch hashes plus relative manifests match. Include binary bytes, an executable, a symlink, a nested untracked file, staged-and-unstaged edits to the same path, a deletion, and a rename. Fingerprint the source working files and `git ls-files --stage -z` before and after capture and assert equality.

Run:

```bash
rtk cargo test --test fork_transaction capture
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [x] **Step 7: Commit operation capture**

```bash
rtk git add src tests/fork_transaction.rs
rtk git commit -m "feat: capture durable worktree fork artifacts"
```

### Task 3: Materialize and prove an exact target worktree

**Files:**
- Modify: `src/git/command.rs`
- Modify: `src/git/fork.rs`
- Modify: `src/git/fingerprint.rs`
- Create: `tests/fork_state.rs`

- [x] **Step 1: Write the failing real-Git duplication matrix**

Create `tests/fork_state.rs`. In one source linked worktree, construct all of these simultaneously:

```text
tracked text modified only in the index
tracked text modified only in the working tree
the same path with one staged version and a second unstaged version
staged deletion
unstaged deletion
staged rename represented with --no-renames
binary staged and binary unstaged content containing NUL bytes
regular file changed from 0644 to 0755
tracked symlink target change
nested untracked regular file with spaces
untracked executable
untracked symlink
ignored file that must not copy
```

Call the low-level `materialize` API with captured artifacts. Assert:

```rust
let source = Git::new().snapshot(&source_cwd).unwrap();
let target = Git::new().snapshot(&target_cwd).unwrap();

assert_eq!(target.head, source.head);
assert_eq!(target.staged, source.staged);
assert_eq!(target.unstaged, source.unstaged);
assert_eq!(target.untracked, source.untracked);
assert_eq!(std::fs::read(target_worktree.join("same.txt")).unwrap(), b"unstaged version\n");
assert!(!target_worktree.join("ignored.secret").exists());
```

Compare snapshots after replacing identity, branch, and cwd-relative fields with their expected target values; do not accidentally compare source worktree identity to target identity.

- [x] **Step 2: Run the state test and verify materialization is absent**

Run: `rtk cargo test --test fork_state -- --nocapture`

Expected: FAIL because target creation and patch application are not implemented.

- [x] **Step 3: Add a Git runner that preserves expected non-zero statuses**

Extend `GitCommand` with:

```rust
#[derive(Clone, Debug)]
pub struct GitOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn output_status<I, S>(&self, cwd: &Path, args: I) -> Result<GitOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>;
```

`output` delegates to `output_status` and requires success. Do not invoke a shell, interpolate a command string, or decode path-bearing stdout as UTF-8.

- [x] **Step 4: Create the worktree and apply tracked state in two layers**

Implement `materialize` with the source common Git directory and the recorded exact source HEAD. Run, in this order:

```text
git -C <source-worktree> worktree add -b <target-branch> <target-path> <source-head>
git -C <target-worktree> apply --index --binary --whitespace=nowarn -- <staged.patch>
git -C <target-worktree> apply --binary --whitespace=nowarn -- <unstaged.patch>
```

Pass patch paths as separate `OsString` arguments. Skip `git apply` only when the immutable patch file length is zero. After each successful command, transition the operation record and save both the target semantic fingerprint and a full cleanup inventory hash appropriate to that phase:

```text
ArtifactsCaptured -> WorktreeCreated
WorktreeCreated   -> StagedApplied
StagedApplied     -> UnstagedApplied
```

Immediately after `worktree add`, resolve the target through `Git::snapshot` and require:

```text
target HEAD == recorded source HEAD
target branch == requested branch
target common_git_dir == source common_git_dir
target canonical worktree == canonical requested target
```

Set `branch_created` and `target_created` in the same durable phase update. Before each mutation, derive the allowed semantic state and complete path set from the last durable inventory plus the immutable patch or manifest entry. Immediately after the mutation, reject any unexpected path, require the observed Git state to equal that allowed state, and capture the exact full inventory as an in-memory `MutationProof` before attempting the phase-record write. A command success without a durable record update is uncertain durable state: the still-running process may roll back only if a fresh observation exactly matches that boundary-captured proof; crash recovery never relies on ephemeral proof and never deletes the target.

The cleanup inventory recursively hashes every target path, type, content or symlink target, and executable bit, including ignored files and empty directories. Exclude only the root worktree `.git` administration entry. Never follow symlinks. This inventory is stricter than the product duplication manifest because rollback must detect an ignored or otherwise unexpected file created after Sesh made the target.

- [x] **Step 5: Restore untracked files without following links**

For every sorted manifest entry:

1. Join the relative path to the canonical target root and reject any component that escapes it.
2. Create parent directories one component at a time; reject an existing symlink or non-directory component.
3. For a regular entry, securely open the operation blob with `O_NOFOLLOW`, verify its length and SHA-256 again, then create the destination with `create_new(true)` and mode `0755` or `0644` independent of umask.
4. For a symlink entry, verify the target bytes hash and call Unix `symlink` with the recorded target. Never resolve or read through the link.
5. Sync each regular file and every newly created directory.

After the full manifest succeeds, transition `UnstagedApplied -> UntrackedCopied`.

Do not copy empty untracked directories: Git does not represent them and the approved manifest is path-based.

- [x] **Step 6: Handle clean submodules without network access**

Dirty submodules and staged gitlink changes already fail preflight. Recreate only the exact initialized topology recorded in `submodules.json`, in parent-before-child order. For each initialized path, invoke the update from its recorded parent repository and pass that one relative path after `--`; never use an unscoped recursive update:

```text
git -C <target-parent-repository> -c protocol.allow=never submodule update --init --no-fetch -- <relative-submodule-path>
```

Set `GIT_TERMINAL_PROMPT=0`. Require the resulting submodule HEAD to equal its recorded gitlink object before processing children. If Git would need any protocol, object fetch, credentials, or new clone, fail before verification and enter safe rollback. Paths recorded as uninitialized are never passed to `submodule update`, so they remain uninitialized in the target. Add local-only tests for initialized, uninitialized, and mixed nested clean submodules; no test may have network access.

- [x] **Step 7: Re-observe both sides and compare semantic state**

After copying, capture a fresh source fingerprint and require it still equals the operation's source fingerprint. Then observe the target and compare:

```text
same HEAD object
target branch is requested branch
same staged path/content/type/executable facts
same unstaged path/content/type/executable facts
same untracked path/content/type/executable/symlink-target facts
no dirty submodule
saved cwd relative path exists as a directory in the target
```

Store the target fingerprint and transition `UntrackedCopied -> Verified` only after every comparison passes. Identity fields differ by design and are compared separately.

Add a negative test that mutates the source from a boundary callback after target creation; verification must fail rather than produce a mixed-state fork.

- [x] **Step 8: Prove the source worktree was not rewritten**

Before materialization, record:

```text
source file/symlink hashes and modes
git ls-files --stage -z bytes
source branch and HEAD
staged.patch and unstaged.patch hashes
```

After success, assert every value is unchanged. Do not compare volatile Git administration files or stat-cache bytes; the invariant is the source working files, semantic index, branch, and HEAD.

- [x] **Step 9: Verify and commit exact duplication**

Run:

```bash
rtk cargo test --test fork_state -- --nocapture
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS on macOS and Linux.

Commit:

```bash
rtk git add src tests/fork_state.rs
rtk git commit -m "feat: duplicate git worktree state exactly"
```

### Task 4: Commit child-session lineage and launch the requested provider

**Files:**
- Modify: `src/model/event.rs`
- Modify: `src/model/session.rs`
- Modify: `src/store/session.rs`
- Modify: `src/handoff.rs`
- Modify: `src/fork.rs`
- Modify: `src/app.rs`
- Create: `tests/fork_north_star.rs`

- [x] **Step 1: Write the failing fork-to-provider acceptance test**

Create `tests/fork_north_star.rs`:

1. Create a repository plus linked source worktree on `feat/oauth`, with saved cwd `apps/web`.
2. Run fake Claude to create prompts, a narrative checkpoint, staged state, unstaged state, an untracked file, a passing command, and one failing command.
3. Exit Claude and fingerprint the source.
4. Run `sesh fork codex` from the source worktree root.
5. Assert fake Codex starts in `<target>/apps/web`.
6. Assert its SessionStart context contains parent objective, decision, next step, failure, child-session identity, target branch, and all dirty paths.
7. Assert the source fingerprint is unchanged.
8. Assert source and target worktree refs point to distinct sessions with correct parent lineage.

The fake providers use the same hook fixtures and `--version` handling as `tests/north_star.rs`; no real model is called.

- [x] **Step 2: Run the acceptance test and verify orchestration is absent**

Run: `rtk cargo test --test fork_north_star -- --nocapture`

Expected: FAIL because app dispatch and child-session lineage are not implemented.

- [x] **Step 3: Add explicit parent/child event facts**

Extend `EventKind`:

```rust
#[serde(rename = "session.forked")]
SessionForked {
    operation_id: OperationId,
    child_session_id: SessionId,
    parent_checkpoint_sequence: u64,
    target_worktree: PathBuf,
    target_branch: String,
},
```

Keep the child's `SessionMeta.parent_session_id` and `parent_checkpoint_sequence` fields. Do not copy parent events into the child journal or create child refs that point to parent checkpoint sequences; cross-session lineage is explicit metadata, not fake local history.

- [x] **Step 4: Split child staging from worktree-ref activation**

Add these `SessionStore` operations:

```rust
pub fn stage_child(
    layout: &StateLayout,
    runtime: &dyn Runtime,
    snapshot: GitSnapshot,
    parent_session_id: SessionId,
    parent_checkpoint_sequence: u64,
    child_session_id: SessionId,
) -> Result<SessionStore>;

pub fn bind_worktree(&self) -> Result<()>;
```

`stage_child` immutably creates the child directory and `meta.json`, then appends child `session.created` and `git.snapshot` events. It does not write the global worktree ref. `bind_worktree` uses immutable create and requires the ref to be absent or already byte-identical to this child.

This split creates one explicit lineage commit point:

```text
before parent session.forked is durable -> rollback may be attempted
after parent session.forked is durable  -> recover forward; never delete target
```

- [x] **Step 5: Implement the full fork transaction**

Add `fork_command` to app dispatch. While holding the parent `SessionOperationLock`:

1. Resolve the source session and recover/refuse leases using the core rules.
2. Run preflight and allocate deterministic target defaults from `OperationId`.
3. Create the operation journal in phase `Prepared`.
4. Capture and revalidate artifacts; advance to `ArtifactsCaptured`.
5. Materialize and verify the target; advance through `Verified`.
6. Revalidate the source fingerprint once more.
7. Create a parent transition checkpoint linked to the latest valid narrative.
8. Allocate the child session ID, call `stage_child` with target snapshot and lineage, acquire the child `SessionOperationLock`, record that ID, and transition `Verified -> ChildStaged`.
9. Append parent `session.forked`; transition `ChildStaged -> LineageCommitted` in the operation record.
10. Call child `bind_worktree`; transition `LineageCommitted -> ChildBound`.
11. Render a lineage-aware child handoff from committed parent facts through the transition checkpoint plus current child Git facts.
12. Build the child run inbox atomically, append child `run.started`, and create the child run lease.
13. Transition `ChildBound -> RunLeased -> Complete`.
14. Release the child and parent operation locks before launching the provider.
15. Supervise, promote checkpoints, record exit/Git facts, and clear the child lease through the same core run-finalization path.

The target provider launch is part of the child session. A provider startup failure does not roll back an already committed fork; the child remains inspectable and can later be resumed with `sesh switch` from its worktree.

Fork and recovery always acquire parent operation lock before child operation lock; no code path may take them in the reverse order. Holding the child lock before global worktree binding prevents a racing `switch` from attaching between child activation and lease creation.

- [x] **Step 6: Render lineage without pretending parent events are child events**

Extend `HandoffInput` with:

```rust
pub struct ParentLineage {
    pub session_id: SessionId,
    pub transition_sequence: u64,
    pub narrative_sequence: Option<u64>,
}

pub parent_lineage: Option<ParentLineage>,
```

The handoff heading says `Forked from session <id> at parent checkpoint <sequence>`. Parent post-narrative events retain labels such as `parent event 42`; child events use `child event 1`. Omitted ranges are scoped by session. Current repository/worktree/branch/cwd and dirty facts always come from the child snapshot.

- [x] **Step 7: Verify lineage, worktree bindings, and launch**

Run:

```bash
rtk cargo test --test fork_north_star -- --nocapture
rtk cargo test --test north_star -- --nocapture
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. The original switch test proves the default still uses the existing worktree; the fork test proves duplication is opt-in.

- [x] **Step 8: Commit child-session fork orchestration**

```bash
rtk git add src tests/fork_north_star.rs
rtk git commit -m "feat: continue forked work in a child session"
```

### Task 5: Make failures recoverable without deleting uncertain work

**Files:**
- Modify: `src/fork.rs`
- Modify: `src/doctor.rs`
- Modify: `src/app.rs`
- Modify: `tests/fork_transaction.rs`
- Modify: `tests/doctor.rs`

- [ ] **Step 1: Write failing phase-injection tests**

Define a test-only `ForkBoundary` implementation that returns an injected error after a selected durable phase. Run one table case after each pre-commit phase:

```text
prepared
artifacts_captured
worktree_created
staged_applied
unstaged_applied
untracked_copied
verified
child_staged
```

For each case, assert:

```text
source semantic fingerprint is unchanged
no provider was launched
no source worktree ref changed
rollback removes only a target/branch still matching Sesh's recorded fingerprint
operation ends rolled_back when cleanup is proven
operation ends needs_manual_recovery when cleanup cannot be proven
```

Add a case whose boundary modifies the target immediately before rollback. Assert Sesh leaves the target, branch, and files intact and reports `needs_manual_recovery`.

Inject a synchronous operation-record write failure immediately after each Git mutation and midway through untracked restoration. Assert rollback occurs only when the fresh target inventory matches the boundary-captured in-memory `MutationProof`. Simulate process death in the same windows and assert `doctor --repair` reports the target without deleting it because ephemeral proof does not survive a crash.

Add post-commit cases after `lineage_committed`, `child_bound`, and `run_leased`. They must recover forward and must never call target or branch removal.

- [ ] **Step 2: Run transaction tests and verify cleanup rules are missing**

Run: `rtk cargo test --test fork_transaction rollback -- --nocapture`

Expected: FAIL because rollback and forward recovery are not implemented.

- [ ] **Step 3: Implement fingerprint-gated synchronous rollback**

On an error before a durable parent `session.forked` event, inspect the operation record and use this order:

1. If a staged child directory exists, require no global worktree ref and no parent `session.forked` event for that child. Verify its complete inventory contains only the expected meta, journal, lock, refs, checkpoint, blob, and run paths created by this operation; then rename it to a private `.deleting-<session>` path and remove it. Any extra path makes recovery manual.
2. If the target exists, re-observe it. Require common Git directory, target branch, HEAD, staged/unstaged/untracked facts, canonical path, and the full cleanup inventory (including ignored files and empty directories) to equal the fingerprints recorded at the last successful phase. In the same live process only, a failed phase-record write may instead use the boundary-captured `MutationProof` for the just-completed mutation; never manufacture that proof later during rollback.
3. Require `git worktree list --porcelain -z` to associate that exact target with the expected branch and HEAD.
4. Only then run `git -C <source> worktree remove --force <target>`.
5. Re-read `refs/heads/<branch>` and delete it with `git branch -D <branch>` only when it still points to the recorded source HEAD and no worktree uses it.
6. Transition the operation to `RolledBack` and retain its record/artifact hashes for diagnosis.

If any proof fails, perform no destructive Git command, transition to `NeedsManualRecovery`, and return an error naming the uncertain artifact.

`--force` is safe only after the exact target fingerprint comparison; do not weaken that prerequisite because `git worktree remove` otherwise destroys untracked files.

- [ ] **Step 4: Detect the lineage commit point from canonical evidence**

Do not trust only `ForkPhase`: a crash can happen after the parent event fsync and before the operation record update. Treat the transaction as committed when the verified parent journal contains `session.forked` with the same operation ID and child session ID.

After that evidence exists:

```text
never remove the target worktree
never delete the target branch
never remove the staged child session
finish an absent byte-identical worktree ref
finish an absent child run inbox/lease only when no live lease exists
leave provider launch to the caller after durable state is complete
```

A conflicting worktree ref or changed target becomes `NeedsManualRecovery`; it is never overwritten.

- [ ] **Step 5: Add operation diagnostics to doctor**

Plain `sesh doctor` scans `$SESH_HOME/operations/*/operation.json` securely and emits:

```text
fork_in_progress             nonterminal phase, process may still be active
fork_precommit_crash         no parent commit evidence; target may be cleanup candidate
fork_postcommit_incomplete   parent event exists; forward repair available
fork_target_changed          target no longer matches recorded fingerprint
fork_record_corrupt          schema/path/phase invariant failed
```

Diagnostics include operation ID, phase, source session, target path, target branch, and a shell-escaped inspection or cleanup command. Human text must quote arguments with `shell_words::quote`; JSON output stores an argument vector as well as display text.

`sesh doctor --repair` may complete a post-commit byte-identical child binding and repair its operation phase. After a crash it never removes a target worktree or branch, including a pristine pre-commit target; it reports the exact command for the developer to inspect and run manually. This matches the approved crash boundary.

- [ ] **Step 6: Prove doctor is non-mutating by default**

For every interrupted phase, snapshot state files, operation files, Git refs, and worktree paths before and after plain `sesh doctor --json`. Assert bytes and mtimes are unchanged. Then run `doctor --repair` for a post-commit missing-binding case and assert it performs only the forward binding and phase update.

- [ ] **Step 7: Verify and commit recovery**

Run:

```bash
rtk cargo test --test fork_transaction -- --nocapture
rtk cargo test --test doctor
rtk cargo test --all-targets
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

Commit:

```bash
rtk git add src tests
rtk git commit -m "feat: recover interrupted worktree forks safely"
```

### Task 6: Document and freeze the full V1 worktree contract

**Files:**
- Modify: `README.md`
- Modify: `tests/cli_contract.rs`
- Modify: `tests/delete_session.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `docs/superpowers/specs/2026-07-16-sesh-v1-design.md`

- [ ] **Step 1: Extend CLI and README contracts**

Update the CLI help test so the exact public V1 command set includes `fork` once. Assert neither `switch --help` nor `run --help` advertises a clone flag.

Document:

```text
sesh switch <provider> continues in the existing worktree and saved cwd
sesh fork <provider> creates a separate branch, worktree, and child session
--branch and --worktree overrides
default branch/worktree naming
ignored-file exclusion
refusals for sparse checkout, conflicts, dirty submodules, and special files
plaintext operation artifacts under SESH_HOME
pre-commit rollback and post-commit forward-recovery boundary
doctor behavior after an interrupted fork
```

Include one exact example:

```bash
sesh run claude
sesh switch codex
sesh fork claude --branch sesh/oauth-experiment --worktree ../oauth-experiment
```

State explicitly that no source commit, stash, reset, clean, or hidden source-worktree rewrite occurs.

- [ ] **Step 2: Extend complete deletion across fork-owned copies**

Operation patches and untracked blobs can contain the same secrets as the source session, and a child run inbox contains an inherited parent handoff. Add deletion tests and rules:

1. Refuse deleting a parent session while any child session metadata names it; list the child IDs and require deleting children first.
2. Refuse deletion while a nonterminal fork operation names the session.
3. When deleting a child, remove its entire session directory, including inherited run handoffs, through the core safe deletion transaction.
4. When deleting a parent with no remaining children, include every terminal operation directory whose `source_session_id` matches in the rename-then-delete transaction. Rename the session and all matching operation directories to unique `.deleting-*` siblings before removing the worktree ref; if any rename or ref removal fails, restore every renamed directory before returning.
5. Leave application worktrees and Git branches untouched.

This preserves the meaning of complete Sesh-session deletion without selective journal rewriting or a new cascade feature.

- [ ] **Step 3: Reconcile the design spec with implemented details**

Update only proven implementation details: operation phase names, the child-staging/parent-event commit point, clean-submodule local-only behavior, and the explicit UTF-8 V1 path boundary. Do not add V2/V3 features.

- [ ] **Step 4: Run the complete cross-platform release gate**

Run:

```bash
rtk cargo fmt --check
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo test --all-targets --all-features
rtk cargo doc --no-deps
rtk cargo test --test north_star -- --nocapture
rtk cargo test --test fork_north_star -- --nocapture
rtk cargo test --test fork_state -- --nocapture
rtk cargo test --test fork_transaction -- --nocapture
rtk git status --short
```

Expected: all commands PASS on macOS and Linux. The CI matrix already runs both operating systems; do not add provider-authenticated jobs.

- [ ] **Step 5: Audit final evidence manually**

Retain one debug test directory and verify with ordinary tools:

```bash
rtk jq . "$SESH_HOME/operations/<operation-id>/operation.json"
rtk jq . "$SESH_HOME/operations/<operation-id>/untracked/manifest.json"
rtk git -C <source> status --short
rtk git -C <target> status --short
rtk git worktree list --porcelain
```

Confirm source and target status facts match, identities differ, source bytes are unchanged, refs point to distinct sessions, and no Sesh state exists in either application worktree.

- [ ] **Step 6: Commit the full V1 fork contract**

```bash
rtk git add README.md .github docs tests
rtk git commit -m "docs: document verified worktree forks"
```

## Fork plan completion gate

V1 is complete only when:

- `switch` reliably stays in the original worktree and saved cwd.
- `fork` duplicates staged, unstaged, untracked, binary, executable, deletion, rename, and symlink state in a new worktree.
- Ignored files, dirty submodules, sparse checkout, conflicts, intent-to-add, staged gitlinks, and special untracked files fail before activation.
- Source working files, semantic index, branch, and HEAD remain unchanged.
- Target state is verified before child-session binding.
- Parent and child session lineage is inspectable without copying parent history into the child journal.
- Synchronous rollback deletes only fingerprint-proven Sesh-created artifacts before the parent commit point.
- Crash recovery never deletes an uncertain target and finishes forward after the parent commit point.
- Fake-provider North Star tests pass on macOS and Linux without model calls.
