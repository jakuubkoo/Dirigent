//! `dirigent-hook git-server` (`gitserve`), the real helper driven by the app's client, on Linux: what the app gets
//! from it inside a WSL distro.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use gitserve::{CAP, Client, Event, Run};

fn server(cap: usize) -> Client {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dirigent-hook"));
    c.arg("git-server");
    Client::start(c, Duration::from_secs(5), cap).unwrap()
}

fn run(cwd: &Path, argv: &[&str]) -> Run {
    Run { cwd: cwd.to_str().unwrap().into(), argv: argv.iter().map(|a| a.to_string()).collect(), ..Run::default() }
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dirigent-gitserve-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sh(d: &Path, script: &str) {
    assert!(Command::new("sh").args(["-c", script]).current_dir(d).status().unwrap().success(), "{script}");
}

/// Whether anything is left in process group `pg`.
fn group_alive(pg: i32) -> bool {
    // SAFETY: signal 0 only checks.
    unsafe { libc::kill(-pg, 0) == 0 }
}

fn wait_gone(pg: i32) {
    let end = Instant::now() + Duration::from_secs(3);
    while group_alive(pg) {
        assert!(Instant::now() < end, "process group {pg} still running");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The pid a request printed first (`echo $$` = its process group).
fn first_line_pid(p: &mut gitserve::Pending) -> i32 {
    match p.next(Duration::from_secs(5)).unwrap() {
        Some(Event::Out(b)) => String::from_utf8(b).unwrap().trim().parse().unwrap(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn git_with_stdin_errors_and_a_missing_program() {
    let d = tmpdir("basic");
    sh(
        &d,
        "git init -q -b main && git config user.name t && git config user.email t@t && printf 'one\\n' > f.txt && \
         git add . && git -c commit.gpgsign=false commit -qm init && printf 'one\\ntwo\\n' > f.txt",
    );
    let c = server(CAP);
    let t = Duration::from_secs(10);
    let st = c.request(&run(&d, &["git", "status", "--porcelain"])).unwrap().output(t).unwrap();
    assert_eq!((st.code, String::from_utf8_lossy(&st.stdout).trim()), (0, "M f.txt"));
    let patch = c.request(&run(&d, &["git", "diff"])).unwrap().output(t).unwrap().stdout;
    let mut apply = run(&d, &["git", "apply", "--cached", "-"]);
    apply.stdin = patch;
    assert_eq!(c.request(&apply).unwrap().output(t).unwrap().code, 0);
    let staged = c.request(&run(&d, &["git", "diff", "--cached", "--name-only"])).unwrap().output(t).unwrap();
    assert_eq!(String::from_utf8_lossy(&staged.stdout).trim(), "f.txt", "the patch came through stdin");
    let mut env = run(&d, &["sh", "-c", r#"printf '%s|%s' "$A" "${HOME-unset}"; echo oops >&2; exit 3"#]);
    env.env = vec![("A".into(), Some("set".into())), ("HOME".into(), None)];
    let failed = c.request(&env).unwrap().output(t).unwrap();
    assert_eq!(failed.stdout, b"set|unset");
    assert_eq!((failed.code, failed.stderr.as_slice()), (3, &b"oops\n"[..]));
    let missing = c.request(&run(&d, &["dirigent-no-such-program"])).unwrap().output(t).unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound, "git missing → GitMissing in the app");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn requests_run_side_by_side() {
    let d = tmpdir("parallel");
    let c = server(CAP);
    let slow = c.request(&run(&d, &["sleep", "2"])).unwrap();
    // A big output nobody reads yet doesn't hold up the others either.
    let mut big = c.request(&run(&d, &["head", "-c", "3000000", "/dev/zero"])).unwrap();
    let t = Instant::now();
    assert_eq!(c.request(&run(&d, &["true"])).unwrap().output(Duration::from_secs(5)).unwrap().code, 0);
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    let (mut chunks, mut total) = (0, 0);
    loop {
        match big.next(Duration::from_secs(5)).unwrap() {
            Some(Event::Out(b)) => (chunks, total) = (chunks + 1, total + b.len()),
            Some(Event::Exit { code, .. }) => break assert_eq!(code, 0),
            None => panic!("stalled"),
        }
    }
    assert_eq!(total, 3_000_000);
    assert!(chunks >= 3_000_000 / gitserve::CHUNK, "chunked: {chunks}");
    assert_eq!(slow.output(Duration::from_secs(5)).unwrap().code, 0);
}

#[test]
fn a_dropped_request_is_killed_with_its_children() {
    let d = tmpdir("cancel");
    let c = server(CAP);
    let mut p = c.request(&run(&d, &["sh", "-c", "echo $$; sleep 30 & wait"])).unwrap();
    let pg = first_line_pid(&mut p);
    assert!(group_alive(pg));
    drop(p);
    wait_gone(pg);
}

#[test]
fn a_request_past_its_timeout_is_stopped() {
    let d = tmpdir("timeout");
    let c = server(CAP);
    let t = Instant::now();
    let e =
        c.request(&run(&d, &["sh", "-c", "echo $$ > pg; sleep 5 & wait"])).unwrap().output(Duration::from_millis(300));
    assert_eq!(e.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    assert!(t.elapsed() < Duration::from_secs(2));
    wait_gone(std::fs::read_to_string(d.join("pg")).unwrap().trim().parse().unwrap());
}

#[test]
fn a_reader_that_stops_reading_delays_nobody() {
    let d = tmpdir("cap");
    let c = server(1 << 20);
    let mut endless = c.request(&run(&d, &["sh", "-c", "echo $$; exec yes"])).unwrap();
    let pg = first_line_pid(&mut endless);
    std::thread::sleep(Duration::from_millis(300)); // `yes` fills its queue past the cap meanwhile
    for _ in 0..5 {
        let t = Instant::now();
        assert_eq!(c.request(&run(&d, &["true"])).unwrap().output(Duration::from_secs(5)).unwrap().code, 0);
        assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
    }
    let e = endless.output(Duration::from_secs(5)).unwrap_err();
    assert!(e.to_string().contains("MiB of output"), "{e}");
    wait_gone(pg);
}

#[test]
fn a_crashed_server_fails_its_requests_and_new_ones() {
    let d = tmpdir("crash");
    let c = server(CAP);
    let p = c.request(&run(&d, &["sleep", "3"])).unwrap();
    // SAFETY: kill(2) on the server we started.
    unsafe { libc::kill(c.pid() as i32, libc::SIGKILL) };
    let t = Instant::now();
    let e = p.output(Duration::from_secs(10)).unwrap_err();
    assert!(gitserve::is_stopped(&e), "{e}");
    assert!(t.elapsed() < Duration::from_secs(2), "no hang: {:?}", t.elapsed());
    assert!(c.is_dead());
    assert!(gitserve::is_stopped(&c.request(&run(&d, &["true"])).err().unwrap()));
}

#[test]
fn dropping_the_client_ends_the_server_and_what_it_runs() {
    let d = tmpdir("shutdown");
    let c = server(CAP);
    let pid = c.pid() as i32;
    let mut p = c.request(&run(&d, &["sh", "-c", "echo $$; sleep 30 & wait"])).unwrap();
    let pg = first_line_pid(&mut p);
    drop(c);
    wait_gone(pg);
    // SAFETY: signal 0 only checks; the server was reaped by the drop.
    assert!(unsafe { libc::kill(pid, 0) } != 0, "server gone");
    assert!(gitserve::is_stopped(&p.next(Duration::from_secs(1)).unwrap_err()));
}
