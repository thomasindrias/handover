# Sesh architecture

Sesh is a local session layer for coding agents. It keeps continuity independent
of the provider while leaving source control to Git.

This document describes the V1 model and its reliability boundaries. It is a
contract for contributors as much as an implementation overview.

## Core model

A session represents one line of work in one Git worktree. It owns:

- repository, worktree, branch, and saved working-directory identity;
- append-only observed events;
- explicit narrative and automatic transition checkpoints;
- provider runs and their lifecycle;
- optional parent or child lineage created by `sesh fork`.

The session is canonical. Provider-native conversation identifiers may be useful
diagnostics, but Sesh never requires a provider transcript to resume work. Each
new attachment starts from a deterministic Sesh handoff.

Facts and narrative remain separate. Sesh can observe a command, process exit,
Git status, path, hash, or timestamp. It cannot infer why a choice was made or
what should happen next. That narrative enters the session only through an
explicit human or provider checkpoint. Missing narrative is reported as missing.

## Components

Sesh is one Rust binary with no daemon requirement.

- The session core defines stable identifiers, events, checkpoints, refs, and
  handoff rendering without depending on provider payloads.
- Storage owns paths, private permissions, locks, checksums, atomic refs, blobs,
  leases, fork operation records, and recovery.
- The Git layer discovers repository and worktree identity, observes dirty state,
  computes fingerprints, and materializes explicit forks. Git is invoked with
  argument vectors and machine-readable output rather than a shell.
- Provider adapters probe executables, materialize inspectable integration
  assets, normalize hooks, and prepare launch specifications.
- The supervisor runs the provider with the caller's terminal, forwards signals,
  owns the run lease, and records process completion and post-run Git facts.

Storage, Git mutation, and provider-specific parsing stay behind separate
boundaries. Adding a provider must not require a new session or checkpoint model.

## Local state

`$SESH_HOME` selects the state root. Otherwise Sesh uses
`$XDG_STATE_HOME/sesh`, then `~/.local/state/sesh`.

```text
$SESH_HOME/
├── FORMAT
├── integrations/
│   ├── claude/<adapter-version>/
│   └── codex/<adapter-version>/
├── refs/worktrees/<identity-hash>.json
├── operations/<operation-id>/
│   ├── operation.json
│   ├── staged.patch
│   ├── unstaged.patch
│   └── untracked/
└── sessions/<session-id>/
    ├── meta.json
    ├── events.jsonl
    ├── lock
    ├── operation.lock
    ├── refs/
    ├── checkpoints/
    ├── blobs/
    └── runs/<run-id>/inbox/
```

Canonical directories use mode `0700` and regular files use `0600`, independent
of a permissive umask. Sesh rejects unexpected ownership, symlink traversal, and
unsafe canonical-state permissions. Session state never belongs in the
application repository.

V1 requires canonical repository, worktree, cwd, dirty, and symlink-target paths
to be valid UTF-8. Unsupported paths fail before session or fork activation; Sesh
does not record lossy replacements.

Worktree refs are keyed by a SHA-256 digest of the canonical Git common directory
and worktree-specific Git directory. The stored ref repeats those identities and
the canonical path, allowing collisions, moved worktrees, and stale bindings to
fail closed.

## Events and checkpoints

`events.jsonl` is the canonical append-only journal. Each line contains a typed
event and a SHA-256 checksum of its canonical JSON representation. Under a short
lock, Sesh reads the last valid event, assigns the next sequence, appends one
newline-terminated envelope, and flushes before returning success.

An incomplete final line may be discarded during startup. A complete invalid
line, checksum mismatch, sequence gap, or corruption earlier in the journal is a
hard diagnostic; Sesh does not guess at repair.

Normalized event families cover session creation and lineage, switch requests,
provider run lifecycle, saved cwd changes, prompts, tools, commands, Git
observations, checkpoints, output references, and recovery. Large bounded output
is stored in session-scoped content-addressed blobs and referenced by hash.

Narrative checkpoints contain the objective, summary, decisions, assumptions,
constraints, completed and in-progress work, blockers, next steps, and optional
related event sequences. Transition checkpoints are created automatically at
provider boundaries and point back to the latest verified narrative checkpoint;
they never invent narrative.

The handoff selects a committed journal prefix, verifies every referenced event
and blob, refreshes Git facts, and renders bounded Markdown. Required facts are
never silently truncated. If they do not fit, switching fails with a diagnostic.

## Run and switch transactions

`sesh run` discovers the current worktree and cwd, refuses an existing binding,
creates the session and binding, writes the first run inbox, acquires a lease,
and launches the provider. Setup failures roll back the new binding and session.
Once the child launches, its exit is history rather than a setup rollback.

`sesh switch` resolves the existing binding from anywhere inside the worktree,
verifies journal and metadata consistency, refuses a live lease, records the
request and transition checkpoint, and builds a handoff. The provider starts in
the latest saved cwd—not necessarily the directory from which `switch` was run.

Normal run and switch paths are observational toward source. They do not create
a worktree, commit, stash, reset, clean, checkout, or rewrite files.

Run leases contain the process identity needed to distinguish a live provider
from a stale record. Stale leases are recovered explicitly and recorded; Sesh
does not permit two live provider attachments to the same session.

## Fork transaction

`sesh fork` deliberately creates a separate line of work. It is not an option on
`run` or `switch`, because copying worktree state has a different risk and
recovery model.

Preflight rejects ambiguous or unsupported state before mutation, including:

- sparse checkout, unmerged or intent-to-add index entries;
- staged gitlinks, dirty submodules, and unsupported special files;
- invalid branch names, existing targets, and nested registered worktrees;
- active or unresolved leases and non-UTF-8 canonical paths.

Sesh captures tracked index state, staged and unstaged binary patches, deletions,
renames, executable bits, regular untracked files, and untracked symlinks.
Ignored files are excluded. Clean initialized submodules are reconstructed only
from proven local Git objects; fork never fetches.

Every fork has a durable operation record outside both worktrees. Its phases are:

```text
prepared → artifacts_captured → worktree_created → staged_applied
→ unstaged_applied → untracked_copied → verified → child_staged
→ lineage_committed → child_bound → run_leased → complete
```

`rolled_back` and `needs_manual_recovery` are terminal diagnostic outcomes.
Artifacts are hashed and source state is fingerprinted before and after capture.
The target receives the artifacts, then must produce the same semantic
fingerprint before lineage can commit.

The verified parent `session.forked` event is the commit point. Before it, Sesh
may remove only branch/worktree artifacts that it can prove it created and that
remain unchanged. After it, recovery is forward-only: doctor completes the
child binding and launch state without deleting committed lineage. If proof is
insufficient, Sesh preserves the artifacts and reports exact inspection steps.

The source worktree is never rewritten. The only source-repository mutation is
the requested branch and worktree registration in shared Git metadata.

## Inspection, recovery, and deletion

`status` combines verified history with a fresh Git observation. `log --json`
returns original event envelopes. `inspect` reports state paths, modes, hashes,
sizes, refs, and lease information without printing blob contents.

`doctor` is observational unless `--repair` is supplied. It validates provider
assets and reports interrupted fork operation ID, phase, source session, target,
branch, and a shell-escaped Git inspection command. Repair never deletes a
crash-left branch or worktree when safe ownership cannot be proven.

`sesh delete` removes the complete local session and its binding, not source
files, worktrees, or Git branches. Parent sessions with children are deleted
child-first, and nonterminal fork operations block deletion. Logical deletion is
not forensic erasure from storage media, snapshots, backups, or exported copies.

## Security boundary

Private modes and path validation reduce accidental disclosure. They do not
isolate processes running as the same Unix user. An unrestricted coding agent
can access anything that user can access. Sesh does not provide sandboxing,
encryption, automatic secret detection, or redaction.

Provider hook inputs and run-inbox checkpoint files are untrusted. They are
bounded, parsed, normalized, and validated before canonical history changes.
Only a child process proven to descend from the active provider may submit a
provider checkpoint. Human checkpoints use a separate path.

## V1 non-goals

V1 does not include cloud or multi-machine synchronization, remote MCP, team
sharing, embeddings, semantic retrieval, transcript scraping, autonomous agent
orchestration, Windows support, or worktree creation during normal switching.
These boundaries keep the core continuation path small and auditable.
