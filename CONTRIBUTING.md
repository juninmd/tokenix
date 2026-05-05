# Contributing to tokenix

Thanks for your interest in contributing! tokenix is a Rust CLI, and contributions of all sizes are welcome — from fixing a typo to adding support for a new language.

## Ways to contribute

- **Report a bug** — open an [issue](https://github.com/juninmd/tokenix/issues) with steps to reproduce
- **Request a feature** — open an issue describing the use case and expected behavior
- **Add a language** — see [Adding a language](#adding-a-language)
- **Improve docs** — typos, clarity, examples
- **Fix a bug** — see [Development setup](#development-setup) then open a PR

## Development setup

**Requirements:**
- [Rust](https://www.rust-lang.org/tools/install) `>= 1.75`
- [Ollama](https://ollama.com/download) running locally
- `nomic-embed-text` model pulled: `ollama pull nomic-embed-text`

```bash
git clone https://github.com/juninmd/tokenix
cd tokenix
cargo build
```

Run the full test suite:

```bash
cargo test
```

Test the hook manually:

```bash
tokenix index .

# Should intercept (exit 2) — large file
echo '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' | cargo run -- hook; echo $?

# Should pass through (exit 0) — small file
echo '{"tool_name":"Read","tool_input":{"file_path":"Cargo.toml"}}' | cargo run -- hook; echo $?
```

## Project structure

| File | Purpose |
|---|---|
| `src/main.rs` | CLI commands (clap). All `cmd_*` functions live here. |
| `src/chunker.rs` | AST chunking per language + `generate_outline()`. Token counting. |
| `src/embed.rs` | Ollama HTTP — `get_embedding()`, `check_ollama()` |
| `src/store.rs` | SQLite schema, CRUD, cosine similarity search, hook log I/O |
| `src/indexer.rs` | File walk + incremental index pipeline |
| `src/query.rs` | Search + result formatting |
| `src/hook.rs` | `run_hook()` — called by Claude Code's PreToolUse hook |
| `src/gain.rs` | Analytics from `.tokenix/hook.log` |

## Adding a language

1. Add the file extension(s) to `INDEXED_EXTS` in `src/chunker.rs`
2. Add a variant to the `Lang` enum
3. Add a match arm in `detect_lang()`
4. Implement `chunk_<lang>()` following the pattern of `chunk_rust()`
5. Register it in the `chunk_file()` dispatcher

Each `chunk_*` function returns a `Vec<Chunk>`. A chunk has a `symbol`, `kind`, `start_line`, `end_line`, and `content`. Symbol-aware chunkers split on function/class/type boundaries; generic chunkers use 400-token line blocks.

## Critical rules

**Never break hook fallback.** `hook.rs::run_hook()` must always `exit(0)` on any error — including missing index, stale index, parse failures, and Ollama errors. A non-zero exit breaks the user's AI session.

**Hook exit codes:** `0` = pass through · `2` = intercept. Never exit `1`.

**Directory filtering:** `filter_entry` for directories uses ONLY `IGNORED_DIRS`. Do NOT call `should_index()` on directories — it returns `false` for dirs without extensions and breaks traversal.

## Pull request checklist

- [ ] `cargo build` and `cargo test` pass
- [ ] Hook behavior preserved (exit 0 on error, never exit 1)
- [ ] New language? Add sample file to `benchmark/samples/`
- [ ] No unnecessary `unwrap()` / `expect()` in hook path
- [ ] PR description explains the *why*, not just the *what*

## Code style

- Follow `rustfmt` defaults (`cargo fmt`)
- `cargo clippy` should produce no warnings
- Comments only when the *why* is non-obvious
- Error handling: use `anyhow::Result` + `?` propagation

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
