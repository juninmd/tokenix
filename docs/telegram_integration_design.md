# Design Proposal: Telegram Alerts & Interactive Bot for Tokenix

This document outlines how the Telegram alerting and interactive bot mechanisms implemented in `dinheirama` can be ported and adapted to **tokenix** (the Rust CLI codebase explorer).

---

## 1. Core Objectives
1. **Real-time Leaked Secrets & Egress Warnings**: Notify the developer instantly on Telegram if an AI agent (e.g., Claude Code, Antigravity) pastes a secret or targets a suspicious outbound host in conversation transcripts.
2. **Remote Codebase Queries**: Expose a private Telegram bot hosted within the `tokenix serve` daemon, allowing the developer to query their codebase via semantic search (`/search`), check project stats (`/stats`), or trigger scans (`/audit`) from their phone or another device.
3. **Stateful Delta Alerts**: Notify the developer of command outcome status or branch-sync state updates during long-running tasks.

---

## 2. Configuration System
We will allow configuration via environment variables (for zero-setup pipelines) and project-local/global `.tokenix.toml` files.

### Configuration Schema (`.tokenix.toml`)
We add a `[telegram]` section to `ProjectConfig` in `src/chunker.rs`:

```toml
[telegram]
enabled = true
bot_token = "123456789:ABCdefGhIJKlmNoPQRsTUVwxyZ"
chat_id = "987654321"

# Alert Routing toggles
alert_on_secrets = true
alert_on_egress = true
alert_on_daemon_state = false
```

### Struct Representation
In `src/chunker.rs`:
```rust
#[derive(serde::Deserialize, Default, Clone)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    pub bot_token: Option<String>,
    pub chat_id: Option<String>,
    #[serde(default = "default_true")]
    pub alert_on_secrets: bool,
    #[serde(default = "default_true")]
    pub alert_on_egress: bool,
    #[serde(default)]
    pub alert_on_daemon_state: bool,
}

fn default_true() -> bool {
    true
}
```

---

## 3. Lightweight Notifier Module (`src/notify.rs`)
We create `src/notify.rs` leveraging `reqwest` (already present in `Cargo.toml` with `blocking` features) to send formatted HTML alerts:

```rust
use crate::chunker::load_project_config;
use anyhow::{anyhow, Result};

pub struct TelegramNotifier {
    bot_token: String,
    chat_id: String,
}

impl TelegramNotifier {
    /// Loads credentials from env variables first, falling back to .tokenix.toml
    pub fn new() -> Option<Self> {
        let env_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
        let env_chat = std::env::var("TELEGRAM_CHAT_ID").ok();

        if let (Some(token), Some(chat)) = (env_token, env_chat) {
            return Some(Self { bot_token: token, chat_id: chat });
        }

        if let Some(config) = load_project_config() {
            if let Some(tel) = config.telegram {
                if tel.enabled {
                    if let (Some(token), Some(chat)) = (tel.bot_token, tel.chat_id) {
                        return Some(Self { bot_token: token, chat_id: chat });
                    }
                }
            }
        }
        None
    }

    /// Dispatches an HTML-formatted message to the registered chat
    pub fn send_message(&self, text: &str) -> Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let client = reqwest::blocking::Client::new();
        
        let payload = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true
        });

        let res = client.post(&url)
            .json(&payload)
            .send()?;

        if !res.status().is_success() {
            let err_body = res.text().unwrap_or_default();
            return Err(anyhow!("Telegram error ({}): {}", res.status(), err_body));
        }
        Ok(())
    }
}
```

---

## 4. Hooking Alerts to Security Scans

### A. Telegram Alerting for Secrets Scanning
Inside `src/secrets_scan.rs::run`, if new secrets are detected, we dispatch a formatted warning containing redacted findings:

```rust
fn alert_telegram_findings(findings: &[Finding]) -> Result<()> {
    let Some(notifier) = TelegramNotifier::new() else { return Ok(()); };
    
    let mut message = format!("<b>⚠️ tokenix: {} Secret Leaks Detected!</b>\n\n", findings.len());
    for (i, f) in findings.iter().take(5).enumerate() {
        message.push_str(&format!(
            "{}. 🔑 <code>{}</code>\n   Agent: <b>{}</b>\n   File: <code>{}</code>\n\n",
            i + 1, f.rule, f.agent, f.file.display()
        ));
    }
    if findings.len() > 5 {
        message.push_str(&format!("<i>...and {} more leaks.</i>\n", findings.len() - 5));
    }
    
    notifier.send_message(&message)
}
```

### B. Telegram Alerting for Network Egress Hits
Similar logic is wired into `src/egress_scan.rs` when dangerous outbound IPs or hosts are detected in transcript histories:

```rust
fn alert_telegram_egress(findings: &[EgressFinding]) -> Result<()> {
    let Some(notifier) = TelegramNotifier::new() else { return Ok(()); };
    
    let mut message = format!("<b>🚨 tokenix: Outbound Egress Alerts!</b>\n\n");
    for (i, f) in findings.iter().take(5).enumerate() {
        message.push_str(&format!(
            "{}. 🌐 Target: <code>{}</code>\n   Verdito: <b>{}</b>\n   Agent: <b>{}</b>\n\n",
            i + 1, f.host, f.verdict, f.agent
        ));
    }
    notifier.send_message(&message)
}
```

---

## 5. Interactive Bot inside `tokenix serve` Daemon
By passing a `--telegram` flag to `tokenix serve`, or enabling it in config, the daemon spawns an asynchronous background thread that executes a long-polling loop matching Telegram messages to local command executions.

### Bot Command Loop in `src/daemon.rs`
```rust
pub fn start_telegram_bot(port: u16) {
    let Some(notifier) = TelegramNotifier::new() else {
        println!("Telegram bot credentials missing. Skipping long-polling bot startup.");
        return;
    };

    println!("Starting Telegram long-polling bot thread...");
    std::thread::spawn(move || {
        let mut last_update_id: i64 = 0;
        let client = reqwest::blocking::Client::new();
        let bot_token = notifier.bot_token.clone();
        let chat_id = notifier.chat_id.clone();

        loop {
            let url = format!("https://api.telegram.org/bot{}/getUpdates", bot_token);
            let payload = serde_json::json!({
                "offset": last_update_id + 1,
                "timeout": 30
            });

            if let Ok(res) = client.post(&url).json(&payload).send() {
                if let Ok(json) = res.json::<serde_json::Value>() {
                    if let Some(arr) = json["result"].as_array() {
                        for update in arr {
                            if let Some(update_id) = update["update_id"].as_i64() {
                                last_update_id = update_id;
                            }
                            if let Some(msg) = update["message"].as_object() {
                                let from_chat = msg["chat"]["id"].as_i64().map(|x| x.to_string());
                                if from_chat.as_ref() == Some(&chat_id) {
                                    if let Some(text) = msg["text"].as_str() {
                                        handle_bot_command(&notifier, text, port);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}
```

### Command Handlers
The bot parses commands matching local endpoints:
* **`/search <query>`**: Triggers local SQL vector similarity + keyword index lookup.
* **`/audit`**: Runs credential scan on conversation histories.
* **`/stats`**: Queries indexing statistics, size of the SQLite files, and token savings.
* **`/index`**: Triggers project indexing.

```rust
fn handle_bot_command(notifier: &TelegramNotifier, cmd: &str, _port: u16) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() { return; }

    let response = match parts[0] {
        "/start" | "/help" => {
            "<b>🤖 tokenix CLI Bot</b>\n\n\
             Comandos:\n\
             • <code>/search &lt;query&gt;</code> — Pesquisa semântica no repo\n\
             • <code>/audit</code> — Escaneia logs em busca de segredos/vazamentos\n\
             • <code>/stats</code> — Estatísticas da base e token index\n\
             • <code>/index</code> — Recarrega e reindexa o diretório ativo"
             .to_string()
        }
        "/search" => {
            if parts.len() < 2 {
                "⚠️ Uso: <code>/search &lt;termo de pesquisa&gt;</code>".to_string()
            } else {
                let query = parts[1..].join(" ");
                match run_local_search(&query) {
                    Ok(res) => res,
                    Err(e) => format!("⚠️ Erro na pesquisa: {e}")
                }
            }
        }
        "/audit" => {
            // Run secrets scan and format results
            match crate::secrets_scan::scan_findings() {
                Ok((findings, _)) => {
                    if findings.is_empty() {
                        "🟢 Nenhum segredo vazado nos logs.".to_string()
                    } else {
                        format!("⚠️ Encontrados <b>{}</b> potenciais segredos nos logs!", findings.len())
                    }
                }
                Err(e) => format!("⚠️ Falha no auditor: {e}")
            }
        }
        _ => "Comando desconhecido. Envie <code>/help</code> para a lista.".to_string()
    };

    let _ = notifier.send_message(&response);
}
```

---

## 6. Portability & Integration Architecture
Porting the mechanism from `dinheirama` to `tokenix` is straightforward because:
1. `tokenix` already depends on `reqwest` (with blocking support), making webhook messaging and updates simple to write without adding heavy async runtimes.
2. The daemon in `tokenix` is already built using standard socket programming and worker threads, so spawning a parallel polling listener thread is robust and doesn't conflict with existing request handling.
3. This adds a critical feedback loop to developers using terminal tools and AI coding agents, aligning perfectly with Tokenix's core goal of making developer interactions secure and efficient.
