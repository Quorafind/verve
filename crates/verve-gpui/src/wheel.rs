//! The wheel over a page: the container under the pointer moves at once, ahead of the renderer,
//! and the renderer is told.
//!
//! Which container moves is the display list's choice ([`DisplayList::wheel`]): the element on
//! top at the pointer, then its scroll chain. The offset moved to is the shell's *prediction*,
//! kept until the next list the renderer publishes, which carries the renderer's own offsets.
//!
//! The page hears what Chromium's wheel event has (observed in Electron 39.8.10): `deltaY` is
//! the opposite of how far the content moves, in pixels, and Shift leaves the delta where it
//! is (`deltaY`) while the scroll turns sideways. Control asks for a zoom and scrolls nothing,
//! which the renderer decides once the page has not prevented the wheel.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Pixels, Point, ScrollWheelEvent, Window, px};
use verve_dom::Pointer;
use verve_electron::msg::ToRendererInput;

use crate::dom_view::DomState;
use crate::{input, keymap};

/// What a notch of a mouse wheel moves a line by, in CSS pixels. Chromium on Windows turns a
/// notch into three lines of a hundred pixels together; Chromium on macOS scrolls 40 a line.
const LINE: f32 = if cfg!(target_os = "macos") {
    40.
} else {
    100. / 3.
};

/// The deltas the page's `wheel` event has for `event`: `[deltaX, deltaY]`. GPUI's delta is how
/// far the content moves and the page's is how far the view does. GPUI's Windows backend has
/// already turned a Shift-wheel into a horizontal one, which is turned back: the page hears
/// `deltaY` and the shift key.
pub(crate) fn page_delta(event: &ScrollWheelEvent) -> [f32; 2] {
    let moved = event.delta.pixel_delta(px(LINE));
    let (x, y) = (-f32::from(moved.x), -f32::from(moved.y));
    let lines = matches!(event.delta, gpui::ScrollDelta::Lines(_));
    if cfg!(windows) && lines && event.modifiers.shift && y == 0. && x != 0. {
        [0., x]
    } else {
        [x, y]
    }
}

/// How far the wheel moves the view, from the deltas the page hears: sideways for Shift.
pub(crate) fn scroll_by(shift: bool, [dx, dy]: [f32; 2]) -> [f32; 2] {
    if shift && dx == 0. {
        [dy, 0.]
    } else {
        [dx, dy]
    }
}

/// The wheel input for `event` over `target`, with the position in the page.
pub(crate) fn wheel_input(
    event: &ScrollWheelEvent,
    origin: Point<Pixels>,
    target: Option<verve_dom::NodeId>,
) -> ToRendererInput {
    let (x, y) = input::local(event.position, origin);
    let [dx, dy] = page_delta(event);
    ToRendererInput::Wheel {
        pointer: Pointer {
            x,
            y,
            modifiers: keymap::modifiers(&event.modifiers),
            ..Pointer::default()
        },
        dx,
        dy,
        target,
    }
}

/// The wheel turned over the page, whose top left corner is at `origin` in the window.
pub(crate) fn turned(
    state: &Rc<RefCell<DomState>>,
    event: &ScrollWheelEvent,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    let mut state = state.borrow_mut();
    if state.picking {
        return;
    }
    let (x, y) = input::local(event.position, origin);
    let delta = scroll_by(event.modifiers.shift, page_delta(event));
    let target = input::hit(&mut state, x, y).map(|(node, _)| node);
    if !event.modifiers.control
        && let Some(list) = state.list.clone()
    {
        let scrolls = list.wheel(x, y, delta, &|node| state.predicted.get(&node).copied());
        if let Some((node, to)) = scrolls {
            state.predict(node, to);
            window.refresh();
        }
    }
    if let Some(tell) = &state.tell {
        tell(wheel_input(event, origin, target));
    }
}

#[cfg(test)]
mod tests {
    use gpui::{ScrollDelta, TouchPhase, point};

    use super::*;

    fn event(delta: ScrollDelta, shift: bool) -> ScrollWheelEvent {
        ScrollWheelEvent {
            position: point(px(30.), px(40.)),
            delta,
            modifiers: gpui::Modifiers {
                shift,
                ..gpui::Modifiers::default()
            },
            touch_phase: TouchPhase::Moved,
        }
    }

    #[test]
    fn pixels_are_the_opposite_of_how_far_the_content_moves() {
        // Observed in Electron 39.8.10: a wheel turned up gives `deltaY` below zero.
        let up = event(ScrollDelta::Pixels(point(px(0.), px(120.))), false);
        assert_eq!(page_delta(&up), [0., -120.]);
        let right = event(ScrollDelta::Pixels(point(px(-15.), px(0.))), false);
        assert_eq!(page_delta(&right), [15., 0.]);
    }

    #[test]
    fn lines_are_a_line_of_pixels_each() {
        let two = event(ScrollDelta::Lines(point(0., -2.)), false);
        assert_eq!(page_delta(&two), [0., 2. * LINE]);
    }

    #[test]
    #[cfg(windows)]
    fn a_shift_wheel_gpui_made_horizontal_is_deltay_for_the_page_and_sideways_for_the_scroll() {
        let sideways = event(ScrollDelta::Lines(point(-3., 0.)), true);
        let heard = page_delta(&sideways);
        assert_eq!(heard, [0., 3. * LINE]);
        assert_eq!(scroll_by(true, heard), [3. * LINE, 0.]);
        // A wheel with no shift is scrolled as it is heard, and so is one with a real delta x.
        assert_eq!(scroll_by(false, heard), heard);
        assert_eq!(scroll_by(true, [5., 7.]), [5., 7.]);
    }

    #[test]
    fn the_input_has_the_pointer_in_the_page_and_the_modifiers() {
        let wheel = event(ScrollDelta::Pixels(point(px(0.), px(10.))), true);
        let origin = point(px(10.), px(10.));
        let ToRendererInput::Wheel {
            pointer, dx, dy, ..
        } = wheel_input(&wheel, origin, Some(4))
        else {
            panic!("a wheel input");
        };
        assert_eq!((pointer.x, pointer.y), (20., 30.));
        assert!(pointer.modifiers.shift);
        assert_eq!((dx, dy), (0., -10.));
    }
}
