use std::rc::Rc;

use gpui::{
    Context, FocusHandle, Focusable, KeyDownEvent, Pixels, ScrollEvent, Task, WeakEntity, Window, div, prelude::*, px,
};
use session::{Session, Size};
use session::alacritty_terminal::term::TermMode;
use session::keys::{Mods, encode};

use crate::terminal_element::{Metrics, TerminalElement};
use crate::theme::{self, MONO_FONT, radius, space, text_size};
use crate::ui::{self, icon};
use crate::workspace::Workspace;

pub struct TerminalView {
    pub session: Rc<Session>,
    focus: FocusHandle,
    metrics: Metrics,
    scroll_px: f32,
    workspace: WeakEntity<Workspace>,
    /// One-line prompt above the terminal; while shown it takes Enter and Esc only and the grid is one row
    /// shorter, so it never covers terminal content. Change it only through `set_banner`.
    banner: Option<Banner>,
    /// The grid size the workspace gives every session; this view's PTY gets one row less while a banner shows.
    grid: Size,
    _wakeups: Task<()>,
}

#[derive(Clone, PartialEq)]
pub enum Banner {
    /// The tab had a Claude session before the restart; holds the exact command Enter will type.
    Resume(String),
    /// Its transcript is gone (deleted, cleaned up after cleanupPeriodDays).
    Gone,
}

impl TerminalView {
    pub fn new(session: Session, metrics: Metrics, workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let session = Rc::new(session);
        let rx = session.wakeups.clone();
        let ws = workspace.clone();
        // Visible: repaint the terminal (gpui coalesces to ≤1 frame). Hidden: the reader thread sends only the
        // unread flip, which repaints just the sidebar. Channel closed = shell exited.
        let wakeups = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                let Ok((visible, moved, id)) = this.update(cx, |this, cx| {
                    let visible = this.session.is_visible();
                    crate::perf::event(if visible { "wakeup (visible)" } else { "wakeup (unread flip)" });
                    if visible {
                        cx.notify();
                    }
                    (visible, this.session.take_cwd_change(), cx.entity_id())
                }) else {
                    return;
                };
                if let Some(dir) = moved {
                    let _ = ws.update(cx, |w, cx| w.shell_cwd_changed(id, dir, cx));
                } else if !visible {
                    let _ = ws.update(cx, |_, cx| cx.notify());
                }
            }
            let _ = this.update(cx, |_, cx| cx.notify());
            let _ = ws.update(cx, |_, cx| cx.notify());
        });
        let grid = session.size();
        Self { session, focus: cx.focus_handle(), metrics, scroll_px: 0., workspace, banner: None, grid, _wakeups: wakeups }
    }

    pub fn banner(&self) -> Option<&Banner> {
        self.banner.as_ref()
    }

    pub fn set_banner(&mut self, banner: Option<Banner>, cx: &mut Context<Self>) {
        if banner != self.banner {
            self.banner = banner;
            self.apply_grid();
            cx.notify();
        }
    }

    /// Workspace-wide grid size; returns whether the PTY was resized.
    pub fn resize_to(&mut self, grid: Size) -> bool {
        self.grid = grid;
        self.apply_grid()
    }

    fn apply_grid(&self) -> bool {
        let reserved = (self.banner.is_some() && self.grid.rows > 1) as u16;
        self.session.resize(Size { rows: self.grid.rows - reserved, ..self.grid })
    }

    fn key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        if m.platform {
            return;
        }
        if let Some(banner) = self.banner.clone()
            && matches!(ks.key.as_str(), "enter" | "escape")
            && !m.control
            && !m.alt
            && !m.shift
        {
            self.set_banner(None, cx);
            if matches!(banner, Banner::Resume(_)) && ks.key == "enter" {
                let id = cx.entity_id();
                let _ = self.workspace.update(cx, |w, cx| w.resume(id, cx));
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if self.session.is_exited() {
            if ks.key == "enter" {
                let id = cx.entity_id();
                let _ = self.workspace.update(cx, |w, cx| w.restart(id, window, cx));
            }
            cx.stop_propagation();
            return;
        }
        if m.shift && !m.control && !m.alt && matches!(ks.key.as_str(), "pageup" | "pagedown") {
            let page = self.session.size().rows as i32 - 1;
            self.session.scroll(if ks.key == "pageup" { page } else { -page });
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let mode = *self.session.term.lock().mode();
        let mods = Mods { ctrl: m.control, alt: m.alt, shift: m.shift };
        if let Some(bytes) = encode(&ks.key, ks.key_char.as_deref(), mods, mode.contains(TermMode::APP_CURSOR)) {
            if self.session.term.lock().grid().display_offset() != 0 {
                self.session.scroll_to_bottom();
                cx.notify();
            }
            self.session.write(&bytes);
            cx.stop_propagation();
        }
    }

    fn scroll(&mut self, ev: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_h: Pixels = self.metrics.line_height;
        self.scroll_px += f32::from(ev.delta.pixel_delta(line_h).y);
        let lines = (self.scroll_px / f32::from(line_h)) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= lines as f32 * f32::from(line_h);
        let mode = *self.session.term.lock().mode();
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Full-screen apps without mouse reporting get arrow keys (like xterm's alternateScroll).
            let key: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            self.session.write(&key.repeat(lines.unsigned_abs() as usize));
        } else {
            self.session.scroll(lines);
            cx.notify();
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("term view render");
        let t = theme::get();
        let banner = self.banner.as_ref().map(|b| {
            let (glyph, label, cmd, hint) = match b {
                Banner::Resume(cmd) => (icon::RESUMABLE, "Resume Claude session?", Some(cmd.clone()), "Enter resume · Esc plain shell"),
                Banner::Gone => (icon::WARNING, "That Claude session no longer exists.", None, "Esc to dismiss"),
            };
            div()
                .flex()
                .gap(px(space::MD))
                .child(ui::icon(glyph, 14., t.accent))
                .child(div().flex_none().child(label))
                .children(cmd.map(|c| {
                    div().flex_1().min_w_0().truncate().font_family(MONO_FONT).text_color(t.text.muted).child(c)
                }))
                .child(ui::hint(hint))
                // Exactly one terminal row tall: the grid is one row shorter while it's shown.
                .flex_none()
                .items_center()
                .overflow_hidden()
                .h(self.metrics.line_height)
                .line_height(self.metrics.line_height)
                .px(px(space::LG))
                .bg(t.bg.elevated)
                .border_b_1()
                .border_color(t.border.subtle)
                .text_color(t.text.primary)
                .text_size(px(text_size::SM))
        });
        let exited = self.session.is_exited().then(|| {
            let code = self.session.exit_code().map_or(String::new(), |c| format!(" (code {c})"));
            div()
                .absolute()
                .bottom(px(space::LG))
                .left(px(space::LG))
                .right(px(space::LG))
                .px(px(space::LG))
                .py(px(space::MD))
                .flex()
                .items_center()
                .gap(px(space::MD))
                .rounded(px(radius::LG))
                .bg(t.bg.elevated)
                .border_1()
                .border_color(t.border.strong)
                .text_color(t.text.primary)
                .text_size(px(text_size::SM))
                .child(ui::icon(icon::RESTART, 14., t.state.error))
                .child(format!("Process exited{code}"))
                .child(ui::hint("Enter to restart"))
        });
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.terminal.bg)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll(cx.listener(Self::scroll))
            .children(banner)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .pl(px(PADDING))
                    .pt(px(PADDING))
                    .child(TerminalElement::new(self.session.clone(), self.metrics.clone())),
            )
            .children(exited)
    }
}

/// Left/top inset of the grid; the workspace subtracts it when computing cols/rows.
pub const PADDING: f32 = 6.;
