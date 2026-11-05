//! Git writes from the panel, the diff view and the commit box: jobs run off the UI thread, one at a time per repo;
//! the watcher's refresh shows the result (no extra polling). Discards ask first (`Modal::Confirm`).

use super::*;
use crate::commit_box::{CommitBox, Props};
use crate::git_ops::{Job, OpError};

impl Workspace {
    /// The repo's commit box (created on first use; the draft lives as long as the app).
    pub(super) fn commit_box(&mut self, root: &Path, cx: &mut Context<Self>) -> Entity<CommitBox> {
        if let Some(b) = self.commit_boxes.get(root) {
            return b.clone();
        }
        let ws = cx.entity().downgrade();
        let root2 = root.to_path_buf();
        let b = cx.new(|cx| CommitBox::new(root2, ws, cx));
        self.commit_boxes.insert(root.to_path_buf(), b.clone());
        b
    }

    /// Pushes branch / staged count / the agent's files / busy into the repo's commit box.
    pub(super) fn sync_commit_box(&mut self, root: &Path, cx: &mut Context<Self>) -> Entity<CommitBox> {
        let b = self.commit_box(root, cx);
        let st = self.repos.status(root);
        let agent_files = self.agent_ran().then(|| self.agent_files(root).len());
        let props = Props {
            branch: st.as_ref().map(|s| s.branch.clone()).unwrap_or_default(),
            staged: st.as_ref().map_or(0, |s| s.files.iter().filter(|f| f.staged).count()),
            agent_files,
            busy: self.git_ops.get(root).is_some_and(|o| o.inflight > 0),
        };
        b.update(cx, |b, cx| b.set_props(props, cx));
        b
    }

    fn agent_ran(&self) -> bool {
        self.tabs.get(self.active).is_some_and(|t| {
            t.claude_session.is_some() || !self.tracker.view(t.id as u32).touched.is_empty()
        })
    }

    /// The active tab's agent files that are changed in `root` now.
    pub fn agent_files(&self, root: &Path) -> Vec<git::FileChange> {
        let (Some(t), Some(st)) = (self.tabs.get(self.active), self.repos.status(root)) else { return Vec::new() };
        let view = self.tracker.view(t.id as u32);
        st.files.iter().filter(|f| view.files.contains_key(&f.path)).cloned().collect()
    }

    /// Runs a git write for `root` in the background (after any earlier one there finished).
    pub fn run_git(&mut self, root: PathBuf, job: Job, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.repo(&root).cloned() else { return };
        let ops = self.git_ops.entry(root.clone()).or_default();
        ops.inflight += 1;
        ops.view.running = Some(job.label());
        ops.view.error = None;
        let lock = ops.lock.clone();
        crate::perf::event_with(|| format!("git write: {}", job.label()));
        self.sync_panel(cx);
        cx.spawn(async move |this, cx| {
            let j = job.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let _one_at_a_time = lock.lock().unwrap_or_else(|e| e.into_inner());
                    j.run(&repo)
                })
                .await;
            let _ = this.update(cx, |w, cx| w.git_done(root, job, result, cx));
        })
        .detach();
    }

    fn git_done(&mut self, root: PathBuf, job: Job, result: Result<(), git::write::GitError>, cx: &mut Context<Self>) {
        let ops = self.git_ops.entry(root.clone()).or_default();
        ops.inflight = ops.inflight.saturating_sub(1);
        if ops.inflight == 0 {
            ops.view.running = None;
        }
        match result {
            Ok(()) => {
                crate::perf::event_with(|| format!("git write done: {}", job.label()));
                if job.is_commit()
                    && let Some(b) = self.commit_boxes.get(&root)
                {
                    b.update(cx, |b, cx| b.committed(cx));
                }
            }
            Err(e) => {
                crate::perf::event_with(|| format!("git write failed: {} ({e})", job.label()));
                let locked = matches!(e, git::write::GitError::IndexLocked);
                let message = if locked {
                    "Another git process holds the index (index.lock). Nothing was changed.".to_string()
                } else {
                    e.to_string()
                };
                ops.view.error = Some(OpError { message, locked, retry: Some(job) });
            }
        }
        self.sync_panel(cx);
    }

    pub fn retry_git(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let job = self.git_ops.get_mut(&root).and_then(|o| o.view.error.take()).and_then(|e| e.retry);
        if let Some(job) = job {
            self.run_git(root, job, cx);
        }
    }

    pub fn dismiss_git_error(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if let Some(o) = self.git_ops.get_mut(&root) {
            o.view.error = None;
            self.sync_panel(cx);
        }
    }

    /// Commit box: commits the staged changes.
    pub fn commit(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(b) = self.commit_boxes.get(&root).cloned() else { return };
        let b = b.read(cx);
        let Some(message) = b.message() else { return };
        let amend = b.amend;
        self.run_git(root, Job::Commit { message, amend }, cx);
    }

    /// "Commit agent's files": lists exactly what will be committed (with the shared / via-command marks) and
    /// commits on Enter / the button.
    pub fn confirm_agent_commit(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(b) = self.commit_boxes.get(&root).cloned() else { return };
        let (message, amend) = {
            let b = b.read(cx);
            (b.message(), b.amend)
        };
        let Some(message) = message else { return };
        let files = self.agent_files(&root);
        let Some(t) = self.tabs.get(self.active).filter(|_| !files.is_empty()) else { return };
        let view = self.tracker.view(t.id as u32);
        let marked = files.iter().map(|f| (f.path.clone(), view.files.get(&f.path).copied().unwrap_or_default())).collect();
        let n = files.len();
        self.modal = Some(Modal::Confirm {
            title: format!("{} {n} file{} of this agent?", if amend { "Amend with" } else { "Commit" }, if n == 1 { "" } else { "s" }),
            body: "Whole file contents are committed, including unstaged parts. Other staged changes stay staged and \
                   out of this commit."
                .to_string(),
            files: marked,
            ok: if amend { "Amend" } else { "Commit" },
            danger: false,
            root,
            job: Job::CommitOnly { message, files, amend },
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Amend on: prefills the last commit's message when the box is empty.
    pub fn toggle_amend(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(b) = self.commit_boxes.get(&root).cloned() else { return };
        let (on, empty) = b.update(cx, |b, cx| {
            b.amend = !b.amend;
            cx.notify();
            (b.amend, b.area.text.trim().is_empty())
        });
        let Some(repo) = self.repos.repo(&root).cloned().filter(|_| on && empty) else { return };
        cx.spawn(async move |_, cx| {
            let last = cx.background_executor().spawn(async move { git::write::last_message(&repo) }).await;
            if let Some(m) = last {
                b.update(cx, |b, cx| {
                    if b.amend && b.area.text.trim().is_empty() {
                        b.set_message(&m, cx);
                    }
                });
            }
        })
        .detach();
    }

    /// Commit box "Generate": `claude -p` over the staged diff, or (`agent_only`) over what committing the agent's
    /// files would commit, fills the message (never commits).
    pub fn generate_message(&mut self, root: PathBuf, agent_only: bool, cx: &mut Context<Self>) {
        let (Some(b), Some(repo)) = (self.commit_boxes.get(&root).cloned(), self.repos.repo(&root).cloned()) else { return };
        if b.read(cx).generating {
            return;
        }
        let files = agent_only.then(|| self.agent_files(&root));
        if files.as_ref().is_some_and(|f| f.is_empty()) {
            return;
        }
        b.update(cx, |b, cx| {
            b.generating = true;
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { crate::git_ops::generate_message(&repo, files.as_deref()) }).await;
            b.update(cx, |b, cx| {
                b.generating = false;
                if let Ok(m) = &result {
                    b.set_message(m, cx);
                }
                cx.notify();
            });
            if let Err(e) = result {
                let _ = this.update(cx, |w, cx| {
                    let ops = w.git_ops.entry(root).or_default();
                    ops.view.error = Some(OpError { message: e, locked: false, retry: None });
                    w.sync_panel(cx);
                });
            }
        })
        .detach();
    }

    /// Asks before discarding (a file or a hunk); Enter/the red button runs `job`.
    pub fn confirm_discard(&mut self, root: PathBuf, job: Job, window: &mut Window, cx: &mut Context<Self>) {
        let (title, body) = match &job {
            Job::Discard(f) if f.kind == '?' => (
                format!("Discard {}?", f.path),
                "The untracked file goes to the trash.".to_string(),
            ),
            Job::Discard(f) => (
                format!("Discard all changes in {}?", f.path),
                "Staged and unstaged changes go back to the last commit (HEAD). Files the last commit doesn't have go to the trash.".to_string(),
            ),
            Job::DiscardHunk(f, _) => (
                format!("Discard this hunk of {}?", f.path),
                "Its staged and unstaged changes go back to the last commit (HEAD).".to_string(),
            ),
            _ => return,
        };
        self.modal = Some(Modal::Confirm { title, body, files: Vec::new(), ok: "Discard", danger: true, root, job });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The confirm dialog's action button (Discard / Commit).
    pub(super) fn confirm_ok(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Modal::Confirm { root, job, .. }) = self.modal.take() {
            self.close_modal(window, cx);
            self.run_git(root, job, cx);
        }
    }
}
