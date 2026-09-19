# tokenix with OpenCode

OpenCode uses tokenix as an **MCP server**. No hook is installed, so nothing is
intercepted automatically. OpenCode gets tokenix tools and calls them when it
decides to.

## Install

```bash
tokenix index .                        # once per repository
tokenix install-binary                 # opencode.json calls plain `tokenix`, so it must be on PATH
tokenix install-hook --tool opencode   # run in the repository root
```

`--tool all` skips OpenCode on purpose: the registration lives in the repository,
so you add it only where you want it.

## What gets written

An `mcp.tokenix` entry in `opencode.json` at the repository root. Other MCP servers
in the file are kept:

```json
{
  "mcp": {
    "tokenix": { "type": "local", "command": ["tokenix", "mcp"] }
  }
}
```

A repository's `opencode.json` is a *repo-controlled input*: tokenix's own audits
(`prompt-audit`, `session-audit`) will not spawn the servers it declares until you
run `tokenix trust`. OpenCode itself decides what it runs.

## What it does

OpenCode sees the tokenix MCP tools: context, semantic search, smart read,
symbols, callers/callees, impact, memory and run. The [MCP guide](mcp.md) has the
full list and the `slim` profile.

A good habit is to tell OpenCode, in your project instructions, to start a task
with `tokenix_context` and to read large files through `tokenix_read`.

## Check that it works

In OpenCode, ask it to "list the tokenix MCP tools", or check its MCP status view.
From a terminal:

```bash
tokenix mcp --help
tokenix                      # dashboard → Stats tab shows OpenCode (local)
```

## Remove

```bash
tokenix remove-hook --tool opencode
```

Only `mcp.tokenix` is deleted. The `mcp` object is removed only if nothing else is
left in it.

[← Back to the README](../../README.md#-agent-guides)
