//! The `claude` shim (`dirigent-hook` under the name `claude`) in front of a stand-in claude (`examples/fake_claude`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const EXE: &str = std::env::consts::EXE_SUFFIX;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dirigent-shim-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn helper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dirigent-hook"))
}

/// `cargo test` builds the examples next to the binaries.
fn fake_claude() -> PathBuf {
    let p = helper().parent().unwrap().join("examples").join(format!("fake_claude{EXE}"));
    assert!(p.is_file(), "{} missing: run the tests with cargo test", p.display());
    p
}

/// `d/shim/claude` = us (a symlink on Unix, like the app makes; a copy on Windows), `d/real/claude` = the fake.
fn layout(d: &Path) -> (PathBuf, PathBuf) {
    let (shim, real) = (d.join("shim"), d.join("real"));
    std::fs::create_dir_all(&shim).unwrap();
    std::fs::create_dir_all(&real).unwrap();
    let me = shim.join(format!("claude{EXE}"));
    #[cfg(unix)]
    std::os::unix::fs::symlink(helper(), &me).unwrap();
    #[cfg(windows)]
    std::fs::copy(helper(), &me).unwrap();
    std::fs::copy(fake_claude(), real.join(format!("claude{EXE}"))).unwrap();
    (me, real)
}

/// PATH: only the given dirs (never a real claude installed on this machine).
fn path(dirs: &[&Path]) -> std::ffi::OsString {
    std::env::join_paths(dirs).unwrap()
}

/// What the fake got: its args and whether `DIRIGENT_PROMPT_FILE` was still set.
fn seen(out: &Path) -> (Vec<String>, bool) {
    let text = std::fs::read_to_string(out).unwrap();
    let mut lines = text.lines();
    (serde_json::from_str(lines.next().unwrap()).unwrap(), lines.next() == Some("env"))
}

fn shim(me: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)]) -> Output {
    let mut c = Command::new(me);
    c.args(args)
        .env_remove("DIRIGENT_CLAUDE_SETTINGS")
        .env_remove("DIRIGENT_PROMPT_FILE")
        .env_remove("DIRIGENT_ARGS_FILE")
        .env_remove("DIRIGENT_BIN")
        .env_remove("DIRIGENT_SOCKET")
        .env_remove("DIRIGENT_TAB_ID");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

#[test]
fn runs_the_real_claude_with_settings_skipping_itself() {
    let d = tmpdir("settings");
    let (me, real) = layout(&d);
    let shim_dir = me.parent().unwrap();
    let (out, settings) = (d.join("out.txt"), d.join("s.json"));
    std::fs::write(&settings, "{}").unwrap();
    let p = path(&[shim_dir, &real]);
    let env = [
        ("PATH", p.as_os_str()),
        ("DIRIGENT_BIN", shim_dir.as_os_str()),
        ("DIRIGENT_CLAUDE_SETTINGS", settings.as_os_str()),
        ("FAKE_CLAUDE_OUT", out.as_os_str()),
        ("FAKE_CLAUDE_EXIT", "5".as_ref()),
    ];
    let o = shim(&me, &["--model", "a b"], &env);
    assert_eq!(o.status.code(), Some(5), "claude's exit code: {}", String::from_utf8_lossy(&o.stderr));
    let s = settings.to_str().unwrap();
    assert_eq!(seen(&out).0, ["--settings", s, "--model", "a b"]);
    // App gone (no settings file): plain claude.
    std::fs::remove_file(&settings).unwrap();
    shim(&me, &["--model", "a b"], &env);
    assert_eq!(seen(&out).0, ["--model", "a b"]);
    // No DIRIGENT_BIN: still never runs itself (its own dir comes from how it was started).
    let o = shim(&me, &["-v"], &[("PATH", p.as_os_str()), ("FAKE_CLAUDE_OUT", out.as_os_str())]);
    assert!(o.status.success());
    assert_eq!(seen(&out).0, ["-v"]);
    // Nothing on PATH but us.
    let only = path(&[shim_dir]);
    let o = shim(&me, &[], &[("PATH", only.as_os_str()), ("DIRIGENT_BIN", shim_dir.as_os_str())]);
    assert_eq!(o.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no claude found on PATH"));
    let _ = std::fs::remove_dir_all(&d);
}

/// The first prompt arrives as one argument after `--`, byte for byte (trailing new lines dropped, as before),
/// and the file and the variable are gone.
#[test]
fn passes_the_prompt_file_and_deletes_it() {
    let d = tmpdir("prompt");
    let (me, real) = layout(&d);
    let shim_dir = me.parent().unwrap();
    let (out, file) = (d.join("out.txt"), d.join("prompt.txt"));
    let prompt =
        "Branch: feat/x\n\nFix the 'login' \"bug\": $HOME `id` %PATH% \\n C:\\x\n  - keep it\n-starts with a dash";
    std::fs::write(&file, format!("{prompt}\n\n")).unwrap();
    let p = path(&[shim_dir, &real]);
    let env = [
        ("PATH", p.as_os_str()),
        ("DIRIGENT_BIN", shim_dir.as_os_str()),
        ("DIRIGENT_PROMPT_FILE", file.as_os_str()),
        ("FAKE_CLAUDE_OUT", out.as_os_str()),
    ];
    assert!(shim(&me, &["--effort", "high"], &env).status.success());
    assert_eq!(seen(&out), (vec!["--effort".into(), "high".into(), "--".into(), prompt.into()], false));
    assert!(!file.exists(), "the prompt file is deleted");
    // Run again with the variable still set (PowerShell / cmd keep it): no file, no prompt, no error.
    assert!(shim(&me, &["--effort", "high"], &env).status.success());
    assert_eq!(seen(&out), (vec!["--effort".into(), "high".into()], false));
    let _ = std::fs::remove_dir_all(&d);
}

/// Flags from the args file (Windows typed lines) go first, byte for byte; the file and the variable are gone.
#[test]
fn takes_flags_from_the_args_file() {
    let d = tmpdir("args");
    let (me, real) = layout(&d);
    let shim_dir = me.parent().unwrap();
    let (out, file, prompt) = (d.join("out.txt"), d.join("args.json"), d.join("prompt.txt"));
    let flags = ["--add-dir", r"C:\it's here\", r#"say "hi""#, "100% $x", "--model", "opus"];
    std::fs::write(&file, serde_json::to_string(&flags).unwrap()).unwrap();
    std::fs::write(&prompt, "do it").unwrap();
    let p = path(&[shim_dir, &real]);
    let env = [
        ("PATH", p.as_os_str()),
        ("DIRIGENT_BIN", shim_dir.as_os_str()),
        ("DIRIGENT_ARGS_FILE", file.as_os_str()),
        ("DIRIGENT_PROMPT_FILE", prompt.as_os_str()),
        ("FAKE_CLAUDE_OUT", out.as_os_str()),
    ];
    assert!(shim(&me, &["-v"], &env).status.success());
    let mut want: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
    want.extend(["-v".into(), "--".into(), "do it".into()]);
    assert_eq!(seen(&out), (want, false), "file flags, typed ones, then the prompt; no variable reaches claude");
    assert!(!file.exists(), "the args file is deleted");
    // Again with the variable still set (PowerShell / cmd keep it): the file is gone, only the typed flags.
    assert!(shim(&me, &["-v"], &env).status.success());
    assert_eq!(seen(&out).0, ["-v"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// The flags for resuming reach the app before claude starts.
#[test]
fn reports_the_flags_to_the_app() {
    let d = tmpdir("report");
    let (me, real) = layout(&d);
    let shim_dir = me.parent().unwrap();
    let (out, settings) = (d.join("out.txt"), d.join("s.json"));
    std::fs::write(&settings, "{}").unwrap();
    let (addr, rx) = app(&d);
    let p = path(&[shim_dir, &real]);
    let env = [
        ("PATH", p.as_os_str()),
        ("DIRIGENT_BIN", shim_dir.as_os_str()),
        ("DIRIGENT_CLAUDE_SETTINGS", settings.as_os_str()),
        ("DIRIGENT_SOCKET", addr.as_ref()),
        ("DIRIGENT_TAB_ID", "3".as_ref()),
        ("FAKE_CLAUDE_OUT", out.as_os_str()),
    ];
    assert!(shim(&me, &["--model", "a b"], &env).status.success());
    let msg: hook::Msg = serde_json::from_slice(&rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap()).unwrap();
    assert_eq!(msg.tab, 3);
    assert_eq!(msg.kind, hook::Kind::Argv { args: vec!["--model".into(), "a b".into()] });
    let _ = std::fs::remove_dir_all(&d);
}

/// Inside WSL the report goes through the relay (`DIRIGENT_RELAY`, here a script that keeps what it got): it arrives
/// with its `seq`, and the relay is never left a zombie child of claude (the shim's pid after the exec; a stand-in
/// that never reaps children, as claude doesn't).
#[test]
#[cfg(target_os = "linux")]
fn relayed_report_leaves_no_zombie() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmpdir("relay");
    let (me, _) = layout(&d);
    let shim_dir = me.parent().unwrap();
    let real = d.join("real-sh");
    std::fs::create_dir_all(&real).unwrap();
    let (relayed, settings) = (d.join("relayed"), d.join("s.json"));
    let script = |p: &Path, body: String| {
        std::fs::write(p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    script(&real.join("claude"), "exec sleep 5".into());
    let relay = d.join("relay.sh");
    script(&relay, format!("cat > '{}.tmp' && mv '{0}.tmp' '{0}'", relayed.display()));
    std::fs::write(&settings, "{}").unwrap();
    let mut child = Command::new(&me)
        .args(["--model", "x"])
        .env("PATH", path(&[shim_dir, &real, Path::new("/usr/bin"), Path::new("/bin")]))
        .env("DIRIGENT_BIN", shim_dir)
        .env("DIRIGENT_CLAUDE_SETTINGS", &settings)
        .env("DIRIGENT_SOCKET", "dirigent-hook-1")
        .env("DIRIGENT_TAB_ID", "3")
        .env("DIRIGENT_RELAY", &relay)
        .env_remove("DIRIGENT_PROMPT_FILE")
        .env_remove("DIRIGENT_ARGS_FILE")
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !relayed.is_file() {
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "the relay never ran");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(300)); // the relay exits
    let ps = Command::new("ps").args(["-o", "stat=", "--ppid", &child.id().to_string()]).output().unwrap();
    let states = String::from_utf8_lossy(&ps.stdout).into_owned();
    let _ = child.kill();
    let _ = child.wait();
    assert!(!states.lines().any(|s| s.trim_start().starts_with('Z')), "zombie child of claude: {states:?}");
    let msg: hook::Msg = serde_json::from_slice(&std::fs::read(&relayed).unwrap()).unwrap();
    assert_eq!(msg.kind, hook::Kind::Argv { args: vec!["--model".into(), "x".into()] });
    assert!(msg.seq.is_some(), "relayed messages carry seq");
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(unix)]
fn app(d: &Path) -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
    let path = d.join("app.sock");
    let sock = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 65536];
        while let Ok(n) = sock.recv(&mut buf) {
            let _ = tx.send(buf[..n].to_vec());
        }
    });
    (path.to_str().unwrap().into(), rx)
}

#[cfg(windows)]
fn app(_: &Path) -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
    use std::io::Read;
    let name = format!("dirigent-shim-test-{}", std::process::id());
    let server = hook::pipe::Server::bind(&name, true).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(conn) = server.accept() {
            let mut buf = Vec::new();
            let _ = conn.file().read_to_end(&mut buf);
            let _ = tx.send(buf);
        }
    });
    (name, rx)
}

/// Windows: `claude.exe` wins over an npm `claude.cmd` in the same dir.
#[test]
#[cfg(windows)]
fn prefers_the_exe_over_npm_launchers() {
    let d = tmpdir("exe-first");
    std::fs::write(d.join("claude.cmd"), "@echo off\r\n").unwrap();
    std::fs::copy(fake_claude(), d.join("claude.exe")).unwrap();
    let p = path(&[&d]);
    assert_eq!(hook::shim::find_claude(&p, None), Some(d.join("claude.exe")));
}

/// Windows npm installs: the shim runs what `claude.cmd` launches (an exe, or node + the script) directly, so a
/// prompt with new lines, quotes and `%` never meets cmd.exe's quoting.
#[test]
#[cfg(windows)]
fn npm_launchers_are_seen_through() {
    let d = tmpdir("npm");
    let (me, _) = layout(&d);
    let shim_dir = me.parent().unwrap();
    let prompt = "two\nlines \"quoted\" 100% $x";
    for (case, cmd, target) in [
        (
            "exe",
            "@ECHO off\r\n\"%~dp0\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe\" %*\r\n",
            r"node_modules\@anthropic-ai\claude-code\bin\claude.exe",
        ),
        (
            "node",
            "@ECHO off\r\nSET dp0=%~dp0\r\n\"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n",
            r"node.exe",
        ),
    ] {
        let npm = d.join(format!("npm-{case}"));
        let pkg = npm.join(r"node_modules\@anthropic-ai\claude-code");
        std::fs::create_dir_all(pkg.join("bin")).unwrap();
        std::fs::write(npm.join("claude.cmd"), cmd).unwrap();
        std::fs::write(pkg.join("cli.js"), "// cli\n").unwrap();
        std::fs::copy(fake_claude(), npm.join(target)).unwrap();
        let (out, file) = (d.join(format!("out-{case}.txt")), d.join(format!("prompt-{case}.txt")));
        std::fs::write(&file, prompt).unwrap();
        let p = path(&[shim_dir, &npm]);
        let env = [
            ("PATH", p.as_os_str()),
            ("DIRIGENT_BIN", shim_dir.as_os_str()),
            ("DIRIGENT_PROMPT_FILE", file.as_os_str()),
            ("FAKE_CLAUDE_OUT", out.as_os_str()),
        ];
        let o = shim(&me, &["--model", "opus"], &env);
        assert!(o.status.success(), "{case}: {}", String::from_utf8_lossy(&o.stderr));
        let mut args = seen(&out).0;
        if case == "node" {
            assert!(args.remove(0).ends_with("cli.js"), "{case}: node gets the script first");
        }
        assert_eq!(args, ["--model", "opus", "--", prompt], "{case}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Windows: a launcher we can't see through runs as a batch file; flags pass, a prompt cmd.exe can't quote is
/// left out with a visible line.
#[test]
#[cfg(windows)]
fn opaque_launcher_keeps_flags_and_says_so() {
    let d = tmpdir("opaque");
    let (me, _) = layout(&d);
    let shim_dir = me.parent().unwrap();
    let npm = d.join("npm");
    std::fs::create_dir_all(&npm).unwrap();
    std::fs::copy(fake_claude(), npm.join("fake.exe")).unwrap();
    std::fs::write(npm.join("claude.cmd"), "@echo off\r\nfake.exe %*\r\n").unwrap();
    let (out, file) = (d.join("out.txt"), d.join("prompt.txt"));
    std::fs::write(&file, "two\nlines").unwrap();
    let p = path(&[shim_dir, &npm]);
    let env = [
        ("PATH", p.as_os_str()),
        ("DIRIGENT_BIN", shim_dir.as_os_str()),
        ("DIRIGENT_PROMPT_FILE", file.as_os_str()),
        ("FAKE_CLAUDE_OUT", out.as_os_str()),
    ];
    let o = shim(&me, &["--model", "opus"], &env);
    assert!(String::from_utf8_lossy(&o.stderr).contains("can't take this first prompt"), "{o:?}");
    assert_eq!(seen(&out).0, ["--model", "opus"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Windows: Ctrl-C typed into the terminal reaches claude; the shim survives it and passes claude's exit code on.
#[test]
#[cfg(windows)]
fn ctrl_c_reaches_claude_and_the_shim_survives() {
    let d = tmpdir("ctrl-c");
    let (me, real) = layout(&d);
    let shim_dir = me.parent().unwrap();
    let out = d.join("out.txt");
    let mut cmd = session::CommandBuilder::new(&me);
    cmd.env("PATH", path(&[shim_dir, &real]));
    cmd.env("DIRIGENT_BIN", shim_dir);
    cmd.env("FAKE_CLAUDE_OUT", &out);
    cmd.env("FAKE_CLAUDE_CTRL_C", "1");
    let size = session::Size { cols: 80, rows: 24, cell_width: 8, cell_height: 16 };
    let s = session::Session::spawn_command(cmd, &d, size).unwrap();
    let ready = || std::fs::read_to_string(&out).is_ok_and(|t| t.contains("ready"));
    wait(ready, "fake claude ready");
    s.write(b"\x03");
    wait(|| s.is_exited(), "exit after Ctrl-C");
    assert!(std::fs::read_to_string(&out).unwrap().contains("ctrl-c"), "claude got the Ctrl-C");
    assert_eq!(s.exit_code(), Some(42), "the shim waited for claude and passed its code on");
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(windows)]
fn wait(mut f: impl FnMut() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !f() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
