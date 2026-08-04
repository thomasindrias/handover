# Changelog

All notable changes to Handover will be documented in this file. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `handover arm`, `handover claim`, and `handover attach`: a switch is now a
  one-shot, expiring capability that can be recorded while a provider is still
  running and completed when it exits. An arm carries authority over a lease
  only when the caller is the run holding it, so `arm` plus `claim` can never
  release a crashed run's lease without the consent prompt
  `handover switch --recover-lease` requires.
- Quitting a supervised provider with a switch armed launches the armed target
  in the same terminal.
- `handover status --json`'s `switch_readiness` block now reports any pending
  arm as an `armed` object (target, sequence, expiry), counts one against
  `ready`, and points `suggested_switch_command` at the armed target — while an
  arm is pending, that is the only switch that will be accepted.
- A provider Handover launched can now arm its own switch: `/handover-switch` in
  Claude, a `handover-switch` skill in Codex, and `handover arm <provider>
  --from-provider` underneath both. Type it where you are working; the session
  moves when you quit.
- MCP `arm`, `claim`, and `attach` tools. `arm` and `claim` are scoped to the
  active run; `attach` is scoped to the worktree, because a session Handover did
  not launch has no run.
- `handover status --json` reports a session's binding tier in a `binding`
  block (`tier`, `provider`, `sequence`, `detached`), and `handover list
  --json` rows gained `tier` and `detached`. The tier is derived from
  whichever of `run.started` or `session.attached` is most recent, so a
  worktree can move between them in either direction with no new event kind
  added to say so. `handover doctor` now states an adopted session's tier as
  a `note`-severity diagnostic, which never affects `doctor`'s exit code.
  Once a claim has moved an attached session on, its provider is reported
  `null` — not the provider that is no longer bound — while the binding
  itself still names it as attached and `detached`.
- `handover arm --replace` supersedes an already-pending arm instead of
  refusing, retiring the superseded one with the same `switch.expired` event
  lazy expiry already uses. `handover switch` has no equivalent flag: it still
  refuses when a different provider is already armed.
- `handover arm --surface desktop` (and `handover switch` reusing an arm
  recorded on that surface) opens the target provider's desktop application
  — `codex app <worktree>` for Codex, `open claude://code/new` for Claude —
  instead of supervising a CLI child. A desktop launch gets no injected files
  and pulls its handover over MCP on its first turn, which requires
  Handover's MCP server to already be configured for that application by
  hand (`docs/mcp.md`); Handover does not register it automatically. A
  failed launch degrades the command rather than failing it: the arm stays
  claimed and the run's own exit code is preserved. `open` is macOS-only, so
  a Claude desktop arm cannot open on Linux. A launch that succeeds journals
  `session.attached` for the target it opened, so `status`, `list` and
  `doctor` report the application the user is now in rather than the provider
  it replaced — and the next arm's permanent `switch.requested.from` names it
  too.
- A Handover-launched Codex session now keeps the user's own skills. The private
  per-run `CODEX_HOME` links each entry of the real `skills/` directory
  individually, beside Handover's own.

## [0.1.1] - 2026-07-28

### Added

- A `/handover-checkpoint` command in the Claude integration that gathers the
  session narrative and submits it, so writing a checkpoint no longer requires
  knowing the raw CLI form.
- Every handover now ends with the exact command that records a checkpoint.
  A provider reads its handover at SessionStart, so any attached agent — Claude
  or Codex — is told how to leave a narrative for the next one.

### Fixed

- Nothing told an agent how to checkpoint. The plugin shipped hooks only, the
  handover never mentioned the command, and the staleness nudge said "ask the
  agent to checkpoint" while the agent had no way to know how. A session could
  therefore accumulate hundreds of events and still hand over with no
  narrative, which is the one thing observed events cannot supply.
- The refusal an attached provider receives now names
  `handover checkpoint --format json --from-provider`, instead of stating only
  that a provider may submit checkpoints and leaving the flag to be guessed.
- `handover doctor` reports an integration that predates a newer Handover as
  `integration.outdated` with the `handover setup <provider>` that fixes it,
  rather than a bare "No such file or directory" from the first asset the older
  install has never seen.

## [0.1.0] - 2026-07-27

### Added

- Local, provider-neutral coding sessions for Claude Code and Codex.
- Append-only, checksummed events and explicit narrative checkpoints.
- Deterministic handovers that restore the existing worktree and saved cwd.
- Inspect, status, log, doctor, setup, and complete logical deletion commands.
- Verified worktree forks with staged, unstaged, and untracked state, durable
  transaction recovery, and parent-child session lineage.
- A global `handover list` command that reports every local session across
  repositories, rendering corrupt sessions as degraded diagnostic rows instead
  of failing the listing.
- A `handover preview <provider>` command that renders the exact handover a
  switch would produce right now, without switching, so a missing or stale
  narrative checkpoint is visible before a switch is spent.
- Checkpoint staleness signals: `handover status` reports
  `latest_narrative_checkpoint` and `events_since_narrative`, and the Stop
  hook warns with a single `systemMessage` when 20 or more events accumulate
  without a fresh narrative checkpoint.
- Switch-moment ergonomics: a live-lease refusal in `handover switch` now names
  the holding provider, pid, and start time with a one-line next step; a
  same-host stale lease is recovered only through an explicit, journaled
  `handover switch` prompt (or `--recover-lease` non-interactively) instead of
  silently auto-recovering; and `handover status` reports a `switch_readiness`
  block (lease state, checkpoint freshness, handover renderability) so a
  switch's success can be seen before quitting the current provider.
- Prebuilt binaries for macOS and Linux on both Apple silicon and x86-64,
  published on each tagged release with a `SHA256SUMS` file, plus a Homebrew
  formula that the release pipeline keeps current. Installing Handover no longer
  requires a Rust toolchain.
- A one-command `install.sh` script that builds and installs handover from
  source (`curl -fsSL .../install.sh | sh`), safe to re-run as an upgrade,
  with a `PATH` check and next-step guidance on success.
- A `handover mcp-server` subcommand exposing `list`, `preview`, and `status` as
  MCP tools over stdio, so a provider attached to a Handover session can query
  it directly instead of a human running commands in a second terminal.
  `handover status --json`'s `switch_readiness` block now also reports a
  `suggested_switch_command` string naming the exact command to run to
  switch.

### Fixed

- `handover doctor` no longer reports a correctly installed Codex integration as
  insecure. The permission walk applied Handover's canonical `0600`/`0700` rules to
  a provider's private home, which Handover creates but does not own: the adapter
  links `hooks.json`, `config.toml`, and `auth.json` into each run's
  `CODEX_HOME`, and a real Codex launch then writes its own databases and
  scratch directory alongside them. `handover setup codex` therefore left `doctor`
  failing, and every Codex run added more errors about files Handover neither
  creates nor controls. Handover now guarantees the `0700` container that keeps
  other users out and leaves its contents to the provider. An unexpected
  symlink in canonical state is still refused, now with a message that says so.
- `handover doctor` now reports a provider that was never set up as
  `integration.missing` with the exact `handover setup <provider>` command,
  instead of a raw "No such file or directory" I/O error.
- A run that stopped without a SessionStart handshake is now a warning, rather
  than an error that pinned `handover doctor` to a failing exit code, once some run
  in that session has handshaken — an earlier handshake proves the provider's
  hooks reach Handover, so that run died for its own reasons. When no run in the
  session has ever handshaken the integration itself is suspect, so it stays an
  error and now names the provider setup command. The message also no longer
  prints a raw `Some(RunId(...))` debug value.
- Codex hook delivery: hooks registered via `-c` config overlays never
  actually fired against real Codex CLI builds. Each Codex launch now gets
  a private, per-run `CODEX_HOME` with a materialized `hooks.json` and
  symlinked real `config.toml`/`auth.json`, verified working end to end
  against Codex 0.145.0.

### Security

- Private local state permissions, ownership and symlink checks, and fail-closed
  validation for canonical session data.
