//! Windows: the shell a session runs and its prompt hook, which reports the cwd with OSC 7 (`session::osc7`)
//! like our zsh wrapper does on Linux. Your profile still loads as usual; nothing of yours is modified.

use std::path::PathBuf;
use std::sync::OnceLock;

use session::CommandBuilder;

use crate::settings::Shell;

/// Runs after your profile, inside your session: puts our `claude` shim first on PATH again (a profile may have
/// rebuilt PATH, e.g. prepended the real claude's dir), keeps your prompt (Starship, oh-my-posh, posh-git: it is
/// called first, so `$?` is still the last command's) and puts an OSC 7 with the current drive path in front of it.
/// Passed as `-EncodedCommand`: a command, not a script file, so the execution policy doesn't apply.
const PS_HOOK: &str = r#"
if ($env:DIRIGENT_BIN) {
    $env:PATH = (@($env:DIRIGENT_BIN) + @($env:PATH -split ';' | Where-Object { $_ -and $_ -ne $env:DIRIGENT_BIN })) -join ';'
}
$global:__dirigent_prompt = $function:prompt
function global:prompt {
    $out = & $global:__dirigent_prompt
    $loc = $executionContext.SessionState.Path.CurrentLocation
    if ($loc.Provider.Name -eq 'FileSystem' -and $loc.ProviderPath -match '^[A-Za-z]:') {
        $path = (($loc.ProviderPath -replace '\\', '/') -split '/' | ForEach-Object { [Uri]::EscapeDataString($_) }) -join '/'
        $out = [char]27 + "]7;file://$env:COMPUTERNAME/$path" + [char]7 + $out
    }
    $out
}
"#;

/// cmd's prompt with the same OSC 7 (`$P` = the raw path, parsed leniently), then the default `C:\x>`.
const CMD_PROMPT: &str = r"$E]7;file://localhost/$P$E\$P$G";

/// How `shell` reads a typed line.
pub fn syntax(shell: Shell) -> crate::projects::Syntax {
    use crate::projects::Syntax;
    match shell {
        Shell::Cmd => Syntax::Cmd,
        Shell::GitBash => Syntax::Posix,
        Shell::Auto | Shell::Pwsh | Shell::Powershell => Syntax::PowerShell,
    }
}

/// The program, arguments and environment for `shell`.
pub fn command(shell: Shell) -> CommandBuilder {
    let shell = match shell {
        Shell::Auto if has_pwsh() => Shell::Pwsh,
        Shell::Auto => Shell::Powershell,
        s => s,
    };
    match shell {
        Shell::Pwsh | Shell::Powershell | Shell::Auto => {
            let mut c = CommandBuilder::new(if shell == Shell::Pwsh { "pwsh.exe" } else { "powershell.exe" });
            c.args(["-NoLogo", "-NoExit", "-EncodedCommand", &encoded_command(PS_HOOK)]);
            c
        }
        Shell::Cmd => {
            let mut c = CommandBuilder::new("cmd.exe");
            c.env("PROMPT", CMD_PROMPT);
            c
        }
        // ponytail: no cwd reports from Git Bash yet (like bash on Linux): add an `--rcfile` wrapper when needed.
        Shell::GitBash => {
            let mut c = CommandBuilder::new(git_bash());
            c.args(["--login", "-i"]);
            c
        }
    }
}

/// Git for Windows' bash (`hook::shell`, the same lookup Claude Code's statusLine rule uses); the usual install
/// path when there is none, so the error names it.
fn git_bash() -> PathBuf {
    hook::shell::git_bash().unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"))
}

/// Whether PowerShell 7 is installed, looked up once per run.
fn has_pwsh() -> bool {
    static PWSH: OnceLock<bool> = OnceLock::new();
    *PWSH.get_or_init(|| hook::shell::find_on_path("pwsh.exe").is_some())
}

/// PowerShell's `-EncodedCommand`: base64 of the UTF-16LE text.
fn encoded_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { A[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(encoded_command("ls"), "bABzAA==");
    }

    /// The real thing on the runner: each PowerShell starts with our hook, `cd`s, and its prompt reports the
    /// directory as OSC 7, read back through ConPTY and our scanner.
    #[test]
    fn powershell_prompt_reports_the_cwd() {
        let dir = std::env::temp_dir();
        for shell in [Shell::Powershell, Shell::Pwsh] {
            let exe = if shell == Shell::Pwsh { "pwsh.exe" } else { "powershell.exe" };
            if hook::shell::find_on_path(exe).is_none() {
                eprintln!("{exe} not installed, skipped");
                continue;
            }
            let size = session::Size { cols: 120, rows: 30, cell_width: 8, cell_height: 16 };
            let s = session::Session::spawn_command(command(shell), &dir, size).unwrap();
            wait(|| s.take_cwd_change().is_some(), "first prompt");
            s.write(b"cd C:\\Windows\r");
            let mut cwd = None;
            wait(
                || {
                    cwd = s.take_cwd_change().or(cwd.take());
                    cwd.as_deref() == Some(Path::new(r"C:\Windows"))
                },
                "prompt after cd",
            );
        }
    }

    #[test]
    fn cmd_prompt_reports_the_cwd() {
        let size = session::Size { cols: 120, rows: 30, cell_width: 8, cell_height: 16 };
        let s = session::Session::spawn_command(command(Shell::Cmd), &std::env::temp_dir(), size).unwrap();
        s.write(b"cd /d C:\\Windows\r");
        wait(|| s.take_cwd_change().is_some_and(|c| c == Path::new(r"C:\Windows")), "prompt after cd");
    }

    fn wait(mut f: impl FnMut() -> bool, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !f() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
