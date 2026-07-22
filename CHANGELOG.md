# Changelog

All notable changes to Sesh will be documented in this file. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

### Fixed

- Codex hook delivery: hooks registered via `-c` config overlays never
  actually fired against real Codex CLI builds. Each Codex launch now gets
  a private, per-run `CODEX_HOME` with a materialized `hooks.json` and
  symlinked real `config.toml`/`auth.json`, verified working end to end
  against Codex 0.145.0.

### Security

- Private local state permissions, ownership and symlink checks, and fail-closed
  validation for canonical session data.
