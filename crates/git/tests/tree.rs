//! Listings and directory change reports for the file tree, against real git and inotify.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use git::{DirWatcher, Discovery, Update, Watcher, list_dir};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dirigent-tree-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn listing_matches_git_check_ignore() {
    let d = tmp("ignore");
    git(&d, &["init", "-q"]);
    std::fs::write(d.join(".gitignore"), "*.log\n!keep.log\ntarget/\n").unwrap();
    std::fs::write(d.join(".git/info/exclude"), "secret\n").unwrap();
    for f in ["a.rs", "B.rs", "x.log", "keep.log", "secret", ".hidden"] {
        std::fs::write(d.join(f), "").unwrap();
    }
    for dir in ["target", "src", "Zeta", "sub"] {
        std::fs::create_dir(d.join(dir)).unwrap();
    }
    std::fs::write(d.join("sub/.gitignore"), "local.txt\n").unwrap();
    std::fs::write(d.join("sub/local.txt"), "").unwrap();
    std::fs::write(d.join("sub/other.txt"), "").unwrap();

    let top = list_dir(&d).unwrap();
    let names: Vec<_> = top.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["src", "sub", "target", "Zeta", ".gitignore", ".hidden", "a.rs", "B.rs", "keep.log", "secret", "x.log"],
        "dirs first, case-insensitive, no .git"
    );
    let mut all = top.entries.clone();
    all.extend(list_dir(&d.join("sub")).unwrap().entries);
    for e in &all {
        let rel = e.path.strip_prefix(&d).unwrap().to_str().unwrap();
        let by_git = !git(&d, &["check-ignore", rel]).trim().is_empty();
        assert_eq!(e.ignored, by_git, "{rel}");
    }
    let ignored: Vec<_> = all.iter().filter(|e| e.ignored).map(|e| e.name.as_str()).collect();
    assert_eq!(ignored, vec!["target", "secret", "x.log", "local.txt"]);
    // Outside a repo nothing is ignored.
    let plain = tmp("plain");
    std::fs::write(plain.join("x.log"), "").unwrap();
    assert!(!list_dir(&plain).unwrap().entries[0].ignored);
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&plain);
}

#[test]
fn repo_watcher_reports_changed_dirs() {
    let d = tmp("dirs");
    git(&d, &["init", "-q"]);
    std::fs::write(d.join(".gitignore"), "target/\n*.log\n").unwrap();
    std::fs::create_dir_all(d.join("target/debug")).unwrap();
    std::fs::create_dir(d.join("src")).unwrap();
    std::fs::write(d.join("src/a.rs"), "a").unwrap();
    let Discovery::Repo(r) = git::discover(&d) else { panic!() };
    let (tx, rx) = mpsc::channel::<Update>();
    let _w = Watcher::start(r.clone(), move |u| {
        let _ = tx.send(u);
    })
    .unwrap();
    let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(first.refreshed && first.dirs.is_empty());
    let next_dirs = || -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        while let Ok(u) = rx.recv_timeout(Duration::from_millis(600)) {
            dirs.extend(u.dirs);
        }
        dirs.sort();
        dirs.dedup();
        dirs
    };
    let src = r.root.join("src");

    std::fs::write(d.join("src/b.rs"), "b").unwrap();
    assert_eq!(next_dirs(), vec![src.clone()], "create");
    std::fs::write(d.join("src/b.rs"), "changed").unwrap();
    assert!(next_dirs().is_empty(), "a content write doesn't change the listing");
    std::fs::rename(d.join("src/b.rs"), d.join("src/c.rs")).unwrap();
    assert_eq!(next_dirs(), vec![src.clone()], "rename");
    std::fs::remove_file(d.join("src/c.rs")).unwrap();
    assert_eq!(next_dirs(), vec![src], "remove");
    std::fs::write(d.join("build.log"), "").unwrap();
    assert_eq!(next_dirs(), vec![r.root.clone()], "an ignored file still changes its (watched) dir's listing");
    for i in 0..20 {
        std::fs::write(d.join(format!("target/debug/{i}.o")), "").unwrap();
    }
    assert!(rx.recv_timeout(Duration::from_millis(600)).is_err(), "ignored dirs aren't watched: no update at all");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn dir_watcher_refcounts() {
    let d = tmp("plainwatch");
    std::fs::create_dir(d.join("sub")).unwrap();
    let (tx, rx) = mpsc::channel();
    let w = DirWatcher::start(move |dirs| {
        let _ = tx.send(dirs);
    })
    .unwrap();
    let settle = || std::thread::sleep(Duration::from_millis(100));
    w.watch(&d);
    w.watch(&d); // a second tab with the same dir expanded
    settle();
    assert_eq!(w.watched(), 1);
    std::fs::write(d.join("new.txt"), "").unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), vec![d.clone()]);
    std::fs::write(d.join("sub/inner.txt"), "").unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "sub isn't expanded, so not watched");
    w.unwatch(&d);
    settle();
    assert_eq!(w.watched(), 1, "still expanded in the other tab");
    w.unwatch(&d);
    settle();
    assert_eq!(w.watched(), 0);
    std::fs::write(d.join("later.txt"), "").unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    let _ = std::fs::remove_dir_all(&d);
}
