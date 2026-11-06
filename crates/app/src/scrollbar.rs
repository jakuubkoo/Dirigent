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
    let (top, h) = thumb_span(track.size.height, max, -base.offset().y, z(MIN_THUMB));
    Some((Bounds { origin: point(track.origin.x, track.origin.y + top), size: gpui::size(track.size.width, h) }, max))
}

/// Thumb (top, height) in a track `view` tall, for content scrolled `scrolled` of `max` (max > 0).
fn thumb_span(view: Pixels, max: Pixels, scrolled: Pixels, min_thumb: Pixels) -> (Pixels, Pixels) {
    let h = (view * (view / (view + max))).max(min_thumb).min(view);
    ((scrolled / max).clamp(0., 1.) * (view - h), h)
}

/// How far to scroll so a thumb `h` tall sits `top` below the track's top.
fn scrolled_for(view: Pixels, max: Pixels, h: Pixels, top: Pixels) -> Pixels {
    let room = view - h;
    if room > Pixels::ZERO { max * (top / room).clamp(0., 1.) } else { Pixels::ZERO }
}

/// Scrolls so the thumb's top sits at `y` (window coordinates).
fn scroll_thumb_to(track: Bounds<Pixels>, handle: &UniformListScrollHandle, y: Pixels) {
    let Some((t, max)) = thumb(track, handle) else { return };
    let scrolled = scrolled_for(track.size.height, max, t.size.height, y - track.origin.y);
    handle.0.borrow().base_handle.set_offset(point(Pixels::ZERO, -scrolled));
}

/// Place inside a `relative()` parent that also holds the list.
pub fn scrollbar(handle: UniformListScrollHandle, drag: Drag, view: EntityId) -> impl IntoElement {
    canvas(
        // The hitbox must come from prepaint: a cached view's replay keeps prepaint hitboxes but not paint ones,
        // so one inserted in paint vanished on the next root redraw and every click missed the scrollbar.
        |track, window, _| window.insert_hitbox(track, HitboxBehavior::Normal),
        move |track, hitbox, window, _| {
            let Some((t, _)) = thumb(track, &handle) else { return };
            window.set_cursor_style(CursorStyle::Arrow, &hitbox);
            let s = &crate::theme::get().scrollbar;
            let color = if drag.get().is_some() { s.active } else { s.thumb };
            let inset = Bounds {
                origin: point(t.origin.x + z(2.), t.origin.y),
                size: gpui::size(z(WIDTH - 3.), t.size.height),
            };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: f32) -> Pixels {
        gpui::px(v) // unscaled: plain math, no zoom
    }

    #[test]
    fn thumb_follows_the_offset_and_drag_maps_back() {
        let (view, max, min) = (p(100.), p(300.), p(24.));
        // 100 of 400 visible → a quarter-height thumb.
        assert_eq!(thumb_span(view, max, p(0.), min), (p(0.), p(25.)));
        assert_eq!(thumb_span(view, max, max, min), (p(75.), p(25.)));
        assert_eq!(thumb_span(view, max, p(150.), min).0, p(37.5));
        // Out-of-range offsets (overscroll) stay in the track.
        assert_eq!(thumb_span(view, max, p(-10.), min).0, p(0.));
        assert_eq!(thumb_span(view, max, p(400.), min).0, p(75.));
        // Huge content: the thumb keeps its minimum height.
        assert_eq!(thumb_span(view, p(100_000.), p(0.), min).1, min);

        // Dragging the thumb's top to `top` scrolls back to the offset that put it there.
        for scrolled in [0., 60., 150., 299., 300.] {
            let (top, h) = thumb_span(view, max, p(scrolled), min);
            assert!((scrolled_for(view, max, h, top) - p(scrolled)).abs() < p(0.01), "{scrolled}");
        }
        // Past either end of the track clamps.
        assert_eq!(scrolled_for(view, max, p(25.), p(-40.)), p(0.));
        assert_eq!(scrolled_for(view, max, p(25.), p(500.)), max);
        // A thumb as tall as the track has no room to move.
        assert_eq!(scrolled_for(view, max, view, p(10.)), p(0.));
    }
}
