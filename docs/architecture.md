# Handover architecture

Handover is a local session layer for coding agents. It keeps continuity independent
of the provider while leaving source control to Git.

This document describes the V1 model and its reliability boundaries. It is a
contract for contributors as much as an implementation overview.

## Core model

A session represents one line of work in one Git worktree. It owns:

- repository, worktree, branch, and saved working-directory identity;
- append-only observed events;
- explicit narrative and automatic transition checkpoints;
- provider runs and their lifecycle;
- optional parent or child lineage created by `handover fork`.

The session is canonical. Provider-native conversation identifiers may be useful
diagnostics, but Handover never requires a provider transcript to resume work. Each
new attachment starts from a deterministic handover.

Facts and narrative remain separate. Handover can observe a command, process exit,
Git status, path, hash, or timestamp. It cannot infer why a choice was made or
what should happen next. That narrative enters the session only through an
explicit human or provider checkpoint. Missing narrative is reported as missing.

## Components

Handover is one Rust binary with no daemon requirement.

- The session core defines stable identifiers, events, checkpoints, refs, and
  handover rendering without depending on provider payloads.
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

`$HANDOVER_HOME` selects the state root. Otherwise Handover uses
`$XDG_STATE_HOME/handover`, then `~/.local/state/handover`.

```text
$HANDOVER_HOME/
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
of a permissive umask. Handover rejects unexpected ownership, symlink traversal, and
unsafe canonical-state permissions. Handover writes only regular files and
directories into canonical state, so a symlink there is refused rather than
followed.

A provider's private home, such as the per-run Codex `CODEX_HOME`, is not
canonical Handover state. Handover materializes it and then the provider writes its own
files there with its own permissions, so Handover guarantees the `0700` directory
that contains them — which is what keeps other users out — and does not police
its contents. Session state never belongs in the application repository.

V1 requires canonical repository, worktree, cwd, dirty, and symlink-target paths
to be valid UTF-8. Unsupported paths fail before session or fork activation; Handover
does not record lossy replacements.

Worktree refs are keyed by a SHA-256 digest of the canonical Git common directory
and worktree-specific Git directory. The stored ref repeats those identities and
the canonical path, allowing collisions, moved worktrees, and stale bindings to
fail closed.

## Events and checkpoints

`events.jsonl` is the canonical append-only journal. Each line contains a typed
event and a SHA-256 checksum of its canonical JSON representation. Under a short
lock, Handover reads the last valid event, assigns the next sequence, appends one
newline-terminated envelope, and flushes before returning success.

An incomplete final line may be discarded during startup. A complete invalid
line, checksum mismatch, sequence gap, or corruption earlier in the journal is a
hard diagnostic; Handover does not guess at repair.

Normalized event families cover session creation and lineage, switch requests,
provider run lifecycle, saved cwd changes, prompts, tools, commands, Git
observations, checkpoints, output references, and recovery. Large bounded output
is stored in session-scoped content-addressed blobs and referenced by hash.

Narrative checkpoints contain the objective, summary, decisions, assumptions,
constraints, completed and in-progress work, blockers, next steps, and optional
related event sequences. Transition checkpoints are created automatically at
provider boundaries and point back to the latest verified narrative checkpoint;
they never invent narrative.

The handover selects a committed journal prefix, verifies every referenced event
and blob, refreshes Git facts, and renders bounded Markdown. Required facts are
never silently truncated. If they do not fit, switching fails with a diagnostic.

## Run and switch transactions

`handover run` discovers the current worktree and cwd, refuses an existing binding,
creates the session and binding, writes the first run inbox, acquires a lease,
and launches the provider. Setup failures roll back the new binding and session.
Once the child launches, its exit is history rather than a setup rollback.

`handover switch` resolves the existing binding from anywhere inside the worktree,
verifies journal and metadata consistency, refuses a live lease, records the
request and transition checkpoint, and builds a handover. The provider starts in
the latest saved cwd—not necessarily the directory from which `switch` was run.

Normal run and switch paths are observational toward source. They do not create
a worktree, commit, stash, reset, clean, checkout, or rewrite files.

Run leases contain the process identity needed to distinguish a live provider
from a stale record. Stale leases are recovered explicitly and recorded; Handover
does not permit two live provider attachments to the same session.

`handover switch` surfaces this at two points: a live or foreign-host lease
refuses with the holding provider, pid, and start time; a same-host dead
lease is recovered only after an explicit `[y/N]` prompt (or
`--recover-lease` non-interactively), never silently. `handover status` reports
a `switch_readiness` block — lease state, narrative checkpoint freshness,
handover renderability, any pending arm, and the suggested switch command — so
this can be checked before quitting the current provider. A pending arm makes
it not ready: the only switch that would be accepted is the one that arm
already names, and the suggested command says so.

A switch is two-phase. `handover arm <provider>` records intent as
`switch.requested` plus `switch.armed` — a one-shot capability identified by
the sequence of its `switch.armed` event, carrying an expiry. `handover claim`
consumes it exactly once, commits the transition checkpoint, and records
`switch.claimed`. `handover switch` composes both in one command and refuses
when an arm for a different provider is already pending.

Expiry is evaluated lazily. Handover has no daemon, so an arm found past its
deadline is retired at read time and `switch.expired` is appended before the
read returns. An arm that is never read again simply stays unobserved.

An arm authorises one narrow thing: releasing a dead lease belonging to the run
that armed it, without the interactive prompt `switch --recover-lease`
otherwise requires. It cannot touch a live lease, nor another run's lease. That
authority is recorded only when the caller *is* that run — an arm typed in a
plain terminal adopts nothing, so `arm` plus `claim` can never become an
unprompted recovery of a lease its caller never owned. That release is journaled
as `run.recovered`, so no lease leaves a session's history unexplained.
Arm-and-complete-on-exit follows from these rules rather than needing
enforcement: a claim attempted while the provider still runs refuses, because
its lease is live.

When the provider `handover run` or `handover switch` supervises exits and an
arm is pending, that supervisor claims it and launches its target in the same
terminal. `handover fork` supervises a child too, but deliberately does not:
a fork is a separate line of work, not a continuation of this one.

## Binding tier

A session's binding — what it is currently attached to, and how — is a
derived fact rather than a journaled one. `session::binding` (`src/session.rs`)
reads the events a session already has and picks whichever of `run.started`
and `session.attached` carries the higher sequence number. No event kind or
payload field was added to say this directly: the journal is append-only,
checksummed, and rejects unknown fields, so a name written into it is
permanent, and the tier is fully recoverable from events Handover was already
recording.

Because the comparison is by sequence rather than by an order assumed in
advance, a worktree moves between tiers in either direction: a session
`handover run` created can later be adopted with `handover attach` once that
run has stopped, and an adopted session can later be superseded by a fresh
`handover run`. Each case is reported as whichever event actually has the
higher sequence, not as whichever tier came first.

- **Supervised** — the binding is a `run.started` event. Handover launched
  the provider, holds its run lease, and observes its lifecycle hooks.
- **Attached** — the binding is a `session.attached` event, recorded either by
  `handover attach` or by a desktop launch that succeeded
  (`docs/providers.md`), which reaches the same fact by opening the
  application rather than by being told about one already open. Handover did
  not launch this provider as a supervised child, so there is no lifecycle to
  observe: the session's journal holds narrative checkpoints and refreshed Git
  facts, but no observed activity. This is not a defect; it is what adoption
  is, and Handover reports it as a fact rather than implying a completeness
  the session does not have.

An attached binding can additionally be reported **detached**: still on
screen, but no longer current. If a `switch.claimed` event has since moved
sequence past the attaching `session.attached`, the attachment stays
reported — its provider named, its sequence held — but with `detached: true`,
and the top-level provider fields that read it (`status`'s `provider`,
`list`'s `last_provider`) go to `null` rather than asserting a binding the
journal no longer supports. Handover reports this rather than resolving it
because it cannot: nothing here can make a desktop application quit, so the
window may genuinely still be open on screen while the journal has already
moved on.

`status --json`'s `binding` block, `list --json`'s `tier`/`detached` row
fields, and `doctor`'s diagnostics all read this same derivation, so the
three cannot disagree about a session's tier. `doctor` reports an adopted
session with a `note`-severity diagnostic — a fact worth stating that is not
a fault — and `doctor`'s exit code keys on `severity == "error"` alone, so a
note never fails the command the way a `warning` or `error` diagnostic would.

## Fork transaction

`handover fork` deliberately creates a separate line of work. It is not an option on
`run` or `switch`, because copying worktree state has a different risk and
recovery model.

Preflight rejects ambiguous or unsupported state before mutation, including:

- sparse checkout, unmerged or intent-to-add index entries;
- staged gitlinks, dirty submodules, and unsupported special files;
- invalid branch names, existing targets, and nested registered worktrees;
- active or unresolved leases and non-UTF-8 canonical paths.

Handover captures tracked index state, staged and unstaged binary patches, deletions,
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

The verified parent `session.forked` event is the commit point. Before it, Handover
may remove only branch/worktree artifacts that it can prove it created and that
remain unchanged. After it, recovery is forward-only: doctor completes the
child binding and launch state without deleting committed lineage. If proof is
insufficient, Handover preserves the artifacts and reports exact inspection steps.

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

`handover delete` removes the complete local session and its binding, not source
files, worktrees, or Git branches. Parent sessions with children are deleted
child-first, and nonterminal fork operations block deletion. Logical deletion is
not forensic erasure from storage media, snapshots, backups, or exported copies.

## Security boundary

Private modes and path validation reduce accidental disclosure. They do not
isolate processes running as the same Unix user. An unrestricted coding agent
can access anything that user can access. Handover does not provide sandboxing,
encryption, automatic secret detection, or redaction.

Provider hook inputs and run-inbox checkpoint files are untrusted. They are
bounded, parsed, normalized, and validated before canonical history changes.
Provider checkpoint submission is scoped to the active run through private
identifiers and an inbox path inherited by launched provider descendants. This
does not prove process ancestry and is not a same-user authorization boundary.
Human checkpoints use a separate path.

The MCP server guard exception ("MCP server" below) is a concrete instance of
this boundary: it lets a launched, attached provider read, and now also arm and
claim a switch, through a different path, what same-user access already
permits. Provider-side writes reuse the checkpoint path's run scoping and add
one more check on top of it: the run must still hold the session's lease.

## V1 non-goals

V1 does not include cloud or multi-machine synchronization, remote MCP, team
sharing, embeddings, semantic retrieval, transcript scraping, autonomous agent
orchestration, Windows support, or worktree creation during normal switching.
These boundaries keep the core continuation path small and auditable.

## MCP server

`handover mcp-server` (`docs/mcp.md`) exposes `list`, `preview`, `status`,
`arm`, `claim`, and `attach` as MCP tools over stdio. `provider_command_allowed`
(src/app.rs) has explicit exceptions for `Command::McpServer` and for `arm` and
`claim` under `--from-provider`, so those can run even when `HANDOVER_RUN_ID` is
set — the situation they are built for, since an MCP client spawns the server as
a subprocess of the very provider Handover launched, and a launched provider is
exactly who should be able to say where the session goes next. No other
command's behavior under that guard changes. Tool calls run the same
value-producing code the CLI uses, entirely in-process — never a subprocess,
never a second pass through the CLI dispatcher.

This exception widens what an attached provider can do. Through the MCP tools
it receives the exact `list`, `preview`, and `status` output that
`provider_command_allowed` refuses it via the CLI. `list` in particular is not
scoped to the attached session: it reports the repository path, worktree path,
and branch for every session on the machine.

Three of the six tools also write. `arm` and `claim` are scoped to the active
run: they pass the same session-id, run-id, and private-inbox-path check that
provider checkpoint submission passes (`checkpoint::active_run`), then require
that the session resolved from the cwd is the one that run is attached to, and
finally that the run still holds that session's lease. The environment proves
which run a process belongs to; the cwd decides which session a command acts
on, and a provider process can change directory, so both are checked. The
lease check closes what those two leave open: a run's directory and inbox
outlive the run itself, so without it a finished run's leftover environment
could still arm or claim a switch nobody asked for.

`attach` is the single deliberate exception. By definition no run exists when a
session is adopted — a provider Handover did not launch has no run environment
at all — so `attach` is scoped to the worktree its cwd resolves to, which is the
same scoping `run` uses.

None of this is an authorization boundary. It is a guardrail against accidental
misuse: the guard does not prove process ancestry, and a provider process
running as the same Unix user can already write `$HANDOVER_HOME` directly (see
"Security boundary" above).
