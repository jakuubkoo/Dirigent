//! Small shared UI pieces built from theme tokens: icons, tooltips, pills, meters, time formatting.

use gpui::{
    prelude::FluentBuilder as _,
    AnyElement, AnyView, App, AppContext, Context, Div, IntoElement, ParentElement, Render, Rgba, SharedString, Styled, Svg, Window,
    div, svg,
};

use crate::line_edit::LineEdit;
use crate::theme::{z, self, radius, space, text_size};

pub mod icon {
    pub const CHEVRON_RIGHT: &str = "icons/chevron-right.svg";
    pub const CHEVRON_DOWN: &str = "icons/chevron-down.svg";
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
    icon::CHEVRON_RIGHT, icon::CHEVRON_DOWN, icon::PLUS, icon::CLOSE, icon::MINIMIZE, icon::MAXIMIZE, icon::RESTORE,
    icon::SIDEBAR_CLOSE, icon::DOCK_RIGHT_CLOSE, icon::SESSIONS, icon::FILES, icon::CHANGES, icon::DONE, icon::SPINNER,
    icon::RESUMABLE, icon::WARNING, icon::BRANCH, icon::REFRESH, icon::RESTART, icon::SHOW, icon::HIDE, icon::FOLDER,
    icon::FOLDER_OPEN, icon::FILE, icon::FILE_CODE, icon::FILE_TEXT, icon::FILE_JSON, icon::FILE_CONFIG,
    icon::FILE_LOCK, icon::FILE_IMAGE, icon::FILE_SHELL, icon::FILE_ARCHIVE, icon::TERMINAL,
    icon::SHARED, icon::VIA_COMMAND, icon::GENERATE, icon::PIN, icon::MORE, icon::SETTINGS, icon::SEARCH,
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
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "go" | "py" | "js" | "jsx" | "ts" | "tsx" | "java" | "kt" | "swift"
        | "rb" | "php" | "cs" | "zig" | "lua" | "html" | "css" | "scss" | "vue" | "svelte" | "sql" => icon::FILE_CODE,
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
            .child(self.0.clone())
    }
}

/// A one-line text input (caret only when focused, placeholder when empty).
pub fn text_input(edit: &LineEdit, focused: bool, placeholder: &'static str) -> AnyElement {
    let t = theme::get();
    let caret = || div().flex_none().w(z(1.5)).h(z(15.)).bg(t.accent);
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
        .when(edit.text.is_empty(), |d| d.when(focused, |d| d.child(caret())).child(div().text_color(t.text.faint).child(placeholder)))
        .when(!edit.text.is_empty(), |d| {
            d.child(div().flex_none().whitespace_nowrap().child(edit.before_cursor().to_string()))
                .when(focused, |d| d.child(caret()))
                .child(div().flex_none().whitespace_nowrap().child(edit.after_cursor().to_string()))
        })
        .into_any_element()
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

pub fn tip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |_, cx| cx.new(|_| Tip(text.into())).into()
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
    let color = if pct >= 90. { m.crit } else if pct >= 70. { m.warn } else { m.ok };
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
