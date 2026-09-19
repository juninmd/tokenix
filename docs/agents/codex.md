# tokenix with OpenAI Codex CLI

Codex gets three things: instructions that point it at tokenix, two shell helpers,
and a `PreToolUse` hook for terminal commands and `grep_search`.

## Install

```bash
tokenix index .                     # once per repository
tokenix install-hook --tool codex   # always user-level, under ~/.codex/
```

Then load the helpers in your shell profile:

```bash
echo 'source ~/.codex/tokenix-init.sh' >> ~/.bashrc     # bash / zsh
```

```powershell
Add-Content $PROFILE '. ~/.codex/tokenix-init.ps1'      # PowerShell
```

## What gets written

| File | Purpose |
|---|---|
| `~/.codex/instructions.md` | A block between `<!-- tokenix -->` markers asking Codex to prefer `tokenix query` / `tokenix read`. Your own text outside the markers is kept |
| `~/.codex/tokenix-init.sh` · `tokenix-init.ps1` | `tx-read <file>` and `tx-query "<text>"`, thin wrappers over `tokenix read` / `tokenix query` |
| `~/.codex/hooks.json` | `PreToolUse` hook, matcher `^(Bash\|run_in_terminal\|grep_search)$` |
| `~/.codex/tokenix-codex-hook.ps1` | **Windows only.** A wrapper the hook calls through `powershell -NoProfile -ExecutionPolicy Bypass`, which forwards the payload to `tokenix hook` |

On Linux and macOS the hook calls `"<abs path>/tokenix" hook` directly.

## What it does

| Codex calls | tokenix answers |
|---|---|
| A shell command that matches a filter | Rewritten to `tokenix run '<cmd>'` (`updatedInput`), same exit code |
| `git status` | Rewritten to `git status --short` |
| `grep_search` | Same path as Claude's `Grep`: symbol lookup, semantic search or a `head_limit` cap |
| Anything else, or no/stale index | Passes through |

Codex's matcher does not include file reads. For big files, the instructions and
`tx-read` steer Codex towards outlines instead.

## Check that it works

```bash
echo '{"tool_name":"Bash","tool_input":{"command":"git status"}}' | tokenix hook
# → {"hookSpecificOutput":{..."updatedInput":{..."command":"git status --short"...}}}

tx-read src/big_file.rs      # outline of a large file
tokenix gain                 # after a session
tokenix                      # dashboard → Stats tab shows Codex status
```

## Remove

```bash
tokenix remove-hook --tool codex
```

This strips the instructions block, deletes the helper files and removes only the
`tokenix` entries from `hooks.json`. Delete the `source` line from your shell
profile yourself.

[← Back to the README](../../README.md#-agent-guides)
