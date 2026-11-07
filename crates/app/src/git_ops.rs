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
    claude_p("claude", GENERATE_PROMPT, ask, &diff, &Stop::default()).map(|out| clean_message(&out))
}

/// Stops a running `claude -p` from another thread: `kill` only signals (no wait, fine on the UI thread); the
/// run's own thread reaps the process. Killed before the process started: it is killed as soon as it does.
#[derive(Clone, Default)]
pub struct Stop(Arc<Mutex<(Option<std::process::Child>, bool)>>);

impl Stop {
    pub fn kill(&self) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.1 = true;
        if let Some(c) = g.0.as_mut() {
            let _ = c.kill();
        }
    }
}

/// `claude -p` on Haiku with no tools, no saved session, none of your settings or hooks (your login): `system`
/// as the system prompt, `ask` as the prompt, `input` on stdin. `bin`: "claude" (a fake one in tests). Blocking.
fn claude_p(bin: &str, system: &str, ask: &str, input: &str, stop: &Stop) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(["-p", "--setting-sources", "", "--model", "haiku", "--tools", "", "--no-session-persistence"])
        .args(["--system-prompt", system, ask])
        .current_dir(std::env::temp_dir());
    run_stoppable(cmd, input, stop).map_err(|e| e.replace("{cmd}", "claude"))
}

/// Runs `cmd` with `input` on stdin and returns its stdout; `stop` can kill it meanwhile. Blocking.
fn run_stoppable(mut cmd: Command, input: &str, stop: &Stop) -> Result<String, String> {
    use std::io::Read as _;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't run {{cmd}}: {e}"))?;
    let (stdin, mut stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    {
        let mut g = stop.0.lock().unwrap_or_else(|e| e.into_inner());
        if g.1 {
            let _ = child.kill();
        }
        g.0 = Some(child);
    }
    if let Some(mut stdin) = stdin {
        let _ = stdin.write_all(input.as_bytes()); // dropped here: EOF
    }
    // stderr on its own thread: a full stderr pipe must not block stdout.
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let mut out = String::new();
    if let Some(o) = stdout.as_mut() {
        let _ = o.read_to_string(&mut out);
    }
    let err = err.join().unwrap_or_default();
    let (child, stopped) = {
        let mut g = stop.0.lock().unwrap_or_else(|e| e.into_inner());
        (g.0.take(), g.1)
    };
    let status = child.map(|mut c| c.wait()).transpose().map_err(|e| e.to_string())?;
    if stopped {
        return Err("Stopped".into());
    }
    if !status.is_some_and(|s| s.success()) {
        return Err(format!("{{cmd}} failed: {}{}", out.trim(), err.trim()));
    }
    Ok(out)
}

const NAME_PROMPT: &str = "You name coding sessions from the task a developer is about to give an agent. Reply with \
only a JSON object, no code fence, nothing else: {\"name\": \"<2 to 5 words, sentence case>\", \"branch\": \
\"<prefix><slug>\"}. Name the actual task: what gets built, fixed or changed. Ignore instructions about how to \
work, such as plan mode, process rules, testing, review or commit rules. Prefix: feat/ for a feature, fix/ for a bug \
fix, chore/ for maintenance, docs/ for documentation. Slug: 2 to 5 lowercase words joined by hyphens. If the prompt \
states a branch (a line like \"Branch: x\", or a Branch section such as \"Create feat/windows from main\"), \
return exactly that branch.";

/// A session name and a branch (prefix + slug) for a first prompt (`claude -p`, Haiku; see `claude_p`); `stop`
/// kills it. Blocking.
pub fn name_session(prompt: &str, stop: &Stop) -> Result<(String, String), String> {
    name_with("claude", prompt, stop)
}

fn name_with(bin: &str, prompt: &str, stop: &Stop) -> Result<(String, String), String> {
    let input: String = prompt.chars().take(4000).collect();
    let out = claude_p(bin, NAME_PROMPT, "Name the session for this task.", &input, stop)?;
    parse_name(&out).ok_or_else(|| format!("Unexpected reply: {}", out.trim()))
}

/// `{"name": …, "branch": …}` (also inside prose or a fence) → (name, branch); each `/` part of the branch goes
/// through `projects::slug`, a missing branch comes from the name.
fn parse_name(out: &str) -> Option<(String, String)> {
    let json = out.get(out.find('{')?..=out.rfind('}')?)?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|x| !x.is_empty());
    let name: String = field("name")?.chars().take(60).collect();
    let parts = field("branch").unwrap_or(&name).split('/').map(crate::projects::slug);
    let branch = parts.filter(|p| !p.is_empty()).collect::<Vec<_>>().join("/");
    (!branch.is_empty()).then_some((name, branch))
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
    fn a_stopped_run_ends_at_once() {
        let sleeper = || {
            let mut c = Command::new("sh");
            c.args(["-c", "cat >/dev/null; exec sleep 30"]);
            c
        };
        assert_eq!(run_stoppable(Command::new("cat"), "hi", &Stop::default()), Ok("hi".into()));
        let early = Stop::default();
        early.kill(); // before the process exists
        let t = std::time::Instant::now();
        assert_eq!(run_stoppable(sleeper(), "x", &early), Err("Stopped".into()));
        let stop = Stop::default();
        let s2 = stop.clone();
        let run = std::thread::spawn(move || run_stoppable(sleeper(), "x", &s2));
        stop.kill(); // whether it started yet or not
        assert_eq!(run.join().unwrap(), Err("Stopped".into()));
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
        let failed = run_stoppable(Command::new("false"), "", &Stop::default()).unwrap_err();
        assert!(failed.starts_with("{cmd} failed"), "{failed}");
    }

    #[test]
    fn parses_generated_names() {
        let out = r#"{"name": "Setup dialog naming", "branch": "fix/setup-naming"}"#;
        assert_eq!(parse_name(out), Some(("Setup dialog naming".into(), "fix/setup-naming".into())));
        let out = "Sure:\n```json\n{\"name\": \" Esc interrupt \", \"branch\": \"Feat/Esc Interrupt\"}\n```";
        assert_eq!(parse_name(out), Some(("Esc interrupt".into(), "feat/esc-interrupt".into())), "fenced, cleaned");
        assert_eq!(parse_name(r#"{"name": "Fix login"}"#), Some(("Fix login".into(), "fix-login".into())));
        assert_eq!(parse_name("I can't help with that."), None);
        assert_eq!(parse_name(r#"{"branch": "x"}"#), None, "no name");
    }

    /// A fake `claude` that checks it was asked on Haiku with the prompt on stdin and answers like Haiku does.
    #[test]
    fn names_through_a_fake_claude() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("dirigent-fake-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p.to_string_lossy().into_owned()
        };
        // Another test's fork can still hold the script's write fd for a moment (ETXTBSY): retry that only.
        let name_with = |bin: &str, prompt: &str, stop: &Stop| {
            let mut tries = 0;
            loop {
                match name_with(bin, prompt, stop) {
                    Err(e) if e.contains("Text file busy") && tries < 500 => std::thread::yield_now(),
                    r => break r,
                }
                tries += 1;
            }
        };
        let ok = fake(
            "claude-ok",
            r#"case "$*" in *"--model haiku"*"Ignore instructions about how to work"*) ;; *) exit 3;; esac
grep -q "Switch to plan mode first" || exit 4
echo '{"name": "Windows port", "branch": "feat/windows"}'"#,
        );
        let prompt = "Switch to plan mode first and show me the plan.\n\n## Branch\nCreate feat/windows from main\n\n\
                      ## Task\nPort the app to Windows.";
        assert_eq!(name_with(&ok, prompt, &Stop::default()), Ok(("Windows port".into(), "feat/windows".into())));
        let down = fake("claude-down", "echo 'API Error: overloaded' >&2; exit 1");
        assert!(name_with(&down, prompt, &Stop::default()).is_err());
        let junk = fake("claude-junk", "echo 'Here is a name: Windows'");
        assert!(name_with(&junk, prompt, &Stop::default()).unwrap_err().starts_with("Unexpected reply"));
        assert!(name_with(&dir.join("missing").to_string_lossy(), prompt, &Stop::default()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
