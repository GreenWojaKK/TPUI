//! Scrollbar over a document view, along either axis.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Along, App, Axis, Bounds, CursorStyle, DispatchPhase, HitboxBehavior, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Window, canvas, fill, point, prelude::*,
    px, size,
};
use viewer_ui::theme;

use super::style;

/// Scroll state read from a list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollMetrics {
    /// How far the content is scrolled, from 0 to `max`.
    pub offset: Pixels,
    pub max: Pixels,
    pub viewport: Pixels,
}

/// The distance from the thumb's start to the point being dragged. Lasts for as long as the view does, across frames.
#[derive(Clone, Default)]
pub struct ScrollbarDrag(Rc<Cell<Option<Pixels>>>);

/// Start and length of the thumb on a track, or `None` when everything fits.
fn thumb(track: Pixels, metrics: ScrollMetrics) -> Option<(Pixels, Pixels)> {
    if metrics.max <= px(0.) || track <= px(0.) {
        return None;
    }
    let content = metrics.viewport + metrics.max;
    let length = (track * (metrics.viewport / content))
        .max(style::GRID_SCROLLBAR_MIN_THUMB)
        .min(track);
    let start = (track - length) * (metrics.offset / metrics.max).clamp(0., 1.);
    Some((start, length))
}

/// The offset that puts a thumb of `length` at `start` on the track.
fn offset_for(start: Pixels, track: Pixels, length: Pixels, max: Pixels) -> Pixels {
    let room = track - length;
    if room <= px(0.) {
        return px(0.);
    }
    max * ((start / room).clamp(0., 1.))
}

/// The scrollbar, drawn along the right or bottom edge of a parent with relative positioning. `metrics` reads the list
/// and `set_offset` scrolls it.
pub fn scrollbar(
    axis: Axis,
    drag: ScrollbarDrag,
    metrics: impl Fn(&App) -> ScrollMetrics + 'static,
    set_offset: impl Fn(Pixels, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let metrics = Rc::new(metrics);
    let set_offset = Rc::new(set_offset);
    let bar = canvas(
        |bounds, window, _cx| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, cx| {
            let track = bounds.size.along(axis);
            let Some((start, length)) = thumb(track, metrics(cx)) else {
                return;
            };
            window.paint_quad(fill(bounds, theme::scrollbar_track()));
            let thumb_bounds = match axis {
                Axis::Vertical => Bounds::new(
                    point(bounds.left(), bounds.top() + start),
                    size(bounds.size.width, length),
                ),
                Axis::Horizontal => Bounds::new(
                    point(bounds.left() + start, bounds.top()),
                    size(length, bounds.size.height),
                ),
            };
            window.paint_quad(fill(thumb_bounds, theme::scrollbar_thumb()));
            window.set_cursor_style(CursorStyle::Arrow, &hitbox);

            let origin = bounds.origin.along(axis);
            let (down_drag, down_metrics, down_set) =
                (drag.clone(), metrics.clone(), set_offset.clone());
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                cx.stop_propagation();
                let current = down_metrics(cx);
                let Some((start, length)) = thumb(track, current) else {
                    return;
                };
                let at = event.position.along(axis) - origin;
                // A press beside the thumb moves the thumb's middle there.
                let grab = if at >= start && at < start + length {
                    at - start
                } else {
                    length / 2.
                };
                down_drag.0.set(Some(grab));
                down_set(
                    offset_for(at - grab, track, length, current.max),
                    window,
                    cx,
                );
            });

            let (move_drag, move_metrics, move_set) =
                (drag.clone(), metrics.clone(), set_offset.clone());
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !event.dragging() {
                    return;
                }
                let Some(grab) = move_drag.0.get() else {
                    return;
                };
                let current = move_metrics(cx);
                if let Some((_, length)) = thumb(track, current) {
                    let at = event.position.along(axis) - origin;
                    move_set(
                        offset_for(at - grab, track, length, current.max),
                        window,
                        cx,
                    );
                }
            });

            let up_drag = drag.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, _cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    up_drag.0.set(None);
                }
            });
        },
    )
    .absolute();
    match axis {
        Axis::Vertical => bar.top_0().right_0().h_full().w(style::GRID_SCROLLBAR_SIZE),
        // Stops short of the vertical bar's corner.
        Axis::Horizontal => bar
            .left_0()
            .right(style::GRID_SCROLLBAR_SIZE)
            .bottom_0()
            .h(style::GRID_SCROLLBAR_SIZE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(offset: f32, max: f32, viewport: f32) -> ScrollMetrics {
        ScrollMetrics {
            offset: px(offset),
            max: px(max),
            viewport: px(viewport),
        }
    }

    #[test]
    fn hides_when_everything_fits() {
        assert_eq!(thumb(px(100.), metrics(0., 0., 100.)), None);
        assert_eq!(thumb(px(0.), metrics(0., 50., 100.)), None);
    }

    #[test]
    fn sizes_the_thumb_by_the_visible_share() {
        let (start, length) = thumb(px(100.), metrics(0., 300., 100.)).unwrap();
        assert_eq!((start, length), (px(0.), px(25.)));
        let (start, _) = thumb(px(100.), metrics(300., 300., 100.)).unwrap();
        assert_eq!(start, px(75.));
        // A very long document keeps a thumb that can be grabbed.
        let (_, length) = thumb(px(100.), metrics(0., 1_000_000., 100.)).unwrap();
        assert_eq!(length, style::GRID_SCROLLBAR_MIN_THUMB);
    }

    #[test]
    fn turns_thumb_positions_back_into_offsets() {
        assert_eq!(offset_for(px(75.), px(100.), px(25.), px(300.)), px(300.));
        assert_eq!(offset_for(px(-5.), px(100.), px(25.), px(300.)), px(0.));
        assert_eq!(offset_for(px(500.), px(100.), px(25.), px(300.)), px(300.));
        assert_eq!(offset_for(px(10.), px(100.), px(100.), px(300.)), px(0.));
    }
}
