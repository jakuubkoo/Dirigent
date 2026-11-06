//! The branch switcher: fuzzy over local and remote branches (listed off-thread on open), create a branch from
//! the current one, switch with a confirmation when there are uncommitted changes. No delete.

use gpui::{
    AnyElement, ClickEvent, FontWeight, HighlightStyle, MouseMoveEvent, SharedString, StyledText,
    UniformListScrollHandle, div, uniform_list,
};

use super::*;
use crate::git_ops::Job;
use crate::projects;
use crate::theme::{self, radius, space, text_size, z};
use crate::ui::{self, icon};
use git::write::Branch;

pub(super) struct Branches {
    root: PathBuf,
    edit: LineEdit,
    /// None while listing.
    all: Option<Result<Vec<Branch>, String>>,
    rows: Vec<Row>,
    sel: usize,
    scroll: UniformListScrollHandle,
}

enum Row {
    /// Create this branch from the current one.
    Create(String),
    /// Index into `all`, matched chars of its name.
    Branch(usize, Vec<usize>),
}

/// The rows for a query: matches (best first, local before remote on ties), plus "Create" when the text isn't an
/// existing branch name.
fn filter(all: &[Branch], q: &str) -> Vec<Row> {
    let q = q.trim();
    let mut hits: Vec<(i64, usize, Vec<usize>)> = all
        .iter()
        .enumerate()
        .filter_map(|(i, b)| projects::fuzzy(q, &b.name).map(|m| (m.score as i64, i, m.chars)))
        .collect();
    // Best match first; with no query the current branch leads, then git's order (most recent commit first).
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(all[b.1].current.cmp(&all[a.1].current)).then(a.1.cmp(&b.1)));
    let exact = all.iter().any(|b| !b.remote && b.name == q);
    let create = (!q.is_empty() && !exact && !q.contains(char::is_whitespace)).then(|| Row::Create(q.to_string()));
    create.into_iter().chain(hits.into_iter().map(|(_, i, hl)| Row::Branch(i, hl))).collect()
}

impl Workspace {
    /// The repo bar's branch button, the status bar branch, Ctrl+Shift+O.
    pub fn open_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.active_root() else { return self.flash("Not in a git repository".into(), cx) };
        self.menu = None;
        self.modal = Some(Modal::Branches(Box::new(Branches {
            root: root.clone(),
            edit: LineEdit::new(""),
            all: None,
            rows: Vec::new(),
            sel: 0,
            scroll: UniformListScrollHandle::new(),
        })));
        cx.spawn(async move |this, cx| {
            let list = cx
                .background_executor()
                .spawn(async move { git::write::branches_full(&root).map_err(|e| e.to_string()) })
                .await;
            let _ = this.update(cx, |w, cx| {
                if let Some(Modal::Branches(b)) = &mut w.modal {
                    b.all = Some(list);
                    w.branches_refilter(cx);
                }
            });
        })
        .detach();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn branches_refilter(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Branches(b)) = &mut self.modal else { return };
        let all = b.all.as_ref().and_then(|r| r.as_ref().ok()).map_or(&[][..], |v| v.as_slice());
        b.rows = filter(all, &b.edit.text);
        // An exact-ish query selects its best branch, not the Create row.
        b.sel = usize::from(matches!(b.rows.first(), Some(Row::Create(_))) && b.rows.len() > 1);
        b.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    pub(super) fn branches_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some(Modal::Branches(b)) = &mut self.modal else { return };
        let ks = &ev.keystroke;
        let n = b.rows.len();
        let moved = |b: &mut Branches, to: usize| {
            b.sel = to.min(n.saturating_sub(1));
            b.scroll.scroll_to_item(b.sel, gpui::ScrollStrategy::Center);
        };
        match ks.key.as_str() {
            "escape" => return self.close_modal(window, cx),
            "up" => moved(b, b.sel.saturating_sub(1)),
            "down" => moved(b, b.sel + 1),
            "enter" if ks.modifiers.control => {
                let name = b.edit.text.trim().to_string();
                if !name.is_empty() {
                    return self.branch_create(name, window, cx);
                }
            }
            "enter" => {
                let sel = b.sel;
                return self.branch_pick(sel, window, cx);
            }
            _ => {
                if b.edit.key(ks, cx) == Edit::Changed {
                    self.branches_refilter(cx);
                }
            }
        }
        cx.notify();
    }

    fn branch_create(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Branches(b)) = &self.modal else { return };
        let root = b.root.clone();
        self.close_modal(window, cx);
        // Uncommitted changes come along to a new branch from HEAD: nothing can clash, no question.
        self.run_git(root, Job::CreateBranch(name), cx);
    }

    /// Enter / click: Create → create; the current branch → close; another → switch (asks first when dirty).
    fn branch_pick(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Branches(b)) = &self.modal else { return };
        let root = b.root.clone();
        let (job, target) = match b.rows.get(i) {
            Some(Row::Create(name)) => return self.branch_create(name.clone(), window, cx),
            Some(Row::Branch(ix, _)) => {
                let Some(br) = b.all.as_ref().and_then(|r| r.as_ref().ok()).and_then(|v| v.get(*ix)) else { return };
                if br.current {
                    return self.close_modal(window, cx);
                }
                let job = if br.remote { Job::SwitchRemote(br.name.clone()) } else { Job::Switch(br.name.clone()) };
                (job, br.name.clone())
            }
            None => return,
        };
        self.close_modal(window, cx);
        self.switch_branch(root, job, target, window, cx);
    }

    /// Switches now on a clean tree; with uncommitted changes asks first (they move along, or git refuses).
    pub(super) fn switch_branch(
        &mut self,
        root: PathBuf,
        job: Job,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dirty = self.repos.status(&root).map_or(0, |s| s.files.len() + s.more);
        if dirty == 0 {
            return self.run_git(root, job, cx);
        }
        let local = target.split_once('/').filter(|_| matches!(job, Job::SwitchRemote(_))).map_or(&*target, |(_, n)| n);
        self.modal = Some(Modal::Confirm {
            title: format!("Switch to {local}?"),
            body: format!(
                "{dirty} uncommitted change{} move{} with you to {local}. If they clash with that branch, git refuses \
                 and nothing changes.",
                if dirty == 1 { "" } else { "s" },
                if dirty == 1 { "s" } else { "" }
            ),
            files: Vec::new(),
            ok: "Switch",
            danger: false,
            then: Then::Git(root, job),
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn branches_view(&self, b: &Branches, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::get();
        let h = (window.viewport_size().height - z(200.)).min(z(440.)).max(z(200.));
        let dirty = self.repos.status(&b.root).map_or(0, |s| s.files.len() + s.more);
        let search = div()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .px(z(space::MD))
            .py(z(space::SM))
            .rounded(z(radius::MD))
            .bg(t.bg.input)
            .border_1()
            .border_color(t.accent)
            .text_size(z(text_size::MD))
            .child(ui::icon(icon::BRANCH, 14., t.text.dim))
            .child(ui::line_text(&b.edit, true, "Switch to a branch, or type a new name"));
        let n = b.rows.len();
        let list = uniform_list(
            "branch-rows",
            n,
            cx.processor(|w: &mut Workspace, range: std::ops::Range<usize>, _window, cx| {
                let Some(Modal::Branches(b)) = &w.modal else { return Vec::new() };
                range.map(|i| branch_row(b, i, cx)).collect()
            }),
        )
        .track_scroll(&b.scroll)
        .flex_1();
        let note = match &b.all {
            None => Some("Reading branches…".to_string()),
            Some(Err(e)) => Some(e.clone()),
            Some(Ok(_)) if n == 0 => Some("No branch matches.".into()),
            _ => None,
        };
        let hint = if dirty > 0 {
            let s = if dirty == 1 { " comes" } else { "s come" };
            format!("↑↓ select · Enter switch · Ctrl+Enter create · {dirty} uncommitted change{s} along")
        } else {
            "↑↓ select · Enter switch · Ctrl+Enter create · Esc".into()
        };
        div()
            .w(z(560.))
            .h(h)
            .p(z(space::LG))
            .flex()
            .flex_col()
            .gap(z(space::MD))
            .rounded(z(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .child(search)
            .child(div().flex_1().min_h_0().flex().flex_col().child(list).children(
                note.map(|s| div().p(z(space::LG)).text_size(z(text_size::SM)).text_color(t.text.dim).child(s)),
            ))
            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(hint))
            .into_any_element()
    }
}

fn branch_row(b: &Branches, i: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let t = theme::get();
    let row = div()
        .id(("branch", i))
        .w_full()
        .flex()
        .items_center()
        .gap(z(space::MD))
        .h(z(30.))
        .px(z(space::MD))
        .rounded(z(radius::MD))
        .cursor_pointer()
        .text_size(z(text_size::SM))
        .when(i == b.sel, |d| d.bg(t.bg.selected_focus))
        .on_mouse_move(cx.listener(move |w, _: &MouseMoveEvent, _, cx| {
            if let Some(Modal::Branches(b)) = &mut w.modal
                && b.sel != i
            {
                b.sel = i;
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |w, _: &ClickEvent, window, cx| w.branch_pick(i, window, cx)));
    let current = b.all.as_ref().and_then(|r| r.as_ref().ok()).and_then(|v| v.iter().find(|b| b.current));
    match &b.rows[i] {
        Row::Create(name) => row
            .child(ui::icon(icon::PLUS, 14., t.accent))
            .child(div().flex_1().min_w_0().truncate().text_color(t.text.primary).child(match current {
                Some(c) => format!("Create branch “{name}” from {}", c.name),
                None => format!("Create branch “{name}”"),
            }))
            .into_any_element(),
        Row::Branch(ix, hl) => {
            let Some(br) = b.all.as_ref().and_then(|r| r.as_ref().ok()).and_then(|v| v.get(*ix)) else {
                return row.into_any_element();
            };
            let hl: Vec<_> = hl
                .iter()
                .filter_map(|&c| br.name.char_indices().nth(c).map(|(b, ch)| b..b + ch.len_utf8()))
                .map(|r| {
                    (
                        r,
                        HighlightStyle {
                            color: Some(t.accent.into()),
                            font_weight: Some(FontWeight::BOLD),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            let ab = br.ab.map(ui::ahead_behind);
            row.child(ui::icon(
                icon::BRANCH,
                14.,
                if br.current {
                    t.accent
                } else if br.remote {
                    t.text.faint
                } else {
                    t.text.dim
                },
            ))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(if br.remote { t.text.muted } else { t.text.primary })
                    .child(StyledText::new(SharedString::from(br.name.clone())).with_highlights(hl)),
            )
            .when(br.current, |d| {
                d.child(div().flex_none().text_size(z(text_size::XS)).text_color(t.text.dim).child("current"))
            })
            .when(br.remote, |d| {
                d.child(div().flex_none().text_size(z(text_size::XS)).text_color(t.text.dim).child("remote"))
            })
            .child(div().flex_1())
            .when(br.upstream.is_some() && br.ab.is_none(), |d| {
                d.child(
                    div().flex_none().text_size(z(text_size::XS)).text_color(t.state.needs_you).child("upstream gone"),
                )
            })
            .children(
                ab.filter(|s| !s.is_empty())
                    .map(|s| div().flex_none().text_size(z(text_size::XS)).text_color(t.text.muted).child(s)),
            )
            .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(name: &str, remote: bool) -> Branch {
        Branch { name: name.into(), remote, ..Default::default() }
    }

    #[test]
    fn rows() {
        let mut all = [b("main", false), b("feat/side", false), b("origin/fix-x", true)];
        all[1].current = true;
        let names = |q: &str| {
            filter(&all, q)
                .iter()
                .map(|r| match r {
                    Row::Create(n) => format!("+{n}"),
                    Row::Branch(i, _) => all[*i].name.clone(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(names(""), ["feat/side", "main", "origin/fix-x"], "the current branch first");
        assert_eq!(names("main"), ["main"], "an existing name: no Create row");
        assert_eq!(names("side"), ["+side", "feat/side"]);
        assert_eq!(names("fix-x"), ["+fix-x", "origin/fix-x"], "a remote's name can still become a local branch");
        assert_eq!(names("new thing"), Vec::<String>::new(), "spaces: no Create");
    }
}
