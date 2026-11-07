//! Worktrees against real temp repos: discovery of a linked worktree, add errors, list (also made outside the app),
//! what removing would lose, remove/force, branch delete, prune, and the watcher seeing branch moves that live in
//! the shared `.git`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use git::write::{self, WorktreeState};
use git::{Discovery, Watcher};

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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `<tmp>/<name>/main` (a repo on `main` with two commits) and `<tmp>/<name>/wt` for worktrees.
fn setup(name: &str) -> (PathBuf, PathBuf) {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("dirigent-worktree-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();
    sh(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    sh(&main, &["add", "."]);
    sh(&main, &["commit", "-q", "-m", "one"]);
    std::fs::write(main.join("a.txt"), "one\ntwo\n").unwrap();
    sh(&main, &["commit", "-qam", "two"]);
    (main.canonicalize().unwrap(), base.canonicalize().unwrap().join("wt"))
}

#[test]
#[cfg_attr(windows, ignore = "Windows git step: canonicalize() gives \\\\?\\ paths git can't create")]
fn add_discover_list_and_errors() {
    let (main, wt) = setup("add");
    let (current, branches) = write::branches(&main).unwrap();
    assert_eq!((current.as_str(), branches), ("main", vec!["main".to_string()]));
    assert!(write::valid_branch(&main, "feat/login-fix"));
    assert!(
        !write::valid_branch(&main, "bad..name")
            && !write::valid_branch(&main, "-x")
            && !write::valid_branch(&main, "")
    );

    let feat = wt.join("feat");
    write::worktree_add(&main, "feat", &feat, "main").unwrap();
    let Discovery::Repo(r) = git::discover(&feat) else { panic!() };
    assert_eq!(r.root, feat);
    assert_eq!(r.main_root(), Some(main.as_path()), "linked: grouped under the main checkout");
    let Discovery::Repo(m) = git::discover(&main) else { panic!() };
    assert_eq!(m.main_root(), None);

    // Errors leave nothing behind.
    let err = write::worktree_add(&main, "feat", &wt.join("again"), "main").unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");
    assert!(!wt.join("again").exists());
    let err = write::worktree_add(&main, "other", &feat, "main").unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");
    assert!(write::worktree_add(&main, "x", &wt.join("x"), "no-such-base").is_err());
    assert!(!wt.join("x").exists());

    // One made outside the app (plain git) is listed too.
    sh(&main, &["worktree", "add", "-q", "--detach", wt.join("ext").to_str().unwrap()]);
    let list = write::worktree_list(&main).unwrap();
    let paths: Vec<_> = list.iter().map(|w| (w.path.clone(), w.branch.clone(), w.main)).collect();
    // Main first, the rest as git sorts them.
    assert_eq!(
        paths,
        vec![
            (main.clone(), Some("main".into()), true),
            (wt.join("ext"), None, false),
            (feat.clone(), Some("feat".into()), false)
        ]
    );

    // Its directory deleted by hand: prunable, then pruned.
    std::fs::remove_dir_all(wt.join("ext")).unwrap();
    assert!(write::worktree_list(&main).unwrap().iter().any(|w| w.path == wt.join("ext") && w.prunable));
    write::worktree_prune(&main).unwrap();
    assert_eq!(write::worktree_list(&main).unwrap().len(), 2);
}

#[test]
#[cfg_attr(windows, ignore = "Windows git step: canonicalize() gives \\\\?\\ paths git can't create")]
fn remove_never_loses_work_without_force() {
    let (main, wt) = setup("remove");
    let feat = wt.join("feat");
    write::worktree_add(&main, "feat", &feat, "main").unwrap();
    assert_eq!(write::worktree_state(&feat, Some("feat")).unwrap(), WorktreeState::default(), "same commits as main");

    // A commit only this branch has, plus an uncommitted and an untracked file.
    std::fs::write(feat.join("b.txt"), "b\n").unwrap();
    sh(&feat, &["add", "b.txt"]);
    sh(&feat, &["commit", "-qm", "b"]);
    std::fs::write(feat.join("a.txt"), "changed\n").unwrap();
    std::fs::write(feat.join("new.txt"), "n\n").unwrap();
    assert_eq!(write::worktree_state(&feat, Some("feat")).unwrap(), WorktreeState { dirty_files: 2, unpushed: 1 });

    // Plain remove refuses: files intact.
    assert!(write::worktree_remove(&main, &feat, false).is_err());
    assert_eq!(std::fs::read_to_string(feat.join("a.txt")).unwrap(), "changed\n");
    // An unmerged branch isn't deleted by -d.
    assert!(write::branch_delete(&main, "feat", false).is_err());

    // The commit also on another local branch: no longer "would be lost".
    sh(&main, &["branch", "keep", "feat"]);
    assert_eq!(write::worktree_state(&feat, Some("feat")).unwrap().unpushed, 0);

    // Confirmed: forced remove, then -D.
    write::worktree_remove(&main, &feat, true).unwrap();
    assert!(!feat.exists());
    write::branch_delete(&main, "feat", true).unwrap();
    assert_eq!(write::branches(&main).unwrap().1, vec!["keep".to_string(), "main".to_string()]);
    // A clean one goes without force.
    write::worktree_add(&main, "clean", &wt.join("clean"), "main").unwrap();
    write::worktree_remove(&main, &wt.join("clean"), false).unwrap();
    write::branch_delete(&main, "clean", false).unwrap();
}

#[test]
#[cfg_attr(windows, ignore = "Windows git step: canonicalize() gives \\\\?\\ paths git can't create")]
fn worktree_watcher_sees_branch_moves_in_the_shared_git_dir() {
    let (main, wt) = setup("watch");
    let feat = wt.join("feat");
    write::worktree_add(&main, "feat", &feat, "main").unwrap();
    let Discovery::Repo(r) = git::discover(&feat) else { panic!() };
    let (tx, rx) = mpsc::channel();
    let w = Watcher::start(r, move |u| {
        if u.status_changed {
            let _ = tx.send(());
        }
    })
    .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).expect("first status");
    assert_eq!(w.latest().unwrap().files.len(), 0);
    // `reset --soft` moves only refs/heads/feat (in the main checkout's .git), not this worktree's index or HEAD:
    // the last commit's change shows up as staged.
    sh(&feat, &["reset", "-q", "--soft", "HEAD~1"]);
    rx.recv_timeout(Duration::from_secs(5)).expect("a branch move in the shared .git refreshes the worktree");
    let st = w.latest().unwrap();
    assert_eq!((st.branch.as_str(), st.files.len()), ("feat", 1));
}
