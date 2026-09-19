# tokenix with Antigravity

Antigravity loads tokenix as a native plugin. Its hooks speak Antigravity's own
dialect: a `toolCall` payload in, and `decision: allow | deny` out instead of exit
codes.

## Install

The installer drives the Antigravity CLI, so `agy` must be on your PATH.

```bash
tokenix index .                                      # once per repository
tokenix install-hook --tool antigravity              # user-level plugin
tokenix install-hook --tool antigravity --local      # this workspace only
```

| Scope | What happens |
|---|---|
| Global (default) | Stages the plugin, runs `agy plugin install`, validates it at `~/.gemini/config/plugins/tokenix/`, and removes the legacy `~/.gemini/antigravity-cli/plugins/tokenix` |
| `--local` | Writes `.agents/plugins/tokenix/` in the workspace and runs `agy plugin validate` |

## What gets written

`plugin.json` (`{"name": "tokenix"}`) and `hooks.json`:

```json
{
  "tokenix-hooks": {
    "PreToolUse": [{
      "matcher": "^(read_file|view_file|grep_search|run_command|run_in_terminal)$",
      "hooks": [{ "type": "command", "command": "\"/abs/path/to/tokenix\" hook-antigravity", "timeout": 10 }]
    }]
  }
}
```

`hook-antigravity` is the same decision engine as `tokenix hook`, with
Antigravity's input and output format.

## What it does

| Antigravity tool | Treated as | Result |
|---|---|---|
| `read_file`, `view_file` | Read | `{"decision":"deny","reason":"<outline>"}` for large code files |
| `grep_search` | Grep | Symbol lookup or semantic results |
| `run_command`, `run_in_terminal` | Bash | Rewritten through `overwrite: {name, args}` to run via `tokenix run` |
| Anything else, or no/stale index | n/a | `{"decision":"allow"}` |

## Check that it works

```bash
echo '{"toolCall":{"name":"view_file","args":{"path":"src/big_file.rs"}}}' | tokenix hook-antigravity
# → {"decision":"deny","reason":"[src/big_file.rs] - N lines, M symbols ..."}

tokenix gain                 # after a session
tokenix                      # dashboard → Stats tab shows Antigravity (global/local)
```

## MCP instead of (or besides) the plugin

`tokenix install-hook --tool mcp` registers the tokenix MCP server in
`~/.gemini/antigravity-cli/mcp_config.json`, independently of the plugin. The
[MCP guide](mcp.md) lists the tools it exposes.

## Remove

```bash
tokenix remove-hook --tool antigravity            # agy plugin uninstall tokenix
tokenix remove-hook --tool antigravity --local    # deletes .agents/plugins/tokenix/
```

[← Back to the README](../../README.md#-agent-guides)
