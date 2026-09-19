# tokenix with Claude Code

Claude Code has the deepest integration. A native `PreToolUse` hook sees every
`Read`, `Grep`, `Bash` and `PowerShell` call before it runs, with no prompt changes.

## Install

```bash
tokenix index .                                  # once per repository
tokenix install-hook --tool claude-code          # all projects: ~/.claude/settings.json
tokenix install-hook --tool claude-code --local  # this project only: .claude/settings.local.json
```

Restart Claude Code so it loads the new hook.

## What gets written

One entry in `hooks.PreToolUse`. Other hooks in the file are left alone:

```json
{
  "matcher": "^(Read|Grep|Bash|PowerShell|grep_search|run_in_terminal)$",
  "hooks": [{ "type": "command", "command": "\"/abs/path/to/tokenix\" hook", "timeout": 10 }]
}
```

The command is the absolute path of the binary you ran `install-hook` with. If you
move the binary, run `install-hook` again.

## What it does

| Claude calls | tokenix answers |
|---|---|
| `Read` of a code file ≥ 200 lines, no `offset`/`limit` | Symbol outline instead of the file (exit `2`, Claude reads the outline from stderr), only when it saves ≥ 30% |
| `Read` with a range, or a small file | Passes through |
| `Grep` for an identifier | Its definition from the symbol graph |
| `Grep` for a 3+ word phrase | Semantic search results |
| `Grep` with unbounded `content` output | Adds `head_limit` (default 100) through `updatedInput` |
| `Bash` matching a filter | Rewritten to `tokenix run '<cmd>'` through `updatedInput`, same exit code |
| `PowerShell` matching a filter | Rewritten to `& 'tokenix' run --shell pwsh '<cmd>'` |
| Anything else, or no/stale index | Passes through (exit `0`) |

Claude then asks for what it needs, typically `Read` with `offset`/`limit` or
`tokenix read <file> --symbol <name>`.

> The installer wires `PreToolUse` only. `tokenix hook-post` (PostToolUse
> redaction and compression) is not installed for Claude Code.

## Check that it works

```bash
echo '{"tool_name":"Read","tool_input":{"file_path":"src/big_file.rs"}}' | tokenix hook; echo "exit=$?"
# exit=2 and an outline on stderr → interception works (use a ≥200-line code file)

tokenix gain              # after a session: calls, intercepts, tokens removed
tokenix gain --history    # recent hook decisions
tokenix                   # dashboard → Stats tab shows "Claude Code: installed"
```

## Day-to-day tips

- `TOKENIX_DISABLED=1 <command>` lets one command run without the hook.
- A filtered command whose output was clipped ends with a
  `tokenix retrieve <key>` marker, and that command returns the full output.
- Add a line to your `CLAUDE.md` if you want Claude to reach for
  `tokenix context "<task>"` at the start of a task. The hook itself needs no
  instructions.

## Remove

```bash
tokenix remove-hook --tool claude-code          # add --local for the project file
```

Only entries that mention `tokenix` are removed from `PreToolUse`/`PostToolUse`.

[← Back to the README](../../README.md#-agent-guides)
