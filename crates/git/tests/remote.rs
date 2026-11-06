//! Network git against a local bare "remote": fetch, fast-forward, diverged, publish, branch switching, and a hung
//! command that cancel kills.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use git::remote::{self, Cancel};
use git::write::{self, GitError};

fn sh(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Setup {
    root: PathBuf,
    bare: PathBuf,
    /// The clone under test.
    mine: PathBuf,
    /// Another clone that pushes to the remote.
    other: PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn setup(name: &str) -> Setup {
    let root = std::env::temp_dir().join(format!("dirigent-git-remote-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    sh(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("f.txt"), "one\n").unwrap();
    sh(&seed, &["add", "."]);
    sh(&seed, &["commit", "-q", "-m", "init"]);
    let bare = root.join("remote.git");
    sh(&root, &["clone", "-q", "--bare", "seed", "remote.git"]);
    for c in ["mine", "other"] {
        sh(&root, &["clone", "-q", "remote.git", c]);
        for (k, v) in [("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false")] {
            sh(&root.join(c), &["config", k, v]);
        }
    }
    Setup { mine: root.join("mine"), other: root.join("other"), bare, root }
}

fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    sh(dir, &["add", "."]);
    sh(dir, &["commit", "-q", "-m", file]);
}

fn ab(dir: &Path) -> String {
    sh(dir, &["rev-list", "--left-right", "--count", "HEAD...@{u}"])
}

fn no_cancel() -> Cancel {
    Arc::new(AtomicBool::new(false))
}

#[test]
fn fetch_fast_forward_and_diverged() {
    let s = setup("ff");
    commit(&s.other, "g.txt", "g\n");
    sh(&s.other, &["push", "-q"]);
    remote::fetch(&s.mine, &no_cancel()).unwrap();
    assert_eq!(ab(&s.mine), "0\t1");
    remote::fast_forward(&s.mine, &no_cancel()).unwrap();
    assert_eq!(ab(&s.mine), "0\t0");
    assert!(s.mine.join("g.txt").exists());

    // Both sides move: no fast-forward, nothing changes.
    commit(&s.other, "h.txt", "h\n");
    sh(&s.other, &["push", "-q"]);
    commit(&s.mine, "mine.txt", "m\n");
    remote::fetch(&s.mine, &no_cancel()).unwrap();
    let head = sh(&s.mine, &["rev-parse", "HEAD"]);
    let err = remote::fast_forward(&s.mine, &no_cancel()).unwrap_err();
    assert!(matches!(err, GitError::NotFastForward), "{err}");
    assert_eq!(sh(&s.mine, &["rev-parse", "HEAD"]), head);

    // A plain push of the diverged branch is refused; git's message comes through.
    let err = remote::push(&s.mine, &no_cancel()).unwrap_err();
    assert!(err.to_string().contains("rejected"), "{err}");
}

#[test]
fn publish_sets_upstream_and_push() {
    let s = setup("publish");
    write::create_branch(&s.mine, "feat/x").unwrap();
    commit(&s.mine, "x.txt", "x\n");
    assert_eq!(remote::push_target(&s.mine, "feat/x", None).as_deref(), Some("origin"));
    remote::push_to(&s.mine, "origin", true, &[], &no_cancel()).unwrap();
    assert_eq!(sh(&s.mine, &["rev-parse", "--abbrev-ref", "@{u}"]), "origin/feat/x");
    assert!(remote::remote_has_head(&s.mine, "origin", "feat/x"));
    commit(&s.mine, "y.txt", "y\n");
    assert!(!remote::remote_has_head(&s.mine, "origin", "feat/x"));
    remote::push(&s.mine, &no_cancel()).unwrap();
    assert_eq!(ab(&s.mine), "0\t0");
    assert_eq!(sh(&s.bare, &["rev-parse", "feat/x"]), sh(&s.mine, &["rev-parse", "HEAD"]));
    assert_eq!(remote::default_branch(&s.mine, "origin"), "main");
    assert_eq!(remote::last_commit(&s.mine).0, "y.txt");
    assert_eq!(remote::push_url(&s.mine, "origin").unwrap(), s.bare.to_string_lossy());
}

#[test]
fn branches_and_switching() {
    let s = setup("switch");
    sh(&s.other, &["switch", "-q", "-c", "side"]);
    commit(&s.other, "f.txt", "side\n");
    sh(&s.other, &["push", "-q", "-u", "origin", "side"]);
    remote::fetch(&s.mine, &no_cancel()).unwrap();
    commit(&s.mine, "local.txt", "l\n");

    let list = write::branches_full(&s.mine).unwrap();
    let names: Vec<_> = list.iter().map(|b| (b.name.as_str(), b.remote, b.current, b.ab)).collect();
    assert_eq!(names, vec![("main", false, true, Some((1, 0))), ("origin/side", true, false, None)]);

    // A dirty file the other branch changes: git refuses, nothing changes.
    std::fs::write(s.mine.join("f.txt"), "dirty\n").unwrap();
    let err = write::switch_remote(&s.mine, "origin/side").unwrap_err();
    assert!(err.to_string().contains("overwritten"), "{err}");
    assert_eq!(sh(&s.mine, &["branch", "--show-current"]), "main");
    assert_eq!(std::fs::read_to_string(s.mine.join("f.txt")).unwrap(), "dirty\n");

    sh(&s.mine, &["checkout", "-q", "--", "f.txt"]);
    write::switch_remote(&s.mine, "origin/side").unwrap();
    assert_eq!(sh(&s.mine, &["rev-parse", "--abbrev-ref", "@{u}"]), "origin/side");
    // A dirty file the other branch doesn't touch comes along.
    std::fs::write(s.mine.join("new.txt"), "n\n").unwrap();
    write::switch(&s.mine, "main").unwrap();
    assert!(s.mine.join("new.txt").exists());
    let list = write::branches_full(&s.mine).unwrap();
    assert_eq!(list.iter().filter(|b| b.remote).count(), 0, "origin/side has a local twin now");
}

#[test]
fn cancel_kills_a_hung_fetch() {
    let s = setup("hung");
    // The "ssh" never answers; git runs core.sshCommand through the shell with its args after `:`.
    sh(&s.mine, &["remote", "set-url", "origin", "ssh://nowhere.invalid/x.git"]);
    sh(&s.mine, &["config", "core.sshCommand", "sleep 30;:"]);
    let cancel = no_cancel();
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c2.store(true, Ordering::Relaxed);
    });
    let t = Instant::now();
    let err = remote::fetch(&s.mine, &cancel).unwrap_err();
    assert!(matches!(err, GitError::Cancelled), "{err}");
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());

    let t = Instant::now();
    let err = remote::run_tool("git", &s.mine, &["fetch"], &no_cancel(), Duration::from_millis(300)).unwrap_err();
    assert!(err.to_string().contains("was stopped"), "{err}");
    assert!(t.elapsed() < Duration::from_secs(3));
}

#[test]
fn unreachable_remote_shows_gits_error() {
    let s = setup("unreachable");
    sh(&s.mine, &["remote", "set-url", "origin", "https://127.0.0.1:9/x.git"]);
    let err = remote::fetch(&s.mine, &no_cancel()).unwrap_err().to_string();
    assert!(err.contains("127.0.0.1"), "{err}");
}
