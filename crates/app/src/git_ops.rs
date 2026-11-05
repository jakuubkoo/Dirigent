//! Git writes from the UI: jobs (run off the UI thread, one at a time per repo under that repo's lock), what the
//! panel shows about them (running / failed with a retry), and the commit-message generator.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

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
    Commit { message: String, amend: bool },
    /// Exactly these files' worktree content; everything else stays as it is.
    CommitOnly { message: String, files: Vec<FileChange>, amend: bool },
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
                if *amend { "Amending…".into() } else { "Committing…".into() }
            }
        }
    }

    pub fn is_commit(&self) -> bool {
        matches!(self, Job::Commit { .. } | Job::CommitOnly { .. })
    }

    /// Blocking: on a background thread.
    pub fn run(&self, repo: &Repo) -> Result<(), GitError> {
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
        }
    }
}

/// What the panel shows about a repo's writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpsView {
    /// The latest running job's label.
    pub running: Option<String>,
    pub error: Option<OpError>,
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
    let ask = if cut { "Write the commit message for this diff (cut at 60 KB)." } else { "Write the commit message for this diff." };
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
            !(t.starts_with("co-authored-by:") || t.contains("generated with") || t.starts_with("```") || t.starts_with("signed-off-by:"))
        })
        .collect();
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_messages_carry_no_attribution() {
        let raw = "```\nAdd hunk staging to the diff view\n\nStage or unstage one hunk.\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n🤖 Generated with [Claude Code](https://claude.com)\n```\n";
        assert_eq!(clean_message(raw), "Add hunk staging to the diff view\n\nStage or unstage one hunk.");
    }
}
