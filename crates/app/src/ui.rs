//! Small shared UI pieces built from theme tokens: icons, tooltips, pills, meters, time formatting.

use gpui::{
    AnyElement, AnyView, App, AppContext, Bounds, ContentMask, Context, Div, HighlightStyle, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Point, Render, Rgba, SharedString,
    Stateful, StatefulInteractiveElement, Styled, StyledText, Svg, TextAlign, TextLayout, TextRun, Window, canvas, div,
    fill, point, prelude::FluentBuilder as _, size, svg,
};

use std::cell::RefCell;
use std::rc::Rc;

use crate::line_edit::LineEdit;
use crate::text_area::TextArea;
use crate::theme::{self, MONO_FONT, radius, space, text_size, z};

pub mod icon {
    pub const CHECK: &str = "icons/check.svg";
    pub const TRASH: &str = "icons/trash-2.svg";
    pub const PULL_REQUEST: &str = "icons/git-pull-request.svg";
    pub const EXTERNAL: &str = "icons/external-link.svg";
    pub const CHEVRON_RIGHT: &str = "icons/chevron-right.svg";
    pub const CHEVRON_DOWN: &str = "icons/chevron-down.svg";
    pub const CHEVRON_UP: &str = "icons/chevron-up.svg";
    pub const PLUS: &str = "icons/plus.svg";
    pub const CLOSE: &str = "icons/x.svg";
    pub const MINIMIZE: &str = "icons/minus.svg";
    pub const MAXIMIZE: &str = "icons/square.svg";
    pub const RESTORE: &str = "icons/copy.svg";
    pub const SIDEBAR_CLOSE: &str = "icons/panel-left-close.svg";
    pub const DOCK_RIGHT_CLOSE: &str = "icons/panel-right-close.svg";
    pub const SESSIONS: &str = "icons/list.svg";
    pub const FILES: &str = "icons/files.svg";
    pub const CHANGES: &str = "icons/git-compare.svg";
    pub const DONE: &str = "icons/circle-check.svg";
    pub const SPINNER: &str = "icons/loader-circle.svg";
    pub const RESUMABLE: &str = "icons/moon.svg";
    pub const WARNING: &str = "icons/triangle-alert.svg";
    pub const BRANCH: &str = "icons/git-branch.svg";
    pub const REFRESH: &str = "icons/refresh-cw.svg";
    pub const RESTART: &str = "icons/rotate-ccw.svg";
    pub const SHOW: &str = "icons/eye.svg";
    pub const HIDE: &str = "icons/eye-off.svg";
    pub const FOLDER: &str = "icons/folder.svg";
    pub const FOLDER_OPEN: &str = "icons/folder-open.svg";
    pub const FILE: &str = "icons/file.svg";
    pub const FILE_CODE: &str = "icons/file-code.svg";
    pub const FILE_TEXT: &str = "icons/file-text.svg";
    pub const FILE_JSON: &str = "icons/file-json.svg";
    pub const FILE_CONFIG: &str = "icons/file-cog.svg";
    pub const FILE_LOCK: &str = "icons/file-lock.svg";
    pub const FILE_IMAGE: &str = "icons/file-image.svg";
    pub const FILE_SHELL: &str = "icons/file-terminal.svg";
    pub const FILE_ARCHIVE: &str = "icons/file-archive.svg";
    pub const TERMINAL: &str = "icons/terminal.svg";
    /// Changes: a file another session changed too / one attributed from a shell command.
    pub const SHARED: &str = "icons/users.svg";
    pub const VIA_COMMAND: &str = "icons/square-terminal.svg";
    /// Commit box: generate a message.
    pub const GENERATE: &str = "icons/sparkles.svg";
    pub const PIN: &str = "icons/pin.svg";
    pub const MORE: &str = "icons/ellipsis.svg";
    pub const SETTINGS: &str = "icons/settings.svg";
    pub const SEARCH: &str = "icons/search.svg";
}

#[cfg(test)]
pub const ALL_ICONS: &[&str] = &[
    icon::CHECK,
    icon::TRASH,
    icon::PULL_REQUEST,
    icon::EXTERNAL,
    icon::CHEVRON_RIGHT,
    icon::CHEVRON_DOWN,
    icon::CHEVRON_UP,
    icon::PLUS,
    icon::CLOSE,
    icon::MINIMIZE,
    icon::MAXIMIZE,
    icon::RESTORE,
    icon::SIDEBAR_CLOSE,
    icon::DOCK_RIGHT_CLOSE,
    icon::SESSIONS,
    icon::FILES,
    icon::CHANGES,
    icon::DONE,
    icon::SPINNER,
    icon::RESUMABLE,
    icon::WARNING,
    icon::BRANCH,
    icon::REFRESH,
    icon::RESTART,
    icon::SHOW,
    icon::HIDE,
    icon::FOLDER,
    icon::FOLDER_OPEN,
    icon::FILE,
    icon::FILE_CODE,
    icon::FILE_TEXT,
    icon::FILE_JSON,
    icon::FILE_CONFIG,
    icon::FILE_LOCK,
    icon::FILE_IMAGE,
    icon::FILE_SHELL,
    icon::FILE_ARCHIVE,
    icon::TERMINAL,
    icon::SHARED,
    icon::VIA_COMMAND,
    icon::GENERATE,
    icon::PIN,
    icon::MORE,
    icon::SETTINGS,
    icon::SEARCH,
];

/// An icon at `size` px in `color` (rasterized once per path/size, recolored for free).
pub fn icon(path: &'static str, size: f32, color: Rgba) -> Svg {
    svg().path(path).flex_none().size(z(size)).text_color(color)
}

/// A generic icon for a file name.
pub fn file_icon(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        _ if lower.ends_with(".lock") || lower == "package-lock.json" => icon::FILE_LOCK,
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "go" | "py" | "js" | "jsx" | "ts" | "tsx" | "java" | "kt"
        | "swift" | "rb" | "php" | "cs" | "zig" | "lua" | "html" | "css" | "scss" | "vue" | "svelte" | "sql" => {
            icon::FILE_CODE
        }
        "md" | "txt" | "rst" | "adoc" | "log" => icon::FILE_TEXT,
        "json" | "jsonc" | "json5" => icon::FILE_JSON,
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "env" | "xml" => icon::FILE_CONFIG,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "avif" => icon::FILE_IMAGE,
        "sh" | "bash" | "zsh" | "fish" | "ps1" => icon::FILE_SHELL,
        "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "7z" | "rar" | "bz2" => icon::FILE_ARCHIVE,
        _ if lower == "makefile" || lower == "dockerfile" => icon::FILE_CONFIG,
        _ => icon::FILE,
    }
}

/// Tooltip content (gpui places it near the cursor after its show delay).
pub struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        div()
            .px_2()
            .py_1()
            .rounded(z(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .text_size(z(text_size::XS))
            .text_color(t.text.primary)
            .children(self.0.lines().map(|l| div().child(l.to_string())))
    }
}

/// A one-line text input (caret only when focused, placeholder when empty).
pub fn text_input(edit: &LineEdit, focused: bool, placeholder: &'static str) -> AnyElement {
    let t = theme::get();
    div()
        .flex()
        .items_center()
        .min_w_0()
        .overflow_hidden()
        .px(z(space::MD))
        .py(z(space::XS + 2.))
        .rounded(z(radius::MD))
        .bg(t.bg.input)
        .border_1()
        .border_color(if focused { t.accent } else { t.border.subtle })
        .text_size(z(text_size::SM))
        .child(line_text(edit, focused, placeholder))
        .into_any_element()
}

/// A `LineEdit`'s text for any field frame: selection, caret (when focused), horizontal scroll that follows the
/// caret, and the mouse (click places the caret, Shift+click / drag selects, double click selects a word). The
/// mouse changes only the selection, so it needs no owner: the next key applies it.
pub fn line_text(edit: &LineEdit, focused: bool, placeholder: &'static str) -> Div {
    let t = theme::get();
    let view = edit.view();
    let text = edit.text.clone();
    let (anchor, cursor) = edit.sel();
    let (accent, sel_bg, faint) = (t.accent, t.bg.selected_focus, t.text.faint);
    let paint_view = view.clone();
    let painted = canvas(
        move |bounds, window, _| {
            let style = window.text_style();
            let empty = text.is_empty();
            let shown: SharedString = if empty { placeholder.into() } else { text.clone().into() };
            let run = TextRun {
                len: shown.len(),
                font: style.font(),
                color: if empty { faint.into() } else { style.color },
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let font_size = style.font_size.to_pixels(window.rem_size());
            let line = window.text_system().shape_line(shown, font_size, &[run], None);
            let mut v = paint_view.borrow_mut();
            let caret_w = z(1.5);
            let (width, caret_x) =
                if empty { (Pixels::ZERO, Pixels::ZERO) } else { (line.width(), line.x_for_index(cursor)) };
            let room = bounds.size.width - caret_w;
            if caret_x - v.scroll > room {
                v.scroll = caret_x - room;
            }
            v.scroll = v.scroll.min(caret_x).min((width - room).max(Pixels::ZERO)).max(Pixels::ZERO);
            v.left = bounds.origin.x;
            v.owner = Some(window.current_view());
            v.text = text.clone();
            v.line = (!empty).then(|| line.clone());
            if !v.pending {
                v.sel = (anchor, cursor);
            }
            (line, caret_x, v.scroll)
        },
        move |bounds, (line, caret_x, scroll), window, cx| {
            let line_h = bounds.size.height;
            let origin = point(bounds.origin.x - scroll, bounds.origin.y);
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                let (a, b) = (anchor.min(cursor), anchor.max(cursor));
                if a < b {
                    let (x0, x1) = (line.x_for_index(a), line.x_for_index(b));
                    window.paint_quad(fill(Bounds::new(point(origin.x + x0, origin.y), size(x1 - x0, line_h)), sel_bg));
                }
                let _ = line.paint(origin, line_h, TextAlign::Left, None, window, cx);
                if focused {
                    window.paint_quad(fill(
                        Bounds::new(point(origin.x + caret_x, origin.y), size(z(1.5), line_h)),
                        accent,
                    ));
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    let (down, drag, up) = (view.clone(), view.clone(), view);
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .cursor_text()
        // Sizes the row like a text line; the canvas draws over it.
        .child(div().text_color(gpui::transparent_black()).child("Xg"))
        .child(painted)
        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _, cx| {
            let mut v = down.borrow_mut();
            let at = v.index_at(ev.position.x);
            v.sel = if ev.click_count == 2 {
                let w = crate::text_area::word_at(&v.text, at);
                (w.start, w.end)
            } else if ev.modifiers.shift {
                (v.sel.0, at)
            } else {
                (at, at)
            };
            v.pending = true;
            v.dragging = ev.click_count == 1;
            if let Some(id) = v.owner {
                cx.notify(id);
            }
        })
        .on_mouse_move(move |ev: &MouseMoveEvent, _, cx| {
            let mut v = drag.borrow_mut();
            if v.dragging && ev.pressed_button == Some(MouseButton::Left) {
                v.sel.1 = v.index_at(ev.position.x);
                v.pending = true;
                if let Some(id) = v.owner {
                    cx.notify(id);
                }
            }
        })
        .on_mouse_up(MouseButton::Left, move |_, _, _| up.borrow_mut().dragging = false)
}

/// A multi-line `TextArea` box: `min`..`max` lines high, then it scrolls; `wrap` soft-wraps long lines (else
/// they are cut). Mouse: click places the caret, Shift+click
/// and drag select. `get` finds the area in the owner, `on_press` runs on a press (focus the field). `layouts` keeps
/// this frame's line layouts for the mouse. The caller adds font, keys and a placeholder.
#[allow(clippy::too_many_arguments)]
pub fn text_area_box<V: 'static>(
    id: &'static str,
    area: &TextArea,
    layouts: &Rc<RefCell<Vec<TextLayout>>>,
    focused: bool,
    (min, max): (usize, usize),
    wrap: bool,
    line_h: Pixels,
    get: fn(&mut V) -> Option<&mut TextArea>,
    on_press: fn(&mut V, &mut Window, &mut Context<V>),
    cx: &mut Context<V>,
) -> Stateful<Div> {
    let t = theme::get();
    let sel = area.selection();
    let (cur_line, cur_col) = area.line_col(area.cursor());
    let mut lay = layouts.borrow_mut();
    lay.clear();
    let mut start = 0;
    let mut lines = Vec::new();
    for (i, line) in area.text.split('\n').enumerate() {
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
        lay.push(layout.clone());
        let cursor = (focused && i == cur_line).then_some(cur_col);
        let color = t.accent;
        // Places the caret under `pos` on this line (`extend`: keeps the selection's other end).
        let click = {
            let layouts = layouts.clone();
            move |v: &mut V, pos: Point<Pixels>, extend: bool| {
                let col = layouts.borrow().get(i).map_or(0, |l| match l.index_for_position(pos) {
                    Ok(c) | Err(c) => c,
                });
                if let Some(a) = get(v) {
                    a.click(a.offset(i, col), extend);
                }
            }
        };
        let drag = click.clone();
        lines.push(
            div()
                .relative()
                .w_full()
                .when(!wrap, |d| d.whitespace_nowrap().overflow_hidden())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |v: &mut V, ev: &MouseDownEvent, window, cx| {
                        click(v, ev.position, ev.modifiers.shift);
                        on_press(v, window, cx);
                        cx.notify();
                    }),
                )
                .on_mouse_move(cx.listener(move |v: &mut V, ev: &MouseMoveEvent, _, cx| {
                    if ev.pressed_button == Some(MouseButton::Left) {
                        drag(v, ev.position, true);
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
    let n = area.text.split('\n').count().clamp(min, max);
    let pad = z(space::SM * 2.) + z(2.); // padding + border
    div()
        .id(id)
        .cursor_text()
        .relative()
        // Wrapped lines are taller than their count: the content sizes the box between min and max.
        .when(!wrap, |d| d.h(line_h * n as f32 + z(space::SM * 2.)))
        .when(wrap, |d| d.min_h(line_h * min as f32 + pad).max_h(line_h * max as f32 + pad))
        .overflow_y_scroll()
        .px(z(space::MD))
        .py(z(space::SM))
        .rounded(z(radius::MD))
        .bg(t.bg.input)
        .border_1()
        .border_color(if focused { t.accent } else { t.border.subtle })
        .text_size(z(text_size::SM))
        .text_color(t.text.primary)
        .children(lines)
}

/// A search option toggle (`.*`, `Aa`, `ab`): filled while on; the caller adds `on_click`.
pub fn toggle(id: &'static str, label: &'static str, on: bool, hint: &'static str) -> Stateful<Div> {
    let t = theme::get();
    div()
        .id(id)
        .flex_none()
        .px(z(space::XS))
        .rounded(z(radius::SM))
        .font_family(MONO_FONT)
        .text_size(z(text_size::XS))
        .cursor_pointer()
        .when(on, |d| d.bg(t.bg.selected).text_color(t.text.primary))
        .when(!on, |d| d.text_color(t.text.dim).hover(|s| s.bg(t.bg.hover)))
        .child(label)
        .tooltip(tip(hint))
}

/// A checkbox's box (12 px; filled with the accent when on).
pub fn check_box(on: bool) -> Div {
    let t = theme::get();
    div()
        .flex_none()
        .size(z(12.))
        .rounded(z(3.))
        .border_1()
        .border_color(if on { t.accent } else { t.border.strong })
        .when(on, |d| d.bg(t.accent))
}

pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone())).into()
}

/// "now", "42s"→"now", "5m", "3h", "2d".
pub fn fmt_ago(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

/// A thin horizontal meter (limits).
pub fn meter(pct: f32, width: f32) -> impl IntoElement {
    let m = &theme::get().meter;
    let color = if pct >= 90. {
        m.crit
    } else if pct >= 70. {
        m.warn
    } else {
        m.ok
    };
    div()
        .flex_none()
        .w(z(width))
        .h(z(4.))
        .rounded_full()
        .bg(m.track)
        .child(div().h_full().rounded_full().bg(color).w(gpui::relative(pct.clamp(0., 100.) / 100.)))
}

/// "+12 −3" (git colors), "bin" for binary/too-big files.
pub fn line_counts(lines: Option<(u32, u32)>) -> AnyElement {
    let t = theme::get();
    match lines {
        Some((a, d)) => div()
            .flex()
            .flex_none()
            .gap_1()
            .child(div().text_color(t.git.added).child(format!("+{a}")))
            .child(div().text_color(t.git.deleted).child(format!("−{d}")))
            .into_any_element(),
        None => div().flex_none().text_color(t.text.dim).child("bin").into_any_element(),
    }
}

/// The strip above a diff or file preview (same height and look as the terminal's slim header).
pub fn overlay_header() -> Div {
    let t = theme::get();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(z(crate::theme::space::MD))
        .h(z(crate::theme::height::HEADER))
        .px(z(crate::theme::space::LG))
        .bg(t.bg.elevated)
        .border_b_1()
        .border_color(t.border.subtle)
        .text_size(z(text_size::SM))
}

/// A small label chip ("staged", "untracked").
pub fn pill(text: &'static str) -> Div {
    let t = theme::get();
    div()
        .flex_none()
        .px(z(6.))
        .rounded(z(radius::SM))
        .bg(t.bg.selected)
        .text_size(z(text_size::XS))
        .text_color(t.text.muted)
        .child(text)
}

/// "Esc to close" and similar right-aligned hints.
pub fn hint(text: &'static str) -> Div {
    div().ml_auto().flex_none().text_size(z(text_size::XS)).text_color(theme::get().text.dim).child(text)
}

/// A dimmed one-line note in an empty list or panel.
pub fn note(text: impl Into<SharedString>) -> Div {
    div()
        .px(z(crate::theme::space::LG))
        .py(z(crate::theme::space::MD))
        .text_size(z(text_size::SM))
        .text_color(theme::get().text.dim)
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago() {
        assert_eq!(fmt_ago(-3), "now");
        assert_eq!(fmt_ago(59), "now");
        assert_eq!(fmt_ago(61), "1m");
        assert_eq!(fmt_ago(3 * 3600 + 5), "3h");
        assert_eq!(fmt_ago(2 * 86400), "2d");
    }

    #[test]
    fn file_icons() {
        assert_eq!(file_icon("main.rs"), icon::FILE_CODE);
        assert_eq!(file_icon("Cargo.lock"), icon::FILE_LOCK);
        assert_eq!(file_icon("Cargo.toml"), icon::FILE_CONFIG);
        assert_eq!(file_icon("README.md"), icon::FILE_TEXT);
        assert_eq!(file_icon("build.sh"), icon::FILE_SHELL);
        assert_eq!(file_icon("LICENSE"), icon::FILE);
    }
}

/// `↑2 ↓3` (zeros left out; both zero → empty).
pub fn ahead_behind((ahead, behind): (u32, u32)) -> String {
    let parts = [(ahead > 0).then(|| format!("↑{ahead}")), (behind > 0).then(|| format!("↓{behind}"))];
    parts.into_iter().flatten().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Check {
    Off,
    On,
    /// Some of it (a partly staged file, some files of a list).
    Partial,
}

/// A 13 px checkbox (the caller adds the click and the tooltip).
pub fn checkbox(state: Check) -> Div {
    let t = theme::get();
    let b = div().flex_none().size(z(13.)).rounded(z(3.)).flex().items_center().justify_center();
    match state {
        Check::Off => b.border_1().border_color(t.border.strong),
        Check::On => b.bg(t.accent).child(icon(icon::CHECK, 10., t.text.on_accent)),
        Check::Partial => b.bg(t.accent).child(icon(icon::MINIMIZE, 10., t.text.on_accent)),
    }
}
