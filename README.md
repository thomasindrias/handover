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

Use `fork` only when you intentionally want a separate line of work:

```bash
sesh run claude
sesh switch codex
sesh fork claude --branch sesh/oauth-experiment --worktree ../oauth-experiment
```

`switch` continues the existing session in its existing worktree and saved cwd. `fork` creates a new branch, a separate Git worktree, and a child Sesh session with explicit parent lineage. Without overrides, the branch is `sesh/<repository-name>-<operation-short-id>` and the sibling worktree is `<repository>-sesh-<operation-short-id>`.

Forking duplicates tracked index state, staged and unstaged changes, untracked regular files and symlinks, binary content, executable bits, deletions, and renames. Ignored files are intentionally excluded. V1 refuses sparse checkout, unmerged entries, intent-to-add entries, staged gitlinks, dirty submodules, unsupported special files, conflicting branches, and existing or nested target paths. Clean initialized submodules are reconstructed only from proven local Git objects; Sesh never fetches during a fork.

The source is observational: Sesh does not commit, stash, reset, clean, checkout, or secretly rewrite source-worktree files or index state. It adds only the requested Git branch/worktree registration in the repository's shared Git metadata.

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

`status` combines verified session history with a fresh Git observation. `log --json` emits the original checksummed event envelopes. `inspect` reports paths, permissions, checkpoint files, blob hashes and sizes, and lease state without printing blob contents. `doctor` is observational unless `--repair` is supplied. It reports interrupted forks with their operation ID, phase, source session, target, branch, and a shell-escaped Git inspection command. Repair can finish a byte-identical child binding after lineage has committed; it never deletes a crash-left worktree or branch.

## Local data and security

Sesh stores private plaintext under `$SESH_HOME`, `$XDG_STATE_HOME/sesh`, or `~/.local/state/sesh`. Session data can contain prompts, decisions, stack traces, business context, and secrets accidentally supplied to a provider. Protect backups and machine access accordingly.

Fork operation records, binary patches, untracked manifests and content-addressed blobs are also private plaintext under `$SESH_HOME/operations/<operation-id>`. The durable phases are `prepared`, `artifacts_captured`, `worktree_created`, `staged_applied`, `unstaged_applied`, `untracked_copied`, `verified`, `child_staged`, `lineage_committed`, `child_bound`, `run_leased`, and `complete`, with `rolled_back` and `needs_manual_recovery` terminal outcomes. Before the verified parent `session.forked` event, synchronous cleanup requires an exact recorded or live-only mutation proof. That parent event is the commit point: recovery is forward-only afterward.

Mode `0700` directories and `0600` files prevent accidental sharing, but an unrestricted provider shell running as your Unix user is not an operating-system security boundary. Same-user processes can access anything that user can access.

Sesh does not parse provider transcripts, use embeddings or semantic retrieval, require cloud services, commit session state to Git, or write `.sesh`, `.claude`, or `.codex` state into the application worktree.

To remove the complete local session and its worktree binding:

```bash
sesh delete
# or, for automation:
sesh delete --yes
```

Deletion removes Sesh's session directory and binding, not repository files. A parent with child sessions must be deleted child-first. Deleting a child removes its inherited handoffs; deleting the parent then removes terminal fork operation patches and blobs that name it. Nonterminal fork operations block deletion. Application worktrees and Git branches are always left untouched. This is complete logical deletion, not forensic erasure of storage media, snapshots, or backups.

## Optional provider smoke tests

These checks validate installed provider CLIs and static integration assets without authentication, prompts, agent sessions, or model usage:

```bash
cargo test --test provider_smoke -- --ignored --exact claude_validates_the_materialized_sesh_plugin_without_opening_a_session
cargo test --test provider_smoke -- --ignored --exact codex_accepts_every_static_hook_overlay_without_opening_a_session
```
