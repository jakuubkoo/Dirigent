//! Settings page over the main area (Ctrl+, / the gear): search on top, categories on the left, rows on the right.
//! Each row shows its default and a reset button. Changes go through `Workspace::change_settings` (live + written to
//! settings.json); project defaults through `Workspace::set_project_defaults` (state file).
//!
//! Handlers are plain closures over weak handles, never `cx.listener`: applying a change updates the workspace,
//! which notifies this view back (re-entrant entity updates panic).

use std::path::PathBuf;

use gpui::{
    ClickEvent, Context, FocusHandle, Focusable, FontWeight, KeyDownEvent, MouseButton, Render, WeakEntity, Window,
    div, prelude::*,
};

use crate::git_panel::{segment, segmented};
use crate::layout::{self, Dock, Layout, Panel};
use crate::line_edit::{Edit, LineEdit};
use crate::projects::{Project, Typed};
use crate::settings::{
    DiffEngine, DiffMode, FETCH_MINUTES, FONT_SIZE, PlusOpens, SCROLLBACK, Settings, SplitStyle, UI_ZOOM,
};
use crate::theme::{self, height, radius, space, text_size, z};
use crate::ui::{self, icon, tip};
use crate::workspace::{Workspace, home, sh_word, tilde};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cat {
    General,
    Terminal,
    Layout,
    Notifications,
    Diff,
    Git,
    Projects,
    Keyboard,
}

const CATS: [(Cat, &str); 8] = [
    (Cat::General, "General"),
    (Cat::Terminal, "Terminal"),
    (Cat::Layout, "Layout"),
    (Cat::Notifications, "Notifications"),
    (Cat::Diff, "Diff"),
    (Cat::Git, "Git"),
    (Cat::Projects, "Projects"),
    (Cat::Keyboard, "Keyboard"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    Trust,
    Zoom,
    Plus,
    FontSize,
    Scrollback,
    NeedsYou,
    Done,
    Sound,
    DiffMode,
    SplitStyle,
    Engine,
    Dock(Panel),
    LeftWidth,
    RightWidth,
    FetchMinutes,
}

enum Ctl {
    Toggle,
    /// Labels, in the order `get`/`set` index them.
    Choice(&'static [&'static str]),
    Number,
}

struct Item {
    key: Key,
    cat: Cat,
    label: &'static str,
    desc: &'static str,
    ctl: Ctl,
}

const ITEMS: &[Item] = &[
    Item {
        key: Key::Trust,
        cat: Cat::General,
        label: "Answer Claude's folder trust dialog",
        desc: "When Dirigent starts claude in a folder it hasn't seen, pick \"Yes, I trust this folder\" for you.",
        ctl: Ctl::Toggle,
    },
    Item {
        key: Key::Plus,
        cat: Cat::General,
        label: "Plain + opens",
        desc: "What a click on a sidebar group's + does. Setup dialog: name, worktree, prompt (Shift+click: a session right away). Otherwise Shift+click starts the other claude choice.",
        ctl: Ctl::Choice(&["Setup dialog", "Project default", "Shell", "Claude"]),
    },
    Item {
        key: Key::Zoom,
        cat: Cat::General,
        label: "UI zoom (%)",
        desc: "Scales the whole window, terminal font included (50–200). Ctrl+= / Ctrl+- step by 10, Ctrl+0 resets.",
        ctl: Ctl::Number,
    },
    Item {
        key: Key::FontSize,
        cat: Cat::Terminal,
        label: "Font size",
        desc: "Terminal text in px at 100 % zoom (8–32); diffs and previews use one less.",
        ctl: Ctl::Number,
    },
    Item {
        key: Key::Scrollback,
        cat: Cat::Terminal,
        label: "Scrollback lines",
        desc: "History kept per session (0–100000). Lowering it drops the oldest lines right away.",
        ctl: Ctl::Number,
    },
    Item {
        key: Key::NeedsYou,
        cat: Cat::Notifications,
        label: "A session needs you",
        desc: "Desktop notification for a permission prompt or a question (not for the session in front of you).",
        ctl: Ctl::Toggle,
    },
    Item {
        key: Key::Done,
        cat: Cat::Notifications,
        label: "A session is done",
        desc: "Desktop notification when an agent finishes its turn while the window isn't focused.",
        ctl: Ctl::Toggle,
    },
    Item {
        key: Key::Sound,
        cat: Cat::Notifications,
        label: "Sound",
        desc: "Play the desktop's message sound with them.",
        ctl: Ctl::Toggle,
    },
    Item {
        key: Key::DiffMode,
        cat: Cat::Diff,
        label: "Default mode",
        desc: "Unified or side by side (Ctrl+Shift+D switches; split needs a wide enough area).",
        ctl: Ctl::Choice(&["Unified", "Split"]),
    },
    Item {
        key: Key::SplitStyle,
        cat: Cat::Diff,
        label: "Split style",
        desc: "Classic: line backgrounds and +/- markers. Clean: plain code, only the changed tokens colored.",
        ctl: Ctl::Choice(&["Classic", "Clean"]),
    },
    Item {
        key: Key::Engine,
        cat: Cat::Diff,
        label: "Engine",
        desc: "Structural: syntax-aware (moved code, reformatting ignored), word diff where it can't. Word: always the word diff.",
        ctl: Ctl::Choice(&["Structural", "Word"]),
    },
    Item {
        key: Key::FetchMinutes,
        cat: Cat::Git,
        label: "Fetch in the background (minutes)",
        desc: "Fetches every open repo this often, so ahead/behind and PR status stay current. 0 = off (fetch with Sync or … → Fetch).",
        ctl: Ctl::Number,
    },
    Item {
        key: Key::Dock(Panel::Sessions),
        cat: Cat::Layout,
        label: "Sessions",
        desc: "Which side of the terminal the session list docks on. Also: drag its tab, or right click it.",
        ctl: DOCKS,
    },
    Item { key: Key::Dock(Panel::Files), cat: Cat::Layout, label: "Files", desc: "The file tree's dock.", ctl: DOCKS },
    Item {
        key: Key::Dock(Panel::Changes),
        cat: Cat::Layout,
        label: "Changes",
        desc: "The git changes' dock.",
        ctl: DOCKS,
    },
    Item {
        key: Key::Dock(Panel::Search),
        cat: Cat::Layout,
        label: "Search",
        desc: "The search results' dock.",
        ctl: DOCKS,
    },
    Item {
        key: Key::LeftWidth,
        cat: Cat::Layout,
        label: "Left dock width",
        desc: "In px at 100 % zoom (100–2000; never narrower than its panels need). Dragging the divider sets it too.",
        ctl: Ctl::Number,
    },
    Item {
        key: Key::RightWidth,
        cat: Cat::Layout,
        label: "Right dock width",
        desc: "In px at 100 % zoom (100–2000; never narrower than its panels need). Dragging the divider sets it too.",
        ctl: Ctl::Number,
    },
];

const DOCKS: Ctl = Ctl::Choice(&["Left", "Right"]);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Val {
    Bool(bool),
    Choice(usize),
    Num(f64),
}

fn get(s: &Settings, k: Key) -> Val {
    let n = &s.notifications;
    match k {
        Key::Trust => Val::Bool(s.trust_auto_accept),
        Key::Plus => Val::Choice(match s.plus_opens {
            PlusOpens::Dialog => 0,
            PlusOpens::Project => 1,
            PlusOpens::Shell => 2,
            PlusOpens::Claude => 3,
        }),
        Key::Zoom => Val::Num(s.ui_zoom as f64),
        Key::FontSize => Val::Num(s.font_size as f64),
        Key::Scrollback => Val::Num(s.scrollback as f64),
        Key::NeedsYou => Val::Bool(n.needs_you),
        Key::Done => Val::Bool(n.done),
        Key::Sound => Val::Bool(n.sound),
        Key::DiffMode => Val::Choice((s.diff_mode == DiffMode::Split) as usize),
        Key::SplitStyle => Val::Choice((s.split_style == SplitStyle::Clean) as usize),
        Key::Engine => Val::Choice((s.diff_engine == DiffEngine::Word) as usize),
        Key::Dock(p) => Val::Choice(s.layout.dock_of(p).ix()),
        Key::LeftWidth => Val::Num(s.layout.left_width as f64),
        Key::RightWidth => Val::Num(s.layout.right_width as f64),
        Key::FetchMinutes => Val::Num(s.fetch_minutes as f64),
    }
}

fn set(s: &mut Settings, k: Key, v: Val) {
    match (k, v) {
        (Key::Trust, Val::Bool(b)) => s.trust_auto_accept = b,
        (Key::Plus, Val::Choice(i)) => {
            s.plus_opens = [PlusOpens::Dialog, PlusOpens::Project, PlusOpens::Shell, PlusOpens::Claude][i.min(3)]
        }
        (Key::Zoom, Val::Num(n)) => s.ui_zoom = n as u32,
        (Key::FontSize, Val::Num(n)) => s.font_size = n as f32,
        (Key::Scrollback, Val::Num(n)) => s.scrollback = n as usize,
        (Key::NeedsYou, Val::Bool(b)) => s.notifications.needs_you = b,
        (Key::Done, Val::Bool(b)) => s.notifications.done = b,
        (Key::Sound, Val::Bool(b)) => s.notifications.sound = b,
        (Key::DiffMode, Val::Choice(i)) => s.diff_mode = if i == 1 { DiffMode::Split } else { DiffMode::Unified },
        (Key::SplitStyle, Val::Choice(i)) => {
            s.split_style = if i == 1 { SplitStyle::Clean } else { SplitStyle::Classic }
        }
        (Key::Engine, Val::Choice(i)) => s.diff_engine = if i == 1 { DiffEngine::Word } else { DiffEngine::Auto },
        (Key::Dock(p), Val::Choice(i)) => s.layout.set_dock(p, if i == 1 { Dock::Right } else { Dock::Left }),
        (Key::LeftWidth, Val::Num(n)) => s.layout.set_width(Dock::Left, n as u32),
        (Key::RightWidth, Val::Num(n)) => s.layout.set_width(Dock::Right, n as u32),
        (Key::FetchMinutes, Val::Num(n)) => s.fetch_minutes = n as u32,
        _ => {}
    }
}

fn show(item: &Item, v: Val) -> String {
    match (v, &item.ctl) {
        (Val::Bool(b), _) => if b { "on" } else { "off" }.into(),
        (Val::Choice(i), Ctl::Choice(labels)) => labels.get(i).copied().unwrap_or_default().into(),
        // Through f32 (font size is one): 13.52 shows as typed, not 13.520000457763672. Every number fits exactly.
        (Val::Num(n), _) => format!("{}", n as f32),
        _ => String::new(),
    }
}

/// Whether `k` is edited by typing: every number field (a key list here once missed the dock widths).
fn typed(k: Key) -> bool {
    ITEMS.iter().any(|i| i.key == k && matches!(i.ctl, Ctl::Number))
}

/// A typed number for `k`, or what's wrong with it.
fn parse_num(k: Key, text: &str) -> Result<f64, String> {
    let t = text.trim();
    match k {
        Key::FontSize => t
            .parse::<f32>()
            .ok()
            .filter(|n| (FONT_SIZE.0..=FONT_SIZE.1).contains(n))
            .map(|n| n as f64)
            .ok_or_else(|| format!("Needs a number from {} to {}", FONT_SIZE.0, FONT_SIZE.1)),
        Key::Zoom => t
            .trim_end_matches('%')
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| (UI_ZOOM.0..=UI_ZOOM.1).contains(n))
            .map(|n| n as f64)
            .ok_or_else(|| format!("Needs a whole number from {} to {}", UI_ZOOM.0, UI_ZOOM.1)),
        Key::FetchMinutes => t
            .trim_end_matches("min")
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| (FETCH_MINUTES.0..=FETCH_MINUTES.1).contains(n))
            .map(|n| n as f64)
            .ok_or_else(|| format!("Needs a whole number from {} to {} (0 = off)", FETCH_MINUTES.0, FETCH_MINUTES.1)),
        Key::LeftWidth | Key::RightWidth => t
            .trim_end_matches("px")
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| (layout::WIDTH.0..=layout::WIDTH.1).contains(n))
            .map(|n| n as f64)
            .ok_or_else(|| format!("Needs a whole number from {} to {}", layout::WIDTH.0, layout::WIDTH.1)),
        _ => t
            .parse::<usize>()
            .ok()
            .filter(|n| (SCROLLBACK.0..=SCROLLBACK.1).contains(n))
            .map(|n| n as f64)
            .ok_or_else(|| format!("Needs a whole number from {} to {}", SCROLLBACK.0, SCROLLBACK.1)),
    }
}

/// What has keyboard focus on the page.
#[derive(Clone, PartialEq, Debug, Default)]
enum Field {
    #[default]
    Search,
    Item(Key),
    /// A project's 0 auto claude, 1 flags, 2 setup command, 3 worktree dir.
    Proj(PathBuf, u8),
}

/// A change to hand to the workspace once this view's update is over.
enum Apply {
    Settings(Key, Val),
    Project(PathBuf, Defaults),
    ResetLayout,
    Close,
}

/// A project's defaults as text (what `Project::apply_defaults` takes).
#[derive(Clone, Default)]
struct Defaults {
    auto: bool,
    flags: String,
    setup: String,
    wt_dir: String,
}

impl Defaults {
    fn of(p: &Project) -> Self {
        Self {
            auto: p.auto_claude,
            flags: p.claude_args.iter().map(|a| sh_word(a)).collect::<Vec<_>>().join(" "),
            setup: p.setup_cmd.clone().unwrap_or_default(),
            wt_dir: p.worktrees_dir.as_deref().map(tilde).unwrap_or_default(),
        }
    }

    fn field(&mut self, i: u8) -> &mut String {
        match i {
            1 => &mut self.flags,
            2 => &mut self.setup,
            _ => &mut self.wt_dir,
        }
    }
}

pub struct SettingsView {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    search: LineEdit,
    cat: Cat,
    form: Form,
}

/// Which row has keyboard focus and the value being typed there (no gpui: unit-tested).
#[derive(Default)]
struct Form {
    field: Field,
    /// The text field being edited (number or project field): its buffer.
    edit: Option<LineEdit>,
    /// A typed value that was refused, shown under its row until the next edit.
    error: Option<(Field, String)>,
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsView {
    pub fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            focus: cx.focus_handle(),
            search: LineEdit::new(""),
            cat: Cat::General,
            form: Form::default(),
        }
    }

    /// Screenshot harness (`DIRIGENT_PERF_SETTINGS`): a category name selects it, anything else is searched.
    pub fn harness(&mut self, what: &str, cx: &mut Context<Self>) {
        match CATS.iter().find(|(_, n)| n.eq_ignore_ascii_case(what)) {
            Some((c, _)) => self.cat = *c,
            None => self.search = LineEdit::new(what),
        }
        cx.notify();
    }

    fn query(&self) -> String {
        self.search.text.trim().to_lowercase()
    }

    fn hit(q: &str, texts: &[&str]) -> bool {
        q.is_empty() || q.split_whitespace().all(|w| texts.iter().any(|t| t.to_lowercase().contains(w)))
    }

    fn items_of(&self, cat: Cat, q: &str) -> Vec<&'static Item> {
        let name = CATS.iter().find(|(c, _)| *c == cat).map_or("", |(_, n)| *n);
        ITEMS.iter().filter(|i| i.cat == cat && Self::hit(q, &[i.label, i.desc, name])).collect()
    }

    fn projects_of(&self, projects: &[Project], q: &str) -> Vec<Project> {
        let mut v: Vec<Project> = projects
            .iter()
            .filter(|p| {
                Self::hit(q, &[&p.display(), &p.root.to_string_lossy(), "projects claude flags worktree setup"])
            })
            .cloned()
            .collect();
        v.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.last_used.cmp(&a.last_used)));
        v
    }

    fn keys_of(q: &str) -> Vec<(String, &'static str)> {
        crate::keys::listed().filter(|(k, l)| Self::hit(q, &[k, l, "keyboard shortcuts keys"])).collect()
    }

    /// Categories shown: the selected one, or every one with hits while searching.
    fn shown(&self, projects: &[Project]) -> Vec<Cat> {
        let q = self.query();
        if q.is_empty() {
            return vec![self.cat];
        }
        CATS.iter().map(|(c, _)| *c).filter(|&c| self.has_hits(c, &q, projects)).collect()
    }

    fn has_hits(&self, c: Cat, q: &str, projects: &[Project]) -> bool {
        match c {
            Cat::Projects => !self.projects_of(projects, q).is_empty(),
            Cat::Keyboard => !Self::keys_of(q).is_empty(),
            c => !self.items_of(c, q).is_empty(),
        }
    }

    /// Tab order: search, then every focusable row shown.
    fn fields(&self, projects: &[Project]) -> Vec<Field> {
        let q = self.query();
        let mut v = vec![Field::Search];
        for c in self.shown(projects) {
            match c {
                Cat::Projects => {
                    for p in self.projects_of(projects, &q) {
                        v.extend((0..4).map(|i| Field::Proj(p.root.clone(), i)));
                    }
                }
                Cat::Keyboard => {}
                c => v.extend(self.items_of(c, &q).iter().map(|i| Field::Item(i.key))),
            }
        }
        v
    }

    fn focus_field(&mut self, f: Field, s: &Settings, projects: &[Project]) -> Option<Apply> {
        self.form.focus_field(f, s, projects)
    }

    fn key(&mut self, ev: &KeyDownEvent, s: &Settings, projects: &[Project], cx: &mut Context<Self>) -> Option<Apply> {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        let key = ks.key.as_str();
        cx.notify();
        if key == "tab"
            || (matches!(key, "down" | "up") && self.form.edit.is_none() && self.form.field != Field::Search)
            || (key == "down" && self.form.field == Field::Search)
        {
            let fields = self.fields(projects);
            let at = fields.iter().position(|f| *f == self.form.field).unwrap_or(0);
            let back = (key == "tab" && m.shift) || key == "up";
            let next = if back { (at + fields.len() - 1) % fields.len() } else { (at + 1) % fields.len() };
            return self.focus_field(fields[next].clone(), s, projects);
        }
        if key == "escape" {
            if self.form.edit.is_some() {
                self.form.edit = None; // drop the typed value
                self.form.error = None;
                return None;
            }
            if self.form.field == Field::Search && !self.search.text.is_empty() {
                self.search = LineEdit::new("");
                return None;
            }
            return Some(Apply::Close);
        }
        // Text: the search field or an edited value.
        let target = if self.form.field == Field::Search { Some(&mut self.search) } else { self.form.edit.as_mut() };
        if let Some(edit) = target {
            return match edit.key(ks, cx) {
                Edit::Submit if self.form.field != Field::Search => self.form.submit(s, projects),
                Edit::Changed => {
                    self.form.error = None;
                    if self.form.field == Field::Search {
                        self.cat = self.shown(projects).first().copied().unwrap_or(self.cat);
                    }
                    None
                }
                _ => None,
            };
        }
        // Toggles and choices.
        match self.form.field.clone() {
            Field::Item(k) => {
                let next = match (get(s, k), &ITEMS.iter().find(|i| i.key == k)?.ctl) {
                    (Val::Bool(b), _) if matches!(key, "space" | "enter") => Val::Bool(!b),
                    (Val::Choice(i), Ctl::Choice(l)) if matches!(key, "right" | "space" | "enter") => {
                        Val::Choice((i + 1) % l.len())
                    }
                    (Val::Choice(i), Ctl::Choice(l)) if key == "left" => Val::Choice((i + l.len() - 1) % l.len()),
                    _ => return None,
                };
                Some(Apply::Settings(k, next))
            }
            Field::Proj(root, 0) if matches!(key, "space" | "enter") => {
                let p = projects.iter().find(|p| p.root == root)?;
                Some(Apply::Project(root, Defaults { auto: !p.auto_claude, ..Defaults::of(p) }))
            }
            _ => None,
        }
    }
}

impl Form {
    /// Moves keyboard focus; an edit in progress is committed first (or dropped with a note if refused).
    fn focus_field(&mut self, f: Field, s: &Settings, projects: &[Project]) -> Option<Apply> {
        let apply = self.commit(s, projects);
        self.edit = match &f {
            Field::Item(k) if typed(*k) => {
                let item = ITEMS.iter().find(|i| i.key == *k)?;
                Some(LineEdit::new(show(item, get(s, *k))))
            }
            Field::Proj(root, i @ 1..=3) => {
                let mut d = projects.iter().find(|p| &p.root == root).map(Defaults::of).unwrap_or_default();
                Some(LineEdit::new(std::mem::take(d.field(*i))))
            }
            _ => None,
        };
        self.field = f;
        apply
    }

    /// The edited field's value, if it changed and is valid.
    fn commit(&mut self, s: &Settings, projects: &[Project]) -> Option<Apply> {
        let edit = self.edit.take()?;
        match self.field.clone() {
            Field::Item(k) => match parse_num(k, &edit.text) {
                Ok(n) if Val::Num(n) != get(s, k) => Some(Apply::Settings(k, Val::Num(n))),
                Ok(_) => None,
                Err(e) => {
                    self.error = Some((
                        self.field.clone(),
                        format!("{e}: kept {}", show(ITEMS.iter().find(|i| i.key == k)?, get(s, k))),
                    ));
                    None
                }
            },
            Field::Proj(root, i) => {
                let p = projects.iter().find(|p| p.root == root)?;
                let old = Defaults::of(p);
                let mut d = old.clone();
                *d.field(i) = edit.text.clone();
                (d.field(i) != old.clone().field(i)).then_some(Apply::Project(root, d))
            }
            Field::Search => None,
        }
    }

    /// Enter in a value, or a click anywhere on the page: the typed value is applied and the same field stays
    /// focused, showing the value as applied (not the one before it). Focus never moves by itself.
    fn submit(&mut self, s: &Settings, projects: &[Project]) -> Option<Apply> {
        let apply = self.commit(s, projects);
        let (mut s, mut projects) = (s.clone(), projects.to_vec());
        match &apply {
            Some(Apply::Settings(k, v)) => set(&mut s, *k, *v),
            Some(Apply::Project(root, d)) => {
                if let Some(p) = projects.iter_mut().find(|p| &p.root == root) {
                    p.apply_defaults(
                        &Typed { auto: d.auto, flags: &d.flags, setup: &d.setup, wt_dir: &d.wt_dir },
                        &home(),
                    );
                }
            }
            _ => {}
        }
        self.focus_field(self.field.clone(), &s, &projects);
        apply
    }
}

/// Runs `f` on the view, then hands its change to the workspace (outside the view's update).
fn act(
    view: &WeakEntity<SettingsView>,
    ws: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut gpui::App,
    f: impl FnOnce(&mut SettingsView, &Settings, &[Project], &mut Context<SettingsView>) -> Option<Apply>,
) {
    let Some(w) = ws.upgrade() else { return };
    let (s, projects) = {
        let w = w.read(cx);
        (w.settings.clone(), w.projects.0.clone())
    };
    let Ok(apply) = view.update(cx, |v, cx| {
        cx.notify();
        f(v, &s, &projects, cx)
    }) else {
        return;
    };
    match apply {
        None => {}
        Some(Apply::Close) => w.update(cx, |w, cx| w.close_overlay(window, cx)),
        Some(Apply::Settings(k, v)) => w.update(cx, |w, cx| w.change_settings(|s| set(s, k, v), cx)),
        Some(Apply::ResetLayout) => w.update(cx, |w, cx| w.change_settings(|s| s.layout = Layout::default(), cx)),
        Some(Apply::Project(root, d)) => w.update(cx, |w, cx| {
            let typed = Typed { auto: d.auto, flags: &d.flags, setup: &d.setup, wt_dir: &d.wt_dir };
            w.set_project_defaults(&root, &typed, cx)
        }),
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        let Some(w) = self.workspace.upgrade() else { return div().into_any_element() };
        let (s, notes, file, projects) = {
            let w = w.read(cx);
            (w.settings.clone(), w.settings_notes.clone(), w.settings_path.clone(), w.projects.0.clone())
        };
        let (view, ws) = (cx.entity().downgrade(), self.workspace.clone());
        let q = self.query();
        let shown = self.shown(&projects);

        let header = ui::overlay_header()
            .gap(z(space::SM))
            .px(z(space::LG))
            .child(ui::icon(icon::SETTINGS, 13., t.text.muted))
            .child(div().text_size(z(text_size::SM)).font_weight(FontWeight::MEDIUM).child("Settings"))
            .child(div().min_w_0().truncate().text_size(z(text_size::XS)).text_color(t.text.dim).child(tilde(&file)))
            .child(div().flex_1())
            .child(ui::hint("Esc to close"));

        let search = {
            let (view, ws) = (view.clone(), ws.clone());
            div()
                .id("settings-search")
                .flex()
                .items_center()
                .gap(z(space::SM))
                .px(z(space::LG))
                .py(z(space::MD))
                .border_b_1()
                .border_color(t.border.subtle)
                .child(ui::icon(icon::SEARCH, 13., t.text.dim))
                .child(div().flex_1().max_w(z(480.)).child(ui::text_input(
                    &self.search,
                    self.form.field == Field::Search,
                    "Search settings",
                )))
                .on_click(move |_: &ClickEvent, window, cx| {
                    act(&view, &ws, window, cx, |v, s, p, _| v.focus_field(Field::Search, s, p))
                })
        };

        let nav = div().flex_none().w(z(180.)).py(z(space::MD)).border_r_1().border_color(t.border.subtle).children(
            CATS.iter().map(|&(c, name)| {
                let selected = if q.is_empty() { c == self.cat } else { shown.first() == Some(&c) };
                let dim = !q.is_empty() && !shown.contains(&c);
                let (view, ws) = (view.clone(), ws.clone());
                div()
                    .id(name)
                    .mx(z(space::SM))
                    .px(z(space::MD))
                    .h(z(height::LIST_ROW + 4.))
                    .flex()
                    .items_center()
                    .rounded(z(radius::MD))
                    .cursor_pointer()
                    .text_size(z(text_size::SM))
                    .text_color(if dim {
                        t.text.faint
                    } else if selected {
                        t.text.primary
                    } else {
                        t.text.muted
                    })
                    .when(selected, |d| d.bg(t.bg.selected))
                    .when(!selected, |d| d.hover(|s| s.bg(t.bg.hover)))
                    .child(name)
                    .on_click(move |_: &ClickEvent, window, cx| {
                        act(&view, &ws, window, cx, |v, s, p, _| {
                            v.search = LineEdit::new("");
                            v.cat = c;
                            v.focus_field(Field::Search, s, p)
                        })
                    })
            }),
        );

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        if !notes.is_empty() {
            rows.push(
                div()
                    .mb(z(space::LG))
                    .p(z(space::MD))
                    .flex()
                    .gap(z(space::SM))
                    .rounded(z(radius::MD))
                    .border_1()
                    .border_color(t.state.needs_you)
                    .text_size(z(text_size::SM))
                    .child(ui::icon(icon::WARNING, 13., t.state.needs_you))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(z(space::XXS))
                            .children(notes.iter().map(|n| div().child(n.clone()))),
                    )
                    .into_any_element(),
            );
        }
        if shown.is_empty() {
            rows.push(ui::note("No setting matches.").into_any_element());
        }
        for &c in &shown {
            let name = CATS.iter().find(|(x, _)| *x == c).map_or("", |(_, n)| *n);
            rows.push(
                div()
                    .pt(z(space::MD))
                    .pb(z(space::XS))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name)
                    .into_any_element(),
            );
            match c {
                Cat::Projects => {
                    let list = self.projects_of(&projects, &q);
                    if list.is_empty() {
                        rows.push(
                            ui::note("No projects yet: open a session from the picker (Ctrl+Shift+T).")
                                .into_any_element(),
                        );
                    }
                    for p in list {
                        rows.push(self.project_card(&p, &view, &ws));
                    }
                }
                Cat::Keyboard => {
                    rows.push(
                        div()
                            .pb(z(space::SM))
                            .text_size(z(text_size::XS))
                            .text_color(t.text.dim)
                            .child("Read-only for now: rebinding comes later.")
                            .into_any_element(),
                    );
                    for (k, l) in Self::keys_of(&q) {
                        rows.push(
                            div()
                                .flex()
                                .items_center()
                                .gap(z(space::LG))
                                .py(z(space::XS + 1.))
                                .border_b_1()
                                .border_color(t.border.subtle)
                                .text_size(z(text_size::SM))
                                .child(div().flex_1().text_color(t.text.muted).child(l))
                                .child(
                                    div()
                                        .flex_none()
                                        .px(z(space::SM))
                                        .rounded(z(radius::SM))
                                        .bg(t.bg.elevated)
                                        .text_size(z(text_size::XS))
                                        .child(k),
                                )
                                .into_any_element(),
                        );
                    }
                }
                c => {
                    for item in self.items_of(c, &q) {
                        rows.push(self.item_row(item, &s, &view, &ws));
                    }
                    if c == Cat::Layout {
                        rows.push(Self::reset_layout(s.layout != Layout::default(), &view, &ws));
                    }
                }
            }
        }

        let (view2, ws2) = (view.clone(), ws.clone());
        let (view3, ws3) = (view.clone(), ws.clone());
        div()
            .id("settings")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg.base)
            .text_color(t.text.primary)
            .on_key_down(move |ev: &KeyDownEvent, window, cx| {
                act(&view2, &ws2, window, cx, |v, s, p, cx| v.key(ev, s, p, cx));
                cx.stop_propagation();
            })
            // A click anywhere commits a value being edited; focus stays (a control's own click then moves it).
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                act(&view3, &ws3, window, cx, |v, s, p, _| v.form.submit(s, p))
            })
            .child(header)
            .child(search)
            .child(
                div().flex().flex_1().min_h_0().child(nav).child(
                    div()
                        .id("settings-rows")
                        .flex_1()
                        .min_w_0()
                        .overflow_y_scroll()
                        .px(z(space::XL))
                        .py(z(space::MD))
                        .child(div().max_w(z(820.)).flex().flex_col().children(rows)),
                ),
            )
            .into_any_element()
    }
}

impl SettingsView {
    /// The reset button: off (faint) when the value already is the default.
    fn reset(id: gpui::ElementId, enabled: bool) -> gpui::Stateful<gpui::Div> {
        let t = theme::get();
        div()
            .id(id)
            .flex_none()
            .size(z(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(z(radius::MD))
            .child(ui::icon(icon::RESTART, 13., if enabled { t.text.muted } else { t.text.faint }))
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(t.bg.hover)).tooltip(tip("Reset to default")))
            .when(!enabled, |d| d.cursor_not_allowed())
    }

    /// "Reset layout": every panel back to its default dock, both widths to their defaults.
    fn reset_layout(enabled: bool, view: &WeakEntity<SettingsView>, ws: &WeakEntity<Workspace>) -> gpui::AnyElement {
        let t = theme::get();
        let (view, ws) = (view.clone(), ws.clone());
        div()
            .pt(z(space::MD))
            .flex()
            .child(
                div()
                    .id("reset-layout")
                    .flex()
                    .items_center()
                    .gap(z(space::SM))
                    .px(z(space::MD))
                    .h(z(height::LIST_ROW + space::XS))
                    .rounded(z(radius::MD))
                    .border_1()
                    .border_color(t.border.subtle)
                    .text_size(z(text_size::SM))
                    .text_color(if enabled { t.text.primary } else { t.text.faint })
                    .child(ui::icon(icon::RESTART, 13., if enabled { t.text.muted } else { t.text.faint }))
                    .child("Reset layout")
                    .when(!enabled, |d| d.cursor_not_allowed())
                    .when(enabled, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.bg(t.bg.hover))
                            .tooltip(tip("Sessions left, the other panels right, default widths"))
                            .on_click(move |_: &ClickEvent, window, cx| {
                                act(&view, &ws, window, cx, |_, _, _, _| Some(Apply::ResetLayout))
                            })
                    }),
            )
            .into_any_element()
    }

    fn row_text(label: String, desc: &str, default: String, error: Option<&String>, focused: bool) -> gpui::Div {
        let t = theme::get();
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(z(space::XXS))
            .child(
                div()
                    .text_size(z(text_size::SM))
                    .text_color(if focused { t.accent } else { t.text.primary })
                    .font_weight(FontWeight::MEDIUM)
                    .child(label),
            )
            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(desc.to_string()))
            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(default))
            .children(error.map(|e| div().text_size(z(text_size::XS)).text_color(t.state.error).child(e.clone())))
    }

    fn item_row(
        &self,
        item: &'static Item,
        s: &Settings,
        view: &WeakEntity<SettingsView>,
        ws: &WeakEntity<Workspace>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let (val, def) = (get(s, item.key), get(&Settings::default(), item.key));
        let field = Field::Item(item.key);
        let focused = self.form.field == field;
        let error = self.form.error.as_ref().filter(|(f, _)| *f == field).map(|(_, e)| e);
        let focus_click = |view: WeakEntity<SettingsView>, ws: WeakEntity<Workspace>, f: Field| {
            move |_: &ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                act(&view, &ws, window, cx, |v, s, p, _| v.focus_field(f.clone(), s, p))
            }
        };
        let control: gpui::AnyElement = match &item.ctl {
            Ctl::Toggle => {
                let on = val == Val::Bool(true);
                let (view, ws) = (view.clone(), ws.clone());
                div()
                    .id(item.label)
                    .p(z(space::XS))
                    .rounded(z(radius::SM))
                    .cursor_pointer()
                    .when(focused, |d| d.border_1().border_color(t.accent))
                    .child(ui::check_box(on))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        let k = item.key;
                        act(&view, &ws, window, cx, |v, s, p, _| {
                            v.focus_field(Field::Item(k), s, p);
                            Some(Apply::Settings(k, Val::Bool(!on)))
                        })
                    })
                    .into_any_element()
            }
            Ctl::Choice(labels) => {
                let Val::Choice(cur) = val else { return div().into_any_element() };
                segmented()
                    .when(focused, |d| d.border_color(t.accent))
                    .children(labels.iter().enumerate().map(|(i, l)| {
                        let (view, ws, k) = (view.clone(), ws.clone(), item.key);
                        segment(l, *l, i == cur, true).on_click(move |_: &ClickEvent, window, cx| {
                            act(&view, &ws, window, cx, |v, s, p, _| {
                                v.focus_field(Field::Item(k), s, p);
                                Some(Apply::Settings(k, Val::Choice(i)))
                            })
                        })
                    }))
                    .into_any_element()
            }
            Ctl::Number => {
                let shown = match (&self.form.edit, focused) {
                    (Some(e), true) => ui::text_input(e, true, ""),
                    _ => ui::text_input(&LineEdit::new(show(item, val)), false, ""),
                };
                div()
                    .id(item.label)
                    .w(z(110.))
                    .cursor_text()
                    .child(shown)
                    .on_click(focus_click(view.clone(), ws.clone(), field.clone()))
                    .into_any_element()
            }
        };
        let (view, ws, k) = (view.clone(), ws.clone(), item.key);
        div()
            .flex()
            .items_center()
            .gap(z(space::LG))
            .py(z(space::MD))
            .border_b_1()
            .border_color(t.border.subtle)
            .child(Self::row_text(
                item.label.into(),
                item.desc,
                format!("Default: {}", show(item, def)),
                error,
                focused,
            ))
            .child(div().flex_none().child(control))
            .child(Self::reset(gpui::ElementId::Name(format!("reset-{}", item.label).into()), val != def).on_click(
                move |_: &ClickEvent, window, cx| {
                    if val != def {
                        act(&view, &ws, window, cx, |v, s, p, _| {
                            v.focus_field(Field::Item(k), s, p);
                            v.form.edit = None;
                            Some(Apply::Settings(k, def))
                        })
                    }
                },
            ))
            .into_any_element()
    }

    fn project_card(
        &self,
        p: &Project,
        view: &WeakEntity<SettingsView>,
        ws: &WeakEntity<Workspace>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let d = Defaults::of(p);
        let root = p.root.clone();
        let is_default = !d.auto && d.flags.is_empty() && d.setup.is_empty() && d.wt_dir.is_empty();
        let id = |s: &str| gpui::ElementId::Name(format!("{s}-{}", root.display()).into());
        let focused = |i: u8| self.form.field == Field::Proj(root.clone(), i);
        let input = |i: u8, value: &str, placeholder: &'static str| -> gpui::AnyElement {
            let el = match (&self.form.edit, focused(i)) {
                (Some(e), true) => ui::text_input(e, true, placeholder),
                _ => ui::text_input(&LineEdit::new(value), false, placeholder),
            };
            let (view, ws, f) = (view.clone(), ws.clone(), Field::Proj(root.clone(), i));
            div()
                .id(id(&format!("p{i}")))
                .cursor_text()
                .child(el)
                .on_click(move |_: &ClickEvent, window, cx| {
                    act(&view, &ws, window, cx, |v, s, p, _| v.focus_field(f.clone(), s, p))
                })
                .into_any_element()
        };
        let label = |text: &'static str| div().text_size(z(text_size::XS)).text_color(t.text.muted).child(text);
        // Live like the defaults dialog: what the allowlist would drop from the flags being typed.
        let typed_flags = match (&self.form.edit, focused(1)) {
            (Some(e), true) => e.text.clone(),
            _ => d.flags.clone(),
        };
        let words = hook::split_words(&typed_flags);
        let kept = hook::resume_args(&words);
        let dropped: Vec<&String> = {
            let mut k = kept.iter().peekable();
            words
                .iter()
                .filter(|w| {
                    if k.peek() == Some(w) {
                        k.next();
                        false
                    } else {
                        true
                    }
                })
                .collect()
        };
        let auto = d.auto;
        let (view_a, ws_a, root_a, d_a) = (view.clone(), ws.clone(), root.clone(), d.clone());
        let (view_r, ws_r, root_r) = (view.clone(), ws.clone(), root.clone());
        div()
            .my(z(space::SM))
            .p(z(space::LG))
            .flex()
            .flex_col()
            .gap(z(space::SM))
            .rounded(z(radius::LG))
            .border_1()
            .border_color(t.border.subtle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(z(space::SM))
                    .when(p.pinned, |d| d.child(ui::icon(icon::PIN, 11., t.text.dim)))
                    .child(div().text_size(z(text_size::SM)).font_weight(FontWeight::MEDIUM).child(p.display()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(z(text_size::XS))
                            .text_color(t.text.dim)
                            .child(tilde(&p.root)),
                    )
                    .child(Self::reset(id("reset"), !is_default).on_click(move |_: &ClickEvent, window, cx| {
                        if !is_default {
                            let root = root_r.clone();
                            act(&view_r, &ws_r, window, cx, |v, _, _, _| {
                                v.form.edit = None;
                                Some(Apply::Project(root, Defaults::default()))
                            })
                        }
                    })),
            )
            .child(
                div()
                    .text_size(z(text_size::XS))
                    .text_color(t.text.dim)
                    .child("Default: shell, no flags, no setup command, worktrees in <repo>-worktrees"),
            )
            .child(
                div()
                    .id(id("auto"))
                    .flex()
                    .items_center()
                    .gap(z(space::SM))
                    .cursor_pointer()
                    .text_size(z(text_size::SM))
                    .text_color(if focused(0) { t.text.primary } else { t.text.muted })
                    .child(
                        div()
                            .p(z(2.))
                            .rounded(z(radius::SM))
                            .when(focused(0), |d| d.border_1().border_color(t.accent))
                            .child(ui::check_box(auto)),
                    )
                    .child("Start claude in new sessions")
                    .on_click(move |_: &ClickEvent, window, cx| {
                        let (root, d) = (root_a.clone(), d_a.clone());
                        act(&view_a, &ws_a, window, cx, |v, s, p, _| {
                            v.focus_field(Field::Proj(root.clone(), 0), s, p);
                            Some(Apply::Project(root, Defaults { auto: !d.auto, ..d }))
                        })
                    }),
            )
            .child(label("Claude flags (only flags safe to replay are kept; permission bypasses never)"))
            .child(input(1, &d.flags, "--model opus"))
            .when(!dropped.is_empty(), |e| {
                e.child(div().text_size(z(text_size::XS)).text_color(t.state.needs_you).child(format!(
                    "Will be dropped (not allowed): {}",
                    dropped.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")
                )))
            })
            .child(label("Setup command for new worktrees (runs before claude)"))
            .child(input(2, &d.setup, "e.g. pnpm install"))
            .child(label("Where new worktrees go (empty: <repo>-worktrees next to the repo)"))
            .child(input(3, &d.wt_dir, "~/worktrees/project"))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_item_round_trips_and_resets() {
        let d = Settings::default();
        let mut s = d.clone();
        for item in ITEMS {
            let v = get(&d, item.key);
            let other = match (v, &item.ctl) {
                (Val::Bool(b), _) => Val::Bool(!b),
                (Val::Choice(i), Ctl::Choice(l)) => Val::Choice((i + 1) % l.len()),
                (Val::Num(_), _) => Val::Num(match item.key {
                    Key::FontSize => 16.,
                    Key::Zoom => 150.,
                    Key::LeftWidth | Key::RightWidth => 300.,
                    Key::FetchMinutes => 15.,
                    _ => 500.,
                }),
                _ => panic!("{} has a mismatched control", item.label),
            };
            set(&mut s, item.key, other);
            assert_eq!(get(&s, item.key), other, "{}", item.label);
            set(&mut s, item.key, v);
        }
        assert_eq!(s, d, "set back to every default");
        for i in 0..4 {
            set(&mut s, Key::Plus, Val::Choice(i));
            assert_eq!(get(&s, Key::Plus), Val::Choice(i), "Plain + opens, choice {i}");
        }
    }

    #[test]
    fn typed_numbers_are_checked() {
        for k in [Key::Zoom, Key::FontSize, Key::Scrollback, Key::LeftWidth, Key::RightWidth, Key::FetchMinutes] {
            assert!(typed(k), "{k:?} can't be typed into");
        }
        assert!(!typed(Key::Trust));
        assert_eq!(parse_num(Key::FontSize, " 15.5 "), Ok(15.5));
        assert!(parse_num(Key::FontSize, "40").is_err());
        assert!(parse_num(Key::Scrollback, "1.5").is_err());
        assert_eq!(parse_num(Key::Scrollback, "0"), Ok(0.));
        assert_eq!(parse_num(Key::Zoom, "120 %"), Ok(120.));
        assert!(parse_num(Key::Zoom, "40").is_err());
        assert_eq!(parse_num(Key::LeftWidth, "260 px"), Ok(260.));
        assert!(parse_num(Key::RightWidth, "50").is_err());
    }

    /// Enter, then a click anywhere (also on empty space): focus stays on the edited field, never back to the
    /// search box, and the field shows the value just applied, not the one before it.
    #[test]
    fn typing_a_value_and_applying_it_keeps_focus_on_the_field() {
        let mut s = Settings::default();
        let mut f = Form::default();
        assert!(f.focus_field(Field::Item(Key::FontSize), &s, &[]).is_none());
        for (typed, want) in [("20", 20f32), ("13.52", 13.52)] {
            f.edit = Some(LineEdit::new(typed));
            let applied = f.submit(&s, &[]); // Enter
            assert!(
                matches!(applied, Some(Apply::Settings(Key::FontSize, Val::Num(n))) if n as f32 == want),
                "{typed} applied"
            );
            set(&mut s, Key::FontSize, Val::Num(want as f64)); // what the workspace does with it
            assert_eq!(f.field, Field::Item(Key::FontSize), "Enter on {typed}");
            assert_eq!(f.edit.as_ref().map(|e| e.text.as_str()), Some(typed), "shown after Enter");
            assert!(f.submit(&s, &[]).is_none(), "a click with nothing new typed applies nothing");
            assert_eq!(f.field, Field::Item(Key::FontSize), "a click on the page after {typed}");
            assert_eq!(f.edit.as_ref().map(|e| e.text.as_str()), Some(typed), "shown after the click");
        }
        // A refused value keeps the field and the old value, with the note.
        f.edit = Some(LineEdit::new("99"));
        assert!(f.submit(&s, &[]).is_none());
        assert_eq!(f.field, Field::Item(Key::FontSize));
        assert_eq!(f.edit.as_ref().map(|e| e.text.as_str()), Some("13.52"));
        assert!(f.error.is_some());
    }

    #[test]
    fn search_matches_all_words_anywhere() {
        assert!(SettingsView::hit("font size", &["Font size", "Terminal text"]));
        assert!(SettingsView::hit("trust", &["Answer Claude's folder trust dialog"]));
        assert!(!SettingsView::hit("font color", &["Font size"]));
        assert!(SettingsView::hit("", &[]));
    }
}
