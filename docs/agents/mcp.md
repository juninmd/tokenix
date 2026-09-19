# tokenix as an MCP server

Any client that speaks the Model Context Protocol over stdio can use tokenix:
Claude Desktop, Cursor, Zed, OpenCode, Antigravity and others.

```bash
tokenix mcp                   # full profile (default)
tokenix mcp --profile slim    # three tools, smaller prompt footprint
```

## Register it in your client

Most clients take a JSON entry like this one. Use the absolute path to the binary
if `tokenix` is not on the client's PATH:

```json
{
  "mcpServers": {
    "tokenix": { "command": "tokenix", "args": ["mcp", "--profile", "slim"] }
  }
}
```

Some agents have a shortcut:

| Agent | Command | Writes |
|---|---|---|
| OpenCode | `tokenix install-hook --tool opencode` | `opencode.json` (repository), see the [OpenCode guide](opencode.md) |
| Antigravity CLI | `tokenix install-hook --tool mcp` | `~/.gemini/antigravity-cli/mcp_config.json` |

For every other client, add the entry to its MCP config by hand.

## Tools

| Profile | Tools |
|---|---|
| **full** | `tokenix_query`, `tokenix_context`, `tokenix_explore`, `tokenix_read`, `tokenix_symbols`, `tokenix_callers`, `tokenix_callees`, `tokenix_impact`, `tokenix_memory_add`, `tokenix_memory_list`, `tokenix_memory_remove`, `tokenix_memory_edit`, `tokenix_run`, `tokenix_gain` |
| **slim** | `tokenix_context`, `tokenix_search_tools`, `tokenix_call` (`tokenix_call` reaches any full-profile tool by name) |

Every tool schema costs prompt tokens on every turn. Use `slim` when the client
loads all MCP schemas up front, and `full` when it defers them.
`tokenix prompt-audit --profile-impact` measures the difference for your agents.

Each `tools/call` runs behind a panic guard, so one bad request cannot take the
server down. The index is refreshed for edited files before retrieval tools
answer, just like the CLI.

## Compress another MCP server's output

`tokenix mcp-proxy` wraps any stdio MCP server and compresses its `tools/call` text
results. It leaves tool schemas alone, so per-tool permissions keep working:

```json
{
  "mcpServers": {
    "github": { "command": "tokenix", "args": ["mcp-proxy", "--name", "github", "--", "github-mcp-server", "stdio"] }
  }
}
```

[← Back to the README](../../README.md#-agent-guides)
