# tokenix with GitHub Copilot

Copilot is the one agent that gets **both** hooks: `PreToolUse` intercepts reads,
greps and commands, and `PostToolUse` (`tokenix hook-post`) redacts secrets and
compresses tool output before the model sees it.

## Install

```bash
tokenix index .                                # once per repository
tokenix install-binary                         # the hooks call plain `tokenix`, so it must be on PATH

tokenix install-hook --tool copilot            # you, in every repo: ~/.copilot/hooks/tokenix.json
tokenix install-hook --tool copilot --local    # this repo, for the whole team: .github/
```

With `--local`, commit the result so every contributor gets it:

```bash
git add .github/copilot-instructions.md .github/hooks/hooks.json
git commit -m "chore: wire tokenix into Copilot"
```

## What gets written

| Scope | Files |
|---|---|
| Global (default) | `~/.copilot/hooks/tokenix.json` |
| `--local` | `.github/hooks/hooks.json` and `.github/copilot-instructions.md`, which tells Copilot to prefer `tokenix query` / `tokenix read` |

Both hook files have the same shape:

```json
{
  "hooks": {
    "PreToolUse":  [{ "type": "command", "command": "tokenix hook",      "windows": "tokenix hook",      "timeout": 10 }],
    "PostToolUse": [{ "type": "command", "command": "tokenix hook-post", "windows": "tokenix hook-post", "timeout": 10 }]
  }
}
```

The command is bare `tokenix` on purpose: a committed file has to work on every
clone, whatever path the binary lives at.

## What it does

Copilot sends `toolName` / `toolArgs`, and `toolArgs` can be a JSON-encoded string.
tokenix maps it onto the same decisions Claude Code gets:

| Copilot tool | Treated as | Result |
|---|---|---|
| `view`, `read` (`path`/`file` argument) | Read | Outline for large code files, pass-through otherwise |
| `grep`, `grep_search` (`query`/`regex`/`search`) | Grep | Symbol lookup, semantic results or a `head_limit` cap |
| Terminal commands | Bash | Filtered through `tokenix run` |
| Any tool result (PostToolUse) | n/a | Known secrets redacted, noisy output compressed |

## Check that it works

```bash
echo '{"toolName":"view","toolArgs":"{\"path\":\"src/big_file.rs\"}"}' | tokenix hook; echo "exit=$?"
# exit=2 and an outline on stderr

tokenix gain                 # after a session
tokenix                      # dashboard → Stats tab shows Copilot (global/local)
```

## Remove

```bash
tokenix remove-hook --tool copilot            # deletes ~/.copilot/hooks/tokenix.json
tokenix remove-hook --tool copilot --local    # deletes the two .github/ files
```

tokenix owns these files, so it deletes them whole. If you added your own hooks to
`.github/hooks/hooks.json`, copy them somewhere safe first.

[← Back to the README](../../README.md#-agent-guides)
