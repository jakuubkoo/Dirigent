//! Git writes against real temp repos: hunks (partially staged), discard, trash, commit --only, a held
//! index.lock, a failing pre-commit hook, an unborn HEAD.

use std::path::{Path, PathBuf};
use std::process::Command;

use git::write::{self, GitError, Span};
use git::{Discovery, FileChange, Repo};

fn sh(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo with `f.txt` = 1..30 committed; local identity so commits work without the user's config.
fn repo(name: &str) -> (Repo, PathBuf) {
    // Under target/ (the home filesystem): GNOME's trash refuses system mounts like /tmp.
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("dirigent-git-write-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    sh(&d, &["init", "-q", "-b", "main"]);
    sh(&d, &["config", "user.name", "t"]);
    sh(&d, &["config", "user.email", "t@t"]);
    sh(&d, &["config", "commit.gpgsign", "false"]);
    let lines: String = (1..=30).map(|i| format!("{i}\n")).collect();
    std::fs::write(d.join("f.txt"), lines).unwrap();
    sh(&d, &["add", "."]);
    sh(&d, &["commit", "-q", "-m", "init"]);
    let Discovery::Repo(r) = git::discover(&d) else { panic!() };
    (r, d)
}

fn edit(d: &Path, from: &str, to: &str) {
    let p = d.join("f.txt");
    let s = std::fs::read_to_string(&p).unwrap().replace(&format!("\n{from}\n"), &format!("\n{to}\n"));
    std::fs::write(p, s).unwrap();
}

fn file(r: &Repo, path: &str) -> FileChange {
    git::status(r).unwrap().files.into_iter().find(|f| f.path == path).unwrap()
}

/// The view's hunks (HEAD→worktree spans), as the diff view gets them.
fn view_spans(r: &Repo, f: &FileChange) -> Vec<Span> {
    let d = git::diff(r, f, || false).unwrap();
    git::layout::layout(&d).lines.iter().filter_map(|l| l.span).collect()
}

#[test]
fn hunks_partially_staged() {
    let (r, d) = repo("hunks");
    // 5 and 20 staged, 25 unstaged: the view shows 5 alone and 20+25 as one hunk.
    edit(&d, "5", "five");
    edit(&d, "20", "twenty");
    sh(&d, &["add", "f.txt"]);
    edit(&d, "25", "tf");
    let f = file(&r, "f.txt");
    let spans = view_spans(&r, &f);
    assert_eq!(spans.len(), 2, "{spans:?}");
    assert_eq!(write::hunk_state(&r, &f, &spans), [(true, false), (true, true)]);
    // Stage the mixed hunk: only its unstaged part (25) moves; nothing else changes.
    write::stage_hunk(&r, &f, spans[1]).unwrap();
    assert_eq!(sh(&d, &["diff"]), "", "nothing unstaged left");
    assert_eq!(write::hunk_state(&r, &f, &spans), [(true, false), (true, false)]);
    // Unstage the first hunk only: 5 back to unstaged, 20 and 25 stay staged.
    write::unstage_hunk(&r, &f, spans[0]).unwrap();
    let cached = sh(&d, &["diff", "--cached"]);
    assert!(!cached.contains("+five") && cached.contains("+twenty") && cached.contains("+tf"), "{cached}");
    assert!(sh(&d, &["diff"]).contains("+five"));
    // Discard the second hunk: back to HEAD in index and worktree; the first stays.
    let f = file(&r, "f.txt");
    write::discard_hunk(&r, &f, view_spans(&r, &f)[1]).unwrap();
    let wt = std::fs::read_to_string(d.join("f.txt")).unwrap();
    assert!(wt.contains("\n20\n") && wt.contains("\n25\n") && wt.contains("\nfive\n"), "{wt}");
    assert_eq!(sh(&d, &["diff", "--cached"]), "", "index back to HEAD there");
}

#[test]
fn files_stage_unstage_discard_and_trash() {
    let (r, d) = repo("files");
    edit(&d, "3", "three");
    std::fs::write(d.join("new.txt"), "n\n").unwrap();
    let (f, n) = (file(&r, "f.txt"), file(&r, "new.txt"));
    write::stage(&r, &[f.clone(), n.clone()]).unwrap();
    assert_eq!(sh(&d, &["diff", "--cached", "--name-only"]), "f.txt\nnew.txt\n");
    write::unstage(&r, std::slice::from_ref(&n)).unwrap();
    assert_eq!(sh(&d, &["diff", "--cached", "--name-only"]), "f.txt\n");
    write::discard(&r, &file(&r, "f.txt")).unwrap();
    assert_eq!(sh(&d, &["status", "--porcelain"]), "?? new.txt\n");
    // Untracked → the trash (a temp XDG_DATA_HOME on the same filesystem), never deleted.
    let data = d.join("xdg-data");
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY: tests in this binary that read XDG_DATA_HOME don't run concurrently with a different value.
    unsafe { std::env::set_var("XDG_DATA_HOME", &data) };
    write::discard(&r, &file(&r, "new.txt")).unwrap();
    assert!(!d.join("new.txt").exists());
    assert!(data.join("Trash/files/new.txt").exists(), "moved to the trash");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn commit_only_leaves_the_rest() {
    let (r, d) = repo("only");
    std::fs::write(d.join("other.txt"), "o\n").unwrap();
    sh(&d, &["add", "other.txt"]); // staged by someone else
    edit(&d, "7", "seven"); // the agent's file, unstaged
    std::fs::write(d.join("agent-new.txt"), "a\n").unwrap(); // the agent's, untracked
    let files = [file(&r, "f.txt"), file(&r, "agent-new.txt")];
    let (d_agent, cut) = write::files_diff(&r, &files, 60_000).unwrap();
    assert!(!cut);
    assert!(d_agent.contains("+seven") && d_agent.contains("new file agent-new.txt\n+a\n"), "{d_agent}");
    assert!(!d_agent.contains("other.txt"), "only the agent's files: {d_agent}");
    write::commit_only(&r, "Agent work", &files, false).unwrap();
    let shown = sh(&d, &["show", "--name-only", "--format=%s", "HEAD"]);
    assert_eq!(shown, "Agent work\n\nagent-new.txt\nf.txt\n");
    assert_eq!(sh(&d, &["status", "--porcelain"]), "A  other.txt\n", "still staged, not committed");
    assert_eq!(write::last_message(&r).as_deref(), Some("Agent work"));
    write::commit(&r, "Agent work, amended", true).unwrap();
    assert_eq!(sh(&d, &["log", "--format=%s"]), "Agent work, amended\ninit\n");
}

#[test]
fn held_index_lock_fails_cleanly_and_retries() {
    let (r, d) = repo("lock");
    edit(&d, "9", "nine");
    let lock = d.join(".git/index.lock");
    std::fs::write(&lock, "").unwrap(); // another git process
    let before = std::fs::read(d.join(".git/index")).unwrap();
    let f = file(&r, "f.txt");
    assert!(matches!(write::stage(&r, std::slice::from_ref(&f)), Err(GitError::IndexLocked)));
    assert!(matches!(write::stage_hunk(&r, &f, view_spans(&r, &f)[0]), Err(GitError::IndexLocked)));
    assert_eq!(std::fs::read(d.join(".git/index")).unwrap(), before, "index untouched");
    assert!(lock.exists(), "we never remove someone else's lock");
    std::fs::remove_file(&lock).unwrap(); // the other process finished
    write::stage(&r, &[f]).unwrap();
    assert_eq!(sh(&d, &["diff", "--cached", "--name-only"]), "f.txt\n");
}

#[test]
fn failing_pre_commit_hook_shows_its_output() {
    let (r, d) = repo("hook");
    let hook = d.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'lint: 2 problems' >&2\nexit 1\n").unwrap();
    std::os::unix::fs::PermissionsExt::set_mode(&mut std::fs::metadata(&hook).unwrap().permissions(), 0o755);
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    edit(&d, "2", "two");
    sh(&d, &["add", "f.txt"]);
    match write::commit(&r, "x", false) {
        Err(GitError::Failed(out)) => assert!(out.contains("lint: 2 problems"), "{out}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(sh(&d, &["diff", "--cached", "--name-only"]), "f.txt\n", "still staged");
}

#[test]
fn unborn_head() {
    let d = std::env::temp_dir().join(format!("dirigent-git-write-{}-unborn", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    sh(&d, &["init", "-q", "-b", "main"]);
    std::fs::write(d.join("a.txt"), "a\n").unwrap();
    let Discovery::Repo(r) = git::discover(&d) else { panic!() };
    let a = file(&r, "a.txt");
    write::stage(&r, std::slice::from_ref(&a)).unwrap();
    write::unstage(&r, &[file(&r, "a.txt")]).unwrap();
    assert_eq!(sh(&d, &["status", "--porcelain"]), "?? a.txt\n");
}
