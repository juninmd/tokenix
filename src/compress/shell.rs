//! Which shell re-executes a rewritten command. The agent wrote the command for
//! the shell its tool runs — Git Bash for Claude Code's Bash tool on Windows —
//! so running it under `cmd /C` changed its meaning: heredocs, `$(...)`,
//! `2>/dev/null`, `echo` and coreutils all broke, and a benchmark session spent
//! its extra turns working around failures tokenix had introduced.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The Git Bash that launched this process, if any. Only consulted on Windows;
/// `MSYSTEM` is what Git for Windows/MSYS2 set inside their shells.
pub(super) fn git_bash() -> Option<PathBuf> {
    resolve(
        std::env::var_os("MSYSTEM"),
        std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH"),
        std::env::var_os("EXEPATH"),
        |p| p.is_file(),
    )
}

fn resolve(
    msystem: Option<OsString>,
    claude_bash: Option<OsString>,
    exepath: Option<OsString>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    msystem.filter(|m| !m.is_empty())?;
    let candidates = [
        claude_bash.map(PathBuf::from),
        exepath.map(|dir| PathBuf::from(dir).join("bash.exe")),
    ];
    candidates.into_iter().flatten().find(|p| exists(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn outside_git_bash_there_is_no_bash_to_reuse() {
        assert_eq!(
            resolve(None, os("C:/bash.exe"), os("C:/Git/bin"), |_| true),
            None
        );
        assert_eq!(resolve(os(""), os("C:/bash.exe"), None, |_| true), None);
    }

    #[test]
    fn prefers_the_bash_claude_code_was_configured_with() {
        let got = resolve(
            os("MINGW64"),
            os("D:/git/bin/bash.exe"),
            os("C:/Git/bin"),
            |_| true,
        );
        assert_eq!(got, Some(PathBuf::from("D:/git/bin/bash.exe")));
    }

    #[test]
    fn falls_back_to_the_git_install_that_started_the_shell() {
        let got = resolve(os("MINGW64"), None, os("C:/Git/bin"), |_| true);
        assert_eq!(got, Some(PathBuf::from("C:/Git/bin").join("bash.exe")));
    }

    #[test]
    fn a_missing_binary_is_skipped() {
        let got = resolve(os("MINGW64"), os("D:/gone/bash.exe"), None, |_| false);
        assert_eq!(got, None);
    }
}
