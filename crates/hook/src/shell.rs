//! Windows: which shell Claude Code runs a statusLine (and shell-form hooks) with, and where Git Bash lives. One
//! place, so the statusLine we configure, the user's statusLine we run and Git Bash sessions agree.

use std::path::{Path, PathBuf};

/// The shell Claude Code on Windows runs a statusLine `command` with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeShell {
    GitBash(PathBuf),
    PowerShell,
}

/// Claude Code's rule: `CLAUDE_CODE_GIT_BASH_PATH` when set, else Git for Windows' bash when installed, else
/// PowerShell.
pub fn claude_shell() -> ClaudeShell {
    let env = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").filter(|v| !v.is_empty()).map(PathBuf::from);
    match env {
        Some(bash) => ClaudeShell::GitBash(bash),
        None => git_bash().map_or(ClaudeShell::PowerShell, ClaudeShell::GitBash),
    }
}

/// The statusLine `command` for Claude Code on Windows (statusLine has no exec form): a bare `dirigent-hook`,
/// found through the session's PATH: no path to quote, which Git Bash and PowerShell treat differently. The same
/// line works in both: a native command first on the line inherits the shell's stdin (the JSON). Not `$input |`:
/// PowerShell's `-Command` leaves it empty (tested through both shells on the CI runner).
pub fn status_command(pipe: &str, tab: u32) -> String {
    format!("dirigent-hook {pipe} {tab} status")
}

/// Git for Windows' `bash.exe`: next to the `git.exe` on PATH (`…\Git\cmd\git.exe` → `…\Git\bin\bash.exe`), else in
/// `%ProgramFiles%\Git`. Never a bare `bash.exe` from PATH: that can be WSL's in System32.
pub fn git_bash() -> Option<PathBuf> {
    pick_git_bash(find_on_path("git.exe"), std::env::var_os("ProgramFiles").map(PathBuf::from), |p| p.is_file())
}

fn pick_git_bash(
    git: Option<PathBuf>,
    program_files: Option<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let next_to_git = git.and_then(|g| Some(g.parent()?.parent()?.join("bin").join("bash.exe")));
    let installed = program_files.map(|p| p.join("Git").join("bin").join("bash.exe"));
    [next_to_git, installed].into_iter().flatten().find(|b| exists(b))
}

/// The first `name` (exact file name) in PATH.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_commands() {
        assert_eq!(status_command("dirigent-hook-9", 4), "dirigent-hook dirigent-hook-9 4 status");
    }

    #[test]
    fn git_bash_next_to_git_then_program_files_then_none() {
        let git = PathBuf::from("/G/Git/cmd/git.exe");
        let pf = PathBuf::from("/PF");
        let next = PathBuf::from("/G/Git/bin/bash.exe");
        let installed = pf.join("Git").join("bin").join("bash.exe");
        assert_eq!(pick_git_bash(Some(git.clone()), Some(pf.clone()), |p| p == next), Some(next.clone()));
        assert_eq!(pick_git_bash(Some(git), Some(pf.clone()), |p| p == installed), Some(installed.clone()));
        assert_eq!(pick_git_bash(None, Some(pf.clone()), |p| p == installed), Some(installed));
        assert_eq!(pick_git_bash(None, Some(pf), |_| false), None, "no Git for Windows: PowerShell");
    }

    /// The three cases of Claude's rule (each test owns its variable values; they don't run concurrently with a
    /// different value: only this test sets them).
    #[test]
    fn claude_shell_rule() {
        // SAFETY: only this test reads or writes these variables.
        unsafe {
            std::env::set_var("CLAUDE_CODE_GIT_BASH_PATH", "/custom/bash.exe");
        }
        assert_eq!(claude_shell(), ClaudeShell::GitBash("/custom/bash.exe".into()), "the variable wins");
        let d = std::env::temp_dir().join(format!("dirigent-shell-rule-{}", std::process::id()));
        let bash = d.join("Git").join("bin").join("bash.exe");
        std::fs::create_dir_all(bash.parent().unwrap()).unwrap();
        std::fs::write(&bash, "").unwrap();
        let (path, pf) = (std::env::var_os("PATH"), std::env::var_os("ProgramFiles"));
        // SAFETY: as above; restored below.
        unsafe {
            std::env::remove_var("CLAUDE_CODE_GIT_BASH_PATH");
            std::env::set_var("PATH", d.join("none"));
            std::env::set_var("ProgramFiles", &d);
        }
        assert_eq!(claude_shell(), ClaudeShell::GitBash(bash.clone()), "Git for Windows installed");
        std::fs::remove_file(&bash).unwrap();
        assert_eq!(claude_shell(), ClaudeShell::PowerShell, "neither");
        // SAFETY: as above.
        unsafe {
            if let Some(p) = path {
                std::env::set_var("PATH", p);
            }
            match pf {
                Some(p) => std::env::set_var("ProgramFiles", p),
                None => std::env::remove_var("ProgramFiles"),
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
