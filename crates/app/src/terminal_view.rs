use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    Bounds, ClickEvent, ClipboardItem, Context, FocusHandle, Focusable, KeyDownEvent, Modifiers, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent, Task, WeakEntity, Window,
    div, prelude::*,
};
use session::alacritty_terminal::index::Side;
use session::alacritty_terminal::term::TermMode;
use session::find::{self, Found, Match, Query, RegexSearch};
use session::keys::{Mods, encode};
use session::links::{self, Link, Target};
use session::mouse::{self, Button, Kind};
use session::{Session, Size, select};

use crate::line_edit::{Edit, LineEdit};
use crate::terminal_element::{Metrics, TerminalElement};
use crate::theme::{self, MONO_FONT, radius, space, text_size, z};
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
    /// Where the grid was last laid out (set by the element in prepaint).
    grid_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The link under the pointer (underlined; Ctrl+click opens it). Paths count only while Ctrl is held.
    hover: Option<Link>,
    /// Pointer position over the grid and whether Ctrl was down (to refresh `hover` on output and Ctrl changes).
    pointer: Option<(Point<Pixels>, bool)>,
    /// A button held in the grid: selecting, or reporting to the app.
    press: Option<Press>,
    /// Last cell (and side) a drag / motion was handled at: repeats in the same cell do nothing.
    last_cell: Option<(usize, usize, Side)>,
    /// Auto-scroll while a selection drag is past the top/bottom edge.
    scrolling: bool,
    _autoscroll: Option<Task<()>>,
    drag_at: Point<Pixels>,
    /// The find bar (Ctrl+Alt+F); None = closed, nothing extra runs.
    find: Option<FindBar>,
    _wakeups: Task<()>,
}

/// After new output the whole scrollback is searched again once output was quiet this long, or at the latest
/// `RESCAN_MAX` after the first new output (Claude's spinner never goes quiet; a flood scans every 2 s), but never
/// sooner than `RESCAN_GAP` after the previous scan (spinner frames ~300 ms apart: 100k lines cost ~110 ms a scan).
const RESCAN_QUIET: Duration = Duration::from_millis(150);
const RESCAN_MAX: Duration = Duration::from_secs(2);
const RESCAN_GAP: Duration = Duration::from_secs(1);

struct FindBar {
    edit: LineEdit,
    focus: FocusHandle,
    case: bool,
    regex: bool,
    /// Built from the query; None = empty or invalid (`error`). The element matches the viewport with it each paint.
    re: Option<Rc<RefCell<RegexSearch>>>,
    error: Option<String>,
    /// The last whole-scrollback scan: the count and Enter / Shift+Enter targets.
    found: Found,
    current: Option<usize>,
    /// A scan is running (its `cancel` flag).
    scanning: bool,
    cancel: Arc<AtomicBool>,
    /// First new output since the last scan started (the `RESCAN_MAX` deadline).
    dirty_since: Option<Instant>,
    last_scan: Instant,
    _scan: Option<Task<()>>,
    _rescan: Option<Task<()>>,
}

impl FindBar {
    fn current_match(&self) -> Option<Match> {
        self.current.and_then(|i| self.found.matches.get(i).cloned())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Press {
    Select,
    Report(Button),
}

fn mods(m: Modifiers) -> Mods {
    Mods { ctrl: m.control, alt: m.alt, shift: m.shift }
}

#[derive(Clone, PartialEq)]
pub enum Banner {
    /// The tab had a Claude session before the restart; holds the exact command Enter will type.
    Resume(String),
    /// Its transcript is gone (deleted, cleaned up after cleanupPeriodDays).
    Gone,
}

impl TerminalView {
    /// Settings → Terminal font size: the next paint uses it (the workspace resizes the PTY after).
    pub fn set_metrics(&mut self, metrics: Metrics, cx: &mut Context<Self>) {
        self.metrics = metrics;
        cx.notify();
    }

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
                        // The text under the pointer may have changed: look again (no extra frame, this one paints).
                        if let Some((pos, ctrl)) = this.pointer {
                            this.hover = this.link_at(pos, ctrl);
                        }
                        if this.find.is_some() {
                            this.output_changed(cx);
                        }
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
        Self {
            session,
            focus: cx.focus_handle(),
            metrics,
            scroll_px: 0.,
            workspace,
            banner: None,
            grid,
            grid_bounds: Default::default(),
            hover: None,
            pointer: None,
            press: None,
            last_cell: None,
            scrolling: false,
            _autoscroll: None,
            drag_at: Point::default(),
            find: None,
            _wakeups: wakeups,
        }
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
        // Ctrl+Shift+C / V: our copy and text paste (plain Ctrl+V stays ^V: Claude Code pastes images with it).
        if m.shift && m.control && !m.alt && (ks.key.eq_ignore_ascii_case("c") || ks.key.eq_ignore_ascii_case("v")) {
            if ks.key.eq_ignore_ascii_case("c") {
                self.copy(cx);
            } else {
                self.paste_clipboard(cx);
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
        if mode.intersects(TermMode::MOUSE_MODE) && !ev.modifiers.shift {
            // The app takes the wheel: one report per line.
            let (col, row, ..) = self.cell_at(ev.position);
            let button = if lines > 0 { Button::WheelUp } else { Button::WheelDown };
            if let Some(bytes) = mouse::report(button, Kind::Press, col, row, mods(ev.modifiers), mode) {
                self.session.write(&bytes.repeat(lines.unsigned_abs() as usize));
            }
            return;
        }
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

/// Mouse, selection, clipboard, links.
impl TerminalView {
    /// The cell under `pos` (clamped into the grid), which half of it, and whether `pos` is above (-1) / below (1)
    /// the grid.
    fn cell_at(&self, pos: Point<Pixels>) -> (usize, usize, Side, i32) {
        let b = self.grid_bounds.get();
        let size = self.session.size();
        let (cols, rows) = (size.cols.max(1) as usize, size.rows.max(1) as usize);
        let x = f32::from(pos.x - b.origin.x).max(0.) / f32::from(self.metrics.cell_width);
        let y = f32::from(pos.y - b.origin.y) / f32::from(self.metrics.line_height);
        let col = (x as usize).min(cols - 1);
        let side = if x as usize >= cols || x.fract() >= 0.5 { Side::Right } else { Side::Left };
        let edge = if y < 0. {
            -1
        } else if y >= rows as f32 {
            1
        } else {
            0
        };
        (col, (y.max(0.) as usize).min(rows - 1), side, edge)
    }

    fn link_at(&self, pos: Point<Pixels>, ctrl: bool) -> Option<Link> {
        let (col, row, _, edge) = self.cell_at(pos);
        if edge != 0 {
            return None;
        }
        let term = self.session.term.lock();
        links::at(&term, select::grid_point(&term, col, row), ctrl)
    }

    /// Sets the hovered link; repaints only when it changed.
    fn set_hover(&mut self, link: Option<Link>, cx: &mut Context<Self>) {
        let key = |l: &Option<Link>| l.as_ref().map(|l| (l.start, l.end));
        if key(&link) != key(&self.hover) {
            self.hover = link;
            cx.notify();
        }
    }

    pub fn has_selection(&self) -> bool {
        self.session.term.lock().selection.is_some()
    }

    pub fn copy(&mut self, cx: &mut Context<Self>) {
        let text = select::text(&self.session.term.lock());
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            self.paste(&text, cx);
        }
    }

    fn paste_primary(&mut self, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if let Some(text) = cx.read_from_primary().and_then(|c| c.text()) {
            self.paste(&text, cx);
        }
    }

    fn paste(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() || self.session.is_exited() {
            return;
        }
        let bracketed = self.session.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if self.session.term.lock().grid().display_offset() != 0 {
            self.session.scroll_to_bottom();
            cx.notify();
        }
        self.session.write(&mouse::paste(text, bracketed));
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        select::select_all(&mut self.session.term.lock());
        cx.notify();
    }

    fn open_link(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let (path, line) = match target {
            Target::Url(url) => match url.strip_prefix("file://") {
                // file://host/path: the path starts at the first '/'.
                Some(rest) => (rest.find('/').map(|i| rest[i..].to_string()).unwrap_or_default(), None),
                None if ["http://", "https://", "mailto:"].iter().any(|s| url.starts_with(s)) => {
                    return cx.open_url(&url);
                }
                None => return,
            },
            Target::Path { path, line, .. } => (path, line),
        };
        if path.is_empty() {
            return;
        }
        let id = cx.entity_id();
        let _ = self.workspace.update(cx, |w, cx| w.open_path_from(id, path, line, window, cx));
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.session.is_exited() {
            return;
        }
        let (col, row, side, _) = self.cell_at(ev.position);
        let m = ev.modifiers;
        // Ctrl+click on a link is always ours, also while the app takes the mouse.
        if m.control
            && ev.button == MouseButton::Left
            && let Some(link) = self.link_at(ev.position, true)
        {
            return self.open_link(link.target, window, cx);
        }
        let mode = *self.session.term.lock().mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !m.shift {
            let button = match ev.button {
                MouseButton::Left => Button::Left,
                MouseButton::Middle => Button::Middle,
                MouseButton::Right => Button::Right,
                _ => return,
            };
            if let Some(bytes) = mouse::report(button, Kind::Press, col, row, mods(m), mode) {
                self.session.write(&bytes);
            }
            self.press = Some(Press::Report(button));
            self.last_cell = Some((col, row, side));
            cx.notify(); // the next paint follows the drag past the grid's edge
            return;
        }
        match ev.button {
            MouseButton::Left => {
                {
                    let mut term = self.session.term.lock();
                    let p = select::grid_point(&term, col, row);
                    if !(m.shift && select::extend(&mut term, p, side)) {
                        select::start(&mut term, ev.click_count, p, side);
                    }
                }
                self.press = Some(Press::Select);
                self.last_cell = Some((col, row, side));
                cx.notify();
            }
            MouseButton::Middle => self.paste_primary(cx),
            MouseButton::Right => {
                let (id, at) = (cx.entity_id(), ev.position);
                let _ = self.workspace.update(cx, |w, cx| w.open_terminal_menu(id, at, cx));
            }
            _ => {}
        }
    }

    /// A move anywhere in the window while a button is held in the grid (registered by the element).
    pub fn drag_to(&mut self, pos: Point<Pixels>, m: Modifiers, held: bool, cx: &mut Context<Self>) {
        let Some(press) = self.press else { return };
        if !held {
            // The release happened before a frame carried the window listener: end it here.
            return self.release(pos, m, cx);
        }
        let (col, row, side, edge) = self.cell_at(pos);
        self.drag_at = pos;
        match press {
            Press::Report(button) => {
                if self.last_cell.is_some_and(|(c, r, _)| (c, r) == (col, row)) {
                    return;
                }
                self.last_cell = Some((col, row, side));
                let mode = *self.session.term.lock().mode();
                if let Some(bytes) = mouse::report(button, Kind::Motion, col, row, mods(m), mode) {
                    self.session.write(&bytes);
                }
            }
            Press::Select => {
                if edge != 0 {
                    self.start_autoscroll(cx);
                }
                if self.last_cell == Some((col, row, side)) {
                    return;
                }
                self.last_cell = Some((col, row, side));
                {
                    let mut term = self.session.term.lock();
                    let p = select::grid_point(&term, col, row);
                    select::extend(&mut term, p, side);
                }
                cx.notify();
            }
        }
    }

    /// Past the top/bottom edge: scroll 1–3 lines every 50 ms (faster further out) and extend the selection.
    /// Ends when the pointer comes back, or on release.
    fn start_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self.scrolling {
            return;
        }
        self.scrolling = true;
        self._autoscroll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                let Ok(true) = this.update(cx, |v, cx| v.autoscroll_step(cx)) else { break };
            }
        }));
    }

    fn autoscroll_step(&mut self, cx: &mut Context<Self>) -> bool {
        let (col, row, side, edge) = self.cell_at(self.drag_at);
        if self.press != Some(Press::Select) || edge == 0 {
            self.scrolling = false;
            return false;
        }
        let b = self.grid_bounds.get();
        let lh = f32::from(self.metrics.line_height);
        let out =
            if edge < 0 { f32::from(b.origin.y - self.drag_at.y) } else { f32::from(self.drag_at.y - b.bottom()) };
        let lines = 1 + ((out / lh) as i32).min(2);
        self.session.scroll(if edge < 0 { lines } else { -lines });
        {
            let mut term = self.session.term.lock();
            let p = select::grid_point(&term, col, row);
            select::extend(&mut term, p, side);
        }
        self.last_cell = None;
        cx.notify();
        true
    }

    /// The release of a button held in the grid, wherever it happens.
    pub fn mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.release(ev.position, ev.modifiers, cx);
    }

    fn release(&mut self, pos: Point<Pixels>, m: Modifiers, cx: &mut Context<Self>) {
        let Some(press) = self.press.take() else { return };
        self.scrolling = false;
        self._autoscroll = None;
        self.last_cell = None;
        match press {
            Press::Report(button) => {
                let (col, row, ..) = self.cell_at(pos);
                let mode = *self.session.term.lock().mode();
                if let Some(bytes) = mouse::report(button, Kind::Release, col, row, mods(m), mode) {
                    self.session.write(&bytes);
                }
            }
            Press::Select => {
                let text = {
                    let mut term = self.session.term.lock();
                    if select::finish(&mut term) { select::text(&term) } else { None }
                };
                // Linux: a selection is the primary selection (middle click pastes it anywhere).
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                if let Some(text) = text {
                    cx.write_to_primary(ClipboardItem::new_string(text));
                }
            }
        }
        cx.notify();
    }

    /// Hover over the grid (no button held): links, and motion reports for apps that want every move (1003).
    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.press.is_some() {
            return; // the element's window listener has it
        }
        self.pointer = Some((ev.position, ev.modifiers.control));
        let mode = *self.session.term.lock().mode();
        if mode.contains(TermMode::MOUSE_MOTION) && !ev.modifiers.shift {
            let (col, row, side, _) = self.cell_at(ev.position);
            if self.last_cell.is_none_or(|(c, r, _)| (c, r) != (col, row)) {
                self.last_cell = Some((col, row, side));
                if let Some(bytes) = mouse::report(Button::None, Kind::Motion, col, row, mods(ev.modifiers), mode) {
                    self.session.write(&bytes);
                }
            }
        }
        let link = self.link_at(ev.position, ev.modifiers.control);
        self.set_hover(link, cx);
    }

    /// Ctrl pressed / released over the grid: paths start / stop being links.
    fn modifiers_changed(&mut self, ev: &ModifiersChangedEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((pos, ctrl)) = self.pointer else { return };
        if ctrl != ev.modifiers.control {
            self.pointer = Some((pos, ev.modifiers.control));
            let link = self.link_at(pos, ev.modifiers.control);
            self.set_hover(link, cx);
        }
    }

    fn hover_changed(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if !hovered {
            self.pointer = None;
            if self.press.is_none() {
                self.last_cell = None;
            }
            self.set_hover(None, cx);
        }
    }
}

/// Find in the terminal: the bar's state, scans off the UI thread, stepping through matches.
impl TerminalView {
    /// Ctrl+Alt+F: opens the bar (prefilled with `query`, else a one-line selection) or focuses it again.
    pub fn open_find(&mut self, query: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            let prefill = query
                .or_else(|| select::text(&self.session.term.lock()).filter(|s| !s.contains('\n') && s.len() <= 200));
            self.find = Some(FindBar {
                edit: LineEdit::new(prefill.unwrap_or_default()),
                focus: cx.focus_handle(),
                case: false,
                regex: false,
                re: None,
                error: None,
                found: Found::default(),
                current: None,
                scanning: false,
                cancel: Default::default(),
                dirty_since: None,
                last_scan: Instant::now(),
                _scan: None,
                _rescan: None,
            });
            self.requery(cx);
        }
        if let Some(f) = &self.find {
            window.focus(&f.focus, cx);
        }
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.find.take() {
            f.cancel.store(true, Ordering::Relaxed);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The text or an option changed: new regex, new scan, the current match chosen afresh.
    fn requery(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_mut() else { return };
        let q = Query { text: f.edit.text.clone(), case: f.case, regex: f.regex };
        (f.re, f.error) = match q.build() {
            Ok(re) => (re.map(|r| Rc::new(RefCell::new(r))), None),
            Err(e) => (None, Some(e)),
        };
        f.found = Found::default();
        f.current = None;
        self.scan(false, cx);
    }

    /// New output while the bar is open: the viewport highlights follow by themselves (matched each paint); the
    /// whole-scrollback scan (count, Enter targets) re-runs after `RESCAN_QUIET` of quiet, at most `RESCAN_MAX`
    /// late. A running scan finishes (a few lines of drift beat a scan that never ends).
    fn output_changed(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_mut() else { return };
        if f.re.is_none() {
            return;
        }
        let now = Instant::now();
        let since = *f.dirty_since.get_or_insert(now);
        let gap = (f.last_scan + RESCAN_GAP).saturating_duration_since(now);
        let wait = RESCAN_QUIET.max(gap).min((since + RESCAN_MAX).saturating_duration_since(now));
        f._rescan = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |v, cx| v.scan(true, cx));
        }));
    }

    /// Scans the screen and scrollback on the background executor. `keep`: stay on the match nearest the current
    /// one (re-scan after output); else pick the first current match and scroll to it.
    fn scan(&mut self, keep: bool, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_mut() else { return };
        f.cancel.store(true, Ordering::Relaxed);
        f._rescan = None;
        f.dirty_since = None;
        f.last_scan = Instant::now();
        let Some(re) = f.re.as_ref() else {
            f.scanning = false;
            f._scan = None;
            return cx.notify();
        };
        let mut re = re.borrow().clone();
        let cancel = Arc::new(AtomicBool::new(false));
        f.cancel = cancel.clone();
        f.scanning = true;
        let term = self.session.term.clone();
        let started = Instant::now();
        crate::perf::event("find start");
        let scan = cx.background_spawn(async move { find::search(&term, &mut re, &cancel) });
        f._scan = Some(cx.spawn(async move |this, cx| {
            let Some(found) = scan.await else { return };
            let _ = this.update(cx, |v, cx| {
                crate::perf::event_with(|| {
                    format!("find done {} matches {} ms", found.matches.len(), started.elapsed().as_millis())
                });
                let Some(f) = v.find.as_mut() else { return };
                let old = f.current_match();
                f.current = match old {
                    Some(m) if keep => find::nearest(&found.matches, *m.start()),
                    _ => find::initial(&v.session.term.lock(), &found.matches),
                };
                f.found = found;
                f.scanning = false;
                if !keep {
                    v.reveal_current();
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn reveal_current(&mut self) {
        if let Some(m) = self.find.as_ref().and_then(FindBar::current_match) {
            find::scroll_to(&mut self.session.term.lock(), &m);
        }
    }

    /// Enter (+1, newer) / Shift+Enter (-1, older), wrapping around.
    fn step(&mut self, dir: isize, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_mut() else { return };
        let n = f.found.matches.len();
        if n == 0 {
            return;
        }
        f.current = Some(match f.current {
            Some(i) => (i as isize + dir).rem_euclid(n as isize) as usize,
            None => find::initial(&self.session.term.lock(), &f.found.matches).unwrap_or(0),
        });
        self.reveal_current();
        cx.notify();
    }

    pub fn toggle_find(&mut self, which: char, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_mut() else { return };
        match which {
            'c' => f.case = !f.case,
            _ => f.regex = !f.regex,
        }
        self.requery(cx);
    }

    fn find_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        let Some(f) = self.find.as_mut() else { return };
        match ks.key.as_str() {
            "enter" if !m.control && !m.alt => self.step(if m.shift { -1 } else { 1 }, cx),
            "escape" => self.close_find(window, cx),
            "c" | "r" if m.alt && !m.control => self.toggle_find(ks.key.chars().next().unwrap_or('r'), cx),
            _ => {
                let before = f.edit.text.clone();
                let result = if m.control && !m.alt && ks.key == "v" {
                    match cx.read_from_clipboard().and_then(|c| c.text()) {
                        Some(text) => {
                            f.edit.insert(text.lines().next().unwrap_or(""));
                            Edit::Changed
                        }
                        None => Edit::Ignored,
                    }
                } else {
                    f.edit.key(&ks.key, ks.key_char.as_deref(), m.control)
                };
                match result {
                    Edit::Changed if f.edit.text != before => self.requery(cx),
                    Edit::Changed => cx.notify(), // the caret moved
                    // Unhandled keys stay in the bar (bound shortcuts were matched before this handler).
                    Edit::Ignored | Edit::Cancel | Edit::Submit => {}
                }
            }
        }
        cx.stop_propagation();
    }

    fn find_bar(&self, f: &FindBar, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        let focused = f.focus.is_focused(window);
        let n = f.found.matches.len();
        let count = match (&f.error, f.re.is_some(), n, f.current) {
            (Some(_), ..) => "Invalid regex".to_string(),
            (_, false, ..) => String::new(),
            (_, _, 0, _) if f.scanning => "…".into(),
            (_, _, 0, _) => "No results".into(),
            (_, _, n, cur) => format!(
                "{} / {n}{}",
                cur.map_or("?".into(), |i| (i + 1).to_string()),
                if f.found.capped { "+" } else { "" }
            ),
        };
        let button = |id: &'static str, glyph: &'static str, hint: &'static str| {
            div()
                .id(id)
                .flex_none()
                .size(z(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(z(radius::SM))
                .cursor_pointer()
                .hover(|s| s.bg(t.bg.hover))
                .child(ui::icon(glyph, 14., t.text.muted))
                .tooltip(ui::tip(hint))
        };
        let input = div()
            .id("find-input")
            .flex_1()
            .min_w_0()
            .child(ui::text_input(&f.edit, focused, "Find in terminal"))
            .when_some(f.error.clone(), |d, e| d.tooltip(ui::tip(e)));
        div()
            .absolute()
            .top(z(space::MD))
            .right(z(space::XL))
            .w(z(420.))
            .flex()
            .items_center()
            .gap(z(space::XS))
            .p(z(space::XS))
            .rounded(z(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(if f.error.is_some() { t.state.error } else { t.border.strong })
            .text_size(z(text_size::SM))
            .text_color(t.text.primary)
            .cursor_default()
            .track_focus(&f.focus)
            .on_key_down(cx.listener(Self::find_key))
            // Clicks on the bar stay on the bar (the grid below would take focus and start a selection).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| {
                    if let Some(f) = &v.find {
                        window.focus(&f.focus, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .child(input)
            .child(
                div()
                    .flex_none()
                    .min_w(z(64.))
                    .text_size(z(text_size::XS))
                    .text_color(if f.error.is_some() { t.state.error } else { t.text.dim })
                    .child(count),
            )
            .child(
                ui::toggle("find-case", "Aa", f.case, "Match case  Alt+C")
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.toggle_find('c', cx))),
            )
            .child(
                ui::toggle("find-regex", ".*", f.regex, "Regular expression  Alt+R")
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.toggle_find('r', cx))),
            )
            .child(
                button("find-prev", icon::CHEVRON_UP, "Previous (older)  Shift+Enter")
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.step(-1, cx))),
            )
            .child(
                button("find-next", icon::CHEVRON_DOWN, "Next (newer)  Enter")
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.step(1, cx))),
            )
            .child(
                button("find-close", icon::CLOSE, "Close  Esc")
                    .on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.close_find(window, cx))),
            )
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("term view render");
        let t = theme::get();
        let banner = self.banner.as_ref().map(|b| {
            let (glyph, label, cmd, hint) = match b {
                Banner::Resume(cmd) => {
                    (icon::RESUMABLE, "Resume Claude session?", Some(cmd.clone()), "Enter resume · Esc plain shell")
                }
                Banner::Gone => (icon::WARNING, "That Claude session no longer exists.", None, "Esc to dismiss"),
            };
            div()
                .flex()
                .gap(z(space::MD))
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
                .px(z(space::LG))
                .bg(t.bg.elevated)
                .border_b_1()
                .border_color(t.border.subtle)
                .text_color(t.text.primary)
                .text_size(z(text_size::SM))
        });
        let exited = self.session.is_exited().then(|| {
            let code = self.session.exit_code().map_or(String::new(), |c| format!(" (code {c})"));
            div()
                .absolute()
                .bottom(z(space::LG))
                .left(z(space::LG))
                .right(z(space::LG))
                .px(z(space::LG))
                .py(z(space::MD))
                .flex()
                .items_center()
                .gap(z(space::MD))
                .rounded(z(radius::LG))
                .bg(t.bg.elevated)
                .border_1()
                .border_color(t.border.strong)
                .text_color(t.text.primary)
                .text_size(z(text_size::SM))
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
                    .id("grid")
                    .flex_1()
                    .min_h_0()
                    .pl(z(PADDING))
                    .pt(z(PADDING))
                    .map(|d| if self.hover.is_some() { d.cursor_pointer() } else { d.cursor_text() })
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
                    .on_modifiers_changed(cx.listener(Self::modifiers_changed))
                    .on_hover(cx.listener(Self::hover_changed))
                    .child(TerminalElement::new(
                        self.session.clone(),
                        self.metrics.clone(),
                        self.grid_bounds.clone(),
                        self.hover.as_ref().map(|l| (l.start, l.end)),
                        self.press.is_some().then(|| cx.entity().downgrade()),
                        self.find.as_ref().and_then(|f| Some((f.re.clone()?, f.current_match()))),
                    )),
            )
            .children(exited)
            .children(self.find.as_ref().map(|f| self.find_bar(f, window, cx)))
    }
}

/// Left/top inset of the grid; the workspace subtracts it when computing cols/rows.
pub const PADDING: f32 = 6.;
