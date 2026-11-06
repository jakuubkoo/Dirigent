//! Client-side decorations, used only when the platform reports `Decorations::Client` (GNOME Wayland has no
//! server-side decorations). X11 and KDE keep server decorations and get none of this.
//! No `window.refresh()` here: resize cursors come from plain hitboxes, so mouse movement never repaints.

use crate::theme::z;
use gpui::{
    AnyElement, CursorStyle, Decorations, Div, MouseButton, Pixels, ResizeEdge, Stateful, Tiling, Window, div,
    prelude::*,
};

use crate::ui::{self, icon};

pub const TITLE_BAR: f32 = crate::theme::height::TITLE_BAR;
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
            Decorations::Server => Self { csd: false, tiling: Tiling::default(), border: z(0.) },
            Decorations::Client { tiling } => {
                let border = if tiling.is_tiled() { z(0.) } else { z(1.) };
                Self { csd: true, tiling, border }
            }
        }
    }

    /// Height taken by the title bar and borders (subtracted from the terminal area).
    pub fn vertical(&self) -> Pixels {
        if self.csd { z(TITLE_BAR) + self.border * 2. } else { z(0.) }
    }

    pub fn horizontal(&self) -> Pixels {
        self.border * 2.
    }
}

/// Minimize / maximize-restore / close, for the client-side title bar.
pub fn window_controls(window: &Window) -> impl IntoElement {
    let t = crate::theme::get();
    let controls = window.window_controls();
    let button = |id: &'static str, glyph: &'static str, hover_bg: gpui::Rgba| {
        div()
            .id(id)
            .w(z(40.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            // Buttons must not start a window move.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(ui::icon(glyph, 14., t.text.muted))
    };
    div()
        .flex()
        .flex_none()
        .h_full()
        .when(controls.minimize, |d| {
            d.child(button("minimize", icon::MINIMIZE, t.bg.hover).on_click(|_, window, _| window.minimize_window()))
        })
        .when(controls.maximize, |d| {
            let glyph = if window.is_maximized() { icon::RESTORE } else { icon::MAXIMIZE };
            d.child(button("maximize", glyph, t.bg.hover).on_click(|_, window, _| window.zoom_window()))
        })
        .child(button("close", icon::CLOSE, t.danger_hover).on_click(|_, window, _| window.remove_window()))
}

/// Invisible strips along the window edges that start a compositor-driven resize. Hidden on tiled edges.
pub fn resize_handles(tiling: Tiling) -> Vec<AnyElement> {
    use ResizeEdge::*;
    let (t, b, l, r) = (!tiling.top, !tiling.bottom, !tiling.left, !tiling.right);
    let handle = |id: &'static str, edge: ResizeEdge, cursor: CursorStyle| {
        div().id(id).absolute().cursor(cursor).on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.start_window_resize(edge);
            cx.stop_propagation();
        })
    };
    let (e, c) = (z(EDGE), z(CORNER));
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
