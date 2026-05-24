# Preference Memory

tokenix should remember durable user and project preferences that are repeatedly useful for coding agents. The goal is not to store chat history. The goal is to persist compact, auditable guidance extracted from user intent.

## Levels

### Global

Global preferences apply across repositories and should live outside the project checkout:

```text
~/.tokenix/memory/preferences.md
```

Examples:

- Prefer focused validation before repo-wide checks when unrelated failures exist.
- Prefer Biome over ESLint when starting new JavaScript or TypeScript lint work.
- Keep credentials in `.env` and never write secrets into committed docs.

### Project

Project preferences apply only to the current repository and should live next to the project index metadata:

```text
~/.tokenix/<project-id>.preferences.md
```

Examples:

- This project is migrating from ESLint to Biome.
- Use `cargo check` and focused graph tests before full benchmarks.
- Keep tokenix indexing low CPU on Windows unless the user opts in to faster settings.

## Markdown Format

Use append-friendly Markdown with stable sections:

```markdown
# tokenix Preference Memory

## Global Preferences

<!-- tokenix:global -->

- [2026-05-24] Prefer Biome over ESLint for JS/TS linting migrations.

## Project Preferences

<!-- tokenix:project id=4f5df81d600d5d3b path="D:/Solutions/pessoal/tokenix" -->

- [2026-05-24] Keep `tokenix index` CPU usage conservative by default on Windows.
```

## Extraction Rules

A preference candidate should be saved only when the user expresses a durable preference, policy, migration decision, or repeated workflow rule.

Good candidates:

- "Migre de ESLint para Biome."
- "Sempre use low CPU quando indexar esse repo."
- "Nesse projeto, valida primeiro com cargo check."
- "Globalmente prefiro pnpm em vez de npm."

Bad candidates:

- One-off bug descriptions.
- Temporary commands.
- Secrets, tokens, credentials, or private URLs.
- Guesses not confirmed by user wording.

## CLI

```bash
tokenix memory add --global "Prefer Biome over ESLint for JS/TS linting."
tokenix memory add "This project is migrating from ESLint to Biome."
tokenix memory list
tokenix memory list --global
tokenix memory list --project
```

`memory add` defaults to project scope. Use `--global` for cross-repository preferences.

## MCP Tools

- `tokenix_memory_add`: save one preference with `scope` set to `project` or `global`.
- `tokenix_memory_list`: list `all`, `project`, or `global` preferences.

## Hook/Agent Flow

1. Agent sees a durable preference in user input.
2. `tokenix context`, `tokenix explore`, and MCP equivalents remind the agent to capture durable preferences.
3. Agent records a short normalized statement with `tokenix_memory_add` when MCP is available.
4. tokenix appends it to the correct Markdown section.
5. Future context includes saved preferences plus the capture rule.

## Privacy and Safety

- Never auto-save secrets or credentials.
- Prefer explicit user intent over inference.
- Keep entries short and editable.
- Use Markdown so users can audit and delete entries without special tooling.

## Roadmap

1. Add optional heuristic extraction suggestions from user prompts.
2. Add richer preference editing commands.
3. Add preference search/ranking once the file grows beyond a small list.
