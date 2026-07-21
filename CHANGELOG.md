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

### Security

- Private local state permissions, ownership and symlink checks, and fail-closed
  validation for canonical session data.
