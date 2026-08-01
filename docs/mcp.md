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

`arm` and `claim` are scoped to the active run: the calling process must supply
the same session id, run id, and private inbox path that provider checkpoint
submission requires, and the session resolved from the working directory must be
the one that run is attached to. `attach` cannot be scoped that way — by
definition no run exists yet — so it is scoped to the worktree its working
directory resolves to. Neither is an authorization boundary; see
`docs/architecture.md`.

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
