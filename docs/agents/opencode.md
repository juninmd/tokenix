# tokenix with OpenCode (V1 & OpenCode 2)

> `tokenix install-hook --tool opencode` is part of the `agents-extra` build feature. Release binaries include it; with `cargo install` add `--features agents-extra` (or `--features full`).

OpenCode gets **both** automatic command/grep interception via a zero-dependency TypeScript plugin (`plugins/tokenix.ts`) **and** the structural MCP server (`mcp.servers.tokenix`). Both **OpenCode 2** (`setup` hooks + `mcp.servers`) and **OpenCode 1** (`server` hooks + legacy `mcp.*` migration) are supported.

## Install

```bash
tokenix index .                        # once per repository
tokenix install-hook --tool opencode   # global: $XDG_CONFIG_HOME/opencode (or ~/.config/opencode)
```

Or keep the setup inside a single repository instead of your user config:

```bash
tokenix install-binary                 # local opencode.json calls plain `tokenix`
tokenix install-hook --tool opencode --local
tokenix trust                          # allows `prompt-audit` / `session-audit` to inspect repo config
```

`--tool all` includes OpenCode in the selected scope (`global` by default, or `--local` when passed).

## What gets written

1. **Hook plugin** (`~/.config/opencode/plugins/tokenix.ts` globally, or `.opencode/plugins/tokenix.ts` with `--local`):
   - **OpenCode 2 (`setup`)**: wires `shell.hook("create.before")` (CLI 2.x shell commands) and `tool.hook("execute.before")` (Desktop / tool calls for shell & grep), skipping duplicate rewrites and failing open on any error or timeout.
   - **OpenCode 1 (`server`)**: wires `"tool.execute.before"` as a fallback for 1.x runtimes.
   - Zero external npm packages (`node:child_process` only); respects `TOKENIX_DISABLED=1` and `TOKENIX_RAW=1`.

2. **MCP server** (`mcp.servers.tokenix` in `opencode.json` or existing `opencode.jsonc`, preserving comments, trailing commas, and foreign servers; legacy `mcp.tokenix` is migrated automatically):

```json
{
  "mcp": {
    "servers": {
      "tokenix": {
        "type": "local",
        "command": ["tokenix", "mcp"]
      }
    }
  }
}
```

A repository's `opencode.json` / `opencode.jsonc` is a *repo-controlled input*: `prompt-audit` and `session-audit` will not spawn the servers it declares until you run `tokenix trust`. OpenCode itself decides what it runs.

## What it does

- **Automatic shell & grep savings:** filtered commands (`cargo test`, `git status`, `pytest`, `terraform`, …) are rewritten to `tokenix run` (keeping exit codes and error lines) and unbounded content `grep` calls get `head_limit` injected automatically. Clipped output stays recoverable via `tokenix retrieve <key> --lines N-M --grep PAT`. `Read` is never intercepted.
- **MCP structure tools:** OpenCode sees tokenix's six MCP tools — `tokenix_symbols`, `tokenix_graph` (callers · callees · impact · flow), `tokenix_blast`, `tokenix_modules`, `tokenix_deps`, and `tokenix_tokenmap`. The [MCP guide](mcp.md) has the full list.

A good habit is to tell OpenCode in your project's `AGENTS.md` to start structural exploration with `tokenix_symbols` / `tokenix_graph`, and use `tokenix read <file> --symbol <name>` via Bash when only one symbol body is needed.

## Check that it works

```bash
echo '{"tool":"bash","input":{"command":"terraform plan"}}' | tokenix hook
tokenix doctor
tokenix prompt-audit --agent opencode
tokenix                      # dashboard → Stats tab shows OpenCode (global / local)
```

## Remove

```bash
tokenix remove-hook --tool opencode           # removes global plugin + mcp.servers.tokenix
tokenix remove-hook --tool opencode --local   # removes repo-local .opencode/plugins/tokenix.ts + opencode.json entry
```

Only `tokenix.ts`, `mcp.servers.tokenix`, and any legacy `mcp.tokenix` entry are deleted; `servers` and `mcp` objects are dropped only when empty, leaving other MCP servers and settings untouched.

[← Back to the README](../../README.md#-agent-guides)

