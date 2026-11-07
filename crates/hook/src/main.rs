//! `dirigent-hook <socket> <tab> hook|status`, run by Claude Code for every hook event and statusLine refresh;
//! `dirigent-hook <socket> <tab> argv <claude args...>`, the old sh shim's flag report (older runtime dirs).
//! Started under the name `claude` it is the shim instead (`hook::shim`).
//! Windows: `dirigent-hook <pipe> 0 relay` passes a message the Linux helper built inside WSL on to the app.
//!
//! Fire-and-forget: forwards a small message to the app (Unix a non-blocking datagram, Windows a named pipe). If the
//! app isn't running the send fails instantly and is ignored. Always exits 0 so Claude Code is never slowed down or
//! broken.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hook::{Kind, Msg, parse, send};

/// Set for a user statusLine we run, so a user statusLine that points back at us can't recurse.
const NESTED: &str = "DIRIGENT_HOOK_NESTED";

fn main() {
    let mut argv = std::env::args_os();
    let argv0 = argv.next().unwrap_or_default();
    if Path::new(&argv0).file_stem().is_some_and(|s| s.eq_ignore_ascii_case("claude")) {
        std::process::exit(hook::shim::run(&argv0, argv.collect()));
    }
    let args: Vec<String> = std::env::args().collect();
    let [_, socket, tab, mode, rest @ ..] = args.as_slice() else { return };
    // Windows, started from WSL through interop: a message the Linux helper built, passed on as is.
    if cfg!(windows) && mode == "relay" {
        let mut bytes = Vec::new();
        let _ = std::io::stdin().take(64 * 1024).read_to_end(&mut bytes);
        #[cfg(windows)]
        hook::relay(socket, &bytes);
        return;
    }
    let mut input = String::new();
    let kind = if mode == "argv" {
        // Never touch stdin here: it is the user's terminal.
        let mut total = 0;
        let args = rest.iter().take_while(|a| {
            total += a.len();
            total <= 8 * 1024
        });
        Some(Kind::Argv { args: args.cloned().collect() })
    } else {
        let _ = std::io::stdin().read_to_string(&mut input);
        parse(mode, &input)
    };

    if let (Ok(tab), Some(kind)) = (tab.parse(), kind)
        && let Ok(bytes) = serde_json::to_vec(&Msg::new(tab, kind))
    {
        send(socket, &bytes);
    }

    if mode == "status" {
        statusline(&input);
    }
}

/// Prints the user's own statusLine (from their Claude settings) if configured, else a minimal one.
fn statusline(input: &str) {
    if std::env::var_os(NESTED).is_none()
        && let Some(cmd) = user_statusline()
        && let Ok(mut child) = user_shell(&cmd).env(NESTED, "1").stdin(Stdio::piped()).stdout(Stdio::inherit()).spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes());
        }
        let _ = child.wait();
        return;
    }
    if let Some(Kind::Status { model, context_pct, five_hour, .. }) = parse("status", input) {
        let mut parts = vec![model.unwrap_or_else(|| "claude".into())];
        if let Some(p) = context_pct {
            parts.push(format!("ctx {p:.0}%"));
        }
        if let Some(l) = five_hour {
            parts.push(format!("5h {:.0}%", l.used_pct));
        }
        println!("{}", parts.join(" · "));
    }
}

/// The shell Claude Code itself would run that statusLine with.
#[cfg(unix)]
fn user_shell(cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", cmd]);
    c
}

#[cfg(windows)]
fn user_shell(cmd: &str) -> Command {
    match hook::shell::claude_shell() {
        hook::shell::ClaudeShell::GitBash(bash) => {
            let mut c = Command::new(bash);
            c.args(["-c", cmd]);
            c
        }
        hook::shell::ClaudeShell::PowerShell => {
            let mut c = Command::new("powershell.exe");
            c.args(["-NoProfile", "-Command", cmd]);
            c
        }
    }
}

/// `statusLine.command` from `$CLAUDE_CONFIG_DIR/settings.json` (Claude's own override), else
/// `~/.claude/settings.json`.
fn user_statusline() -> Option<String> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()).map(PathBuf::from);
    let dir = dir.or_else(|| std::env::home_dir().map(|h| h.join(".claude")))?;
    let text = std::fs::read_to_string(dir.join("settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["statusLine"]["command"].as_str().map(str::to_owned)
}
