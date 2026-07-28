# Provider integrations

Handover V1 supports Claude Code and Codex as interchangeable clients of the same
local session. Provider adapters translate lifecycle details; they do not own
session history or narrative.

## Prerequisites

Install and authenticate the provider CLI using its official instructions. Handover
does not manage provider credentials.

Confirm the executables you intend to use are available:

```bash
claude --version
codex --version
```

Then materialize and review Handover's integration assets:

```bash
handover setup claude
handover setup codex
handover doctor
```

Setup writes versioned, inspectable assets under Handover's private state directory.
For provider trust steps it opens the provider interactively for review without
supplying a task prompt. It does not edit an application repository, replace
global user configuration, bypass managed policy, or silently grant trust.

## Launch behavior

Provider arguments after `--` are passed directly as an argument vector, never
through a shell:

```bash
handover run claude -- --model sonnet
handover switch codex -- --model gpt-5
```

The adapter launches with the caller's terminal and a verified cwd. It receives
private paths through environment variables, including `HANDOVER_HOOK_BIN` and the
current run inbox. A switch or fork also receives Handover's fixed bootstrap message
and a deterministic handover. Do not provide a second positional task prompt on
those commands.

The handover includes the last explicit narrative checkpoint, current verified
Git facts, recent activity and failures, provider transition, fork lineage when
present, omissions, and the expected next action. It is generated from committed
Handover state rather than a provider transcript.

`handover preview <provider> [--json]` renders this same content without
switching: no event is appended, no checkpoint is committed, and no
provider launches. It applies the same fail-closed snapshot verification as
`switch`, so a missing or stale narrative checkpoint is visible before a
switch is spent.

## Checkpoints

A checkpoint is the portable narrative that observed activity cannot supply.
Providers should write one after a meaningful unit of work and before a likely
handover. Humans can create the same checkpoint directly.

The JSON shape is:

```json
{
  "objective": "Implement the OAuth callback",
  "summary": "Callback and PKCE are implemented; one test still fails",
  "decisions": [
    {
      "statement": "Keep the verifier in the session cookie",
      "reason": "Avoid server-side state"
    }
  ],
  "assumptions": [],
  "constraints": ["Do not change the public callback route"],
  "completed": ["OAuth callback", "PKCE support"],
  "in_progress": ["Integration test repair"],
  "blockers": ["Callback integration test fails on state validation"],
  "next_steps": ["Reproduce and fix the state validation failure"],
  "related_event_sequences": []
}
```

For a human-authored checkpoint:

```bash
handover checkpoint --format json < checkpoint.json
```

With terminal stdin, `handover checkpoint` opens a private temporary JSON template
using `$VISUAL`, then `$EDITOR`. The temporary file is removed after parsing.

An attached provider must submit through its run-scoped inbox:

```bash
printf '%s' "$CHECKPOINT_JSON" \
  | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
```

An attached provider does not have to be told this command. Every handover ends
with it, so any provider that read its handover already has it, and the Claude
integration also installs a `/handover-checkpoint` command that gathers the
narrative and submits it. Submitting only stages the checkpoint in the run
inbox; the next hook of that run promotes it into the journal, which is when
`checkpoint.created` appears in `handover log`.

Handover requires a nonempty objective and summary, at least one next step, bounded
fields and item counts, and sorted unique event references that already exist.
Unknown fields and oversized payloads are rejected.

Provider processes cannot use the human mutation path. A provider checkpoint is
accepted only when its session ID, run ID, and private inbox path match the
active run. Those values are inherited by launched provider descendants, but
this does not prove process ancestry or create a same-user authorization
boundary. Inbox input remains untrusted until validation and canonical journal
promotion complete.

## Hooks

Adapters normalize documented lifecycle hooks into provider-neutral events:

- session start and handshake;
- user prompt submission;
- tool request, completion, and failure;
- command metadata and bounded output references;
- provider stop.

Hooks are best-effort observations, not authorization. Unknown provider payloads
or fields do not become canonical data automatically. Hook input is size-bounded,
sensitive environment values are not copied, and canonical writes still pass
through session locks and schema validation.

On `Stop`, Handover may reply with a single `systemMessage` warning when 20 or
more events have been observed since the latest narrative checkpoint. The
shape follows Claude Code's documented Stop-hook output contract, which
validates Stop output strictly; the warning never blocks the stop and a
failure to compute it degrades to empty output. Codex documents the same
output fields; hook delivery to Codex is now confirmed working end to end
(see below), so the nudge is exercised the same way on both providers.
Verified against a real Claude Code session: the hook contract holds
(payloads normalize, the Stop response is accepted without error), but
`systemMessage` is a UI-layer notice shown only in an interactive session —
headless invocations (`claude -p`, any `--output-format`) never surface it,
by design.

Claude assets are stored as a versioned plugin manifest and hook
definitions, loaded per session via `--plugin-dir`. Codex assets are stored
as a versioned `hooks.json`; each launch gives the child process a private,
per-run `CODEX_HOME` containing that file plus best-effort symlinks to the
user's real `config.toml`/`auth.json`, so login and preferences carry over
without Handover ever writing to the user's actual `~/.codex` or the target
repository. `handover doctor` checks that materialized assets still match the
Handover version.

## Optional smoke tests

The normal test suite uses deterministic local provider fixtures and never logs
in, opens an agent session, supplies a prompt, or spends model quota.

One ignored test can validate the installed Claude CLI against a static
integration asset without starting a model conversation:

```bash
cargo test --test provider_smoke -- --ignored --exact \
  claude_validates_the_materialized_handover_plugin_without_opening_a_session
```

It runs plugin validation against a temporary materialization. Codex has no
equivalent: no installed-CLI command inspects `hooks.json` without starting
a real session (`codex doctor` was tried and confirmed to ignore the file
entirely), so `tests/provider_smoke.rs` documents that gap in a comment
rather than asserting a check that would only test authentication.
`CodexAdapter`'s own unit tests still cover the materialized file's shape
and content without a real CLI; confirming Codex actually reads and fires
the hooks requires a real, manually run session.

Provider releases can change their CLI or hook contracts. When an optional smoke
test fails, inspect the installed provider version and adapter assets before
changing Handover's provider-neutral session model.

## Adding a provider

A new adapter must be able to probe the executable, prepare a launch without
replacing user configuration, normalize supported hooks, and inject the current
Handover protocol and handover. It must preserve the same storage, Git, checkpoint,
lease, and handover contracts and must be testable without calling a real model.
