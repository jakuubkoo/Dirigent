//! Client-side decorations, used only when the platform reports `Decorations::Client` (GNOME Wayland has no
//! server-side decorations). X11 and KDE keep server decorations and get none of this.
//! No `window.refresh()` here: resize cursors come from plain hitboxes, so mouse movement never repaints.

use gpui::{
    AnyElement, CursorStyle, Decorations, Div, Hsla, MouseButton, Pixels, ResizeEdge, Stateful, Tiling, Window, div,
    prelude::*, px, rgb,
};

pub const TITLE_BAR: f32 = 30.;
const EDGE: f32 = 5.; // grab width for resizing
const CORNER: f32 = 12.;

pub struct Chrome {
    pub csd: bool,
    pub tiling: Tiling,
    pub border: Pixels,
}

impl Chrome {
    pub fn of(window: &Window) -> Self {
        match window.window_decorations() {
            Decorations::Server => Self { csd: false, tiling: Tiling::default(), border: px(0.) },
            Decorations::Client { tiling } => {
                let border = if tiling.is_tiled() { px(0.) } else { px(1.) };
                Self { csd: true, tiling, border }
            }
        }
    }

    /// Height taken by the title bar and borders (subtracted from the terminal area).
    pub fn vertical(&self) -> Pixels {
        if self.csd { px(TITLE_BAR) + self.border * 2. } else { px(0.) }
    }

    pub fn horizontal(&self) -> Pixels {
        self.border * 2.
    }
}

/// The title bar; the caller attaches the drag/double-click handlers (they need workspace state).
pub fn title_bar(title: String, window: &Window, bg: Hsla, dim: Hsla, hover: Hsla) -> Stateful<Div> {
    let controls = window.window_controls();
    let button = |id: &'static str, label: &'static str, hover_bg: Hsla| {
        div()
            .id(id)
            .w(px(40.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_color(dim)
            .hover(move |s| s.bg(hover_bg).text_color(gpui::white()))
            // Buttons must not start a window move.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(label)
    };
    div()
        .id("title-bar")
        .flex()
        .flex_none()
        .items_center()
        .w_full()
        .h(px(TITLE_BAR))
        .bg(bg)
        .text_sm()
        .child(div().flex_1().pl_3().text_color(dim).truncate().child(title))
        .when(controls.minimize, |d| {
            d.child(button("minimize", "─", hover).on_click(|_, window, _| window.minimize_window()))
        })
        .when(controls.maximize, |d| {
            let label = if window.is_maximized() { "❐" } else { "□" };
            d.child(button("maximize", label, hover).on_click(|_, window, _| window.zoom_window()))
        })
        .child(button("close", "✕", rgb(0xc4314b).into()).on_click(|_, window, _| window.remove_window()))
}

/// Invisible strips along the window edges that start a compositor-driven resize. Hidden on tiled edges.
pub fn resize_handles(tiling: Tiling) -> Vec<AnyElement> {
    use ResizeEdge::*;
    let (t, b, l, r) = (!tiling.top, !tiling.bottom, !tiling.left, !tiling.right);
    let handle = |id: &'static str, edge: ResizeEdge, cursor: CursorStyle| {
        div()
            .id(id)
            .absolute()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.start_window_resize(edge);
                cx.stop_propagation();
            })
    };
    let (e, c) = (px(EDGE), px(CORNER));
    let mut out = Vec::new();
    let mut add = |on: bool, el: Stateful<Div>| {
        if on {
            out.push(el.into_any_element());
        }
    };
    add(t, handle("rz-top", Top, CursorStyle::ResizeUpDown).top_0().left(c).right(c).h(e));
    add(b, handle("rz-bottom", Bottom, CursorStyle::ResizeUpDown).bottom_0().left(c).right(c).h(e));
    add(l, handle("rz-left", Left, CursorStyle::ResizeLeftRight).left_0().top(c).bottom(c).w(e));
    add(r, handle("rz-right", Right, CursorStyle::ResizeLeftRight).right_0().top(c).bottom(c).w(e));
    let corner = CursorStyle::ResizeUpLeftDownRight;
    let anti = CursorStyle::ResizeUpRightDownLeft;
    add(t && l, handle("rz-tl", TopLeft, corner).top_0().left_0().size(c));
    add(t && r, handle("rz-tr", TopRight, anti).top_0().right_0().size(c));
    add(b && l, handle("rz-bl", BottomLeft, anti).bottom_0().left_0().size(c));
    add(b && r, handle("rz-br", BottomRight, corner).bottom_0().right_0().size(c));
    out
}
