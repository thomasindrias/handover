# MCP server

`sesh mcp-server` exposes three read-only tools over the Model Context
Protocol's stdio transport, so a provider attached to a Sesh session (Claude
Code, Codex, or any MCP-capable client) can query Sesh directly instead of a
human running commands in a second terminal.

## Tools

- **`list`** — no arguments. Returns exactly what `sesh list --json` returns:
  every local session across every repository.
- **`handoff`** — `{"provider": "claude" | "codex"}`. Returns exactly what
  `sesh handoff <provider> --json` returns: the rendered handoff markdown plus
  narrative checkpoint and capture-gap metadata, without switching.
- **`status`** — no arguments. Returns exactly what `sesh status --json`
  returns, including a `switch_readiness` block with a
  `suggested_switch_command` string — the exact command to run to actually
  switch.

There is no `switch` tool. Switching takes over the calling terminal and
blocks until the new provider exits, and the current session's own run lease
is always live while that session is asking — a tool call from inside that
session would always refuse. `status`'s `suggested_switch_command` gives the
calling agent the exact next command to hand to the human instead.

A domain-level failure (for example, no Sesh session bound to the directory
the server was started in) comes back as a normal tool result with
`isError: true` and the same diagnostic text the CLI would print — never a
JSON-RPC protocol error. A JSON-RPC error is reserved for malformed requests
or a method/tool name that was never advertised in `tools/list`.

## Configuring a client

Point your MCP client at the `sesh` binary with the `mcp-server` subcommand
and no arguments. For Claude Code, in `.mcp.json`:

```json
{
  "mcpServers": {
    "sesh": {
      "command": "sesh",
      "args": ["mcp-server"]
    }
  }
}
```

For Codex, in `~/.codex/config.toml`:

```toml
[mcp_servers.sesh]
command = "sesh"
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
