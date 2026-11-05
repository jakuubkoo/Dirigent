//! A thin vertical scrollbar for a `uniform_list`: an absolutely positioned canvas over the list's right edge.
//! The thumb is computed and painted in the paint phase from the list's scroll handle, so it's always in sync
//! with the frame being drawn and costs nothing while idle (no state, no timers, no extra renders). Dragging the
//! thumb or clicking the track moves the list and notifies the owning view only.

use crate::theme::z;
use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, CursorStyle, EntityId, HitboxBehavior, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Styled, UniformListScrollHandle, canvas, point,
};

const WIDTH: f32 = 8.;
const MIN_THUMB: f32 = 24.;

/// Per-list drag state, kept by the owning view: the grab point's distance from the thumb's top.
pub type Drag = Rc<Cell<Option<Pixels>>>;

/// (thumb bounds, max scroll offset) for a track, or None when the content fits.
fn thumb(track: Bounds<Pixels>, handle: &UniformListScrollHandle) -> Option<(Bounds<Pixels>, Pixels)> {
    let base = &handle.0.borrow().base_handle;
    let max = base.max_offset().y;
    if max <= z(0.) {
        return None;
    }
    let view = track.size.height;
    let h = (view * (view / (view + max))).max(z(MIN_THUMB)).min(view);
    let top = (-base.offset().y / max).clamp(0., 1.) * (view - h);
    Some((Bounds { origin: point(track.origin.x, track.origin.y + top), size: gpui::size(track.size.width, h) }, max))
}

/// Scrolls so the thumb's top sits at `y` (window coordinates).
fn scroll_thumb_to(track: Bounds<Pixels>, handle: &UniformListScrollHandle, y: Pixels) {
    let Some((t, max)) = thumb(track, handle) else { return };
    let room = track.size.height - t.size.height;
    let frac = if room > z(0.) { ((y - track.origin.y) / room).clamp(0., 1.) } else { 0. };
    handle.0.borrow().base_handle.set_offset(point(z(0.), -max * frac));
}

/// Place inside a `relative()` parent that also holds the list.
pub fn scrollbar(handle: UniformListScrollHandle, drag: Drag, view: EntityId) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |track, (), window, _| {
            let Some((t, _)) = thumb(track, &handle) else { return };
            let hitbox = window.insert_hitbox(track, HitboxBehavior::Normal);
            window.set_cursor_style(CursorStyle::Arrow, &hitbox);
            let s = &crate::theme::get().scrollbar;
            let color = if drag.get().is_some() { s.active } else { s.thumb };
            let inset = Bounds { origin: point(t.origin.x + z(2.), t.origin.y), size: gpui::size(z(WIDTH - 3.), t.size.height) };
            window.paint_quad(gpui::fill(inset, color).corner_radii(z(3.)));

            let (h, d) = (handle.clone(), drag.clone());
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if !phase.bubble() || e.button != MouseButton::Left || !hitbox.is_hovered(window) {
                    return;
                }
                let Some((t, _)) = thumb(track, &h) else { return };
                if t.contains(&e.position) {
                    d.set(Some(e.position.y - t.origin.y));
                } else {
                    // Track click: jump so the thumb is centered on the click, then keep dragging from there.
                    let grab = t.size.height / 2.;
                    scroll_thumb_to(track, &h, e.position.y - grab);
                    d.set(Some(grab));
                }
                cx.notify(view);
                cx.stop_propagation();
            });
            let (h, d) = (handle.clone(), drag.clone());
            window.on_mouse_event(move |e: &MouseMoveEvent, _, _, cx| {
                if let Some(grab) = d.get() {
                    if e.pressed_button != Some(MouseButton::Left) {
                        d.set(None);
                    } else {
                        scroll_thumb_to(track, &h, e.position.y - grab);
                    }
                    cx.notify(view);
                }
            });
            let d = drag.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                if d.take().is_some() {
                    cx.notify(view);
                }
            });
        },
    )
    .absolute()
    .top_0()
    .right_0()
    .bottom_0()
    .w(z(WIDTH))
}
