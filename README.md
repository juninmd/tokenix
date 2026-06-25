<div align="center">
  <img src=".github/prints/logo.jpg" alt="tokenix logo" style="max-height: 450px;" />

  <p><strong>Local semantic search, symbol graphs, secrets scanning, output filters, and CLI hooks that save 60-90% LLM tokens.</strong></p>

  <p>
    <a href="https://github.com/juninmd/tokenix/releases"><img src="https://img.shields.io/github/v/release/juninmd/tokenix?style=flat-square&color=orange&label=release" alt="Latest Release" /></a>
    <a href="https://crates.io/crates/tokenix"><img src="https://img.shields.io/crates/v/tokenix?style=flat-square&color=orange" alt="crates.io" /></a>
    <a href="https://crates.io/crates/tokenix"><img src="https://img.shields.io/crates/d/tokenix?style=flat-square&color=orange&label=downloads" alt="crates.io downloads" /></a>
    <a href="https://github.com/juninmd/tokenix/stargazers"><img src="https://img.shields.io/github/stars/juninmd/tokenix?style=flat-square&color=yellow" alt="GitHub stars" /></a>
    <a href="https://github.com/juninmd/tokenix/actions/workflows/rust.yml"><img src="https://img.shields.io/github/actions/workflow/status/juninmd/tokenix/rust.yml?branch=main&style=flat-square&label=CI" alt="CI" /></a>
    <a href="https://github.com/juninmd/tokenix/actions/workflows/supply-chain.yml"><img src="https://img.shields.io/github/actions/workflow/status/juninmd/tokenix/supply-chain.yml?branch=main&style=flat-square&label=supply%20chain" alt="Supply Chain" /></a>
    <a href="https://scorecard.viewer/?uri=github.com/juninmd/tokenix"><img src="https://img.shields.io/ossf-scorecard/github.com/juninmd/tokenix?style=flat-square&label=scorecard" alt="OpenSSF Scorecard" /></a>
    <a href="https://github.com/juninmd/tokenix/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License" /></a>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/built%20with-Rust-orange?style=flat-square&logo=rust" alt="Built with Rust" /></a>
    <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey?style=flat-square" alt="Platforms" />
  </p>

  <p>
    <a href="#-quick-install">Install</a> ·
    <a href="#-interactive-dashboard">Dashboard</a> ·
    <a href="#-how-it-works">How it Works</a> ·
    <a href="#-usage">Usage</a> ·
    <a href="#-setup-by-tool">Setup</a> ·
    <a href="#-commands-reference">Commands</a> ·
    <a href="CONTRIBUTING.md">Contributing</a>
  </p>
</div>

---

> **tokenix** is a local-first Rust CLI that helps AI coding agents understand a repository without dumping huge files into the prompt. It indexes your code, finds relevant chunks by meaning, returns compact file outlines, and can hook into AI tools to replace noisy reads and command output with smaller, more useful context. Works with Claude Code, GitHub Copilot, OpenAI Codex CLI, OpenCode, Gemini, and any MCP client. **No Ollama or external server required.**