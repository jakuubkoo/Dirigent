mod terminal_element;

use std::rc::Rc;

use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, KeyDownEvent, Pixels, ScrollEvent, Task, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use session::alacritty_terminal::term::TermMode;
use session::keys::{Mods, encode};
use session::{Session, Size};
use terminal_element::{Metrics, TerminalElement, palette};

struct TerminalView {
    session: Rc<Session>,
    focus: FocusHandle,
    metrics: Metrics,
    scroll_px: f32,
    _wakeups: Task<()>,
}

impl TerminalView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let metrics = Metrics::new(window);
        let session = Rc::new(
            Session::spawn(Size { cols: 80, rows: 24, cell_width: 8, cell_height: 16 }).expect("spawn shell"),
        );
        // Perf harness: `AGENT_IDE_RUN="cmd"` types cmd into the shell at startup (see CLAUDE.md).
        if let Ok(cmd) = std::env::var("AGENT_IDE_RUN") {
            session.write(format!("{cmd}\r").as_bytes());
        }
        // One notify per batch of PTY output; gpui coalesces notifies into at most one frame.
        let rx = session.wakeups.clone();
        let wakeups = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
            cx.update(|cx| cx.quit()); // shell exited
        });
        Self { session, focus: cx.focus_handle(), metrics, scroll_px: 0., _wakeups: wakeups }
    }

    fn key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        if m.platform {
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
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(palette::BG)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll(cx.listener(Self::scroll))
            .child(TerminalElement::new(self.session.clone(), self.metrics.clone()))
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.), px(700.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |window, cx| {
                let view = cx.new(|cx| TerminalView::new(window, cx));
                window.focus(&view.read(cx).focus.clone(), cx);
                view
            },
        )
        .expect("open window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
