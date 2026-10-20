# tokenix as an MCP server

Any client that speaks the Model Context Protocol over stdio can use tokenix:
Claude Desktop, Cursor, Zed, OpenCode, Antigravity and others.

```bash
tokenix mcp
```

## Register it in your client

Most clients take a JSON entry like this one. Use the absolute path to the binary
if `tokenix` is not on the client's PATH:

```json
{
  "mcpServers": {
    "tokenix": { "command": "tokenix", "args": ["mcp"] }
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

| Tool | What it answers |
|---|---|
| `tokenix_symbols` | Where a symbol is defined (`path:line`, never code) |
| `tokenix_graph` | Around one symbol: `direction` = `callers`, `callees`, `impact` (both ways, `depth` hops) or `flow` (forward from an entry point) |
| `tokenix_blast` | Symbols that depend on the working-tree diff since a git ref |
| `tokenix_modules` | Groups of symbols that mostly call each other, and how the groups depend on one another |
| `tokenix_deps` | File imports, or (`reverse`) which files import a file |
| `tokenix_tokenmap` | The directories and files holding the most tokens |

Graph tools take `format`: `text` (default), `mermaid`, `dot` or `json` (`blast`:
`text`, `mermaid`, `json`). Ask for `mermaid` when the answer is going into a PR
description or a doc.

They return **locations and relations, not code**, and do not search file
contents: the agent reads files with its own Read and searches them with its own
grep. There is no tool that reads files, runs commands or stores notes, because
the agent already has those and `tokenix hook` compresses what its Bash returns.
Every tool schema costs prompt tokens each turn; `tokenix prompt-audit
--profile-impact` prints the footprint of this set.

Each `tools/call` runs behind a panic guard, so one bad request cannot take the
server down. The index is refreshed for edited files before graph and read tools
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
