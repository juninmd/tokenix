# AGENTS.md — tokenix

## Project

**tokenix** — Rust CLI for token-efficient codebase exploration. Builds a local SQLite index with in-process embeddings (fastembed/ONNX), then exposes semantic search, compact file reads, and AI-tool hook integration.

Stack: Rust · SQLite · fastembed (ONNX, in-process) · Claude Code `PreToolUse` hook · background daemon (TCP)

Reduces token usage 60–90% by intercepting large reads and replacing them with focused context.

## Build & Install