# MCP server

`handover mcp-server` exposes six tools over the Model Context Protocol's stdio
transport, so a provider attached to a Handover session (Claude Code, Codex, or
any MCP-capable client) can query and drive Handover directly instead of a human
running commands in a second terminal.

## Read tools

- **`list`** — no arguments. Returns exactly what `handover list --json` returns:
  every local session across every repository.
- **`preview`** — `{"provider": "claude" | "codex"}`. Returns exactly what
  `handover preview <provider> --json` returns: the rendered handover markdown plus
  narrative checkpoint and capture-gap metadata, without switching.
- **`status`** — no arguments. Returns exactly what `handover status --json`
  returns, including a `switch_readiness` block with a
  `suggested_switch_command` string.

## Write tools

- **`arm`** — `{"provider": "claude" | "codex", "surface"?: "auto" | "cli" |
  "desktop", "ttl"?: "15m"}`. Records a pending switch without launching
  anything, exactly as `handover arm` does. The switch completes when the
  session's provider exits.
- **`claim`** — `{"arm"?: <sequence>}`. Consumes the pending arm, commits the
  transition checkpoint, and returns the handover. Refuses while a provider
  still holds the run lease, so it succeeds only once the outgoing session has
  actually ended.
- **`attach`** — `{"provider": "claude" | "codex"}`. Binds the current worktree
  to a session for a provider Handover did not launch, resolving to the existing
  session when one exists.

`arm` and `claim` scope themselves to whichever the caller can prove. A caller
carrying `HANDOVER_RUN_ID` — which every provider Handover launches does — takes
the run-scoped path, and three things must hold. The calling process must supply
the same session id, run id, and private inbox path that provider checkpoint
submission requires; the session resolved from the working directory must be the
one that run is attached to; and that run must still hold the session's lease.
The third gate is why a stale run environment is not enough: a run whose lease is
gone is finished, and a finished run may not arm a switch the user never asked
for.

A caller with no run environment takes the worktree-scoped path instead, which is
what the plain CLI `handover arm` and `handover claim` already do for any process
in that worktree, and what `attach` has always done here — by definition no run
exists for a session Handover did not launch. This is what lets a desktop
application Handover opened arm its way back out over MCP rather than needing a
human at a terminal to end the leg. It grants nothing a process in that worktree
did not already have, and neither path is an authorization boundary; see
`docs/architecture.md`.

`claim`'s "refuses while a provider still holds the run lease" behavior above
can only trigger for a session that has a lease at all, and only a session
`handover run` or `handover switch` created ever gets one. `handover attach`
(`docs/architecture.md`, `docs/providers.md`) adopts a session Handover never
launched, so such a session never holds a lease — a pending arm on it has
nothing for that refusal to find. An attached session's own process has no run
credentials — Handover injects nothing into a provider it did not launch, which
is the same reason it cannot submit a checkpoint through the run-scoped inbox
(`docs/providers.md`) — so it reaches these tools by the worktree-scoped path,
exactly as a plain `handover claim` typed in that worktree would.

There is no `switch` tool. Switching takes over the calling terminal and blocks
until the new provider exits, and the calling session's own run lease is always
live while it is asking. `arm` is the in-session form of the same intent: it
records where to go, and the supervisor completes the move when the provider
exits.

A domain-level failure (for example, no Handover session bound to the directory
the server was started in) comes back as a normal tool result with
`isError: true` and the same diagnostic text the CLI would print — never a
JSON-RPC protocol error. A JSON-RPC error is reserved for malformed requests
or a method/tool name that was never advertised in `tools/list`.

## Configuring a client

Point your MCP client at the `handover` binary with the `mcp-server` subcommand
and no arguments. For Claude Code, in `.mcp.json`:

```json
{
  "mcpServers": {
    "handover": {
      "command": "handover",
      "args": ["mcp-server"]
    }
  }
}
```

For Codex, in `~/.codex/config.toml`:

```toml
[mcp_servers.handover]
command = "handover"
args = ["mcp-server"]
```

Verify the exact configuration key names against each provider's current MCP
documentation — both have evolved this surface before and may again.

## Non-goals

Local only: stdio transport, no network listener, no remote or hosted
reachability. Cloud/hosted reachability for other AI providers "online" is a
deliberate, separate later redesign — see the roadmap's "Future Direction —
Programmable Control Surface" note — not an incremental extension of this
server.
