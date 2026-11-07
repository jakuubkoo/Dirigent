//! The helper binary against a stand-in app: a datagram socket on Unix, the named pipe on Windows.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use hook::{Kind, Msg};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dirigent-hook-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A stand-in app: its address (socket path / pipe name) and the messages it received.
struct App {
    addr: String,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl App {
    #[cfg(unix)]
    fn start(d: &Path, name: &str) -> App {
        let path = d.join(format!("{name}.sock"));
        let sock = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = sock.recv(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        });
        App { addr: path.to_str().unwrap().into(), rx }
    }

    #[cfg(windows)]
    fn start(_: &Path, name: &str) -> App {
        use std::io::Read;
        let addr = format!("dirigent-hook-test-{}-{name}", std::process::id());
        let server = hook::pipe::Server::bind(&addr, true).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(conn) = server.accept() {
                let mut buf = Vec::new();
                let _ = conn.file().read_to_end(&mut buf);
                if tx.send(buf).is_err() {
                    return;
                }
            }
        });
        App { addr, rx }
    }

    fn recv(&self) -> (Msg, usize) {
        let bytes = self.rx.recv_timeout(Duration::from_secs(5)).expect("no message reached the app");
        (serde_json::from_slice(&bytes).unwrap(), bytes.len())
    }
}

/// An address nobody serves.
fn nobody(d: &Path) -> String {
    if cfg!(windows) {
        format!("dirigent-hook-test-{}-nobody", std::process::id())
    } else {
        d.join("none.sock").to_str().unwrap().into()
    }
}

/// Runs the helper with `CLAUDE_CONFIG_DIR=<d>/.claude` (never the real user's settings).
fn run(addr: &str, mode: &str, stdin: &str, d: &Path) -> (Output, Duration) {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_dirigent-hook"))
        .args([addr, "7", mode])
        .env("CLAUDE_CONFIG_DIR", d.join(".claude"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (out, started.elapsed())
}

#[test]
fn app_not_running_is_silent_and_fast() {
    let d = tmpdir("dead");
    // Unix: also a stale socket left by a crashed app (nobody bound to it). A pipe dies with its process.
    #[cfg(unix)]
    let addrs = {
        let stale = d.join("stale.sock");
        drop(std::os::unix::net::UnixDatagram::bind(&stale).unwrap());
        vec![nobody(&d), stale.to_str().unwrap().into()]
    };
    #[cfg(windows)]
    let addrs = vec![nobody(&d)];
    for addr in &addrs {
        let (out, took) = run(addr, "hook", &fixture("hook_Stop"), &d);
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        assert!(took < Duration::from_millis(500), "took {took:?}");
    }
    let (out, _) = run(&addrs[0], "status", &fixture("status_after_response"), &d);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Haiku 5.5 · ctx 6% · 5h 14%");
}

#[test]
fn forwards_to_the_app() {
    let d = tmpdir("live");
    let app = App::start(&d, "live");
    let (out, _) = run(&app.addr, "hook", &fixture("hook_UserPromptSubmit"), &d);
    assert!(out.status.success());
    let (msg, n) = app.recv();
    assert_eq!(msg.tab, 7);
    assert!(matches!(msg.kind, Kind::Hook { ref event, .. } if event == "UserPromptSubmit"));
    assert!(n < 1024, "message must stay small, was {n} bytes");
}

#[test]
fn runs_the_users_own_statusline() {
    let d = tmpdir("user-statusline");
    std::fs::create_dir_all(d.join(".claude")).unwrap();
    // The user's command sees the same stdin JSON. Windows: the shell Claude would use (Git Bash on the runners).
    let cmd = match cfg!(windows).then(hook::shell::claude_shell) {
        Some(hook::shell::ClaudeShell::PowerShell) => {
            r#"$j = $input | Out-String | ConvertFrom-Json; 'mine:"display_name": "' + $j.model.display_name + '"'"#
        }
        _ => r#"printf 'mine:'; grep -o '"display_name": "[^"]*"' | head -1"#,
    };
    let settings = serde_json::json!({ "statusLine": { "type": "command", "command": cmd } });
    std::fs::write(d.join(".claude/settings.json"), settings.to_string()).unwrap();
    let (out, _) = run(&nobody(&d), "status", &fixture("status_after_response"), &d);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), r#"mine:"display_name": "Haiku 5.5""#);
}

#[test]
fn argv_mode_never_reads_stdin() {
    let d = tmpdir("argv");
    let app = App::start(&d, "argv");
    // stdin stays open and empty (like the user's terminal): reading it would hang forever.
    let mut child = Command::new(env!("CARGO_BIN_EXE_dirigent-hook"))
        .args([app.addr.as_str(), "7", "argv", "--model", "haiku", "fix it"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        assert!(started.elapsed() < Duration::from_secs(2), "argv mode blocked on stdin");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.recv().0.kind, Kind::Argv { args: vec!["--model".into(), "haiku".into(), "fix it".into()] });
}

/// Windows: the statusLine command we configure, run the way Claude Code runs it (Git Bash `-c`, PowerShell
/// `-Command`) with our bin dir on PATH, reaches the app with the limits and prints the line.
#[test]
#[cfg(windows)]
fn status_command_reaches_the_app_through_both_shells() {
    use hook::shell::{ClaudeShell, git_bash, status_command};
    let d = tmpdir("status-shells");
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_dirigent-hook"), bin.join("dirigent-hook.exe")).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.clone()).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let mut shells = vec![ClaudeShell::PowerShell];
    shells.extend(git_bash().map(ClaudeShell::GitBash));
    assert!(shells.len() == 2, "the runner has Git Bash: both shells must be tested");
    for (i, shell) in shells.iter().enumerate() {
        let app = App::start(&d, &format!("status-{i}"));
        let line = status_command(&app.addr, 7);
        let mut c = match shell {
            ClaudeShell::GitBash(bash) => {
                let mut c = Command::new(bash);
                c.args(["-c", &line]);
                c
            }
            ClaudeShell::PowerShell => {
                let mut c = Command::new("powershell.exe");
                c.args(["-NoProfile", "-Command", &line]);
                c
            }
        };
        let mut child = c
            .env("PATH", &path)
            .env("CLAUDE_CONFIG_DIR", d.join("no-user-settings"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(fixture("status_after_response").as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("5h 14%"),
            "{shell:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        match app.recv().0.kind {
            Kind::Status { five_hour: Some(l), .. } => assert_eq!(l.used_pct.round(), 14.0, "{shell:?}"),
            other => panic!("{shell:?}: {other:?}"),
        }
    }
}

/// WSL: the Linux helper (musl build, `DIRIGENT_TEST_WSL_HOOK`) inside the distro (`DIRIGENT_TEST_WSL`) reaches the
/// app's pipe through the Windows helper started by interop (`relay` mode), every message with its `seq`; the
/// statusLine runs the user's own Linux one. Skipped with a note without a distro.
#[test]
#[cfg(windows)]
fn wsl_helper_relays_to_the_app() {
    let (Ok(distro), Ok(linux_hook)) = (std::env::var("DIRIGENT_TEST_WSL"), std::env::var("DIRIGENT_TEST_WSL_HOOK"))
    else {
        eprintln!("DIRIGENT_TEST_WSL / DIRIGENT_TEST_WSL_HOOK unset: WSL relay test skipped");
        return;
    };
    // A drive path as WSL sees it (long names: %TEMP% can be an 8.3 path).
    let in_wsl = |p: &Path| {
        let p = p.canonicalize().unwrap().to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/");
        format!("/mnt/{}{}", p[..1].to_ascii_lowercase(), &p[2..])
    };
    let d = tmpdir("wsl-relay");
    let app = App::start(&d, "wsl");
    let relay = in_wsl(Path::new(env!("CARGO_BIN_EXE_dirigent-hook")));
    let (helper, fixtures) = (in_wsl(Path::new(&linux_hook)), in_wsl(Path::new(&fixture_dir())));
    let user = format!("/tmp/dirigent-relay-{}", std::process::id());
    let script = format!(
        r#"set -e
cp "$HELPER" /tmp/dh-$$ && chmod +x /tmp/dh-$$
mkdir -p {user}/.claude
printf '%s' '{{"statusLine":{{"type":"command","command":"printf mine:; grep -o \"\\\"display_name\\\": \\\"[^\\\"]*\\\"\" | head -1"}}}}' > {user}/.claude/settings.json
for i in $(seq 20); do /tmp/dh-$$ "$PIPE" 7 hook < "$FX/hook_UserPromptSubmit.json"; done
CLAUDE_CONFIG_DIR={user}/.claude /tmp/dh-$$ "$PIPE" 7 status < "$FX/status_after_response.json"
rm -rf /tmp/dh-$$ {user}"#
    );
    let out = Command::new("wsl.exe")
        .args(["-d", &distro, "-e", "env"])
        .arg(format!("DIRIGENT_RELAY={relay}"))
        .arg(format!("HELPER={helper}"))
        .arg(format!("PIPE={}", app.addr))
        .arg(format!("FX={fixtures}"))
        .args(["sh", "-c", &script])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), r#"mine:"display_name": "Haiku 5.5""#);
    let mut seqs = Vec::new();
    let mut status = None;
    while let Ok(bytes) = app.rx.recv_timeout(Duration::from_secs(10)) {
        let m: Msg = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m.tab, 7);
        seqs.push(m.seq.expect("relayed messages carry seq"));
        if let Kind::Status { five_hour, .. } = m.kind {
            status = five_hour;
        }
        if seqs.len() == 21 {
            break;
        }
    }
    assert_eq!(seqs.len(), 21, "all 20 hooks and the status arrived");
    assert_eq!(status.map(|l| l.used_pct.round()), Some(14.0));
    seqs.sort();
    seqs.dedup();
    assert_eq!(seqs.len(), 21, "distinct seqs");
}

#[cfg(windows)]
fn fixture_dir() -> String {
    format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))
}
