# Changelog

All notable changes to Handover will be documented in this file. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `handover arm`, `handover claim`, and `handover attach`: a switch is now a
  one-shot, expiring capability that can be recorded while a provider is still
  running and completed when it exits.
- Quitting a supervised provider with a switch armed launches the armed target
  in the same terminal.

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
