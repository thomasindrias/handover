# Sesh

Sesh lets you switch AI coding providers without losing your place. It keeps a local, provider-neutral session beside Git—not inside your repository—so a Claude Code run can stop and Codex can continue in the same worktree and working directory with verified context.

V1 supports macOS and Linux, requires Git, and integrates with the installed `claude` and `codex` command-line tools.

## Install

Install the Rust toolchain, clone this repository, then install from source:

```bash
cargo install --path .
```

Review and trust each provider integration once in an interactive terminal:

```bash
sesh setup claude
sesh setup codex
```

Setup materializes inspectable, versioned hook assets in Sesh's private state directory. It opens the provider only for reviewing those hooks; it supplies no prompt and should consume no model turn.

## End-to-end workflow

Run Sesh from the exact directory where the provider should begin:

```bash
cd ~/src/platform/oauth-worktree/apps/web
sesh run claude -- --model sonnet
```

During the run, create an explicit provider checkpoint from a Bash tool:

```bash
printf '%s' '{"objective":"Implement OAuth callback with PKCE","summary":"Callback and PKCE are implemented","decisions":[{"statement":"Keep the verifier in the session cookie","reason":"Avoid server-side state"}],"assumptions":[],"constraints":[],"completed":["OAuth callback","PKCE"],"in_progress":[],"blockers":["integration test failure"],"next_steps":["Fix the callback integration test"],"related_event_sequences":[]}' | "$SESH_HOOK_BIN" checkpoint --format json --from-provider
```

If Claude becomes unavailable, run this anywhere inside the same worktree:

```bash
cd ~/src/platform/oauth-worktree
sesh switch codex -- --model gpt-5
```

Codex starts in the saved `apps/web` directory. Sesh uses the existing worktree by default: it does not clone, reset, stash, clean, or recreate it. The handoff contains the verified checkpoint, current Git facts, recent events, command statuses, failures, and explicit omissions.

## Human checkpoints

Pipe the same JSON shape without `--from-provider`:

```bash
sesh checkpoint --format json < checkpoint.json
```

With terminal stdin, `sesh checkpoint` opens a private temporary JSON template using `$VISUAL`, then `$EDITOR`. The file is removed after parsing. Provider-attached processes may only submit through their private checkpoint inbox; they cannot use the human mutation path.

## Inspect a session

```bash
sesh status --json
sesh log --json
sesh log --from 5
sesh inspect --json
sesh doctor --json
```

`status` combines verified session history with a fresh Git observation. `log --json` emits the original checksummed event envelopes. `inspect` reports paths, permissions, checkpoint files, blob hashes and sizes, and lease state without printing blob contents. `doctor` is observational unless `--repair` is supplied; repair is limited to incomplete journal tails, verified checkpoint refs, and capture-failure sentinels.

## Local data and security

Sesh stores private plaintext under `$SESH_HOME`, `$XDG_STATE_HOME/sesh`, or `~/.local/state/sesh`. Session data can contain prompts, decisions, stack traces, business context, and secrets accidentally supplied to a provider. Protect backups and machine access accordingly.

Mode `0700` directories and `0600` files prevent accidental sharing, but an unrestricted provider shell running as your Unix user is not an operating-system security boundary. Same-user processes can access anything that user can access.

Sesh does not parse provider transcripts, use embeddings or semantic retrieval, require cloud services, commit session state to Git, or write `.sesh`, `.claude`, or `.codex` state into the application worktree.

To remove the complete local session and its worktree binding:

```bash
sesh delete
# or, for automation:
sesh delete --yes
```

Deletion removes Sesh's session directory and binding, not repository files. It is complete logical deletion, not forensic erasure of storage media, snapshots, or backups.

## Optional provider smoke tests

These checks validate installed provider CLIs and static integration assets without authentication, prompts, agent sessions, or model usage:

```bash
cargo test --test provider_smoke -- --ignored --exact claude_validates_the_materialized_sesh_plugin_without_opening_a_session
cargo test --test provider_smoke -- --ignored --exact codex_accepts_every_static_hook_overlay_without_opening_a_session
```
