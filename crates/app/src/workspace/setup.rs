//! The New session setup dialog (name, shared directory or a new git worktree, claude + first prompt), the
//! close-a-worktree dialog (keep / remove, with what would be lost) and the project's Worktrees list. Every git call
//! runs off the UI thread (`git::write`); the dialogs only show its results.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AnyElement, ClickEvent, FontWeight, SharedString, TextLayout, deferred, div};

use super::*;
use crate::projects::{self, PREFIXES};
use crate::text_area::{Key, TextArea};
use crate::theme::{self, MONO_FONT, radius, space, text_size, z};
use crate::ui::{self, icon};
use git::write::{Worktree, WorktreeState};

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Field {
    Name,
    Mode,
    Prefix,
    Branch,
    Base,
    Location,
    Claude,
    Prompt,
}

pub(super) struct Setup {
    /// The project: its directory in shared mode, the repo new worktrees branch from.
    pub project: PathBuf,
    /// Branches listed (off-thread): None while loading; Some(empty current + empty list) = not a git repo.
    pub branches: Option<(String, Vec<String>)>,
    pub name: LineEdit,
    /// Typed by hand and not empty: the first prompt no longer names the session.
    name_edited: bool,
    /// The name and branch slug claude made from the first prompt (None: its first words).
    generated: Option<(String, String)>,
    /// Bumped by every prompt change: a naming run for an older prompt is dropped.
    gen_seq: u64,
    /// The running naming call (at most one: a new one or closing the dialog kills it) and the prompt version it
    /// names; `named_for`: the last version a call finished for (named, or failed: the first words stay).
    naming_run: Option<(crate::git_ops::Stop, u64)>,
    named_for: Option<u64>,
    pub naming: bool,
    /// Create pressed while the name still comes from the prompt: creates once the naming call is done.
    create_pending: bool,
    pub worktree: bool,
    /// `PREFIXES` index put before the branch field's text.
    pub prefix: usize,
    /// The project's remembered prefix; picked in this dialog (a `Branch:` line then no longer sets it).
    prefix_saved: usize,
    prefix_picked: bool,
    pub prefix_open: bool,
    pub branch: LineEdit,
    branch_edited: bool,
    /// Filters the base branch list.
    pub base: LineEdit,
    pub base_sel: usize,
    pub location: LineEdit,
    location_edited: bool,
    pub claude: bool,
    pub prompt: TextArea,
    prompt_layouts: Rc<RefCell<Vec<TextLayout>>>,
    pub focus: Field,
    pub busy: bool,
    pub error: Option<(Option<Field>, String)>,
}

impl Setup {
    fn new(project: PathBuf, claude: bool, prefix_saved: usize) -> Self {
        Setup {
            project,
            branches: None,
            name: LineEdit::new(""),
            name_edited: false,
            generated: None,
            gen_seq: 0,
            naming_run: None,
            named_for: None,
            naming: false,
            create_pending: false,
            worktree: false,
            prefix: prefix_saved,
            prefix_saved,
            prefix_picked: false,
            prefix_open: false,
            branch: LineEdit::new(""),
            branch_edited: false,
            base: LineEdit::new(""),
            base_sel: 0,
            location: LineEdit::new(""),
            location_edited: false,
            claude,
            prompt: TextArea::default(),
            prompt_layouts: Default::default(),
            focus: Field::Name,
            busy: false,
            error: None,
        }
    }

    fn is_repo(&self) -> bool {
        self.branches.as_ref().is_some_and(|(_, b)| !b.is_empty())
    }

    fn fields(&self) -> Vec<Field> {
        let mut f = vec![Field::Name, Field::Mode];
        if self.worktree {
            f.extend([Field::Prefix, Field::Branch, Field::Base, Field::Location]);
        }
        f.push(Field::Claude);
        if self.claude {
            f.push(Field::Prompt);
        }
        f
    }

    /// Base candidates: the current branch first, then the rest (fuzzy-filtered when typing).
    pub fn bases(&self) -> Vec<String> {
        let Some((current, all)) = &self.branches else { return Vec::new() };
        let q = self.base.text.trim();
        let mut list: Vec<(i32, &String)> = all
            .iter()
            .filter_map(|b| projects::fuzzy(q, b).map(|m| (m.score + if b == current { 1000 } else { 0 }, b)))
            .collect();
        list.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        list.into_iter().map(|(_, b)| b.clone()).collect()
    }

    fn location_path(&self) -> PathBuf {
        let t = self.location.text.trim();
        match t.strip_prefix('~') {
            Some(rest) => home().join(rest.trim_start_matches('/')),
            None if Path::new(t).is_absolute() => PathBuf::from(t),
            None => self.project.join(t),
        }
    }

    /// The name still comes from the prompt and no naming call finished for this version of it.
    fn wants_name(&self) -> bool {
        self.claude && !self.name_edited && !self.prompt.text.trim().is_empty() && self.named_for != Some(self.gen_seq)
    }

    /// The branch to create: the prefix and the branch field.
    fn full_branch(&self) -> String {
        format!("{}{}", PREFIXES[self.prefix], self.branch.text.trim())
    }

    /// First prompt → name (until typed) → branch slug (a `Branch:` line wins) → location, each until edited.
    fn derive(&mut self, worktrees_dir: Option<&Path>) {
        // Without claude there is no first prompt.
        let prompt = if self.claude { self.prompt.text.as_str() } else { "" };
        let generated = self.generated.clone().filter(|_| self.claude);
        let explicit = projects::branch_line(prompt);
        if !self.name_edited {
            let name = match &generated {
                Some((n, _)) => n.clone(),
                None => projects::fallback_name(prompt),
            };
            if name != self.name.text {
                self.name = LineEdit::new(name);
            }
        }
        if !self.prefix_picked {
            self.prefix = explicit.as_ref().map_or(self.prefix_saved, |(i, _)| *i);
        }
        if !self.branch_edited {
            let b = match (explicit, &generated) {
                (Some((_, b)), _) => b,
                (None, Some((_, slug))) if !self.name_edited => slug.clone(),
                _ => projects::slug(&self.name.text),
            };
            self.branch = LineEdit::new(b);
        }
        if !self.location_edited {
            let b = if self.branch.text.trim().is_empty() { "new".to_string() } else { self.full_branch() };
            let p = projects::worktree_path(&self.project, worktrees_dir, &b);
            self.location = LineEdit::new(tilde(&p));
        }
    }
}

/// Closing the dialog stops a naming call still running.
impl Drop for Setup {
    fn drop(&mut self) {
        if let Some((stop, _)) = self.naming_run.take() {
            stop.kill();
        }
    }
}

/// Closing the last session in a linked worktree (or Remove… from the Worktrees list).
pub(super) struct CloseWt {
    pub path: PathBuf,
    pub main: PathBuf,
    pub branch: Option<String>,
    /// The session being closed (None: from the Worktrees list).
    pub tab: Option<usize>,
    /// What removing would lose (read off-thread; None while reading).
    pub state: Option<Result<WorktreeState, String>>,
    pub understand: bool,
    pub delete_branch: bool,
    pub busy: bool,
    pub error: Option<String>,
}

impl CloseWt {
    fn at_risk(&self) -> bool {
        matches!(&self.state, Some(Ok(s)) if s.dirty_files > 0 || s.unpushed > 0)
    }

    fn can_remove(&self) -> bool {
        !self.busy && matches!(self.state, Some(Ok(_))) && (!self.at_risk() || self.understand)
    }
}

pub(super) struct WtList {
    pub main: PathBuf,
    pub list: Option<Result<Vec<Worktree>, String>>,
    pub sel: usize,
    pub busy: bool,
}

impl Workspace {
    /// The setup dialog for a new session in `project` (picker Enter, `▾ New session…`). Lists branches off-thread.
    pub(super) fn open_setup(&mut self, project: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let p = self.projects.get(&project).cloned().unwrap_or_default();
        let mut setup = Setup::new(project.clone(), p.auto_claude, projects::prefix_index(p.branch_prefix.as_deref()));
        setup.derive(p.worktrees_dir.as_deref());
        self.modal = Some(Modal::Setup(Box::new(setup)));
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { git::write::branches(&project).ok() }).await;
            let _ = this.update(cx, |w, cx| {
                if let Some(Modal::Setup(s)) = &mut w.modal {
                    s.branches = Some(found.unwrap_or_default());
                    cx.notify();
                }
            });
        })
        .detach();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// `DIRIGENT_PERF_SETUP`: the dialog in worktree mode with `prompt` typed into First prompt.
    pub(super) fn perf_setup(&mut self, project: PathBuf, prompt: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_setup(project, window, cx);
        if let Some(Modal::Setup(s)) = &mut self.modal {
            s.worktree = true;
            s.claude = true;
            s.prompt.insert(prompt, false);
            s.focus = Field::Prompt;
        }
        self.setup_prompt_changed(cx);
        self.setup_name(window, cx);
    }

    pub(super) fn setup_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some(Modal::Setup(s)) = &mut self.modal else { return };
        let ks = &ev.keystroke;
        if s.create_pending && ks.key == "escape" {
            return self.close_modal(window, cx); // closing kills the naming call
        }
        if s.busy || s.create_pending {
            return; // creating the worktree, or naming before it: wait
        }
        let m = ks.modifiers;
        let ctrl = m.control || m.platform;
        let key = ks.key.as_str();
        let cur = s.prefix;
        let was = s.focus;
        if s.prefix_open {
            match key {
                "up" => return self.setup_pick_prefix(s_prev(cur), false, cx),
                "down" => return self.setup_pick_prefix((cur + 1).min(PREFIXES.len() - 1), false, cx),
                "escape" | "enter" | "space" | "tab" => s.prefix_open = false,
                _ => {}
            }
            return cx.notify();
        }
        let fields = s.fields();
        let at = fields.iter().position(|f| *f == s.focus).unwrap_or(0);
        let step =
            |s: &mut Setup, d: isize| s.focus = fields[(at as isize + d).rem_euclid(fields.len() as isize) as usize];
        match (key, s.focus) {
            ("escape", _) => return self.close_modal(window, cx),
            ("tab", _) => step(s, if m.shift { -1 } else { 1 }),
            ("enter", Field::Prompt) if !ctrl => return self.setup_prompt_key(ev, cx),
            ("enter", _) => return self.setup_create(window, cx),
            (_, Field::Prompt) => return self.setup_prompt_key(ev, cx),
            ("up", Field::Base) => s.base_sel = s.base_sel.saturating_sub(1),
            ("down", Field::Base) => s.base_sel = (s.base_sel + 1).min(s.bases().len().saturating_sub(1)),
            ("up", _) => step(s, -1),
            ("down", _) => step(s, 1),
            ("space" | "left" | "right", Field::Mode) => {
                if s.is_repo() {
                    s.worktree = !s.worktree;
                }
            }
            ("space", Field::Prefix) => s.prefix_open = true,
            ("left", Field::Prefix) => return self.setup_pick_prefix(s_prev(cur), true, cx),
            ("right", Field::Prefix) => {
                return self.setup_pick_prefix((cur + 1) % PREFIXES.len(), true, cx);
            }
            ("space", Field::Claude) => {
                s.claude = !s.claude;
                self.setup_derive();
            }
            (_, field) => {
                let edit = match field {
                    Field::Name => &mut s.name,
                    Field::Branch => &mut s.branch,
                    Field::Base => &mut s.base,
                    Field::Location => &mut s.location,
                    Field::Mode | Field::Prefix | Field::Claude | Field::Prompt => return,
                };
                if edit.key(ks, cx) == Edit::Changed {
                    let mut prompt_again = false;
                    match field {
                        Field::Branch => s.branch_edited = true,
                        Field::Location => s.location_edited = true,
                        Field::Base => s.base_sel = 0,
                        Field::Name => {
                            // Cleared: the first prompt names the session again.
                            s.name_edited = !s.name.text.is_empty();
                            prompt_again = !s.name_edited;
                        }
                        _ => {}
                    }
                    s.error = None;
                    if prompt_again {
                        return self.setup_prompt_changed(cx);
                    }
                    self.setup_derive();
                }
            }
        }
        if let Some(Modal::Setup(s)) = &self.modal
            && was == Field::Prompt
            && s.focus != Field::Prompt
        {
            self.setup_name(window, cx); // left First prompt (Tab, Shift+Tab)
        }
        cx.notify();
    }

    fn setup_derive(&mut self) {
        let Some(Modal::Setup(s)) = &self.modal else { return };
        let dir = self.projects.get(&s.project).and_then(|p| p.worktrees_dir.clone());
        if let Some(Modal::Setup(s)) = &mut self.modal {
            s.derive(dir.as_deref());
        }
    }

    /// A key in the multi-line First prompt (Enter: new line; Ctrl+Enter, Tab, Esc are handled before).
    fn setup_prompt_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(Modal::Setup(s)) = &mut self.modal else { return };
        let ks = &ev.keystroke;
        let ctrl = ks.modifiers.control || ks.modifiers.platform;
        let result = if ctrl && ks.key == "v" {
            match cx.read_from_clipboard().and_then(|c| c.text()) {
                Some(text) => {
                    s.prompt.insert(&text, false);
                    Key::Changed
                }
                None => Key::Ignored,
            }
        } else {
            s.prompt.key(&ks.key, ks.key_char.as_deref(), ctrl, ks.modifiers.shift)
        };
        match result {
            Key::Changed => {
                s.error = None;
                self.setup_prompt_changed(cx);
            }
            Key::Copy(text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                self.setup_prompt_changed(cx); // a cut changed it
            }
            Key::Moved | Key::Ignored => cx.notify(),
        }
    }

    /// The first prompt changed (or Name was cleared): name and branch from its first words now; claude names it
    /// when you leave the field or press Create (`setup_name`).
    fn setup_prompt_changed(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Setup(s)) = &mut self.modal else { return };
        s.gen_seq += 1;
        if s.prompt.text.trim().is_empty() {
            s.generated = None;
        }
        self.setup_derive();
        cx.notify();
    }

    /// Focus moves to `f` (click or key); leaving First prompt names the session.
    pub(super) fn setup_focus(&mut self, f: Field, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Setup(s)) = &mut self.modal else { return };
        let left = s.focus == Field::Prompt && f != Field::Prompt;
        s.focus = f;
        if left {
            self.setup_name(window, cx);
        }
    }

    /// Asks claude (`claude -p`, Haiku, off the UI thread) for a name and branch slug for the prompt as it is now,
    /// unless one is running or done for it. A call still running for an older prompt is killed first: at most one.
    /// False when nothing is needed.
    fn setup_name(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(Modal::Setup(s)) = &mut self.modal else { return false };
        if !s.wants_name() {
            return false;
        }
        let seq = s.gen_seq;
        if s.naming_run.as_ref().is_some_and(|(_, at)| *at == seq) {
            return true; // already naming this prompt
        }
        if let Some((old, _)) = s.naming_run.take() {
            old.kill();
        }
        let stop = crate::git_ops::Stop::default();
        s.naming_run = Some((stop.clone(), seq));
        s.naming = true;
        let prompt = s.prompt.text.trim().to_string();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result =
                cx.background_executor().spawn(async move { crate::git_ops::name_session(&prompt, &stop) }).await;
            let _ = this.update_in(cx, |w, window, cx| {
                let Some(Modal::Setup(s)) = &mut w.modal else { return };
                if s.naming_run.as_ref().is_none_or(|(_, at)| *at != seq) {
                    return; // replaced by a newer call (this one was killed)
                }
                s.naming_run = None;
                s.naming = false;
                s.named_for = Some(seq);
                // No claude or no answer: the first words stay. Typed meanwhile: dropped.
                if let (Ok(g), true) = (result, s.gen_seq == seq && !s.name_edited) {
                    s.generated = Some(g);
                }
                let create = std::mem::take(&mut s.create_pending);
                w.setup_derive();
                cx.notify();
                if create {
                    w.setup_create(window, cx);
                }
            });
        })
        .detach();
        true
    }

    /// A prefix picked (list, ↑↓ in the open list, ←/→): remembered for the project; `close` closes the list.
    pub(super) fn setup_pick_prefix(&mut self, i: usize, close: bool, cx: &mut Context<Self>) {
        let Some(Modal::Setup(s)) = &mut self.modal else { return };
        s.prefix = i;
        s.prefix_picked = true;
        s.prefix_open &= !close;
        s.focus = Field::Prefix;
        let project = s.project.clone();
        if self.projects.get(&project).is_none() {
            self.projects.touch(&project, now_secs()); // a project not remembered yet
        }
        if let Some(p) = self.projects.get_mut(&project) {
            p.branch_prefix = Some(PREFIXES[i].to_string());
            self.mark_dirty(cx);
        }
        self.setup_derive();
        cx.notify();
    }

    /// Enter: a shared session now, or `git worktree add` off-thread and then the session in it.
    pub(super) fn setup_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The name still comes from the prompt: name it first, then create (Esc meanwhile cancels).
        if self.setup_name(window, cx) {
            if let Some(Modal::Setup(s)) = &mut self.modal {
                s.create_pending = true;
            }
            return cx.notify();
        }
        let Some(Modal::Setup(s)) = &mut self.modal else { return };
        let name = Some(s.name.text.trim().to_string()).filter(|n| !n.is_empty());
        let prompt = Some(s.prompt.text.trim().to_string()).filter(|p| s.claude && !p.is_empty());
        let project = s.project.clone();
        if !s.worktree {
            let opts =
                SessionOpts { name, claude: Some(s.claude), prompt, setup: false, project: Some(project.clone()) };
            self.modal = None;
            self.new_session_with(project, opts, window, cx);
            return;
        }
        let branch = s.full_branch();
        let Some(base) = s.bases().get(s.base_sel).cloned() else {
            s.error = Some((Some(Field::Base), "Pick a base branch".into()));
            return cx.notify();
        };
        let path = s.location_path();
        let claude = s.claude;
        s.busy = true;
        s.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let (root, b, p) = (project.clone(), branch.clone(), path.clone());
            let result = cx
                .background_executor()
                .spawn(async move {
                    if !git::write::valid_branch(&root, &b) {
                        return Err((Some(Field::Branch), format!("Not a valid branch name: {b:?}")));
                    }
                    git::write::worktree_add(&root, &b, &p, &base).map_err(|e| {
                        let msg = e.to_string();
                        let field = if msg.contains("a branch named") {
                            Some(Field::Branch)
                        } else if msg.contains("already exists") {
                            Some(Field::Location)
                        } else if msg.contains("invalid reference") || msg.contains("not a valid") {
                            Some(Field::Base)
                        } else {
                            None
                        };
                        (field, msg)
                    })
                })
                .await;
            let _ = this.update_in(cx, |w, window, cx| match result {
                Ok(()) => {
                    w.modal = None;
                    let opts = SessionOpts { name, claude: Some(claude), prompt, setup: true, project: Some(project) };
                    w.new_session_with(path, opts, window, cx);
                }
                Err(e) => {
                    if let Some(Modal::Setup(s)) = &mut w.modal {
                        s.busy = false;
                        s.focus = e.0.unwrap_or(s.focus);
                        s.error = Some(e);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Opens the keep/remove dialog for a worktree and reads what removing it would lose.
    pub(super) fn ask_close_worktree(
        &mut self,
        path: PathBuf,
        main: PathBuf,
        branch: Option<String>,
        tab: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let c = CloseWt {
            path: path.clone(),
            main,
            branch: branch.clone(),
            tab,
            state: None,
            understand: false,
            delete_branch: false,
            busy: false,
            error: None,
        };
        self.modal = Some(Modal::CloseWorktree(Box::new(c)));
        cx.spawn(async move |this, cx| {
            let state = cx
                .background_executor()
                .spawn(async move { git::write::worktree_state(&path, branch.as_deref()).map_err(|e| e.to_string()) })
                .await;
            let _ = this.update(cx, |w, cx| {
                if let Some(Modal::CloseWorktree(c)) = &mut w.modal {
                    c.state = Some(state);
                    cx.notify();
                }
            });
        })
        .detach();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn close_wt_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some(Modal::CloseWorktree(c)) = &self.modal else { return };
        if c.busy {
            return;
        }
        match ev.keystroke.key.as_str() {
            "escape" => self.close_modal(window, cx),
            "enter" => self.close_wt_keep(window, cx),
            "delete" => self.close_wt_remove(window, cx),
            _ => {}
        }
    }

    /// Keep: close the session only (from the list: just close the dialog).
    pub(super) fn close_wt_keep(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::CloseWorktree(c)) = self.modal.take() else { return };
        if let Some(i) = c.tab.and_then(|id| self.tabs.iter().position(|t| t.id == id)) {
            self.close_tab(i, window, cx);
        }
        self.close_modal(window, cx);
    }

    /// Remove: close the session, then (off-thread) re-check, `git worktree remove [--force]`, optionally delete the
    /// branch. Errors stay in the dialog; the worktree is then still there (Worktrees… can retry).
    pub(super) fn close_wt_remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::CloseWorktree(c)) = &mut self.modal else { return };
        if !c.can_remove() {
            return;
        }
        c.busy = true;
        c.error = None;
        let (path, main, branch, tab, understand, delete_branch) =
            (c.path.clone(), c.main.clone(), c.branch.clone(), c.tab, c.understand, c.delete_branch);
        if let Some(i) = tab.and_then(|id| self.tabs.iter().position(|t| t.id == id)) {
            self.close_tab(i, window, cx);
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let bg = cx.background_executor().clone();
            // Let the killed shell/claude go before git looks at the files.
            bg.timer(Duration::from_millis(500)).await;
            let m = main.clone();
            let result = bg
                .spawn(async move {
                    let main = m;
                    let st = git::write::worktree_state(&path, branch.as_deref()).map_err(|e| e.to_string())?;
                    let at_risk = st.dirty_files > 0 || st.unpushed > 0;
                    if at_risk && !understand {
                        return Err(
                            "It has changes now that weren't there when you opened this. Nothing removed.".to_string()
                        );
                    }
                    git::write::worktree_remove(&main, &path, at_risk).map_err(|e| e.to_string())?;
                    if let (true, Some(b)) = (delete_branch, &branch) {
                        // -D only when its commits are acknowledged as lost or exist elsewhere.
                        git::write::branch_delete(&main, b, understand || st.unpushed == 0)
                            .map_err(|e| format!("Worktree removed; the branch stays: {e}"))?;
                    }
                    Ok(())
                })
                .await;
            let _ = this.update_in(cx, |w, window, cx| {
                match result {
                    Ok(()) => {
                        let reopen =
                            matches!(&w.modal, Some(Modal::CloseWorktree(c)) if c.tab.is_none()).then(|| main.clone());
                        w.close_modal(window, cx);
                        if let Some(main) = reopen {
                            w.open_worktrees(main, window, cx);
                        }
                    }
                    Err(e) => {
                        if let Some(Modal::CloseWorktree(c)) = &mut w.modal {
                            c.busy = false;
                            c.tab = None; // the session is already closed
                            c.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The project's worktrees (`git worktree list`, also ones made outside the app).
    pub(super) fn open_worktrees(&mut self, main: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.modal = Some(Modal::Worktrees(Box::new(WtList { main: main.clone(), list: None, sel: 0, busy: false })));
        self.reload_worktrees(main, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn reload_worktrees(&mut self, main: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let list = cx
                .background_executor()
                .spawn(async move { git::write::worktree_list(&main).map_err(|e| e.to_string()) })
                .await;
            let _ = this.update(cx, |w, cx| {
                if let Some(Modal::Worktrees(l)) = &mut w.modal {
                    l.sel = l.sel.min(list.as_ref().map_or(0, |v| v.len().saturating_sub(1)));
                    l.list = Some(list);
                    l.busy = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn worktrees_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some(Modal::Worktrees(l)) = &mut self.modal else { return };
        let n = l.list.as_ref().and_then(|r| r.as_ref().ok()).map_or(0, Vec::len);
        match ev.keystroke.key.as_str() {
            "escape" => return self.close_modal(window, cx),
            "up" => l.sel = l.sel.saturating_sub(1),
            "down" => l.sel = (l.sel + 1).min(n.saturating_sub(1)),
            "enter" => {
                let sel = l.sel;
                return self.worktree_open(sel, window, cx);
            }
            "delete" => {
                let sel = l.sel;
                return self.worktree_remove_ask(sel, window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    fn worktree_row(&self, i: usize) -> Option<(PathBuf, Worktree)> {
        let Some(Modal::Worktrees(l)) = &self.modal else { return None };
        let w = l.list.as_ref()?.as_ref().ok()?.get(i)?.clone();
        Some((l.main.clone(), w))
    }

    pub(super) fn worktree_open(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((main, w)) = self.worktree_row(i).filter(|(_, w)| !w.prunable) else { return };
        self.modal = None;
        self.new_session_with(w.path, SessionOpts { project: Some(main), ..Default::default() }, window, cx);
    }

    pub(super) fn worktree_remove_ask(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((main, w)) = self.worktree_row(i).filter(|(_, w)| !w.main && !w.prunable) else { return };
        if self.tabs.iter().any(|t| t.repo == TabRepo::Repo(w.path.clone())) {
            return; // open sessions there: close them first (each asks)
        }
        self.ask_close_worktree(w.path, main, w.branch, None, window, cx);
    }

    pub(super) fn worktrees_prune(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Worktrees(l)) = &mut self.modal else { return };
        l.busy = true;
        let main = l.main.clone();
        cx.spawn(async move |this, cx| {
            let m = main.clone();
            let _ = cx.background_executor().spawn(async move { git::write::worktree_prune(&m) }).await;
            let _ = this.update(cx, |w, cx| w.reload_worktrees(main, cx));
        })
        .detach();
        cx.notify();
    }

    // ---- rendering ----

    pub(super) fn setup_view(&self, s: &Setup, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::get();
        // Built first: it needs `cx` mutably.
        let prompt = s.claude.then(|| {
            let empty = s.prompt.text.is_empty();
            let focused = s.focus == Field::Prompt;
            ui::text_area_box(
                "setup-prompt",
                &s.prompt,
                &s.prompt_layouts,
                focused,
                (3, 10),
                true,
                window.line_height(),
                |w: &mut Self| match &mut w.modal {
                    Some(Modal::Setup(s)) => Some(&mut s.prompt),
                    _ => None,
                },
                |w, _, cx| {
                    if let Some(Modal::Setup(s)) = &mut w.modal {
                        s.focus = Field::Prompt;
                        s.prefix_open = false;
                        cx.notify();
                    }
                },
                cx,
            )
            .when(empty && !focused, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(z(space::SM))
                        .left(z(space::MD))
                        .text_color(t.text.faint)
                        .child("optional: what claude should start with (a \"Branch: x\" line names the branch)"),
                )
            })
            .into_any_element()
        });
        let label = |text: &'static str| {
            div()
                .flex_none()
                .w(z(96.))
                .pt(z(space::SM))
                .text_size(z(text_size::SM))
                .text_color(t.text.muted)
                .child(text)
        };
        let err_for = |f: Field| s.error.as_ref().filter(|(ef, _)| *ef == Some(f)).map(|(_, m)| m.clone());
        let row =
            |f: Field, name: &'static str, control: AnyElement| {
                div()
                    .id(SharedString::from(format!("setup-{f:?}")))
                    .flex()
                    .gap(z(space::MD))
                    .child(label(name))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(z(space::XS)).child(control).children(
                        err_for(f).map(|e| div().text_size(z(text_size::XS)).text_color(t.state.error).child(e)),
                    ))
                    .on_click(cx.listener(move |w, _: &ClickEvent, window, cx| {
                        if let Some(Modal::Setup(s)) = &mut w.modal {
                            s.prefix_open = false;
                        }
                        w.setup_focus(f, window, cx);
                        cx.notify();
                    }))
            };
        let mut rows: Vec<AnyElement> = Vec::new();
        let from_prompt = !s.name_edited && s.claude && !s.prompt.text.trim().is_empty();
        let name = div()
            .flex()
            .flex_col()
            .gap(z(space::XS))
            .child(text_input(&s.name, s.focus == Field::Name, "Session name (shown in the sidebar and notifications)"))
            .when(from_prompt, |d| {
                d.child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(if s.naming {
                    "From the first prompt · claude is naming it…"
                } else if s.named_for == Some(s.gen_seq) {
                    "From the first prompt (typing here keeps your name)"
                } else {
                    "From the first prompt's first words; claude names it when you leave the prompt or create"
                }))
            });
        rows.push(row(Field::Name, "Name", name.into_any_element()).into_any_element());
        let is_repo = s.is_repo();
        // No ring around the row: keyboard focus only tints the other pill's border (Space switches to it).
        let mode_focused = s.focus == Field::Mode;
        let pill = |id: &'static str, text: &'static str, on: bool, enabled: bool| {
            div()
                .id(id)
                .px(z(space::MD))
                .py(z(space::XS))
                .rounded(z(radius::MD))
                .text_size(z(text_size::SM))
                .when(on, |d| d.bg(t.accent).text_color(t.text.on_accent))
                .when(!on, |d| {
                    d.border_1()
                        .border_color(if mode_focused && enabled { t.accent } else { t.border.strong })
                        .text_color(if enabled { t.text.primary } else { t.text.faint })
                })
                .when(enabled, |d| d.cursor_pointer())
                .child(text)
        };
        let mode = div()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .child(pill("mode-shared", "Shared directory", !s.worktree, true).on_click(cx.listener(
                |w, _: &ClickEvent, _, cx| {
                    if let Some(Modal::Setup(s)) = &mut w.modal {
                        s.worktree = false;
                        s.focus = Field::Mode;
                        cx.notify();
                    }
                },
            )))
            .child(pill("mode-wt", "New worktree", s.worktree, is_repo).on_click(cx.listener(
                move |w, _: &ClickEvent, _, cx| {
                    if let Some(Modal::Setup(s)) = &mut w.modal
                        && is_repo
                    {
                        s.worktree = true;
                        s.focus = Field::Mode;
                        cx.notify();
                    }
                },
            )))
            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(match &s.branches {
                None => "checking git…",
                Some(_) if !is_repo => "not a git repo: shared only",
                Some(_) if s.worktree => "own branch and directory",
                Some(_) => "",
            }));
        rows.push(row(Field::Mode, "Mode", mode.into_any_element()).into_any_element());
        if s.worktree {
            let prefix_label = |i: usize| if PREFIXES[i].is_empty() { "none" } else { PREFIXES[i] };
            let list = div()
                .absolute()
                .top(z(28.))
                .left_0()
                .min_w(z(96.))
                .p(z(space::XS))
                .flex()
                .flex_col()
                .rounded(z(radius::MD))
                .bg(t.bg.elevated)
                .border_1()
                .border_color(t.border.strong)
                .shadow_md()
                .children((0..PREFIXES.len()).map(|i| {
                    div()
                        .id(("prefix", i))
                        .px(z(space::SM))
                        .py(z(2.))
                        .rounded(z(radius::SM))
                        .cursor_pointer()
                        .font_family(MONO_FONT)
                        .text_size(z(text_size::SM))
                        .when(i == s.prefix, |d| d.bg(t.bg.selected_focus))
                        .hover(|d| d.bg(t.bg.hover))
                        .child(prefix_label(i))
                        .on_click(cx.listener(move |w, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            w.setup_pick_prefix(i, true, cx);
                        }))
                }));
            let prefix_focused = s.focus == Field::Prefix;
            let prefix = div()
                .relative()
                .flex_none()
                .child(
                    div()
                        .id("setup-prefix")
                        .flex()
                        .items_center()
                        .gap(z(space::XS))
                        .h_full()
                        .px(z(space::SM))
                        .py(z(space::XS + 2.))
                        .rounded(z(radius::MD))
                        .bg(t.bg.input)
                        .border_1()
                        .border_color(if prefix_focused || s.prefix_open { t.accent } else { t.border.subtle })
                        .cursor_pointer()
                        .font_family(MONO_FONT)
                        .text_size(z(text_size::SM))
                        .text_color(if s.prefix == PREFIXES.len() - 1 { t.text.dim } else { t.text.primary })
                        .child(prefix_label(s.prefix))
                        .child(ui::icon(icon::CHEVRON_DOWN, 12., t.text.dim))
                        .tooltip(ui::tip("Branch prefix (remembered for this project) · ←/→ or Space"))
                        .on_click(cx.listener(|w, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            if let Some(Modal::Setup(s)) = &mut w.modal {
                                s.prefix_open = !s.prefix_open;
                            }
                            w.setup_focus(Field::Prefix, window, cx);
                            cx.notify();
                        })),
                )
                .when(s.prefix_open, |d| d.child(deferred(list).with_priority(1)));
            let branch = div()
                .flex()
                .flex_col()
                .gap(z(space::XS))
                .child(div().flex().gap(z(space::SM)).child(prefix).child(div().flex_1().min_w_0().child(text_input(
                    &s.branch,
                    s.focus == Field::Branch,
                    "new branch name",
                ))))
                .when(!s.branch.text.trim().is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(z(text_size::XS))
                            .text_color(t.text.dim)
                            .child(format!("Creates the branch {}", s.full_branch())),
                    )
                });
            rows.push(row(Field::Branch, "Branch", branch.into_any_element()).into_any_element());
            let bases = s.bases();
            let current = s.branches.as_ref().map(|(c, _)| c.clone()).unwrap_or_default();
            let list = div().flex().flex_col().max_h(z(150.)).overflow_hidden().children(
                bases.iter().take(6).enumerate().map(|(i, b)| {
                    let sel = i == s.base_sel;
                    div()
                        .id(("base", i))
                        .flex()
                        .items_center()
                        .gap(z(space::SM))
                        .px(z(space::SM))
                        .py(z(2.))
                        .rounded(z(radius::SM))
                        .cursor_pointer()
                        .font_family(MONO_FONT)
                        .text_size(z(text_size::XS))
                        .when(sel, |d| d.bg(t.bg.selected_focus))
                        .hover(|d| d.bg(t.bg.hover))
                        .child(ui::icon(icon::BRANCH, 11., t.text.dim))
                        .child(b.clone())
                        .when(*b == current, |d| d.child(div().text_color(t.text.dim).child("current")))
                        .on_click(cx.listener(move |w, _: &ClickEvent, _, cx| {
                            if let Some(Modal::Setup(s)) = &mut w.modal {
                                s.base_sel = i;
                                s.focus = Field::Base;
                                cx.notify();
                            }
                        }))
                }),
            );
            let base = div()
                .flex()
                .flex_col()
                .gap(z(space::XS))
                .child(text_input(&s.base, s.focus == Field::Base, "filter branches"))
                .child(list);
            rows.push(row(Field::Base, "Base", base.into_any_element()).into_any_element());
            let loc = s.location_path();
            let inside = projects::inside_repo(&s.project, &loc);
            let location = div()
                .flex()
                .flex_col()
                .gap(z(space::XS))
                .child(text_input(&s.location, s.focus == Field::Location, "directory"))
                .when(inside, |d| {
                    d.child(div().text_size(z(text_size::XS)).text_color(t.state.needs_you).child(
                        "Inside the repo: the main checkout will watch and list it too. A sibling directory is better.",
                    ))
                });
            rows.push(row(Field::Location, "Location", location.into_any_element()).into_any_element());
        }
        let check = div()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .p(z(2.))
            .text_size(z(text_size::SM))
            // No ring like the inputs: keyboard focus only brightens the label.
            .text_color(if s.focus == Field::Claude { t.text.primary } else { t.text.muted })
            .child(
                div()
                    .size(z(12.))
                    .rounded(z(3.))
                    .border_1()
                    .border_color(if s.claude { t.accent } else { t.border.strong })
                    .when(s.claude, |d| d.bg(t.accent)),
            )
            .child("Start claude")
            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(if s.worktree {
                "after the project's setup command, if any"
            } else {
                ""
            }));
        rows.push(
            row(Field::Claude, "Claude", check.into_any_element())
                .on_click(cx.listener(|w, _: &ClickEvent, window, cx| {
                    if let Some(Modal::Setup(s)) = &mut w.modal {
                        s.claude = !s.claude;
                    }
                    w.setup_focus(Field::Claude, window, cx);
                    w.setup_derive();
                    cx.notify();
                }))
                .into_any_element(),
        );
        if let Some(prompt) = prompt {
            rows.push(row(Field::Prompt, "First prompt", prompt).into_any_element());
        }
        let general = s.error.as_ref().filter(|(f, _)| f.is_none()).map(|(_, m)| m.clone());
        div()
            .w(z(640.))
            .p(z(space::XL))
            .flex()
            .flex_col()
            .gap(z(space::MD))
            .rounded(z(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(z(space::SM))
                    .child(ui::icon(icon::PLUS, 15., t.accent))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("New session"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(z(text_size::SM))
                            .text_color(t.text.dim)
                            .child(tilde(&s.project)),
                    ),
            )
            .children(rows)
            .children(general.map(|e| div().text_size(z(text_size::SM)).text_color(t.state.error).child(e)))
            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(if s.busy {
                "Creating worktree…"
            } else if s.create_pending {
                "Naming the session, then creating it… · Esc cancel"
            } else if s.focus == Field::Prompt {
                "Enter new line · Ctrl+Enter create · Tab next field · Esc cancel"
            } else {
                "Tab / ↑↓ next field · Space toggles · Enter / Ctrl+Enter create · Esc cancel"
            }))
            .into_any_element()
    }

    pub(super) fn close_wt_view(&self, c: &CloseWt, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::get();
        let checkbox = |id: &'static str, on: bool, text: String| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(z(space::SM))
                .cursor_pointer()
                .text_size(z(text_size::SM))
                .child(
                    div()
                        .flex_none()
                        .size(z(12.))
                        .rounded(z(3.))
                        .border_1()
                        .border_color(if on { t.accent } else { t.border.strong })
                        .when(on, |d| d.bg(t.accent)),
                )
                .child(text)
        };
        let state_line = match &c.state {
            None => "Checking for uncommitted work…".to_string(),
            Some(Err(e)) => format!("Couldn't check: {e}"),
            Some(Ok(s)) if s.dirty_files == 0 && s.unpushed == 0 => {
                "Clean: every commit is on another branch or a remote.".to_string()
            }
            Some(Ok(s)) => format!(
                "{} uncommitted file{} and {} commit{} on no other branch or remote would be lost.",
                s.dirty_files,
                if s.dirty_files == 1 { "" } else { "s" },
                s.unpushed,
                if s.unpushed == 1 { "" } else { "s" }
            ),
        };
        let can_remove = c.can_remove();
        let button = |id: &'static str, label: &'static str, kind: u8, enabled: bool| {
            div()
                .id(id)
                .px(z(space::MD))
                .py(z(space::XS))
                .rounded(z(radius::MD))
                .text_size(z(text_size::SM))
                .when(kind == 0, |d| d.border_1().border_color(t.border.strong).text_color(t.text.primary))
                .when(kind == 1, |d| d.bg(t.accent).text_color(t.text.on_accent))
                .when(kind == 2 && enabled, |d| d.bg(t.state.error).text_color(t.text.on_accent))
                .when(kind == 2 && !enabled, |d| d.border_1().border_color(t.border.subtle).text_color(t.text.faint))
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.85)))
                .when(!enabled, |d| d.cursor_not_allowed())
                .child(label)
        };
        div()
            .w(z(560.))
            .p(z(space::XL))
            .flex()
            .flex_col()
            .gap(z(space::MD))
            .rounded(z(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .child(div().flex().items_center().gap(z(space::SM)).child(ui::icon(icon::BRANCH, 15., t.accent)).child(
                div().font_weight(FontWeight::SEMIBOLD).child(if c.tab.is_some() {
                    "Close the last session in this worktree"
                } else {
                    "Remove worktree"
                }),
            ))
            .child(div().font_family(MONO_FONT).text_size(z(text_size::SM)).text_color(t.text.muted).child(format!(
                "{}{}",
                tilde(&c.path),
                c.branch.as_ref().map(|b| format!("  ({b})")).unwrap_or_default()
            )))
            .child(
                div()
                    .text_size(z(text_size::SM))
                    .text_color(if c.at_risk() { t.state.needs_you } else { t.text.muted })
                    .child(state_line),
            )
            .when(c.at_risk(), |d| {
                d.child(
                    checkbox(
                        "wt-understand",
                        c.understand,
                        "I understand: remove it anyway (git worktree remove --force)".into(),
                    )
                    .on_click(cx.listener(|w, _: &ClickEvent, _, cx| {
                        if let Some(Modal::CloseWorktree(c)) = &mut w.modal {
                            c.understand = !c.understand;
                            cx.notify();
                        }
                    })),
                )
            })
            .children(c.branch.clone().map(|b| {
                checkbox("wt-branch", c.delete_branch, format!("Also delete the branch {b}")).on_click(cx.listener(
                    |w, _: &ClickEvent, _, cx| {
                        if let Some(Modal::CloseWorktree(c)) = &mut w.modal {
                            c.delete_branch = !c.delete_branch;
                            cx.notify();
                        }
                    },
                ))
            }))
            .children(c.error.clone().map(|e| div().text_size(z(text_size::SM)).text_color(t.state.error).child(e)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(z(space::SM))
                    .child(
                        button("wt-cancel", "Cancel", 0, !c.busy)
                            .on_click(cx.listener(|w, _: &ClickEvent, window, cx| w.close_modal(window, cx))),
                    )
                    .when(c.tab.is_some(), |d| {
                        d.child(
                            button("wt-keep", "Keep worktree", 1, !c.busy)
                                .on_click(cx.listener(|w, _: &ClickEvent, window, cx| w.close_wt_keep(window, cx))),
                        )
                    })
                    .child(
                        button("wt-remove", if c.busy { "Removing…" } else { "Remove worktree" }, 2, can_remove)
                            .on_click(cx.listener(|w, _: &ClickEvent, window, cx| w.close_wt_remove(window, cx))),
                    ),
            )
            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(if c.tab.is_some() {
                "Enter keep · Delete remove · Esc cancel (the session stays)"
            } else {
                "Delete remove · Esc cancel"
            }))
            .into_any_element()
    }

    pub(super) fn worktrees_view(&self, l: &WtList, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::get();
        let rows: Vec<AnyElement> = match &l.list {
            None => vec![div().text_size(z(text_size::SM)).text_color(t.text.dim).child("Listing…").into_any_element()],
            Some(Err(e)) => {
                vec![div().text_size(z(text_size::SM)).text_color(t.state.error).child(e.clone()).into_any_element()]
            }
            Some(Ok(list)) => list
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    let sessions = self.tabs.iter().filter(|t| t.repo == TabRepo::Repo(w.path.clone())).count();
                    let small = |id: &'static str, label: &'static str| {
                        div()
                            .id((id, i))
                            .px(z(space::SM))
                            .py(z(2.))
                            .rounded(z(radius::SM))
                            .border_1()
                            .border_color(t.border.strong)
                            .cursor_pointer()
                            .hover(|s| s.bg(t.bg.hover))
                            .text_size(z(text_size::XS))
                            .child(label)
                    };
                    div()
                        .id(("wt", i))
                        .flex()
                        .items_center()
                        .gap(z(space::MD))
                        .px(z(space::MD))
                        .py(z(space::SM))
                        .rounded(z(radius::MD))
                        .when(i == l.sel, |d| d.bg(t.bg.selected_focus))
                        .child(ui::icon(icon::BRANCH, 13., if w.main { t.accent } else { t.text.dim }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div().truncate().text_size(z(text_size::SM)).child(
                                        w.branch.clone().unwrap_or_else(|| {
                                            format!("detached {}", w.head.get(..7).unwrap_or(&w.head))
                                        }),
                                    ),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .font_family(MONO_FONT)
                                        .text_size(z(text_size::XS))
                                        .text_color(t.text.dim)
                                        .child(tilde(&w.path)),
                                ),
                        )
                        .when(w.main, |d| {
                            d.child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child("main checkout"))
                        })
                        .when(w.prunable, |d| {
                            d.child(
                                div().text_size(z(text_size::XS)).text_color(t.state.needs_you).child("directory gone"),
                            )
                        })
                        .when(w.locked, |d| {
                            d.child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child("locked"))
                        })
                        .when(sessions > 0, |d| {
                            d.child(
                                div()
                                    .text_size(z(text_size::XS))
                                    .text_color(t.text.muted)
                                    .child(format!("{sessions} open")),
                            )
                        })
                        .when(!w.prunable, |d| {
                            d.child(small("wt-open", "Open").on_click(
                                cx.listener(move |w, _: &ClickEvent, window, cx| w.worktree_open(i, window, cx)),
                            ))
                        })
                        .when(!w.main && !w.prunable && sessions == 0, |d| {
                            d.child(small("wt-rm", "Remove…").on_click(
                                cx.listener(move |w, _: &ClickEvent, window, cx| w.worktree_remove_ask(i, window, cx)),
                            ))
                        })
                        .on_click(cx.listener(move |w, _: &ClickEvent, _, cx| {
                            if let Some(Modal::Worktrees(l)) = &mut w.modal {
                                l.sel = i;
                                cx.notify();
                            }
                        }))
                        .into_any_element()
                })
                .collect(),
        };
        let prunable =
            l.list.as_ref().and_then(|r| r.as_ref().ok()).map_or(0, |v| v.iter().filter(|w| w.prunable).count());
        div()
            .w(z(680.))
            .max_h(z(560.))
            .p(z(space::XL))
            .flex()
            .flex_col()
            .gap(z(space::MD))
            .rounded(z(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(z(space::SM))
                    .child(ui::icon(icon::BRANCH, 15., t.accent))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Worktrees"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(z(text_size::SM))
                            .text_color(t.text.dim)
                            .child(tilde(&l.main)),
                    ),
            )
            .child(div().id("wt-list").flex().flex_col().gap(z(2.)).overflow_y_scroll().children(rows))
            .when(prunable > 0, |d| {
                d.child(
                    div()
                        .id("wt-prune")
                        .cursor_pointer()
                        .text_size(z(text_size::SM))
                        .text_color(t.accent)
                        .child(if l.busy {
                            "Pruning…".to_string()
                        } else {
                            format!(
                                "Prune {prunable} stale entr{} (directory already gone)",
                                if prunable == 1 { "y" } else { "ies" }
                            )
                        })
                        .on_click(cx.listener(|w, _: &ClickEvent, _, cx| w.worktrees_prune(cx))),
                )
            })
            .child(
                div()
                    .text_size(z(text_size::XS))
                    .text_color(t.text.dim)
                    .child("↑↓ select · Enter open a session · Delete remove… · Esc close"),
            )
            .into_any_element()
    }
}

pub(crate) use crate::ui::text_input;

fn s_prev(i: usize) -> usize {
    (i + PREFIXES.len() - 1) % PREFIXES.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Setup {
        let mut s = Setup::new(PathBuf::from("/c/app"), true, 0);
        s.worktree = true;
        s
    }

    fn prompt(s: &mut Setup, text: &str) {
        s.prompt = TextArea::default();
        s.prompt.insert(text, false);
        s.derive(None);
    }

    #[test]
    fn name_slug_prefix_and_location() {
        let mut s = setup();
        s.name = LineEdit::new("Esc/Ctrl-C");
        s.name_edited = true;
        s.derive(None);
        assert_eq!((s.branch.text.as_str(), s.full_branch().as_str()), ("esc-ctrl-c", "feat/esc-ctrl-c"));
        assert_eq!(s.location.text, "/c/app-worktrees/feat-esc-ctrl-c");
        s.prefix = PREFIXES.len() - 1;
        assert_eq!(s.full_branch(), "esc-ctrl-c", "none");
    }

    #[test]
    fn first_prompt_names_until_typed() {
        let mut s = setup();
        prompt(&mut s, "Fix the Esc/Ctrl+C interrupt in the terminal");
        assert_eq!(s.name.text, "Fix the Esc/Ctrl+C interrupt in", "no claude yet: the first words");
        assert_eq!(s.branch.text, "fix-the-esc-ctrl-c-interrupt-in");
        s.generated = Some(("Esc interrupt".into(), "esc-interrupt".into()));
        s.derive(None);
        assert_eq!((s.name.text.as_str(), s.full_branch().as_str()), ("Esc interrupt", "feat/esc-interrupt"));
        s.name = LineEdit::new("Mine");
        s.name_edited = true;
        prompt(&mut s, "Something else entirely");
        assert_eq!((s.name.text.as_str(), s.branch.text.as_str()), ("Mine", "mine"), "a typed name stays");
        s.claude = false;
        s.name_edited = false;
        s.derive(None);
        assert_eq!(s.name.text, "", "no claude: no first prompt");
    }

    #[test]
    fn claude_names_once_per_prompt_version() {
        let mut s = setup();
        assert!(!s.wants_name(), "no prompt");
        prompt(&mut s, "Fix the Esc key");
        s.gen_seq += 1; // as a prompt change does
        assert!(s.wants_name());
        s.named_for = Some(s.gen_seq); // the call finished (named or failed)
        assert!(!s.wants_name(), "leaving the field again or Create: no second call");
        s.gen_seq += 1;
        assert!(s.wants_name(), "the prompt changed since");
        s.name_edited = true;
        assert!(!s.wants_name(), "a typed name");
        s.name_edited = false;
        s.claude = false;
        assert!(!s.wants_name(), "no claude: no prompt");
    }

    #[test]
    fn explicit_branch_line_wins_unless_picked_or_typed() {
        let mut s = setup();
        s.prefix_saved = 2; // chore/ remembered
        prompt(&mut s, "Branch: fix/esc-key from main.\nMake Esc work.");
        assert_eq!((s.full_branch().as_str(), s.name.text.as_str()), ("fix/esc-key", "Make Esc work"));
        s.generated = Some(("Esc key".into(), "esc-key-handling".into()));
        s.derive(None);
        assert_eq!(s.full_branch(), "fix/esc-key", "the explicit line beats claude's slug");
        prompt(&mut s, "Branch: jakub/try");
        assert_eq!(s.full_branch(), "jakub/try", "unknown prefix: as written, prefix none");
        prompt(&mut s, "No branch line");
        assert_eq!(PREFIXES[s.prefix], "chore/", "back to the remembered prefix");
        s.prefix_picked = true;
        s.prefix = 3;
        prompt(&mut s, "Branch: fix/x");
        assert_eq!(s.full_branch(), "docs/x", "a prefix picked here stays");
        s.branch = LineEdit::new("hand/typed");
        s.branch_edited = true;
        prompt(&mut s, "Branch: fix/y");
        assert_eq!(s.full_branch(), "docs/hand/typed", "a typed branch stays");
    }
}
