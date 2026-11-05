use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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

fn run(socket: &Path, mode: &str, stdin: &str, home: &Path) -> (Output, Duration) {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_dirigent-hook"))
        .args([socket.to_str().unwrap(), "7", mode])
        .env("HOME", home)
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
    // No socket file at all, then a stale one left by a crashed app (nobody bound to it).
    let missing = d.join("missing.sock");
    let stale = d.join("stale.sock");
    drop(UnixDatagram::bind(&stale).unwrap());
    for sock in [&missing, &stale] {
        let (out, took) = run(sock, "hook", &fixture("hook_Stop"), &d);
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        assert!(took < Duration::from_millis(500), "took {took:?}");
    }
    let (out, _) = run(&missing, "status", &fixture("status_after_response"), &d);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Haiku 5.5 · ctx 6% · 5h 14%");
}

#[test]
fn forwards_to_the_app() {
    let d = tmpdir("live");
    let sock_path = d.join("app.sock");
    let app = UnixDatagram::bind(&sock_path).unwrap();
    app.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let (out, _) = run(&sock_path, "hook", &fixture("hook_UserPromptSubmit"), &d);
    assert!(out.status.success());
    let mut buf = [0u8; 4096];
    let n = app.recv(&mut buf).unwrap();
    let msg: Msg = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(msg.tab, 7);
    assert!(matches!(msg.kind, Kind::Hook { ref event, .. } if event == "UserPromptSubmit"));
    assert!(n < 1024, "datagram must stay small, was {n} bytes");
}

#[test]
fn runs_the_users_own_statusline() {
    let d = tmpdir("user-statusline");
    std::fs::create_dir_all(d.join(".claude")).unwrap();
    // The user's command sees the same stdin JSON.
    let settings = r#"{"statusLine":{"type":"command","command":"printf 'mine:'; grep -o '\"display_name\": \"[^\"]*\"' | head -1"}}"#;
    std::fs::write(d.join(".claude/settings.json"), settings).unwrap();
    let (out, _) = run(&d.join("none.sock"), "status", &fixture("status_after_response"), &d);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), r#"mine:"display_name": "Haiku 5.5""#);
}

#[test]
fn argv_mode_never_reads_stdin() {
    let d = tmpdir("argv");
    let sock_path = d.join("app.sock");
    let app = UnixDatagram::bind(&sock_path).unwrap();
    app.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    // stdin stays open and empty (like the user's terminal): reading it would hang forever.
    let mut child = Command::new(env!("CARGO_BIN_EXE_dirigent-hook"))
        .args([sock_path.to_str().unwrap(), "7", "argv", "--model", "haiku", "fix it"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        assert!(started.elapsed() < Duration::from_secs(2), "argv mode blocked on stdin");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut buf = [0u8; 4096];
    let n = app.recv(&mut buf).unwrap();
    let msg: Msg = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(msg.kind, Kind::Argv { args: vec!["--model".into(), "haiku".into(), "fix it".into()] });
}
