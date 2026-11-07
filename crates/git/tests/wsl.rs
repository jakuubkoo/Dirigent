//! Git for a repo inside a WSL distro, run through `wsl.exe` (the `windows-wsl` CI job). Skipped with a note unless
//! `DIRIGENT_TEST_WSL` names an installed distro.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use git::write::{self, GitError};
use git::{Discovery, remote, wsl};

/// The test distro, with git going through its server when `DIRIGENT_TEST_WSL_HOOK` names the Linux helper (as in
/// the app), else per call.
fn distro() -> Option<String> {
    let d = std::env::var("DIRIGENT_TEST_WSL").ok().filter(|d| !d.is_empty());
    if d.is_none() {
        eprintln!("DIRIGENT_TEST_WSL unset: WSL test skipped");
    }
    wsl::set_server_helper(helper());
    d
}

/// `DIRIGENT_TEST_WSL_HOOK` as the distro sees it (`/mnt/<d>/…`).
fn helper() -> Option<String> {
    let p = PathBuf::from(std::env::var_os("DIRIGENT_TEST_WSL_HOOK")?);
    wsl::to_linux(&git::plain_path(p.canonicalize().ok()?).to_string_lossy())
}

/// Kills the distro's git server (as a crash would).
fn kill_server(d: &str) {
    wsh(d, "pkill -KILL -f 'dirigent-hook [g]it-server' || true");
}

/// `sh -c script` inside the distro; its stdout.
fn wsh(d: &str, script: &str) -> String {
    let out = Command::new("wsl.exe").args(["-d", d, "-e", "sh", "-c", script]).output().unwrap();
    assert!(out.status.success(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repo with one commit (`f.txt`) at `/tmp/dirigent-wsl-<pid>-<name>`: (its Linux path, the app's path).
fn repo(d: &str, name: &str) -> (String, PathBuf) {
    let linux = format!("/tmp/dirigent-wsl-{}-{name}", std::process::id());
    wsh(
        d,
        &format!(
            "rm -rf {linux} && mkdir -p {linux}/sub && cd {linux} && git init -q -b main && git config user.name t && \
             git config user.email t@t && git config commit.gpgsign false && printf 'one\\n' > f.txt && \
             git add . && git commit -qm init"
        ),
    );
    (linux.clone(), wsl::to_windows(d, &linux))
}

fn found(dir: &std::path::Path) -> git::Repo {
    match git::discover(dir) {
        Discovery::Repo(r) => r,
        other => panic!("{}: {other:?}", dir.display()),
    }
}

#[test]
fn discover_status_stage_commit_inside_wsl() {
    let Some(d) = distro() else { return };
    let (linux, win) = repo(&d, "basic");
    let r = found(&win.join("sub"));
    assert_eq!(r.root, win, "root in the app's form");
    assert_eq!(r.git_dir, wsl::to_windows(&d, &format!("{linux}/.git")));
    wsh(&d, &format!("cd {linux} && printf 'one\\ntwo\\n' > f.txt && printf 'n\\n' > new.txt"));
    let st = git::status(&r).unwrap();
    let f = st.files.iter().find(|f| f.path == "f.txt").expect("modified");
    assert_eq!(f.lines, Some((1, 0)));
    assert!(st.files.iter().any(|f| f.path == "new.txt" && f.kind == '?'), "{:?}", st.files);
    let diff = git::diff(&r, f, || false).expect("diff");
    assert!(format!("{diff:?}").contains("two"), "{diff:?}");
    write::stage(&r, std::slice::from_ref(f)).unwrap();
    write::commit(&r, "second", false).unwrap();
    assert_eq!(wsh(&d, &format!("cd {linux} && git log -1 --format=%s")), "second");
    assert!(write::trash(&win.join("new.txt")).unwrap_err().to_string().contains("WSL"));
    assert_eq!(wsh(&d, &format!("cat {linux}/new.txt")), "n", "never deleted");
    wsh(&d, &format!("rm -rf {linux}"));
}

#[test]
fn worktrees_inside_wsl_use_linux_paths() {
    let Some(d) = distro() else { return };
    let (linux, win) = repo(&d, "wt");
    let wt = wsl::to_windows(&d, &format!("{linux}-feat"));
    write::worktree_add(&win, "feat", &wt, "HEAD").unwrap();
    let list = write::worktree_list(&win).unwrap();
    assert_eq!(list.iter().map(|w| w.path.clone()).collect::<Vec<_>>(), [win.clone(), wt.clone()]);
    assert_eq!(found(&wt).main_root(), Some(win.as_path()));
    write::worktree_remove(&win, &wt, false).unwrap();
    wsh(&d, &format!("rm -rf {linux} {linux}-feat"));
}

#[test]
fn cancel_kills_a_hung_fetch_inside_wsl() {
    let Some(d) = distro() else { return };
    let (linux, win) = repo(&d, "hung");
    // The "ssh" never answers (git runs core.sshCommand through sh with its args after `:`).
    wsh(
        &d,
        &format!(
            "cd {linux} && git remote add origin ssh://nowhere.invalid/x.git && git config core.sshCommand 'sleep 31;:'"
        ),
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        c2.store(true, Ordering::Relaxed);
    });
    let t = Instant::now();
    let err = remote::fetch(&win, &cancel).unwrap_err();
    assert!(matches!(err, GitError::Cancelled), "{err}");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(wsh(&d, "pgrep -f 'slee[p] 31' || true"), "", "the hung ssh died with it");
    wsh(&d, &format!("rm -rf {linux}"));
}

#[test]
fn repos_inside_wsl_refresh_when_touched() {
    let Some(d) = distro() else { return };
    let (linux, win) = repo(&d, "touch");
    let r = found(&win);
    let (tx, rx) = mpsc::channel();
    let w = git::Watcher::start(r, move |u| {
        let _ = tx.send(u);
    })
    .unwrap();
    let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let st = first.status.unwrap();
    assert!(st.watch_error.as_deref().is_some_and(|e| e.contains("WSL")), "{st:?}");
    assert!(st.files.is_empty());
    wsh(&d, &format!("printf 'x\\n' > {linux}/sub-new.txt"));
    assert!(rx.recv_timeout(Duration::from_secs(2)).is_err(), "no watches: nothing until told");
    w.touched(vec![win.clone()]);
    let u = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(u.status.unwrap().files.iter().any(|f| f.path == "sub-new.txt"));
    assert_eq!(u.dirs, std::slice::from_ref(&win));
    assert!(u.settled >= w.received().load(Ordering::Relaxed));
    drop(w);
    wsh(&d, &format!("rm -rf {linux}"));
}

/// One full refresh of the git panel with a diff open, exactly the git calls the app makes (keep in step with
/// `Repos::acquire`'s `on_update` and `Workspace::load_overlay`): the watcher's `status` (status + numstat), the
/// open diff (`diff`: HEAD check + diff), its hunk states (staged + unstaged diff), and beside them the old side's
/// text for syntax colors (`head_text`). Timed through the distro's git server (warm: no `wsl.exe` started) and per
/// call through `wsl.exe`. The counters are process-wide: CI runs this binary with `--test-threads=1`.
#[test]
fn wsl_refresh_cost() {
    let Some(d) = distro() else { return };
    let Some(helper) = helper() else { return eprintln!("DIRIGENT_TEST_WSL_HOOK unset: refresh cost skipped") };
    let (linux, win) = repo(&d, "cost");
    wsh(&d, &format!("cd {linux} && printf 'one\\ntwo\\n' > f.txt"));
    let r = found(&win);
    let mut lines = Vec::new();
    for (how, server) in [("through the git server", true), ("per call (wsl.exe each)", false)] {
        wsl::set_server_helper(server.then(|| helper.clone()));
        let _ = git::status(&r); // warm: the distro (and the server) run, as in a session
        let mut runs = Vec::new();
        for _ in 0..5 {
            let (requests, spawns) = (wsl::requests(), wsl::spawns());
            let t = Instant::now();
            let st = git::status(&r).unwrap();
            let f = st.files.iter().find(|f| f.path == "f.txt").unwrap().clone();
            git::diff(&r, &f, || false).unwrap();
            write::hunk_state(&r, &f, &[]);
            let serial = t.elapsed();
            let t = Instant::now();
            git::head_text(&r, "f.txt", 1 << 20).unwrap();
            let side = t.elapsed();
            assert_eq!(wsl::requests() - requests, 7, "this test's list of a refresh's git calls is out of date");
            assert_eq!(wsl::spawns() - spawns, if server { 0 } else { 7 }, "{how}");
            runs.push((serial, side));
        }
        runs.sort();
        let (serial, side) = runs[runs.len() / 2];
        lines.push(format!(
            "[wsl refresh cost] {how}: 7 git calls; status → diff → hunk states: median {} ms (max {} ms); \
             head_text beside: {} ms",
            serial.as_millis(),
            runs.last().unwrap().0.as_millis(),
            side.as_millis()
        ));
    }
    for l in lines {
        eprintln!("{l}");
    }
    wsh(&d, &format!("rm -rf {linux}"));
}

#[test]
fn server_restarts_after_a_crash() {
    let Some(d) = distro() else { return };
    if helper().is_none() {
        return;
    }
    let (linux, win) = repo(&d, "restart");
    let r = found(&win);
    git::status(&r).unwrap();
    kill_server(&d);
    let spawns = wsl::spawns();
    git::status(&r).unwrap();
    assert_eq!(wsl::spawns() - spawns, 1, "one new server, no per-call wsl.exe");
    git::status(&r).unwrap();
    assert_eq!(wsl::spawns() - spawns, 1, "and it serves the next call");
    wsh(&d, &format!("rm -rf {linux}"));
}

/// The server dies while git runs: a read is sent again to the restarted server and succeeds; a write is not (a
/// commit may have happened), it fails, and its hook ran once.
#[test]
fn a_read_survives_a_server_crash() {
    let Some(d) = distro() else { return };
    if helper().is_none() {
        return;
    }
    let (linux, win) = repo(&d, "crash");
    // status asks the fsmonitor hook first, a commit runs pre-commit: both take 2 s.
    wsh(
        &d,
        &format!(
            "cd {linux} && printf '#!/bin/sh\\nsleep 2\\nexit 1\\n' > .git/fsmon && chmod +x .git/fsmon && \
             git config core.fsmonitor \"$PWD/.git/fsmon\" && \
             printf '#!/bin/sh\\necho ran >> hook-runs\\nsleep 2\\n' > .git/hooks/pre-commit && \
             chmod +x .git/hooks/pre-commit && printf 'two\\n' > f.txt && git add f.txt"
        ),
    );
    let r = found(&win);
    git::status(&r).unwrap(); // the server is up
    let d2 = d.clone();
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        kill_server(&d2);
    });
    let (spawns, t) = (wsl::spawns(), Instant::now());
    let st = git::status(&r).expect("retried on the restarted server");
    assert!(t.elapsed() > Duration::from_secs(2), "the kill came while git ran: {:?}", t.elapsed());
    assert_eq!(wsl::spawns() - spawns, 1, "on a restarted server");
    assert!(st.files.iter().any(|f| f.path == "f.txt"), "{:?}", st.files);
    killer.join().unwrap();
    wsh(&d, &format!("cd {linux} && git config --unset core.fsmonitor"));
    let d2 = d.clone();
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        kill_server(&d2);
    });
    assert!(write::commit(&r, "second", false).is_err(), "a write is not sent again");
    killer.join().unwrap();
    std::thread::sleep(Duration::from_secs(3)); // the orphaned first attempt finishes
    assert_eq!(wsh(&d, &format!("cat {linux}/hook-runs")), "ran", "the commit ran once");
    wsh(&d, &format!("rm -rf {linux}"));
}

#[test]
fn server_stops_after_the_grace_only() {
    let Some(d) = distro() else { return };
    if helper().is_none() {
        return;
    }
    let (linux, win) = repo(&d, "grace");
    let r = found(&win);
    wsl::set_grace(Duration::from_secs(1));
    let running = || wsh(&d, "pgrep -f 'dirigent-hook [g]it-server' || true");
    let held = wsl::Hold::new(&d);
    git::status(&r).unwrap();
    drop(held);
    std::thread::sleep(Duration::from_millis(300));
    let held = wsl::Hold::new(&d); // a new tab within the grace
    std::thread::sleep(Duration::from_millis(1500));
    let spawns = wsl::spawns();
    git::status(&r).unwrap();
    assert_eq!(wsl::spawns(), spawns, "still the same server");
    drop(held);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(running(), "", "stopped after the grace");
    let spawns = wsl::spawns();
    git::status(&r).unwrap();
    assert_eq!(wsl::spawns() - spawns, 1, "the next call starts it again");
    wsl::set_grace(Duration::from_secs(30));
    // The drop above armed a 1 s stop; a hold kept for the rest of the run calls it off, so it can't stop the
    // server under a later test. (`repos_inside_wsl_refresh_when_touched` arms the default 30 s one when its watcher
    // goes; the hold above, a few seconds later, calls that off too.)
    std::mem::forget(wsl::Hold::new(&d));
    wsh(&d, &format!("rm -rf {linux}"));
}

#[test]
fn missing_helper_falls_back_per_call() {
    let Some(d) = distro() else { return };
    let (linux, win) = repo(&d, "fallback");
    wsl::set_server_helper(Some("/nonexistent/dirigent-hook".into()));
    let r = found(&win);
    assert!(wsl::server_off(&d), "given up for the run");
    let spawns = wsl::spawns();
    git::status(&r).unwrap();
    assert_eq!(wsl::spawns() - spawns, 1, "per call");
    wsl::set_server_helper(helper());
    wsh(&d, &format!("rm -rf {linux}"));
}
