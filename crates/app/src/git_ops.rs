//! Git writes from the UI: jobs (run off the UI thread, one at a time per repo under that repo's lock), what the
//! panel shows about them (running / failed with a retry), and the commit-message generator.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use git::remote::{self, Cancel};
use git::write::{self, GitError, Span};
use git::{FileChange, Repo};

#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    Stage(Vec<FileChange>),
    Unstage(Vec<FileChange>),
    StageAll,
    Discard(FileChange),
    StageHunk(FileChange, Span),
    UnstageHunk(FileChange, Span),
    DiscardHunk(FileChange, Span),
    Commit {
        message: String,
        amend: bool,
    },
    /// Exactly these files' worktree content; everything else stays as it is.
    CommitOnly {
        message: String,
        files: Vec<FileChange>,
        amend: bool,
    },
    Fetch,
    /// Fast-forward to the upstream (never a merge or rebase).
    Pull,
    /// Push; without an upstream, publish to the push target (`-u`).
    Push,
    /// Fetch, then the one safe step: publish, fast-forward or push; diverged → explain, write nothing.
    Sync,
    Switch(String),
    /// `origin/x` → a local `x` tracking it.
    SwitchRemote(String),
    CreateBranch(String),
    /// The remote branch was deleted (merged): switch to the remote's default branch, then fast-forward it.
    SwitchDefault,
    /// Publish a branch whose remote branch was deleted (asked first: it creates it again).
    Republish,
    /// Open the branch's pull/merge request (pushing first when needed): via gh, GitLab push options, or the
    /// browser form.
    CreatePr,
    /// Several jobs in order (commit & push); stops at the first failure, whose retry is the rest.
    Seq(Vec<Job>),
}

/// What a finished job has to say: a note for the panel ("Pushed to origin/x") and a URL to open (a new PR).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Done {
    pub note: Option<String>,
    pub open: Option<String>,
}

impl Done {
    fn note(s: impl Into<String>) -> Self {
        Done { note: Some(s.into()), open: None }
    }

    fn then(self, next: Done) -> Done {
        Done { note: next.note.or(self.note), open: next.open.or(self.open) }
    }
}

/// A failed job: the error and what Retry runs (the job itself, or the rest of a sequence).
#[derive(Debug)]
pub struct Failed {
    pub error: GitError,
    pub retry: Job,
}

fn name(f: &FileChange) -> &str {
    f.path.rsplit('/').next().unwrap_or(&f.path)
}

fn count(files: &[FileChange]) -> String {
    match files {
        [f] => name(f).to_string(),
        _ => format!("{} files", files.len()),
    }
}

impl Job {
    /// The progress line ("Staging a.rs…").
    pub fn label(&self) -> String {
        match self {
            Job::Stage(f) => format!("Staging {}…", count(f)),
            Job::Unstage(f) => format!("Unstaging {}…", count(f)),
            Job::StageAll => "Staging all changes…".into(),
            Job::Discard(f) => format!("Discarding {}…", name(f)),
            Job::StageHunk(f, _) => format!("Staging a hunk of {}…", name(f)),
            Job::UnstageHunk(f, _) => format!("Unstaging a hunk of {}…", name(f)),
            Job::DiscardHunk(f, _) => format!("Discarding a hunk of {}…", name(f)),
            Job::Commit { amend, .. } | Job::CommitOnly { amend, .. } => {
                if *amend {
                    "Amending…".into()
                } else {
                    "Committing…".into()
                }
            }
            Job::Fetch => "Fetching…".into(),
            Job::Pull => "Pulling…".into(),
            Job::Push => "Pushing…".into(),
            Job::Sync => "Syncing…".into(),
            Job::Switch(b) | Job::SwitchRemote(b) => format!("Switching to {b}…"),
            Job::CreateBranch(b) => format!("Creating {b}…"),
            Job::SwitchDefault => "Switching to the default branch…".into(),
            Job::Republish => "Publishing again…".into(),
            Job::CreatePr => "Creating the pull request…".into(),
            Job::Seq(jobs) => jobs.first().map(Job::label).unwrap_or_default(),
        }
    }

    pub fn is_commit(&self) -> bool {
        match self {
            Job::Seq(jobs) => jobs.first().is_some_and(Job::is_commit),
            j => matches!(j, Job::Commit { .. } | Job::CommitOnly { .. }),
        }
    }

    /// Talks to a remote (can take long, can be cancelled).
    pub fn is_network(&self) -> bool {
        match self {
            Job::Seq(jobs) => jobs.iter().any(Job::is_network),
            j => matches!(
                j,
                Job::Fetch | Job::Pull | Job::Push | Job::Sync | Job::CreatePr | Job::SwitchDefault | Job::Republish
            ),
        }
    }

    /// Blocking: on a background thread.
    pub fn run(&self, repo: &Repo, cancel: &Cancel) -> Result<Done, Failed> {
        if let Job::Seq(jobs) = self {
            let mut done = Done::default();
            for (i, j) in jobs.iter().enumerate() {
                match j.run(repo, cancel) {
                    Ok(d) => done = done.then(d),
                    Err(f) => return Err(Failed { error: f.error, retry: Job::Seq(jobs[i..].to_vec()) }),
                }
            }
            return Ok(done);
        }
        self.run_one(repo, cancel).map_err(|error| Failed { error, retry: self.clone() })
    }

    fn run_one(&self, repo: &Repo, cancel: &Cancel) -> Result<Done, GitError> {
        let root = &repo.root;
        let quiet = |r: Result<(), GitError>| r.map(|()| Done::default());
        match self {
            Job::Fetch => remote::fetch(root, cancel).map(|()| Done::note("Fetched")),
            Job::Pull => {
                remote::fast_forward(root, cancel)?;
                Ok(Done::note(format!("Updated to {}", remote::upstream(root).unwrap_or_default())))
            }
            Job::Push => push(root, cancel),
            Job::Sync => sync(root, cancel),
            Job::Switch(b) => quiet(write::switch(root, b)),
            Job::SwitchRemote(b) => quiet(write::switch_remote(root, b)),
            Job::CreateBranch(b) => quiet(write::create_branch(root, b)),
            Job::SwitchDefault => switch_default(root, cancel),
            Job::Republish => publish(root, cancel),
            Job::CreatePr => {
                crate::pr::create(root, cancel).map(|(url, note)| Done { note: Some(note), open: Some(url) })
            }
            Job::Seq(_) => unreachable!("run handles sequences"),
            j => quiet(j.run_local(repo)),
        }
    }

    fn run_local(&self, repo: &Repo) -> Result<(), GitError> {
        match self {
            Job::Stage(f) => write::stage(repo, f),
            Job::Unstage(f) => write::unstage(repo, f),
            Job::StageAll => write::stage_all(repo),
            Job::Discard(f) => write::discard(repo, f),
            Job::StageHunk(f, s) => write::stage_hunk(repo, f, *s),
            Job::UnstageHunk(f, s) => write::unstage_hunk(repo, f, *s),
            Job::DiscardHunk(f, s) => write::discard_hunk(repo, f, *s),
            Job::Commit { message, amend } => write::commit(repo, message, *amend),
            Job::CommitOnly { message, files, amend } => write::commit_only(repo, message, files, *amend),
            _ => unreachable!("network and branch jobs run in run_one"),
        }
    }
}

/// Push, or publish when the branch has no upstream yet.
fn push(root: &std::path::Path, cancel: &Cancel) -> Result<Done, GitError> {
    // A deleted remote branch is never published again from here (only `Republish`, which asks first).
    if remote::upstream_gone(root) {
        return Err(GitError::Failed(GONE.into()));
    }
    if let Some(up) = remote::upstream(root) {
        remote::push(root, cancel)?;
        return Ok(Done::note(format!("Pushed to {up}")));
    }
    publish(root, cancel)
}

/// `push -u <push target> HEAD`.
fn publish(root: &std::path::Path, cancel: &Cancel) -> Result<Done, GitError> {
    let branch =
        remote::current_branch(root).ok_or_else(|| GitError::Failed("Not on a branch (detached HEAD)".into()))?;
    let Some(r) = remote::push_target(root, &branch, None) else {
        return Err(GitError::Failed(match remote::remotes(root).len() {
            0 => "This repository has no remote to push to. Add one with `git remote add`.".into(),
            _ => "Several remotes and none is the default: run `git push -u <remote> HEAD` in a terminal once.".into(),
        }));
    };
    remote::push_to(root, &r, true, &[], cancel)?;
    Ok(Done::note(format!("Published to {r}/{branch}")))
}

/// Why Sync / Push / Create PR stop on a branch whose remote branch is gone.
pub const GONE: &str = "The remote branch was deleted (merged?). Nothing was pushed. Use … → Switch to the default \
branch and pull, or … → Publish again… to create it again.";

/// Switches to the remote's default branch (a local one, else tracking the remote's) and fast-forwards it.
fn switch_default(root: &std::path::Path, cancel: &Cancel) -> Result<Done, GitError> {
    let branch = remote::current_branch(root).unwrap_or_default();
    let r = remote::push_target(root, &branch, None).ok_or_else(|| GitError::Failed("No remote".into()))?;
    let default = remote::default_branch(root, &r);
    remote::fetch(root, cancel)?;
    let local = write::branches_full(root)?.into_iter().any(|b| !b.remote && b.name == default);
    if local {
        write::switch(root, &default)?
    } else {
        write::switch_remote(root, &format!("{r}/{default}"))?
    }
    if remote::ahead_behind(root).is_some_and(|(a, b)| a == 0 && b > 0) {
        remote::fast_forward(root, cancel)?;
    }
    Ok(Done::note(format!("Switched to {default} (up to date with {r}/{default})")))
}

fn sync(root: &std::path::Path, cancel: &Cancel) -> Result<Done, GitError> {
    if remote::upstream_gone(root) {
        return Err(GitError::Failed(GONE.into()));
    }
    if remote::upstream(root).is_none() {
        return push(root, cancel);
    }
    remote::fetch(root, cancel)?;
    match remote::ahead_behind(root) {
        None => push(root, cancel), // the upstream vanished on fetch --prune: publish again
        Some((0, 0)) => Ok(Done::note("Up to date")),
        Some((0, _)) => {
            remote::fast_forward(root, cancel)?;
            Ok(Done::note("Pulled (fast-forward)"))
        }
        Some((_, 0)) => push(root, cancel),
        Some((a, b)) => Err(GitError::Failed(format!(
            "This branch and its upstream diverged ({a} ahead, {b} behind). Sync never merges or rebases: do that in a terminal."
        ))),
    }
}

/// What the panel shows about a repo's writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpsView {
    /// The latest running job's label.
    pub running: Option<String>,
    /// The running job talks to a remote: the panel offers Cancel.
    pub cancellable: bool,
    pub error: Option<OpError>,
    /// The last finished job's note ("Pushed to origin/x"), until the next job or a dismiss.
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpError {
    pub message: String,
    /// Another git process held index.lock: retrying is the fix.
    pub locked: bool,
    /// The job to run again (None: nothing to retry, e.g. a failed message generation).
    pub retry: Option<Job>,
}

/// Per repo: the lock that serializes its writes, and the view.
#[derive(Default)]
pub struct RepoOps {
    pub lock: Arc<Mutex<()>>,
    pub inflight: usize,
    pub view: OpsView,
    /// Cancels the running network job (a fresh flag per job).
    pub cancel: Cancel,
}

const GENERATE_PROMPT: &str = "You write git commit messages. Reply with only the message: an imperative subject line of \
at most 72 characters, optionally a blank line and a short body of plain sentences. No quotes, no code fences, no \
attribution, no co-author or \"generated with\" lines.";
const MAX_GENERATE_DIFF: usize = 60 * 1024;

/// A commit message from `claude -p` (Haiku, no tools, no saved session, none of your settings or hooks; your
/// login) for the staged diff, or (`files`) for exactly what committing those files would commit. Blocking.
pub fn generate_message(repo: &Repo, files: Option<&[FileChange]>) -> Result<String, String> {
    let (diff, cut) = match files {
        Some(f) => write::files_diff(repo, f, MAX_GENERATE_DIFF),
        None => write::staged_diff(repo, MAX_GENERATE_DIFF),
    }
    .map_err(|e| e.to_string())?;
    if diff.trim().is_empty() {
        return Err("Nothing to describe".into());
    }
    let ask = if cut {
        "Write the commit message for this diff (cut at 60 KB)."
    } else {
        "Write the commit message for this diff."
    };
    let mut child = Command::new("claude")
        .args(["-p", "--setting-sources", "", "--model", "haiku", "--tools", "", "--no-session-persistence"])
        .args(["--system-prompt", GENERATE_PROMPT, ask])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't run claude: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(diff.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stdout).trim().to_string() + String::from_utf8_lossy(&out.stderr).trim();
        return Err(format!("claude failed: {msg}"));
    }
    Ok(clean_message(&String::from_utf8_lossy(&out.stdout)))
}

/// Drops attribution lines and code fences a model may add anyway.
fn clean_message(raw: &str) -> String {
    let lines: Vec<&str> = raw
        .lines()
        .filter(|l| {
            let t = l.trim().to_ascii_lowercase();
            !(t.starts_with("co-authored-by:")
                || t.contains("generated with")
                || t.starts_with("```")
                || t.starts_with("signed-off-by:"))
        })
        .collect();
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_retry_the_rest() {
        let commit = Job::Commit { message: "m".into(), amend: false };
        let seq = Job::Seq(vec![commit.clone(), Job::Push]);
        assert!(seq.is_commit() && seq.is_network());
        assert_eq!(seq.label(), "Committing…");
        // Not a repo: the commit (first step) fails and Retry is the whole sequence.
        let d = std::env::temp_dir().join(format!("dirigent-seq-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let repo = Repo { root: d.clone(), git_dir: d.join(".git"), common_dir: d.join(".git") };
        let f = seq.run(&repo, &Cancel::default()).unwrap_err();
        assert_eq!(f.retry, seq);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A merged PR's branch: deleted on the remote, pruned by a (background) fetch. Nothing pushes it again
    /// except Republish; Switch to the default branch and pull is the way out.
    #[test]
    fn deleted_remote_branch_is_never_published_by_itself() {
        let root = std::env::temp_dir().join(format!("dirigent-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| -> String {
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
        };
        git(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        git(&root, &["clone", "-q", "remote.git", "mine"]);
        let mine = root.join("mine");
        for (k, v) in [("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false")] {
            git(&mine, &["config", k, v]);
        }
        std::fs::write(mine.join("a.txt"), "a\n").unwrap();
        git(&mine, &["add", "."]);
        git(&mine, &["commit", "-q", "-m", "a"]);
        git(&mine, &["push", "-q", "-u", "origin", "main"]);
        git(&mine, &["switch", "-q", "-c", "feat/done"]);
        std::fs::write(mine.join("b.txt"), "b\n").unwrap();
        git(&mine, &["add", "."]);
        git(&mine, &["commit", "-q", "-m", "b"]);
        let repo = Repo { root: mine.clone(), git_dir: mine.join(".git"), common_dir: mine.join(".git") };
        let c = Cancel::default();
        Job::Push.run(&repo, &c).unwrap(); // published
        let bare = root.join("remote.git");
        git(&bare, &["branch", "-D", "feat/done"]); // merged and deleted on the remote
        Job::Fetch.run(&repo, &c).unwrap(); // the background fetch: --prune
        let on_remote = || git(&bare, &["branch", "--list", "feat/done"]);
        assert!(on_remote().is_empty(), "a fetch never pushes");
        for job in [Job::Sync, Job::Push, Job::CreatePr, Job::Seq(vec![Job::Fetch, Job::Push])] {
            let f = job.run(&repo, &c).unwrap_err();
            assert!(f.error.to_string().contains("remote branch was deleted"), "{job:?}: {}", f.error);
            assert!(on_remote().is_empty(), "{job:?} published the branch again");
        }
        // The way out: the default branch, fast-forwarded.
        Job::SwitchDefault.run(&repo, &c).unwrap();
        assert_eq!(git(&mine, &["branch", "--show-current"]), "main");
        // Or, asked first in the UI: publish again.
        git(&mine, &["switch", "-q", "feat/done"]);
        Job::Republish.run(&repo, &c).unwrap();
        assert_eq!(on_remote(), "feat/done");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn generated_messages_carry_no_attribution() {
        let raw = "```\nAdd hunk staging to the diff view\n\nStage or unstage one hunk.\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n🤖 Generated with [Claude Code](https://claude.com)\n```\n";
        assert_eq!(clean_message(raw), "Add hunk staging to the diff view\n\nStage or unstage one hunk.");
    }
}
