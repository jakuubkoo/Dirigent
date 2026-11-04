use std::rc::Rc;

use gpui::{
    Context, FocusHandle, Focusable, KeyDownEvent, Pixels, ScrollEvent, Task, WeakEntity, Window, div, prelude::*, px,
    rgb,
};
use session::Session;
use session::alacritty_terminal::term::TermMode;
use session::keys::{Mods, encode};

use crate::terminal_element::{Metrics, TerminalElement, palette};
use crate::workspace::Workspace;

pub struct TerminalView {
    pub session: Rc<Session>,
    focus: FocusHandle,
    metrics: Metrics,
    scroll_px: f32,
    workspace: WeakEntity<Workspace>,
    _wakeups: Task<()>,
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
                let Ok(visible) = this.update(cx, |this, cx| {
                    let visible = this.session.is_visible();
                    crate::perf::event(if visible { "wakeup (visible)" } else { "wakeup (unread flip)" });
                    if visible {
                        cx.notify();
                    }
                    visible
                }) else {
                    return;
                };
                if !visible {
                    let _ = ws.update(cx, |_, cx| cx.notify());
                }
            }
            let _ = this.update(cx, |_, cx| cx.notify());
            let _ = ws.update(cx, |_, cx| cx.notify());
        });
        Self { session, focus: cx.focus_handle(), metrics, scroll_px: 0., workspace, _wakeups: wakeups }
    }

    fn key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        if m.platform {
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
        let exited = self.session.is_exited().then(|| {
            let code = self.session.exit_code().map_or(String::new(), |c| format!(" (code {c})"));
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .px_3()
                .py_1()
                .bg(rgb(0x3a2a2e))
                .text_color(palette::FG)
                .text_sm()
                .child(format!("Process exited{code} — press Enter to restart"))
        });
        div()
            .relative()
            .size_full()
            .pl(px(PADDING))
            .pt(px(PADDING))
            .bg(palette::BG)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll(cx.listener(Self::scroll))
            .child(TerminalElement::new(self.session.clone(), self.metrics.clone()))
            .children(exited)
    }
}

/// Left/top inset of the grid; the workspace subtracts it when computing cols/rows.
pub const PADDING: f32 = 6.;
