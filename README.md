# Sesh

Switch AI coding providers without losing your place.

Sesh keeps a local, provider-neutral record of a coding session: its repository,
worktree, working directory, objective, progress, failures, and next steps. When
one provider becomes unavailable, another can continue from the same state.

## Why Sesh

Coding agents accumulate useful context, but that context usually belongs to a
provider-specific conversation. A rate limit or outage can turn a simple switch
into ten minutes of reconstructing what just happened.

Sesh makes the work session the source of truth. Providers are interchangeable
clients attached to it. The result should feel deliberately uneventful:

```text
sesh run claude
# Claude becomes unavailable
sesh switch codex
# Codex continues in the same worktree and saved cwd
```

Sesh is local-first, inspectable, and intentionally boring. It uses files,
JSONL, checksums, and Git facts—not embeddings, a vector database, or a cloud
service.

## Quick start

Sesh V1 supports macOS and Linux. It requires Git, Rust 1.88 or newer, and an
installed Claude Code or Codex CLI. There are no prebuilt binaries yet, so
every install method compiles from source.

Install with one command:

```bash
curl -fsSL https://raw.githubusercontent.com/thomasindrias/sesh/main/install.sh | sh
```

Prefer to inspect the script first, or build by hand? Either works:

```bash
# clone and build manually
git clone https://github.com/thomasindrias/sesh.git
cd sesh
cargo install --path .

# or install straight from the Git repository, no separate clone step
cargo install --git https://github.com/thomasindrias/sesh --locked
```

Then set up the providers you use:

```bash
sesh setup claude
sesh setup codex
```

Start Sesh from the directory where the provider should work:

```bash
cd ~/src/platform/apps/web
sesh run claude
```

Switch providers from anywhere inside that worktree:

```bash
sesh switch codex
```

Inspect what Sesh knows:

```bash
sesh list
sesh status
sesh handoff codex
sesh log
sesh inspect
sesh doctor
```

Run `sesh --help` or `sesh <command> --help` for the complete command surface.

## Switch or fork?

| Command | Use it when | Result |
| --- | --- | --- |
| `sesh switch codex` | You want to continue the same work | Same session, worktree, and saved cwd |
| `sesh fork codex` | You want a separate line of work | New branch, worktree, and child session |

`switch` never clones, stashes, resets, cleans, or recreates the worktree.
`fork` copies the verified staged, unstaged, and non-ignored untracked state into
a new Git worktree. Ignored files are intentionally excluded.

## Reliability contract

- The existing worktree remains the source of truth during `run` and `switch`.
- Sesh records observed facts separately from human or provider-written narrative.
- A fork does not commit, stash, reset, clean, or rewrite the source worktree.
- Fork creation is journaled, verified, and recoverable across interruption.
- Session state lives outside the application repository and is readable with
  ordinary tools such as an editor, `grep`, and `jq`.
- Corrupt or unsupported state fails closed; `sesh doctor` reports recovery steps.

The [architecture](docs/architecture.md) documents the storage model,
transactions, and guarantees. [Provider integrations](docs/providers.md)
explains setup, checkpoints, handoffs, and smoke tests.

## Security

Sesh stores session state as private plaintext under `$SESH_HOME`,
`$XDG_STATE_HOME/sesh`, or `~/.local/state/sesh`. That state may include prompts,
decisions, stack traces, business context, and secrets accidentally given to an
agent. Directories use mode `0700` and files use `0600`, but a provider process
running as your Unix user has the same access that user has.

See [SECURITY.md](SECURITY.md) for the trust model and private vulnerability
reporting instructions.

## Project status

Sesh is early-stage V1 software. The core Claude Code to Codex continuation path,
checkpoints, handoff previews, cross-repository session listing, inspection,
deletion, and explicit worktree forks are implemented and tested on Unix-like
systems. Storage formats and CLI details may still change before a stable
release.

## Roadmap

Planned, in rough order:

- **v0.1.0** — switch-moment polish, prebuilt binaries, a Homebrew formula,
  and a short tutorial.
- **More providers** — Gemini CLI, opencode, Cursor CLI. Not every CLI exposes
  lifecycle hooks, so provider support will be tiered and documented honestly.
- **Multi-machine** — sync through a private Git remote you control, and
  single-file session export and import.
- **A session browser** — the form is deliberately undecided until `list` and
  `handoff` have seen real daily use.

Not planned: a cloud service, Windows support (for now), embeddings, or
AI-generated summaries. Sync will always be a Git remote you own.

## Contributing

Bug reports and focused contributions are welcome. Please read
[CONTRIBUTING.md](CONTRIBUTING.md) and the
[Code of Conduct](CODE_OF_CONDUCT.md) before participating.

## License

Licensed under the [Apache License 2.0](LICENSE).
