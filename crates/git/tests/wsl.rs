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

fn distro() -> Option<String> {
    let d = std::env::var("DIRIGENT_TEST_WSL").ok().filter(|d| !d.is_empty());
    if d.is_none() {
        eprintln!("DIRIGENT_TEST_WSL unset: WSL test skipped");
    }
    d
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
/// text for syntax colors (`head_text`). Prints the time; over ~0.5 s one long-lived git per distro would pay off.
/// The spawn count is process-wide: CI runs this binary with `--test-threads=1`.
#[test]
fn wsl_refresh_cost() {
    let Some(d) = distro() else { return };
    let (linux, win) = repo(&d, "cost");
    wsh(&d, &format!("cd {linux} && printf 'one\\ntwo\\n' > f.txt"));
    let r = found(&win);
    let _ = git::status(&r); // the distro is warm from here on, as in a running session
    let mut runs = Vec::new();
    for _ in 0..5 {
        let before = wsl::spawns();
        let t = Instant::now();
        let st = git::status(&r).unwrap();
        let f = st.files.iter().find(|f| f.path == "f.txt").unwrap().clone();
        git::diff(&r, &f, || false).unwrap();
        write::hunk_state(&r, &f, &[]);
        let serial = t.elapsed();
        let t = Instant::now();
        git::head_text(&r, "f.txt", 1 << 20).unwrap();
        let side = t.elapsed();
        assert_eq!(wsl::spawns() - before, 7, "this test's list of a refresh's git calls is out of date");
        runs.push((serial, side));
    }
    runs.sort();
    let (serial, side) = runs[runs.len() / 2];
    eprintln!(
        "[wsl refresh cost] 7 wsl.exe calls; status → diff → hunk states: median {} ms (max {} ms); head_text \
         beside: {} ms",
        serial.as_millis(),
        runs.last().unwrap().0.as_millis(),
        side.as_millis()
    );
    wsh(&d, &format!("rm -rf {linux}"));
}
