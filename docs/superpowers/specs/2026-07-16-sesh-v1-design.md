# Sesh V1 Design

**Status:** Approved for implementation planning  
**Date:** 2026-07-16

## North Star

Switch AI coding providers without losing the active work session.

The repository, worktree, working directory, objective, observed activity,
decisions, progress, failures, and next steps belong to the Sesh session. A
provider is a replaceable client attached to that session.

The defining V1 workflow is:

```text
sesh run claude
        |
        | provider becomes unavailable
        v
sesh switch codex
        |
        v
Codex continues in the same worktree and cwd with a deterministic handoff
```

The switch must not require the developer to restate the task, copy a prompt,
reconstruct Git state, or explain the latest failure.

## Scope

V1 is a local-only Rust CLI for Unix-like systems, initially macOS and Linux.
It requires Git and supports Claude Code and Codex through provider adapters.

V1 includes:

- A provider-neutral session model
- Local, inspectable storage outside application repositories
- Append-only normalized events
- Explicit narrative checkpoints and automatic transition checkpoints
- Git repository, worktree, branch, cwd, and dirty-state observation
- Provider lifecycle hooks
- Reliable provider switching
- Explicit session forking into a duplicated Git worktree
- Diagnostics, inspection, and complete local-session deletion

V1 does not include:

- Cloud or multi-machine synchronization
- Remote MCP support
- Team collaboration or session sharing
- Embeddings, vector storage, RAG, or semantic search
- Autonomous task orchestration
- Native provider transcript parsing
- AI-generated summaries owned by Sesh
- Windows support
- Automatic secret detection, redaction, or encryption
- Worktree creation during the normal `run` or `switch` path

The storage format must remain suitable for a later private-Git sync transport,
but remote synchronization is not part of V1.

## Product Principles

### The session is canonical

Provider-native session identifiers may be recorded for diagnostics, but Sesh
does not depend on resuming a provider-native conversation. Every attachment
starts from the canonical Sesh handoff. This prevents a provider transcript
from becoming a hidden second source of truth.

### Facts and narrative are separate

Sesh observes facts: commands, hook outcomes, process lifecycle, Git state,
paths, hashes, exit statuses, and timestamps. Objective, rationale, decisions,
assumptions, blockers, and next steps are narrative. Narrative enters the
session only through an explicit human- or provider-written checkpoint.

Sesh never silently turns activity into a model-generated summary. If narrative
is absent, the handoff says that it is absent and presents only observed facts.

### Source and session data are separate

Source code remains in Git and the active worktree. Sesh records dirty paths and
content hashes, not routine copies of source files. Session data never enters
the application repository. The only operation that duplicates source is the
explicit `sesh fork` workflow, which creates another Git worktree.

### Git-like, not Git-dependent storage

Sesh borrows Git's useful properties: local ownership, plain inspectable data,
append-only history, explicit snapshots, stable identifiers, hashes, and refs.
It does not make a Git commit for each event or require a second Git repository
at runtime. This avoids high-frequency commit noise and makes complete session
deletion tractable.

## Command Model

The public V1 commands are:

```text
sesh run <claude|codex> [-- <provider flags>]
sesh switch <claude|codex> [-- <provider flags>]
sesh fork <claude|codex> [--branch <name>] [--path <path>] [-- <provider flags>]
sesh checkpoint [--format json] [--from-provider]
sesh status [--json]
sesh log [--from <sequence>] [--json]
sesh inspect [--json]
sesh delete [--yes]
sesh setup <claude|codex>
sesh doctor [--json]
```

`sesh run` creates a session bound to the current Git worktree, then launches
the requested provider in the current cwd. Exactly one live Sesh session may be
bound to a worktree. If one is already bound, `run` fails with the session ID
and directs the developer to `switch`, `fork`, or `delete`.

`sesh switch` requires an existing session for the current worktree. It attaches
a new provider run to that session and restores the session's saved cwd, even
when `switch` is invoked from another directory inside the worktree. The target
provider may equal the previous provider; V1 still creates a fresh provider
conversation from the Sesh handoff.

`sesh fork` creates a child session and a new Git worktree that duplicates the
source session's staged, unstaged, and untracked state. `fork` is used instead
of `clone` because Git users reasonably understand clone to mean copying an
entire repository.

Provider arguments after `--` are passed as an argument vector and never
through a shell. On `switch` and `fork`, the adapter owns the initial bootstrap
prompt, so passthrough arguments may contain provider flags but not a second
positional prompt.

`sesh checkpoint` opens `$VISUAL`, then `$EDITOR`, for a human checkpoint when
stdin is a terminal. It accepts validated JSON on stdin with `--format json`.
`--from-provider` is an internal-safe path that writes only to the active run's
checkpoint inbox; it cannot modify canonical history directly.

`status`, `log`, and `inspect` are read-only. `delete` removes a complete local
session after confirmation. It does not promise forensic erasure from SSDs,
backups, filesystem snapshots, or previously exported copies.

`setup` materializes the versioned integration assets owned by Sesh, probes the
provider without spending model quota, and guides any provider-native hook trust
step. It never grants trust programmatically, changes an application repository,
or weakens managed provider policy. `doctor` verifies setup without mutating it.

## Architecture

Sesh is one Rust binary with focused modules and no daemon requirement.

### Session core

Owns session and run identifiers, normalized event types, checkpoint schemas,
refs, version compatibility, and deterministic handoff rendering. It knows
nothing about Claude or Codex payload shapes.

### Storage

Owns paths, permissions, locks, journal appends, checksums, atomic refs, blobs,
leases, operation journals, and crash recovery. All mutating storage operations
go through this boundary.

### Git observer

Owns repository discovery, worktree identity, cwd validation, status parsing,
dirty-file hashing, snapshot comparison, and worktree forking. Git commands are
executed without a shell and use machine-readable, NUL-delimited output where
available.

### Provider adapters

Each adapter has four responsibilities:

1. Probe the executable and required capabilities.
2. Prepare a launch specification without replacing user configuration.
3. Normalize documented hook inputs into provider-neutral events.
4. Inject the current Sesh protocol and handoff at session start.

Adding another provider must not require storage, checkpoint, Git, or handoff
schema changes.

### Process supervisor

Launches the provider with inherited stdin, stdout, stderr, terminal, and cwd.
It does not allocate a replacement PTY or scrape screen output. It owns the run
lease, signal forwarding, child exit recording, the provider checkpoint inbox,
and post-exit Git observation.

## Local Storage Layout

`$SESH_HOME` overrides the state root. Otherwise Sesh uses
`$XDG_STATE_HOME/sesh`, falling back to `~/.local/state/sesh`.

```text
$SESH_HOME/
├── FORMAT
├── integrations/
│   ├── claude/<adapter-version>/
│   └── codex/<adapter-version>/
├── refs/
│   └── worktrees/<sha256>.json
├── operations/
│   └── <operation-id>.json
└── sessions/
    └── <session-id>/
        ├── meta.json
        ├── events.jsonl
        ├── lock
        ├── operation.lock
        ├── refs/
        │   ├── active-run.json
        │   ├── latest-checkpoint
        │   └── latest-narrative-checkpoint
        ├── checkpoints/
        │   ├── 000000000123.json
        │   └── 000000000123.md
        ├── blobs/
        │   └── sha256/<first-two>/<remaining-hash>
        └── runs/
            └── <run-id>/
                └── inbox/
                    ├── handoff.md
                    ├── recent-events.jsonl
                    └── checkpoints/
```

State directories are created with mode `0700` and regular files with mode
`0600`, independent of a permissive process umask. Sesh rejects unexpected
ownership, symlink traversal, and insecure permissions on canonical state.
Run-inbox files are derived or untrusted input; canonical storage never trusts
them without parsing and validation.

Canonical V1 JSON requires repository, worktree, cwd, dirty, and symlink-target
paths to be valid UTF-8. Sesh refuses an unsupported path before session or fork
activation and never records a lossy replacement string.

Blobs are scoped to a session rather than globally deduplicated. This makes
complete session deletion understandable: deleting one session removes every
blob owned by that session without a cross-session reference count.

The worktree ref key is a SHA-256 hash of the canonical Git common directory and
worktree-specific Git directory. The ref value repeats both identities and the
canonical path so hash collisions or stale moved paths can be detected rather
than assumed safe.

## Journal and Event Schema

`events.jsonl` is the canonical append-only history. Every line is an envelope:

```json
{
  "checksum": "sha256:<hex>",
  "event": {
    "schema_version": 1,
    "sequence": 42,
    "occurred_at": "2026-07-16T08:30:00.000Z",
    "recorded_at": "2026-07-16T08:30:00.015Z",
    "session_id": "<uuid>",
    "run_id": "<uuid-or-null>",
    "provider": "claude",
    "idempotency_key": "<provider-stable-key-or-null>",
    "type": "provider.tool.completed",
    "payload": {}
  }
}
```

The checksum covers the canonical UTF-8 JSON encoding of `event`. Typed payloads
use stable struct fields and ordered maps so encoding is reproducible. Under the
short-lived journal lock, storage reads the last valid event, allocates the next
sequence, appends exactly one newline-terminated envelope, and flushes it before
success.

On startup, Sesh may discard only an incomplete final line without a newline.
A complete checksum-invalid final line and invalid data anywhere earlier are
corruption and cause a fail-closed diagnostic; Sesh does not guess at history
repair.

V1 event families are:

- `session.created`
- `session.forked`
- `switch.requested`
- `run.started`
- `run.handshake`
- `run.stopped`
- `run.recovered`
- `cwd.changed`
- `provider.prompt.submitted`
- `provider.tool.requested`
- `provider.tool.completed`
- `provider.tool.failed`
- `provider.stop.observed`
- `git.snapshot`
- `checkpoint.created`
- `capture.failed`

Provider payloads are normalized into these types. Raw hook documents and native
conversation transcripts are not persisted. Every normalized provider event
records the adapter version and detected provider version. Unknown input fields
are ignored for forward compatibility; missing fields required for a fact cause
a diagnostic rather than a fabricated default.

Command events record the exact command string supplied to the provider's Bash
tool, provider-reported output, exit status when supplied, duration when
supplied, and whether each field was available. Large stdout and stderr values
are stored as SHA-256 blobs. Sesh never claims provider-truncated output is
complete.

Recognized test commands are labeled by a documented, deterministic token rule
covering common runners such as `cargo test`, `pytest`, `go test`, and package
manager test scripts. An unrecognized command remains a command. The handoff
shows both the latest recognized test result and latest failed command, avoiding
AI judgment or a false claim that every test runner is recognized.

Command history covers commands submitted through provider tool hooks and Sesh's
own lifecycle commands. Sesh does not intercept unrelated commands entered in a
different parent shell or terminal.

## Git and Worktree State

V1 requires `git rev-parse --is-inside-work-tree` to succeed. A Git snapshot
contains:

- Canonical repository/common Git directory
- Worktree-specific Git directory and canonical worktree path
- Saved cwd relative to the worktree
- Branch name or explicit detached-HEAD state
- HEAD object ID
- Staged paths and index-side object IDs where available
- Unstaged paths
- Untracked paths, excluding ignored paths
- File type, executable bit, size, and SHA-256 content hash for dirty files
- Submodule status

Snapshots record facts, not patches or routine source copies. A provider hook's
cwd may update the saved cwd only when it resolves inside the bound worktree.
If the saved cwd is removed, `switch` fails and reports it; it never silently
falls back to the worktree root.

Sesh observes Git state at session creation, after relevant provider tool
completion or failure, at each provider stop, before a switch or fork, and after
the child process exits. File-tool paths are recorded even when a full Git
snapshot reports no tracked difference.

## Provider Integration

Sesh uses documented lifecycle hooks rather than terminal scraping or native
transcript parsing.

The normalized hook lifecycle is:

| Hook | Sesh behavior |
| --- | --- |
| `SessionStart` | Record the adapter handshake and return protocol plus handoff context |
| `UserPromptSubmit` | Record the submitted prompt before processing |
| `PreToolUse` | Record intended Bash and file-tool activity |
| `PostToolUse` | Record the outcome and observe Git state |
| Tool failure | Record the failure and observe Git state |
| `Stop` | Record turn completion and observe Git state |

The supervisor independently records process exit, including signal and status,
because lifecycle hooks are not guaranteed after a hard process failure.

When a provider exposes an opaque tool response but no structured command exit
status, Sesh stores that response verbatim and labels the status as incomplete.
It does not parse provider-formatted prose to manufacture a structured fact.

Claude receives a bundled Sesh plugin through `--plugin-dir`. Plugin hooks merge
with existing Claude hooks and do not write `.claude` files into the repository.

Codex receives static Sesh hook definitions through a supported per-launch
configuration overlay. The overlay composes with user configuration and any
user-selected profile. Hook definitions contain no session data; session and
run IDs arrive through inherited environment variables. Codex hook trust is a
one-time explicit `sesh setup codex` action. Sesh does not pass a global
hook-trust bypass. Claude setup validates the local plugin and any trust policy
that applies to `--plugin-dir`.

Both adapters preserve the provider's authentication, model, permission,
sandbox, tool, and personal configuration. Sesh adds only its hook layer, the
narrow checkpoint inbox permission, cwd, and switch bootstrap prompt.

Before accepting work, every launch must produce a `SessionStart` handshake. A
provider version, user setting, or enterprise policy that prevents hooks from
running is an error, not a silent snapshot-only mode. `sesh doctor` checks the
executable, minimum capabilities, hook configuration, trust state where
observable, Git, storage permissions, and format compatibility.

The SessionStart context tells the provider:

- The Sesh session is canonical.
- The worktree is already in the desired state.
- Existing changes must not be reset, discarded, or recommitted merely to
  recreate context.
- Facts and explicit narrative in the handoff must remain distinguished.
- A structured checkpoint should be written after a material milestone and
  before announcing a material stop.

Provider checkpoints are best-effort narrative authored by that provider. Hook,
Git, command, and process facts remain deterministic even when a provider hits a
rate limit before writing its next checkpoint.

## Checkpoint Model and Schema

There are two checkpoint kinds:

- A **narrative checkpoint** is explicitly authored by a human or provider. It
  contains objective, decisions, assumptions, progress, blockers, and next
  steps.
- A **transition checkpoint** is created automatically before a switch or fork.
  It fixes the current factual event boundary and refers to the latest narrative
  checkpoint, if one exists. It never invents or edits narrative.

Every checkpoint is immutable. The narrative-checkpoint schema is:

```json
{
  "schema_version": 1,
  "kind": "narrative",
  "through_sequence": 123,
  "author": { "kind": "human", "provider": null },
  "objective": "Implement OAuth callback handling",
  "summary": "Callback and PKCE support are implemented.",
  "decisions": [
    { "statement": "Store the verifier in the encrypted session cookie.", "reason": "Avoid server-side state." }
  ],
  "assumptions": [],
  "constraints": [],
  "completed": [],
  "in_progress": [],
  "blockers": [],
  "next_steps": ["Fix the remaining callback integration test"],
  "related_event_sequences": [117, 121]
}
```

For a narrative checkpoint, `objective`, `summary`, and `next_steps` must be
present; arrays may be empty. A transition checkpoint instead contains
`kind: "transition"`, `through_sequence`, and
`narrative_checkpoint_sequence`, which is null when no narrative exists. Text
has explicit per-field and total byte limits. Event references must exist in the
same session at or before `through_sequence`.

The V1 typed narrative limit is 32 KiB, leaving deterministic room in the
65,536-byte handoff for repository facts, recent failures, and omission
metadata.

A provider submission enters an inbox as an atomic file, then the supervisor
validates it, appends `checkpoint.created`, and writes immutable JSON and
rendered Markdown named by the resulting event sequence. A transition
checkpoint is built directly from committed canonical state under the session
journal lock.

`latest-checkpoint` points to either kind. `latest-narrative-checkpoint` points
only to explicit narrative. Both refs contain only a sequence and change through
an atomic write-flush-rename operation.

## Deterministic Handoff

Before switching, Sesh locks the session, confirms no run is active, records a
Git snapshot, appends `switch.requested`, creates a transition checkpoint, and
renders a handoff through the last committed event sequence.

The handoff contains, in order:

1. Session, provider-transition, repository, worktree, branch, HEAD, and cwd
2. Transition-checkpoint boundary and latest narrative checkpoint, labeled with
   its author and sequence
3. Current staged, unstaged, and untracked facts
4. Prompts and normalized events after the narrative checkpoint, or from
   session creation when no narrative exists
5. Recent commands and exit statuses
6. Latest recognized test result
7. Latest failed command and bounded output excerpt; the default excerpt keeps
   the first 2 KiB and final 6 KiB and states the omitted byte count
8. Any capture gaps or incomplete facts
9. Exact omitted event ranges and local inspection commands

The default rendered limit is 65,536 UTF-8 bytes. Selection is structural and
deterministic; no model ranks content. The latest checkpoint, current Git
counts/fingerprint, and latest narrative checkpoint are retained. Path details,
post-narrative events, recent commands, and capture-gap details have explicit
omission counts or ranges when they exceed the remaining space. `handoff.md`
and a bounded `recent-events.jsonl` copy are placed in the new run inbox so the
provider can inspect them without canonical-store access.

The new provider receives the detailed handoff through `SessionStart` and only
a fixed initial user prompt:

> Continue the active Sesh session from its injected handoff. Verify the current
> worktree state, then proceed with the recorded next action.

If no narrative checkpoint exists, the transition checkpoint contains a null
narrative reference. The handoff includes available prompts and facts and
explicitly states that objective, decisions, and next steps were not
checkpointed.

## Provider Switch Transaction

`sesh switch <provider>` performs these steps:

1. Resolve and validate the worktree ref.
2. Acquire the session lifecycle-operation lock.
3. Reject a live provider lease.
4. Recover a verifiably stale lease if present.
5. Validate the saved cwd.
6. Observe and append the final pre-switch Git snapshot.
7. Append `switch.requested`.
8. Create and append the transition checkpoint.
9. Render the handoff and run inbox atomically.
10. Create a pending run lease owned by the current supervisor and append
    `run.started`.
11. Release the lifecycle-operation lock.
12. Launch the provider in the saved cwd and atomically add its child-process
    identity to the lease.
13. Require the SessionStart handshake within a 60-second startup deadline.
14. If the handshake fails, terminate the child, reacquire the
    lifecycle-operation lock, record the failed run, and clear the lease.
15. On normal exit, reacquire the lifecycle-operation lock, append `run.stopped`,
    observe Git, and clear the lease.

`operation.lock` serializes provider lifecycle and fork transactions, but is not
held for the interactive provider lifetime. `lock` serializes journal sequence
allocation and append, including hook writes. Keeping the locks distinct lets a
SessionStart hook append its handshake after the lifecycle transaction creates
the lease. The live lease prevents a second provider attachment.

## Session Fork Transaction

`sesh fork <provider>` requires no live provider and performs:

1. Lock and snapshot the source session and worktree.
2. Create an operation journal with source fingerprint and intended target.
3. Materialize the staged binary full-index diff, unstaged binary diff, and
   untracked manifest in the operation area.
4. Snapshot the source again and abort if artifact generation observed mixed
   source states.
5. Create a new branch from the exact source HEAD.
6. Create a Git worktree for that branch.
7. Apply the staged diff with index state.
8. Apply the working-tree-versus-index diff unstaged.
9. Copy untracked regular files and symlinks, preserving executable bits.
10. Snapshot the source again; abort if it changed during duplication.
11. Snapshot the target and compare staged, unstaged, untracked, type, mode, and
    content hashes.
12. Create a transition checkpoint in the parent, append `session.forked` to
    the parent, create the child session with parent session and checkpoint
    lineage, and atomically bind the target worktree ref to the child.
13. Mark the operation complete and launch the requested provider through the
    normal switch path.

The default branch is `sesh/<repository-name>-<short-id>`. Repository names are
sanitized and validated with Git's ref rules. The default worktree is
a sibling named `<repository>-sesh-<short-id>`. Both are overrideable.

Ignored files are excluded. A dirty submodule, sparse-checkout state that cannot
be reproduced, FIFO, socket, device, or other unsupported file type causes a
clear refusal before child-session activation.

On a synchronous failure, Sesh rolls back only artifacts whose fingerprints
prove they are unchanged Sesh-created intermediates. After a crash, it never
silently deletes a target worktree. `sesh doctor` reports the operation journal
and an exact cleanup command.

## Concurrency and Failure Model

Each journal mutation uses the journal advisory lock. Parallel provider hooks
are serialized in receipt order and retain both provider occurrence time and
Sesh record time. IDs supplied by a provider are used as idempotency keys where
available so a retried identical hook cannot create a second fact.

A run lease contains session ID, run ID, provider, host, supervisor PID/start
identity, and child PID/start identity once spawned. PID alone is insufficient
because PIDs are reused. On the same host, Sesh clears a lease only after proving
both recorded processes are gone or no longer match their start identities. If
the supervisor died but its provider child remains alive, Sesh reports the live
orphan and refuses another attachment. A lease from another host is not
auto-recovered in V1.

Capture failures are fail-closed:

- A failed SessionStart handshake terminates startup and reports setup repair.
- A failed `UserPromptSubmit` or `PreToolUse` record blocks that next action.
- A failure after a completed tool action cannot undo the action. It writes a
  run failure sentinel where possible, surfaces the error, and blocks the next
  prompt or tool action until storage is repaired.
- A killed provider or supervisor is recovered from its lease on the next Sesh
  command, followed by a fresh Git snapshot.

Provider exit text is not parsed to infer rate limiting, authentication failure,
or outage. Sesh records the supplied exit facts and the explicit subsequent
switch.

## Privacy and Security

Session data is plain local data, comparable to a local Git database. V1 relies
on user-only filesystem permissions, not encryption. The developer can inspect
it with an editor, `grep`, and `jq`.

Sesh records user prompts and provider-reported command output, which may contain
secrets. It makes no automatic-redaction promise. Session data must be reviewed
before future export or sync. Complete `sesh delete` is the only V1 history
removal operation; selective event rewriting is deferred because it complicates
append-only integrity and can leave secret copies in checkpoint or blob history.

Fork artifacts and child run handoffs can contain parent-session content. Sesh
therefore refuses parent deletion while child sessions remain, deletes children
first, and removes terminal operation artifacts with the parent session. Git
worktrees and branches remain source-code state and are never deleted by session
deletion.

Adapters add only the per-run checkpoint inbox to provider writable roots;
canonical storage is not added as a workspace. Hook subprocesses perform
canonical writes after validation. This is not an OS security boundary: a
provider given unrestricted same-user shell access may still reach any file the
developer can. Checkpoint promotion validates schema, size, session/run
association, event references, file type, ownership, and path containment.

Executables and Git are invoked with argument vectors. Hook commands are static
and expand only the quoted `SESH_HOOK_BIN` executable path; they contain no
interpolated prompt, branch, worktree, or session content. Provider
names are a closed V1 enum, and passthrough arguments remain individual OS
strings.

## Diagnostics

`sesh doctor` reports, without mutation:

- Sesh format and binary compatibility
- State ownership and permissions
- Journal integrity and recoverable final tails
- Stale or foreign-host leases
- Incomplete operation journals
- Git version and current worktree identity
- Claude and Codex executable paths and versions
- Required hook/plugin/config capabilities
- Hook trust or policy restrictions where observable
- Inbox containment and permissions

Repairs are explicit commands. `doctor` does not silently rewrite corrupted
history, remove a worktree, trust a hook, or weaken provider policy.

## TDD Strategy

Implementation proceeds as small vertical red-green-refactor slices. Every
production behavior begins with a test that fails for the intended reason.

### Unit tests

Unit tests cover:

- Event and checkpoint schema validation
- Stable event encoding and checksums
- Sequence allocation and idempotency
- Truncated-tail recovery and mid-journal corruption refusal
- Atomic refs, permissions, ownership, and path containment
- Blob hashing and deduplication within one session
- Lease identity and stale-process decisions
- Provider fixture normalization
- Deterministic handoff ordering and byte limits
- Command classification without model judgment
- CLI parsing and provider-argument boundaries

Time and identifier generation are injectable at the session-core boundary so
golden outputs do not depend on wall-clock time or random UUIDs. Git behavior is
tested against real Git rather than a broad mock.

### Git integration tests

Temporary repositories and linked worktrees cover:

- Main and linked-worktree identity
- Nested cwd restoration
- Branch and detached HEAD
- Staged and unstaged changes to the same path
- Added, deleted, renamed, binary, executable, and symlink changes
- Nested untracked files with spaces and Unicode
- Ignored-file exclusion
- Source mutation during fork
- Dirty-submodule and unsupported-file refusal
- Original-worktree immutability after fork
- Exact target status and hash equivalence

### Provider and CLI integration tests

Test fixtures place deterministic Bash programs named `claude` and `codex` at
the front of `PATH`. They validate received argv, cwd, environment, hook setup,
SessionStart handshake, prompts, tool events, checkpoint inbox promotion,
signals, exit status, and the next provider's handoff. These tests require no
provider account, network, model, or quota.

Sanitized, versioned JSON fixtures represent documented Claude and Codex hook
payloads. Contract tests prove required-field handling and unknown-field forward
compatibility. Optional ignored smoke tests may exercise locally installed real
providers, but normal CI never spends provider quota.

### Fault injection

Storage tests inject failures before append, during append, before flush, during
blob promotion, and before ref rename. Process tests kill fake providers and the
supervisor. Operation tests interrupt fork creation at each durable phase.

### CI

CI runs on current stable Rust for macOS and Linux:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo doc --no-deps
```

## Acceptance Criteria

The North Star acceptance test must:

1. Create a temporary repository, linked worktree, branch, and nested cwd.
2. Start fake Claude using `sesh run claude`.
3. Emit a user objective, file edits, commands, a passing recognized test, a
   failing recognized test, and an explicit checkpoint.
4. Simulate provider unavailability.
5. Run `sesh switch codex`.
6. Prove fake Codex starts in the identical worktree and saved cwd.
7. Prove the injected handoff contains the checkpoint objective, decisions,
   changed paths, recent command statuses, failing test output, and next action.
8. Prove no Sesh state was written into the application repository.

The fork acceptance test must:

1. Create staged, unstaged, binary, executable, symlink, and nested untracked
   state in a source worktree.
2. Run `sesh fork codex`.
3. Prove source and target staged, unstaged, and untracked fingerprints match.
4. Prove the original working files and source index fingerprint are unchanged;
   the common Git directory necessarily gains the new branch and worktree
   metadata.
5. Prove the child session records parent checkpoint lineage.

V1 is successful when these workflows are boringly reliable on macOS and Linux,
including paths containing spaces and non-ASCII characters.
