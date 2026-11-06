//! Git writes from the panel, the diff view and the commit box: jobs run off the UI thread, one at a time per repo;
//! the watcher's refresh shows the result (no extra polling). Discards ask first (`Modal::Confirm`).

use super::*;
use crate::commit_box::{CommitBox, Props};
use crate::git_ops::{Done, Failed, Job, OpError};

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
            staged: st.as_ref().map_or(0, |s| s.files.iter().filter(|f| f.staged).count()),
            agent_files,
            scope_agent: self.commit_scope_agent(root),
            busy: self.git_ops.get(root).is_some_and(|o| o.inflight > 0),
        };
        b.update(cx, |b, cx| b.set_props(props, cx));
        b
    }

    pub(super) fn agent_ran(&self) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|t| t.claude_session.is_some() || !self.tracker.view(t.id as u32).touched.is_empty())
    }

    /// The active tab's agent files that are changed in `root` now.
    pub fn agent_files(&self, root: &Path) -> Vec<git::FileChange> {
        let (Some(t), Some(st)) = (self.tabs.get(self.active), self.repos.status(root)) else { return Vec::new() };
        let view = self.tracker.view(t.id as u32);
        st.files.iter().filter(|f| view.files.contains_key(&f.path)).cloned().collect()
    }

    /// The active session's repo root.
    pub(super) fn active_root(&self) -> Option<PathBuf> {
        match &self.tabs.get(self.active)?.repo {
            TabRepo::Repo(root) => Some(root.clone()),
            _ => None,
        }
    }

    /// A git job in the active session's repo (Command Center git commands); none → a status-bar note.
    pub(super) fn git_active(&mut self, job: Job, cx: &mut Context<Self>) {
        match self.active_root() {
            Some(root) => self.run_git(root, job, cx),
            None => self.flash("Not in a git repository".into(), cx),
        }
    }

    /// Runs a git write for `root` in the background (after any earlier one there finished).
    pub fn run_git(&mut self, root: PathBuf, job: Job, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.repo(&root).cloned() else { return };
        let ops = self.git_ops.entry(root.clone()).or_default();
        ops.inflight += 1;
        ops.view.running = Some(job.label());
        ops.view.cancellable = job.is_network();
        ops.view.error = None;
        ops.view.note = None;
        // A fresh flag per job: Cancel stops this one (queued jobs keep theirs).
        let cancel = git::remote::Cancel::default();
        ops.cancel = cancel.clone();
        let lock = ops.lock.clone();
        crate::perf::event_with(|| format!("git write: {}", job.label()));
        self.sync_panel(cx);
        cx.spawn(async move |this, cx| {
            let j = job.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let _one_at_a_time = lock.lock().unwrap_or_else(|e| e.into_inner());
                    j.run(&repo, &cancel)
                })
                .await;
            let _ = this.update(cx, |w, cx| w.git_done(root, job, result, cx));
        })
        .detach();
    }

    fn git_done(&mut self, root: PathBuf, job: Job, result: Result<Done, Failed>, cx: &mut Context<Self>) {
        let ops = self.git_ops.entry(root.clone()).or_default();
        ops.inflight = ops.inflight.saturating_sub(1);
        if ops.inflight == 0 {
            ops.view.running = None;
            ops.view.cancellable = false;
        }
        // A commit that went through (also when a later push in the same sequence failed) clears the box.
        let committed = match &result {
            Ok(_) => job.is_commit(),
            Err(f) => job.is_commit() && !f.retry.is_commit(),
        };
        if committed && let Some(b) = self.commit_boxes.get(&root) {
            b.update(cx, |b, cx| b.committed(cx));
        }
        match result {
            Ok(done) => {
                crate::perf::event_with(|| format!("git write done: {}", job.label()));
                ops.view.note = done.note;
                if let Some(url) = done.open {
                    cx.open_url(&url);
                }
            }
            Err(Failed { error, retry }) => {
                crate::perf::event_with(|| format!("git write failed: {} ({error})", job.label()));
                let locked = matches!(error, git::write::GitError::IndexLocked);
                let message = if locked {
                    "Another git process holds the index (index.lock). Nothing was changed.".to_string()
                } else {
                    error.to_string()
                };
                let cancelled = matches!(error, git::write::GitError::Cancelled);
                if cancelled {
                    ops.view.note = Some("Cancelled".into());
                } else {
                    ops.view.error = Some(OpError { message, locked, retry: Some(retry) });
                }
            }
        }
        // Refs a network job moved show through the watcher; a refresh covers packed-refs-only updates. A push or a
        // new request changes the strip.
        if job.is_network() {
            self.repos.refresh(&root);
            self.pr_lookup(root.clone(), false, cx);
        }
        self.sync_panel(cx);
    }

    /// Looks up the PR/MR strip for the repo's current branch off-thread (`recheck`: ask gh/glab about their login
    /// again). The panel re-renders only if the answer changed.
    pub(super) fn pr_lookup(&mut self, root: PathBuf, recheck: bool, cx: &mut Context<Self>) {
        let Some(branch) = self.repos.status(&root).map(|s| s.branch.clone()) else { return };
        let slot = self.prs.entry(root.clone()).or_default();
        if slot.branch != branch {
            slot.view = None;
        } else if slot.loading && !recheck {
            return;
        }
        slot.branch = branch.clone();
        if branch.is_empty() || branch.starts_with("(detached") {
            slot.loading = false;
            return;
        }
        slot.loading = true;
        crate::perf::event_with(|| format!("pr lookup ({})", if recheck { "recheck" } else { "branch/push" }));
        cx.spawn(async move |this, cx| {
            let (r, b) = (root.clone(), branch.clone());
            let view = cx.background_executor().spawn(async move { crate::pr::lookup(&r, &b, recheck) }).await;
            let _ = this.update(cx, |w, cx| {
                let Some(slot) = w.prs.get_mut(&root).filter(|s| s.branch == branch) else { return };
                slot.loading = false;
                if slot.view.as_ref() == Some(&view) {
                    return;
                }
                // Once ever: say what gh/glab would add (the link works without them).
                if matches!(view.info, crate::pr::Info::Link(_))
                    && !w.pr_hint_shown
                    && let Some(tool) = crate::pr::tool_for(view.forge)
                {
                    w.pr_hint_shown = true;
                    w.git_ops.entry(root.clone()).or_default().view.note = Some(format!(
                        "Tip: install {tool} and run `{tool} auth login` to see {} status here",
                        view.noun()
                    ));
                    w.mark_dirty(cx);
                }
                slot_set(w, &root, view);
                w.sync_panel(cx);
            });
        })
        .detach();
    }

    /// Settings → Git: one timer that fetches every watched repo every N minutes (none when 0).
    pub(super) fn restart_fetch_timer(&mut self, cx: &mut Context<Self>) {
        let minutes = self.settings.fetch_minutes;
        self.fetch_task = (minutes > 0).then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(minutes as u64 * 60)).await;
                    if this.update(cx, |w, cx| w.background_fetch(cx)).is_err() {
                        return;
                    }
                }
            })
        });
    }

    /// Fetches each watched repo that has a remote, one after another, under its write lock, without touching the
    /// panel (no progress label); a failure is a status-bar note. Ahead/behind and the PR strip follow.
    fn background_fetch(&mut self, cx: &mut Context<Self>) {
        let jobs: Vec<_> =
            self.repos.roots().map(|r| (r.clone(), self.git_ops.entry(r.clone()).or_default().lock.clone())).collect();
        crate::perf::event_with(|| format!("background fetch: {} repos", jobs.len()));
        cx.spawn(async move |this, cx| {
            for (root, lock) in jobs {
                let r = root.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        if git::remote::remotes(&r).is_empty() {
                            return Ok(false);
                        }
                        let _one_at_a_time = lock.lock().unwrap_or_else(|e| e.into_inner());
                        git::remote::fetch(&r, &git::remote::Cancel::default()).map(|()| true)
                    })
                    .await;
                let ok = this.update(cx, |w, cx| match result {
                    Ok(true) => {
                        w.repos.refresh(&root);
                        w.pr_lookup(root.clone(), false, cx);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        let name = root.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                        let first = e.to_string().lines().next().unwrap_or_default().to_string();
                        w.flash(format!("Background fetch failed in {name}: {first}"), cx);
                    }
                });
                if ok.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Stops the repo's running network job (git and its ssh are killed; nothing half-written: git's ref updates
    /// are atomic).
    pub fn cancel_git(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if let Some(o) = self.git_ops.get(&root) {
            o.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        cx.notify();
    }

    pub fn dismiss_git_note(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if let Some(o) = self.git_ops.get_mut(&root) {
            o.view.note = None;
            self.sync_panel(cx);
        }
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

    /// The ⋯ / Commit ▾ dropdown of the Changes panel at `at`.
    pub fn open_git_menu(
        &mut self,
        root: PathBuf,
        kind: GitMenu,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.open_menu(root, projects_ui::MenuKind::Git(kind), at, cx);
    }

    pub(super) fn git_menu_items(&self, root: &Path, kind: GitMenu, cx: &gpui::App) -> Vec<projects_ui::Item> {
        let agent = self.agent_ran() && !self.agent_files(root).is_empty();
        let gone = self.repos.status(root).is_some_and(|s| s.upstream.is_some() && s.ab.is_none());
        let pr = match self.prs.get(root).and_then(|p| p.view.as_ref()) {
            Some(v) if v.forge == git::remote::Forge::GitLab => "Commit, push & create MR",
            _ => "Commit, push & create PR",
        };
        match kind {
            GitMenu::More if gone => {
                let default = self.prs.get(root).and_then(|p| p.view.as_ref()?.default.clone());
                let mut v = vec![
                    (
                        "to_default",
                        match &default {
                            Some(d) => format!("Switch to {d} and pull").into(),
                            None => "Switch to the default branch and pull".into(),
                        },
                        "The usual next step after a merge (asks first with uncommitted changes)",
                    ),
                    ("republish", "Publish again…".into(), "Create the remote branch again (asks first)"),
                    ("fetch", "Fetch".into(), "Get the remote's new commits; nothing local changes"),
                    ("pr_refresh", "Refresh PR status".into(), "Asks gh / glab again (also whether you're logged in)"),
                ];
                if self.agent_ran() {
                    v.push((
                        "forget",
                        "Forget agent's files".into(),
                        "Clears this agent's file list; no file is touched",
                    ));
                }
                v
            }
            GitMenu::More => {
                let mut v = vec![
                    ("fetch", "Fetch".into(), "Get the remote's new commits; nothing local changes"),
                    ("pull", "Pull (fast-forward only)".into(), "Never merges or rebases"),
                    ("push", "Push".into(), "A branch without an upstream is published (push -u)"),
                    ("pr_refresh", "Refresh PR status".into(), "Asks gh / glab again (also whether you're logged in)"),
                ];
                if self.agent_ran() {
                    v.push((
                        "forget",
                        "Forget agent's files".into(),
                        "Clears this agent's file list; no file is touched",
                    ));
                }
                v
            }
            GitMenu::Commit => {
                let mut v = Vec::new();
                if agent {
                    v.push((
                        "commit_agent",
                        "Commit agent's files…".into(),
                        "Only this session's agent files (asks first)",
                    ));
                }
                v.push(("commit_push", "Commit & push".into(), "What the button commits, then push (publish)"));
                v.push(("commit_pr", pr.into(), "Commit, push, then open the new request"));
                v
            }
            GitMenu::DiffView => {
                let Some(Overlay::Diff(d)) = &self.overlay else { return Vec::new() };
                let (split, narrow, style, structural, why) = d.read(cx).view_state();
                let on = |b: bool| if b { "Shown now" } else { "" };
                let mut v = vec![
                    ("unified", "Unified".into(), on(!split)),
                    (
                        "split",
                        "Split".into(),
                        if narrow { "Too narrow here (needs 120 terminal columns)" } else { on(split) },
                    ),
                ];
                if split {
                    v.push((
                        "clean",
                        "Clean split".into(),
                        if style == crate::settings::SplitStyle::Clean {
                            "Shown now"
                        } else {
                            "Plain code, only changed tokens colored"
                        },
                    ));
                    v.push((
                        "classic",
                        "Classic split".into(),
                        if style == crate::settings::SplitStyle::Classic {
                            "Shown now"
                        } else {
                            "Line backgrounds and +/- markers"
                        },
                    ));
                }
                v.push((
                    "structural",
                    "Structural".into(),
                    why.unwrap_or(if structural {
                        "Shown now"
                    } else {
                        "Changed tokens, moved code; reformatting ignored"
                    }),
                ));
                v.push(("word", "Word".into(), on(!structural)));
                v
            }
        }
    }

    pub(super) fn git_menu_action(&mut self, root: PathBuf, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            "fetch" => self.run_git(root, Job::Fetch, cx),
            "pull" => self.run_git(root, Job::Pull, cx),
            "push" => self.run_git(root, Job::Push, cx),
            "pr_refresh" => self.pr_lookup(root, true, cx),
            "to_default" => {
                let d = self.prs.get(&root).and_then(|p| p.view.as_ref()?.default.clone());
                self.switch_branch(
                    root,
                    Job::SwitchDefault,
                    d.unwrap_or_else(|| "the default branch".into()),
                    window,
                    cx,
                )
            }
            "republish" => {
                let branch = self.repos.status(&root).map(|s| s.branch.clone()).unwrap_or_default();
                self.modal = Some(Modal::Confirm {
                    title: format!("Publish {branch} again?"),
                    body: "Its remote branch was deleted, usually after its pull/merge request was merged. Publishing \
                           creates it on the remote again (push -u)."
                        .into(),
                    files: Vec::new(),
                    ok: "Publish again",
                    danger: false,
                    then: Then::Git(root, Job::Republish),
                });
                window.focus(&self.focus, cx);
                cx.notify();
            }
            "forget" => {
                self.clear_agent_changes(cx);
                self.sync_panel(cx);
            }
            "commit_agent" => self.confirm_agent_commit(root, Vec::new(), window, cx),
            "commit_push" => self.commit_main(root, vec![Job::Push], window, cx),
            "commit_pr" => self.commit_main(root, vec![Job::CreatePr], window, cx),
            "unified" => self.set_diff_mode(crate::settings::DiffMode::Unified, cx),
            "split" => self.set_diff_mode(crate::settings::DiffMode::Split, cx),
            "clean" => self.set_split_style(crate::settings::SplitStyle::Clean, cx),
            "classic" => self.set_split_style(crate::settings::SplitStyle::Classic, cx),
            "structural" => self.set_diff_engine(crate::settings::DiffEngine::Auto, cx),
            "word" => self.set_diff_engine(crate::settings::DiffEngine::Word, cx),
            _ => {}
        }
    }

    /// Whether the commit box's main button commits the agent's files (This agent shown and it has some).
    pub(super) fn commit_scope_agent(&self, root: &Path) -> bool {
        self.agent_ran()
            && self.tabs.get(self.active).is_some_and(|t| t.agent_only.unwrap_or(true))
            && !self.agent_files(root).is_empty()
    }

    /// The commit box's main action (button, Ctrl+Enter), then `after` (push, create PR) in the same job.
    pub fn commit_main(&mut self, root: PathBuf, after: Vec<Job>, window: &mut Window, cx: &mut Context<Self>) {
        if self.commit_scope_agent(&root) {
            self.confirm_agent_commit(root, after, window, cx);
        } else {
            self.commit(root, after, cx);
        }
    }

    /// Commits the staged changes (then `after`).
    pub fn commit(&mut self, root: PathBuf, after: Vec<Job>, cx: &mut Context<Self>) {
        let Some(b) = self.commit_boxes.get(&root).cloned() else { return };
        let b = b.read(cx);
        let Some(message) = b.message() else { return };
        let amend = b.amend;
        self.run_git(root, then(Job::Commit { message, amend }, after), cx);
    }

    /// "Commit agent's files": lists exactly what will be committed (with the shared / via-command marks) and
    /// commits (then `after`) on Enter / the button.
    pub fn confirm_agent_commit(
        &mut self,
        root: PathBuf,
        after: Vec<Job>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(b) = self.commit_boxes.get(&root).cloned() else { return };
        let (message, amend) = {
            let b = b.read(cx);
            (b.message(), b.amend)
        };
        let Some(message) = message else { return };
        let files = self.agent_files(&root);
        let Some(t) = self.tabs.get(self.active).filter(|_| !files.is_empty()) else { return };
        let view = self.tracker.view(t.id as u32);
        let marked =
            files.iter().map(|f| (f.path.clone(), view.files.get(&f.path).copied().unwrap_or_default())).collect();
        let n = files.len();
        // A chained push / PR is part of what Enter does: the dialog says so.
        let (then_text, ok) = match after.last() {
            Some(Job::CreatePr) => (" Then push and open the pull/merge request.", "Commit, push & create PR"),
            Some(Job::Push) => (" Then push (a new branch is published).", "Commit & push"),
            _ => ("", if amend { "Amend" } else { "Commit" }),
        };
        self.modal = Some(Modal::Confirm {
            title: format!(
                "{} {n} file{} of this agent?",
                if amend { "Amend with" } else { "Commit" },
                if n == 1 { "" } else { "s" }
            ),
            body: format!(
                "Whole file contents are committed, including unstaged parts. Other staged changes stay staged and \
                 out of this commit.{then_text}"
            ),
            files: marked,
            ok,
            danger: false,
            then: Then::Git(root, then(Job::CommitOnly { message, files, amend }, after)),
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
        let (Some(b), Some(repo)) = (self.commit_boxes.get(&root).cloned(), self.repos.repo(&root).cloned()) else {
            return;
        };
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
            let result = cx
                .background_executor()
                .spawn(async move { crate::git_ops::generate_message(&repo, files.as_deref()) })
                .await;
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
        self.modal = Some(Modal::Confirm {
            title,
            body,
            files: Vec::new(),
            ok: "Discard",
            danger: true,
            then: Then::Git(root, job),
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The confirm dialog's action button (Discard / Commit / Close session / Restart shell).
    pub(super) fn confirm_ok(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Confirm { then, .. }) = self.modal.take() else { return };
        self.close_modal(window, cx);
        match then {
            Then::Git(root, job) => self.run_git(root, job, cx),
            Then::Close(id) => {
                if let Some(i) = self.tab_idx(id) {
                    self.close_now(i, window, cx);
                }
            }
            Then::Restart(id) => {
                if let Some(i) = self.tab_idx(id) {
                    self.restart_tab(i, window, cx);
                }
            }
        }
    }
}

fn slot_set(w: &mut Workspace, root: &Path, view: crate::pr::View) {
    if let Some(slot) = w.prs.get_mut(root) {
        slot.view = Some(view);
    }
}

/// `job`, or `job` then `after` as one sequence.
fn then(job: Job, after: Vec<Job>) -> Job {
    if after.is_empty() { job } else { Job::Seq(std::iter::once(job).chain(after).collect()) }
}
