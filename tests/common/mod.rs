//! Hermetic sandbox for end-to-end tests: a throwaway git repo plus a private
//! `TOKENIX_HOME`, so no test reads or writes the developer's `~/.tokenix`.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

pub struct Sandbox {
    root: PathBuf,
    pub home: PathBuf,
    pub repo: PathBuf,
}

pub struct Run {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Sandbox {
    pub fn new(name: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("tokenix-e2e-{name}-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).expect("sandbox home");
        std::fs::create_dir_all(&repo).expect("sandbox repo");
        let git = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .expect("git must be installed for e2e tests");
        assert!(git.success(), "git init failed");
        Self { root, home, repo }
    }

    /// A sandbox holding the demo project, already indexed without embeddings
    /// (no model download, so the suite runs offline and in seconds).
    pub fn indexed(name: &str) -> Self {
        let sb = Self::new(name);
        sb.write("src/main.rs", MAIN_RS);
        sb.write("src/billing.rs", &billing_rs());
        sb.write(
            "README.md",
            "# Demo\n\nArithmetic and billing helpers for the e2e suite.\n",
        );
        let run = sb.tokenix(&["index", ".", "--no-embed"]);
        assert_eq!(run.code, 0, "index failed: {}", run.stderr);
        sb
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.repo.join(rel)
    }

    pub fn write(&self, rel: &str, content: &str) {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tokenix"));
        cmd.current_dir(&self.repo)
            .env("TOKENIX_HOME", &self.home)
            .env("TOKENIX_NO_TUI", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    pub fn tokenix(&self, args: &[&str]) -> Run {
        self.tokenix_with_stdin(args, "")
    }

    pub fn tokenix_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut cmd = self.command();
        cmd.args(args).stdin(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        into_run(cmd.output().expect("tokenix exit"))
    }

    pub fn tokenix_with_stdin(&self, args: &[&str], stdin: &str) -> Run {
        let mut child = self.command().args(args).spawn().expect("spawn tokenix");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        into_run(child.wait_with_output().expect("tokenix exit"))
    }

    /// Feed a Claude Code `PreToolUse` payload to `tokenix hook`.
    pub fn hook(&self, tool: &str, input: serde_json::Value) -> Run {
        let payload = serde_json::json!({
            "session_id": "e2e",
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "tool_input": input,
        });
        self.tokenix_with_stdin(&["hook"], &payload.to_string())
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn into_run(out: Output) -> Run {
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

pub fn line_count(path: &Path) -> usize {
    std::fs::read_to_string(path).unwrap().lines().count()
}

pub const MAIN_RS: &str = r#"mod billing;

fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() {
    println!("{}", greet("world"));
    println!("1 + 2 = {}", add(1, 2));
    println!("{}", billing::invoice_total(&[1, 2, 3]));
}
"#;

/// A 232-line file: large enough for the Read hook to outline it, with bodies
/// big enough that the outline clears the 30% savings floor.
pub fn billing_rs() -> String {
    let mut s = String::from(
        "pub fn invoice_total(items: &[u32]) -> u32 {\n    items.iter().map(|i| apply_tax(*i)).sum()\n}\n\n\
         fn apply_tax(amount: u32) -> u32 {\n    amount + amount / 10\n}\n",
    );
    for i in 1..=25 {
        s.push_str(&format!(
            "\npub fn helper_{i}(x: u32) -> u32 {{\n    let base = x.saturating_mul({i});\n    \
             let shifted = base.rotate_left(3);\n    let masked = shifted & 0xFFFF;\n    \
             let mixed = masked ^ (base >> 2);\n    let bounded = mixed.min(1_000_000);\n    \
             bounded + {i}\n}}\n"
        ));
    }
    s
}
