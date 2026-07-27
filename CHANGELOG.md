# Changelog

All notable changes to Sesh will be documented in this file. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-27

### Added

- Local, provider-neutral coding sessions for Claude Code and Codex.
- Append-only, checksummed events and explicit narrative checkpoints.
- Deterministic handoffs that restore the existing worktree and saved cwd.
- Inspect, status, log, doctor, setup, and complete logical deletion commands.
- Verified worktree forks with staged, unstaged, and untracked state, durable
  transaction recovery, and parent-child session lineage.
- A global `sesh list` command that reports every local session across
  repositories, rendering corrupt sessions as degraded diagnostic rows instead
  of failing the listing.
- A `sesh handoff <provider>` command that renders the exact handoff a
  switch would produce right now, without switching, so a missing or stale
  narrative checkpoint is visible before a switch is spent.
- Checkpoint staleness signals: `sesh status` reports
  `latest_narrative_checkpoint` and `events_since_narrative`, and the Stop
  hook warns with a single `systemMessage` when 20 or more events accumulate
  without a fresh narrative checkpoint.
- Switch-moment ergonomics: a live-lease refusal in `sesh switch` now names
  the holding provider, pid, and start time with a one-line next step; a
  same-host stale lease is recovered only through an explicit, journaled
  `sesh switch` prompt (or `--recover-lease` non-interactively) instead of
  silently auto-recovering; and `sesh status` reports a `switch_readiness`
  block (lease state, checkpoint freshness, handoff renderability) so a
  switch's success can be seen before quitting the current provider.
- A one-command `install.sh` script that builds and installs sesh from
  source (`curl -fsSL .../install.sh | sh`), safe to re-run as an upgrade,
  with a `PATH` check and next-step guidance on success.
- A `sesh mcp-server` subcommand exposing `list`, `handoff`, and `status` as
  MCP tools over stdio, so a provider attached to a Sesh session can query
  it directly instead of a human running commands in a second terminal.
  `sesh status --json`'s `switch_readiness` block now also reports a
  `suggested_switch_command` string naming the exact command to run to
  switch.

### Fixed

- `sesh doctor` no longer reports a correctly installed Codex integration as
  insecure. The permission walk rejected every symlink under the state root,
  but the Codex adapter deliberately links `hooks.json`, `config.toml`, and
  `auth.json` into each run's private `CODEX_HOME`, so `sesh setup codex`
  left `doctor` failing and every Codex run added three more errors. A
  symlink is now judged by its target, which must be a regular file owned by
  the current user with mode `0600`, and is never followed into a directory.
  The message names the target rather than the link.
- `sesh doctor` no longer applies Sesh's canonical `0600`/`0700` permissions to
  the contents of a provider's private home. A real Codex launch writes its own
  databases and scratch directory into the per-run `CODEX_HOME`, which left
  `doctor` reporting seven errors about files Sesh does not create and cannot
  control. Sesh now guarantees the `0700` container and leaves its contents to
  the provider.
- `sesh doctor` now reports a provider that was never set up as
  `integration.missing` with the exact `sesh setup <provider>` command,
  instead of a raw "No such file or directory" I/O error.
- Codex hook delivery: hooks registered via `-c` config overlays never
  actually fired against real Codex CLI builds. Each Codex launch now gets
  a private, per-run `CODEX_HOME` with a materialized `hooks.json` and
  symlinked real `config.toml`/`auth.json`, verified working end to end
  against Codex 0.145.0.

### Security

- Private local state permissions, ownership and symlink checks, and fail-closed
  validation for canonical session data.
