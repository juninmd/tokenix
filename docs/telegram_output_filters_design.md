# Design Proposal: Stateful Delta Output Filters with Telegram Alerting

This document describes a new, stateful extension to the **tokenix** output filter pipeline. It leverages caching to calculate execution-to-execution state differences (deltas) and integrates **Telegram alerting** to notify the developer of regressions or critical status changes.

---

## 1. The Core Concept
Plain output filters (like those in `assets/filters/`) compress command output in a stateless manner (e.g., stripping ANSI, trimming lines, limiting height). 

However, for commands run repeatedly during a development loop (e.g., `kubectl get pods`, `npm test`, `git status`, `docker ps`), stateless filtering still returns redundant information to the LLM agent, wasting tokens.

**Stateful Delta Filtering** solves this by:
1. **Caching** the filtered command output of the previous execution (keyed by base command + branch/repo).
2. **Computing the Diff/Delta** (newly added, removed, or changed lines) on subsequent executions.
3. **Failing Open / Alerting on Regression**: Returning only the delta to the LLM to save up to 90% more tokens, while sending a **Telegram Alert** to the developer if a critical line matches a threat pattern (e.g., a test starts failing or a service enters `CrashLoopBackOff`).

---

## 2. Configuration Schema
We define a `[stateful]` section inside the `FilterDef` struct (`src/filters.rs`):

```toml
[filters.kubectl-pods]
match_command = "^kubectl get pods"
strip_ansi = true

[filters.kubectl-pods.stateful]
enabled = true
cache_key = "k8s-pods"
delta_only = true          # Only return modified lines to the LLM
alert_on_regex = "(?i)error|fail|crash|unknown|backoff"
```

### Struct Updates (`src/filters.rs` / `src/chunker.rs`)
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct StatefulDef {
    #[serde(default)]
    pub enabled: bool,
    pub cache_key: String,
    #[serde(default)]
    pub delta_only: bool,
    pub alert_on_regex: Option<String>,
}
```
Add `pub stateful: Option<StatefulDef>` inside `FilterDef` in `src/filters.rs`.

---

## 4. Rust Implementation Architecture

### Delta Calculation (`src/filters.rs`)
```rust
use std::collections::HashSet;
use std::fs;
use crate::notify::TelegramNotifier;

pub fn apply_stateful_filter(
    current_lines: Vec<String>,
    stateful: &StatefulDef,
    cmd_context: &str
) -> Vec<String> {
    if !stateful.enabled {
        return current_lines;
    }

    let cache_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".tokenix")
        .join("cache")
        .join("state");
    
    let _ = fs::create_dir_all(&cache_dir);
    let cache_file = cache_dir.join(format!("{}.log", stateful.cache_key));

    // Load previous state
    let previous_content = fs::read_to_string(&cache_file).unwrap_or_default();
    let previous_set: HashSet<&str> = previous_content.lines().collect();

    // Calculate delta: find lines in current execution that weren't in the previous one
    let mut added_lines = Vec::new();
    for line in &current_lines {
        if !previous_set.contains(line.as_str()) {
            added_lines.push(line.clone());
        }
    }

    // Save current state for next time
    let current_content = current_lines.join("\n");
    let _ = fs::write(&cache_file, current_content);

    // If there is no previous run (first run), return full content
    if previous_content.is_empty() {
        return current_lines;
    }

    // Check delta for critical issues & trigger Telegram notifications
    if let Some(ref regex_str) = stateful.alert_on_regex {
        if let Ok(re) = regex::Regex::new(regex_str) {
            let triggered_lines: Vec<&String> = added_lines
                .iter()
                .filter(|line| re.is_match(line))
                .collect();

            if !triggered_lines.is_empty() {
                let _ = send_telegram_alert(stateful, cmd_context, &triggered_lines);
            }
        }
    }

    if stateful.delta_only {
        if added_lines.is_empty() {
            vec!["(no changes since last run)".to_string()]
        } else {
            let mut output = vec![format!("--- DELTA REPORT (changes since last run) ---")];
            output.extend(added_lines);
            output
        }
    } else {
        current_lines
    }
}
```

### Telegram Alerting Function
```rust
fn send_telegram_alert(
    stateful: &StatefulDef,
    cmd_context: &str,
    triggered_lines: &[&String]
) -> anyhow::Result<()> {
    let Some(notifier) = TelegramNotifier::new() else { return Ok(()); };

    let mut msg = format!(
        "<b>🚨 tokenix: Stateful Output Alert!</b>\n\
         Comando: <code>{}</code>\n\
         Chave: <code>{}</code>\n\n\
         <b>Novas ocorrências de erro/falha detectadas:</b>\n",
        cmd_context, stateful.cache_key
    );

    for line in triggered_lines.iter().take(5) {
        msg.push_str(&format!("• <code>{}</code>\n", line));
    }

    if triggered_lines.len() > 5 {
        msg.push_str(&format!("<i>...e mais {} linhas.</i>\n", triggered_lines.len() - 5));
    }

    notifier.send_message(&msg)
}
```

---

## 5. Proactive Token-Waste Telegram Alerts
To make output filters even more intelligent, we can notify developers of **unfiltered commands** that are wasting LLM tokens during their session.

In `src/compress.rs` or the CLI runner:
* If a command execution returns output > 2,000 tokens (approx. 8,000 characters) and **no custom/bundled filter was matched**, it is flagged as a "Token Sink".
* `tokenix` sends a Telegram Alert:
  > 💡 **tokenix Tip**: Command `npm run test --verbose` returned **4,800 tokens** of unfiltered output. 
  > 
  > Reply with `/generate npm run test` to automatically create a TOML output filter.

---

## 6. Benefits for Tokenix
* **Massive Token Reductions**: Shrinks token consumption from repetitive commands down to near-zero when there are no changes, keeping the context window clean.
* **Proactive Developer Awareness**: Keeps developers aware of regressions/failures in real-time on their device, even when the agent is trying to analyze the output.
* **Filter Discovery**: Encourages developers to build new output filters dynamically when large command outputs are detected.
