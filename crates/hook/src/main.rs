//! `agent-ide-hook <socket> <tab> hook|status`, run by Claude Code for every hook event and statusLine refresh.
//!
//! Fire-and-forget: forwards a small datagram to the app on a non-blocking socket. If the app isn't running the
//! send fails instantly and is ignored. Always exits 0 so Claude Code is never slowed down or broken.

use std::io::{Read, Write};
use std::os::unix::net::UnixDatagram;
use std::process::{Command, Stdio};

use hook::{Kind, Msg, parse};

/// Set for a user statusLine we run, so a user statusLine that points back at us can't recurse.
const NESTED: &str = "AGENT_IDE_HOOK_NESTED";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let [_, socket, tab, mode] = args.as_slice() else { return };

    if let (Ok(tab), Some(kind)) = (tab.parse(), parse(mode, &input))
        && let Ok(sock) = UnixDatagram::unbound()
        && sock.set_nonblocking(true).is_ok()
        && let Ok(bytes) = serde_json::to_vec(&Msg { tab, kind })
    {
        let _ = sock.send_to(&bytes, socket);
    }

    if mode == "status" {
        statusline(&input);
    }
}

/// Prints the user's own statusLine (from ~/.claude/settings.json) if configured, else a minimal one.
fn statusline(input: &str) {
    if std::env::var_os(NESTED).is_none()
        && let Some(cmd) = user_statusline()
        && let Ok(mut child) =
            Command::new("sh").args(["-c", &cmd]).env(NESTED, "1").stdin(Stdio::piped()).stdout(Stdio::inherit()).spawn()
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

fn user_statusline() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let text = std::fs::read_to_string(std::path::Path::new(&home).join(".claude/settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["statusLine"]["command"].as_str().map(str::to_owned)
}
