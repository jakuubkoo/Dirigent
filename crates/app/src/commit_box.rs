//! The commit box at the bottom of the Changes panel: a multi-line message (`text_area`) with Generate, Amend,
//! and one split button whose main action follows the panel's scope (This agent → the agent's files, with a
//! confirmation; All → the staged changes); its ▾ adds Commit & push / Commit, push & create PR. One per repo (the draft survives tab switches); the git work happens in the
//! workspace (`run_git`), off the UI thread.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    Bounds, ClickEvent, ClipboardItem, Context, FocusHandle, HighlightStyle, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, StyledText, TextLayout, WeakEntity, Window, canvas, div, fill, prelude::*, size,
};

use crate::text_area::{Key, TextArea};
use crate::theme::{self, MONO_FONT, radius, space, text_size, z};
use crate::ui::{self, icon, tip};
use crate::workspace::Workspace;

const MIN_LINES: usize = 3;
const MAX_LINES: usize = 10;

pub struct CommitBox {
    pub root: PathBuf,
    pub area: TextArea,
    pub amend: bool,
    pub generating: bool,
    focus: FocusHandle,
    /// This frame's line layouts (for clicks and the cursor).
    layouts: Rc<RefCell<Vec<TextLayout>>>,
    dragging: bool,
    workspace: WeakEntity<Workspace>,
    /// Pushed by the workspace with each panel sync.
    pub props: Props,
}

#[derive(Clone, Default, PartialEq)]
pub struct Props {
    pub staged: usize,
    /// The active tab's agent has changed files here (count).
    pub agent_files: Option<usize>,
    /// The main button commits the agent's files (This agent shown and it has some).
    pub scope_agent: bool,
    pub busy: bool,
}

impl CommitBox {
    pub fn new(root: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            root,
            area: TextArea::default(),
            amend: false,
            generating: false,
            focus: cx.focus_handle(),
            layouts: Default::default(),
            dragging: false,
            workspace,
            props: Props::default(),
        }
    }

    pub fn set_props(&mut self, props: Props, cx: &mut Context<Self>) {
        if props != self.props {
            self.props = props;
            cx.notify();
        }
    }

    /// The message to commit (trimmed; None when empty).
    pub fn message(&self) -> Option<String> {
        let m = self.area.text.trim();
        (!m.is_empty()).then(|| m.to_string())
    }

    /// After a successful commit.
    pub fn committed(&mut self, cx: &mut Context<Self>) {
        self.area = TextArea::default();
        self.amend = false;
        cx.notify();
    }

    pub fn set_message(&mut self, text: &str, cx: &mut Context<Self>) {
        self.area.set_text(text);
        cx.notify();
    }

    /// Whether the main button / Ctrl+Enter can run.
    fn can_commit(&self) -> bool {
        let p = &self.props;
        let what = if p.scope_agent { p.agent_files.is_some_and(|n| n > 0) } else { p.staged > 0 || self.amend };
        self.message().is_some() && !p.busy && what
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let (ctrl, shift) = (ks.modifiers.control, ks.modifiers.shift);
        if ctrl && ks.key == "enter" {
            if self.can_commit() {
                let (root, ws) = (self.root.clone(), self.workspace.clone());
                // Not now: the workspace reads this box back while committing.
                cx.defer_in(window, move |_, window, cx| {
                    let _ = ws.update(cx, |w, cx| w.commit_main(root, Vec::new(), window, cx));
                });
            }
            cx.stop_propagation();
            return;
        }
        if ks.key == "escape" {
            let ws = self.workspace.clone();
            cx.defer_in(window, move |_, window, cx| {
                let _ = ws.update(cx, |w, cx| w.focus_main(window, cx));
            });
            cx.stop_propagation();
            return;
        }
        let result = if ctrl && ks.key == "v" {
            match cx.read_from_clipboard().and_then(|c| c.text()) {
                Some(text) => {
                    self.area.insert(&text, false);
                    Key::Changed
                }
                None => Key::Ignored,
            }
        } else {
            self.area.key(&ks.key, ks.key_char.as_deref(), ctrl, shift)
        };
        match result {
            Key::Ignored => return,
            Key::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Key::Changed | Key::Moved => {}
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The byte offset under a mouse position on line `line`.
    fn offset_at(&self, line: usize, pos: gpui::Point<gpui::Pixels>) -> usize {
        let layouts = self.layouts.borrow();
        let col = layouts.get(line).map_or(0, |l| match l.index_for_position(pos) {
            Ok(i) | Err(i) => i,
        });
        self.area.offset(line, col)
    }

    fn lines(&self, focused: bool, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let t = theme::get();
        let sel = self.area.selection();
        let (cur_line, cur_col) = self.area.line_col(self.area.cursor());
        let mut layouts = self.layouts.borrow_mut();
        layouts.clear();
        let mut start = 0;
        let mut out = Vec::new();
        for (i, line) in self.area.text.split('\n').enumerate() {
            let end = start + line.len();
            // Selected part of this line (the newline shows as one selected space at the end).
            let mut hl = Vec::new();
            let (a, b) = (sel.start.max(start), sel.end.min(end + 1));
            if a < b {
                let shown_end = (b - start).min(line.len() + 1);
                hl.push((
                    a - start..shown_end,
                    HighlightStyle { background_color: Some(t.bg.selected_focus.into()), ..Default::default() },
                ));
            }
            // A trailing space keeps empty lines measurable and shows a selected newline.
            let text = StyledText::new(format!("{line} ")).with_highlights(hl);
            let layout = text.layout().clone();
            layouts.push(layout.clone());
            let cursor = (focused && i == cur_line).then_some(cur_col);
            let color = t.accent;
            out.push(
                div()
                    .relative()
                    .w_full()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |b: &mut Self, ev: &MouseDownEvent, window, cx| {
                            let at = b.offset_at(i, ev.position);
                            b.area.click(at, ev.modifiers.shift);
                            b.dragging = true;
                            window.focus(&b.focus, cx);
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(move |b: &mut Self, ev: &MouseMoveEvent, _, cx| {
                        if b.dragging && ev.pressed_button == Some(MouseButton::Left) {
                            let at = b.offset_at(i, ev.position);
                            b.area.click(at, true);
                            cx.notify();
                        }
                    }))
                    .child(text)
                    .children(cursor.map(|col| {
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, _| {
                                if let Some(p) = layout.position_for_index(col) {
                                    window.paint_quad(fill(Bounds::new(p, size(z(1.5), layout.line_height())), color));
                                }
                            },
                        )
                        .absolute()
                        .size_full()
                    }))
                    .into_any_element(),
            );
            start = end + 1;
        }
        out
    }
}

impl Render for CommitBox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        let focused = self.focus.is_focused(window);
        let line_h = window.line_height();
        let n = self.area.text.split('\n').count().clamp(MIN_LINES, MAX_LINES);
        let empty = self.area.text.is_empty();
        let lines = self.lines(focused, cx);
        let editor = div()
            .id("commit-message")
            .track_focus(&self.focus)
            .key_context("CommitBox")
            .on_key_down(cx.listener(Self::key))
            .on_mouse_up(MouseButton::Left, cx.listener(|b: &mut Self, _, _, _| b.dragging = false))
            .cursor_text()
            .relative()
            .h(line_h * n as f32 + z(space::SM * 2.))
            .overflow_y_scroll()
            .px(z(space::MD))
            .py(z(space::SM))
            .rounded(z(radius::MD))
            .bg(t.bg.input)
            .border_1()
            .border_color(if focused { t.accent } else { t.border.subtle })
            .font_family(MONO_FONT)
            .text_size(z(text_size::SM))
            .text_color(t.text.primary)
            .children(lines)
            .when(empty && !focused, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(z(space::SM))
                        .left(z(space::MD))
                        .font_family(theme::UI_FONT)
                        .text_color(t.text.faint)
                        .child(if self.amend { "Amended message" } else { "Message (Ctrl+Enter commits)" }),
                )
            });
        let p = &self.props;
        let ws = self.workspace.clone();
        let can = self.can_commit();
        let root = self.root.clone();
        let generating = self.generating;
        let scope_agent = p.scope_agent;
        // ✦ in the message box: a message for exactly what the main button commits.
        let generate = {
            let (root, ws) = (root.clone(), ws.clone());
            div()
                .id("generate")
                .absolute()
                .top(z(space::XS))
                .right(z(space::XS))
                .p(z(space::XXS))
                .rounded(z(radius::SM))
                .when(!generating, |d| d.cursor_pointer().hover(|s| s.bg(t.bg.hover)))
                .when(generating, |d| d.cursor_not_allowed())
                .child(ui::icon(icon::GENERATE, 13., if generating { t.text.faint } else { t.accent }))
                .tooltip(tip(match (generating, scope_agent) {
                    (true, _) => "Generating…",
                    (false, true) => "Write a message for the agent's files (whole contents vs HEAD; claude -p, Haiku)",
                    (false, false) => "Write a message for the staged diff (claude -p, Haiku)",
                }))
                .on_click(move |_: &ClickEvent, _, cx| {
                    if !generating {
                        let _ = ws.update(cx, |w, cx| w.generate_message(root.clone(), scope_agent, cx));
                    }
                })
        };
        let editor = editor.child(generate);
        let label = match (scope_agent, self.amend) {
            (true, false) => {
                format!("Commit agent's {} file{}…", p.agent_files.unwrap_or(0), plural(p.agent_files.unwrap_or(0)))
            }
            (true, true) => {
                format!("Amend with agent's {} file{}…", p.agent_files.unwrap_or(0), plural(p.agent_files.unwrap_or(0)))
            }
            (false, true) => "Amend".to_string(),
            (false, false) if p.staged == 0 => "Nothing staged".to_string(),
            (false, false) => format!("Commit {} staged", p.staged),
        };
        let more_ok = self.message().is_some() && !p.busy;
        let (r1, r2, r3) = (root.clone(), root.clone(), root);
        let (w1, w2, w3) = (ws.clone(), ws.clone(), ws);
        let split = div()
            .flex_none()
            .flex()
            .h(z(24.))
            .rounded(z(radius::MD))
            .overflow_hidden()
            .text_size(z(text_size::SM))
            .when(can, |d| d.bg(t.accent).text_color(t.text.on_accent))
            .when(!can, |d| d.border_1().border_color(t.border.subtle).text_color(t.text.faint))
            .child(
                div()
                    .id("commit-go")
                    .flex()
                    .items_center()
                    .px(z(space::MD))
                    .when(can, |d| d.cursor_pointer().hover(|s| s.opacity(0.85)))
                    .when(!can, |d| d.cursor_not_allowed())
                    .child(label)
                    .tooltip(tip(if scope_agent {
                        "Commit only this session's agent files (asks first; Ctrl+Enter). All shows every change."
                    } else {
                        "Commit the staged changes (Ctrl+Enter)"
                    }))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        if can {
                            let _ = w1.update(cx, |w, cx| w.commit_main(r1.clone(), Vec::new(), window, cx));
                        }
                    }),
            )
            .child(div().w(z(1.)).my(z(4.)).bg(if can { t.text.on_accent } else { t.border.subtle }).opacity(0.5))
            .child(
                div()
                    .id("commit-more")
                    .flex()
                    .items_center()
                    .px(z(space::XS))
                    .when(more_ok, |d| d.cursor_pointer().hover(|s| s.bg(t.bg.hover)))
                    .when(!more_ok, |d| d.cursor_not_allowed())
                    .child(ui::icon(icon::CHEVRON_DOWN, 12., if can { t.text.on_accent } else { t.text.dim }))
                    .tooltip(tip("Commit & push, commit, push & create PR, commit the agent's files"))
                    .on_click(move |ev: &ClickEvent, _, cx| {
                        if more_ok {
                            let at = ev.position();
                            let _ = w2.update(cx, |w, cx| {
                                w.open_git_menu(r2.clone(), crate::workspace::GitMenu::Commit, at, cx)
                            });
                        }
                    }),
            );
        let row = div()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .text_size(z(text_size::SM))
            .text_color(t.text.muted)
            .child(
                div()
                    .id("commit-amend")
                    .flex()
                    .items_center()
                    .gap(z(space::XS))
                    .cursor_pointer()
                    .child(ui::checkbox(if self.amend { ui::Check::On } else { ui::Check::Off }))
                    .child("Amend")
                    .tooltip(tip("Rewrite the last commit (prefills its message)"))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        let _ = w3.update(cx, |w, cx| w.toggle_amend(r3.clone(), cx));
                    }),
            )
            .child(div().flex_1())
            .child(split);
        div().flex().flex_col().gap(z(space::SM)).px(z(space::MD)).py(z(space::MD)).child(editor).child(row)
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
