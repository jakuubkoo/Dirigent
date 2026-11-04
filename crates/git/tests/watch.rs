//! The watcher against a real repo in a temp dir: refreshes only on relevant changes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use git::{Discovery, Watcher};

fn sh_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

fn repo(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("agent-ide-git-watch-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("target/debug")).unwrap();
    sh_git(&d, &["init", "-q", "-b", "main"]);
    std::fs::write(d.join(".gitignore"), "target/\n*.log\n").unwrap();
    std::fs::write(d.join("a.txt"), "one\ntwo\n").unwrap();
    sh_git(&d, &["add", "."]);
    sh_git(&d, &["commit", "-q", "-m", "init"]);
    d
}

/// Waits for the next status change (or fails after 5 s).
fn next(rx: &mpsc::Receiver<()>, w: &Watcher) -> std::sync::Arc<git::Status> {
    rx.recv_timeout(Duration::from_secs(5)).expect("status change");
    w.latest().unwrap()
}

/// Nothing changes for a while: no refresh at all, and no status change.
fn quiet(rx: &mpsc::Receiver<()>, w: &Watcher, what: &str) {
    let before = w.refreshes();
    assert!(rx.recv_timeout(Duration::from_millis(600)).is_err(), "{what}: status changed");
    assert_eq!(w.refreshes(), before, "{what}: git status ran");
}

#[test]
fn refreshes_only_on_relevant_changes() {
    let d = repo("main");
    let Discovery::Repo(r) = git::discover(&d.join("target")) else { panic!("not discovered") };
    assert_eq!(r.root.canonicalize().unwrap(), d.canonicalize().unwrap());
    let (tx, rx) = mpsc::channel();
    let w = Watcher::start(r, move |changed| {
        if changed {
            let _ = tx.send(());
        }
    })
    .unwrap();
    let st = next(&rx, &w);
    assert_eq!((st.branch.as_str(), st.files.len()), ("main", 0));
    quiet(&rx, &w, "idle after the first status (our own git calls must not trigger events)");

    std::fs::write(d.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    let st = next(&rx, &w);
    let f = &st.files[0];
    assert_eq!((f.path.as_str(), f.kind, f.staged, f.unstaged, f.lines), ("a.txt", 'M', false, true, Some((2, 1))));
    let refreshes = w.refreshes();

    // Build output: an ignored directory isn't even watched, an ignored file is filtered by check-ignore.
    for i in 0..50 {
        std::fs::write(d.join(format!("target/debug/out{i}.o")), "x").unwrap();
    }
    std::fs::write(d.join("build.log"), "log").unwrap();
    quiet(&rx, &w, "ignored writes");
    assert_eq!(w.refreshes(), refreshes);

    sh_git(&d, &["add", "a.txt"]);
    let st = next(&rx, &w);
    assert!(st.files[0].staged && !st.files[0].unstaged, "index change → staged");

    sh_git(&d, &["checkout", "-q", "-b", "feat"]);
    assert_eq!(next(&rx, &w).branch, "feat");

    // A new directory gets a watch: a file created in it later is seen.
    std::fs::create_dir(d.join("src")).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    std::fs::write(d.join("src/new.rs"), "fn main() {}\n").unwrap();
    let st = next(&rx, &w);
    let new = st.files.iter().find(|f| f.path == "src/new.rs").expect("untracked file in new dir");
    assert_eq!((new.kind, new.lines), ('?', Some((1, 0))));
    assert_eq!(st.totals(), (2, 3, 1));

    quiet(&rx, &w, "idle at the end");
    drop(w);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn diff_of_tracked_and_untracked_files() {
    let d = repo("diff");
    let Discovery::Repo(r) = git::discover(&d) else { panic!() };
    std::fs::write(d.join("a.txt"), "one\nTWO\n").unwrap();
    std::fs::write(d.join("b.txt"), "new\n").unwrap();
    let st = git::status(&r).unwrap();
    let a = st.files.iter().find(|f| f.path == "a.txt").unwrap();
    let diff = git::diff(&r, a, || false).unwrap();
    let text: Vec<_> = diff.lines.iter().filter(|l| l.kind != git::LineKind::Header).map(|l| l.text.as_str()).collect();
    assert_eq!(text, vec!["@@ -1,2 +1,2 @@", " one", "-two", "+TWO"]);
    let b = st.files.iter().find(|f| f.path == "b.txt").unwrap();
    let diff = git::diff(&r, b, || false).unwrap();
    assert!(diff.lines.iter().any(|l| l.kind == git::LineKind::Add && l.text == "+new"));
    assert!(git::diff(&r, a, || true).is_none(), "cancelled");
    assert_eq!(git::discover(Path::new("/")), Discovery::NotRepo);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn unborn_head() {
    let d = std::env::temp_dir().join(format!("agent-ide-git-watch-{}-unborn", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    sh_git(&d, &["init", "-q", "-b", "main"]);
    std::fs::write(d.join("x.txt"), "a\nb\n").unwrap();
    sh_git(&d, &["add", "x.txt"]);
    let Discovery::Repo(r) = git::discover(&d) else { panic!() };
    let st = git::status(&r).unwrap();
    let f = &st.files[0];
    assert_eq!((f.kind, f.staged, f.lines), ('A', true, Some((2, 0))));
    let diff = git::diff(&r, f, || false).unwrap();
    assert_eq!(diff.lines.iter().filter(|l| l.kind == git::LineKind::Add).count(), 2);
    let _ = std::fs::remove_dir_all(&d);
}
