# Provider integrations

Sesh V1 supports Claude Code and Codex as interchangeable clients of the same
local session. Provider adapters translate lifecycle details; they do not own
session history or narrative.

## Prerequisites

Install and authenticate the provider CLI using its official instructions. Sesh
does not manage provider credentials.

Confirm the executables you intend to use are available:

```bash
claude --version
codex --version
```

Then materialize and review Sesh's integration assets:

```bash
sesh setup claude
sesh setup codex
sesh doctor
```

Setup writes versioned, inspectable assets under Sesh's private state directory.
For provider trust steps it opens the provider interactively for review without
supplying a task prompt. It does not edit an application repository, replace
global user configuration, bypass managed policy, or silently grant trust.

## Launch behavior

Provider arguments after `--` are passed directly as an argument vector, never
through a shell:

```bash
sesh run claude -- --model sonnet
sesh switch codex -- --model gpt-5
```

The adapter launches with the caller's terminal and a verified cwd. It receives
private paths through environment variables, including `SESH_HOOK_BIN` and the
current run inbox. A switch or fork also receives Sesh's fixed bootstrap message
and a deterministic handoff. Do not provide a second positional task prompt on
those commands.

The handoff includes the last explicit narrative checkpoint, current verified
Git facts, recent activity and failures, provider transition, fork lineage when
present, omissions, and the expected next action. It is generated from committed
Sesh state rather than a provider transcript.

`sesh handoff <provider> [--json]` renders this same content without
switching: no event is appended, no checkpoint is committed, and no
provider launches. It applies the same fail-closed snapshot verification as
`switch`, so a missing or stale narrative checkpoint is visible before a
switch is spent.

## Checkpoints

A checkpoint is the portable narrative that observed activity cannot supply.
Providers should write one after a meaningful unit of work and before a likely
handoff. Humans can create the same checkpoint directly.

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
sesh checkpoint --format json < checkpoint.json
```

With terminal stdin, `sesh checkpoint` opens a private temporary JSON template
using `$VISUAL`, then `$EDITOR`. The temporary file is removed after parsing.

An attached provider must submit through its run-scoped inbox:

```bash
printf '%s' "$CHECKPOINT_JSON" \
  | "$SESH_HOOK_BIN" checkpoint --format json --from-provider
```

Sesh requires a nonempty objective and summary, at least one next step, bounded
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

Claude assets are stored as a versioned plugin manifest and hook definitions.
Codex assets are stored as versioned configuration overlays. `sesh doctor`
checks that materialized assets still match the Sesh version.

## Optional smoke tests

The normal test suite uses deterministic local provider fixtures and never logs
in, opens an agent session, supplies a prompt, or spends model quota.

Two ignored tests can validate the installed CLIs and static integration assets:

```bash
cargo test --test provider_smoke -- --ignored --exact \
  claude_validates_the_materialized_sesh_plugin_without_opening_a_session

cargo test --test provider_smoke -- --ignored --exact \
  codex_accepts_every_static_hook_overlay_without_opening_a_session
```

The Claude test runs plugin validation against a temporary materialization. The
Codex test asks strict configuration parsing to list features with each static
overlay. Neither test starts a model conversation.

Provider releases can change their CLI or hook contracts. When an optional smoke
test fails, inspect the installed provider version and adapter assets before
changing Sesh's provider-neutral session model.

## Adding a provider

A new adapter must be able to probe the executable, prepare a launch without
replacing user configuration, normalize supported hooks, and inject the current
Sesh protocol and handoff. It must preserve the same storage, Git, checkpoint,
lease, and handoff contracts and must be testable without calling a real model.
