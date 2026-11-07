//! The wheel over a page: the container under the pointer moves at once, ahead of the renderer,
//! and the renderer is told.
//!
//! Which container moves is the display list's choice ([`DisplayList::wheel`]): the element on
//! top at the pointer, then its scroll chain. The offset moved to is the shell's *prediction*,
//! kept until the next list the renderer publishes, which carries the renderer's own offsets.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Pixels, Point, ScrollWheelEvent, Window, px};
use verve_dom::{NodeId, Pointer};
use verve_electron::msg::ToRendererInput;

use crate::dom_view::DomState;

/// What a notch of a mouse wheel moves a line by, in CSS pixels. Chromium on Windows turns a
/// notch into three lines of a hundred pixels together; Chromium on macOS scrolls 40 a line.
const LINE: f32 = if cfg!(target_os = "macos") {
    40.
} else {
    100. / 3.
};

/// The wheel turned over the page, whose top left corner is at `origin` in the window.
pub(crate) fn turned(
    state: &Rc<RefCell<DomState>>,
    event: &ScrollWheelEvent,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    let moved = event.delta.pixel_delta(px(LINE));
    // GPUI's delta is how far the content moves; the page's is how far the view does.
    let delta = [-f32::from(moved.x), -f32::from(moved.y)];
    let at = event.position - origin;
    let (x, y) = (f32::from(at.x), f32::from(at.y));
    let mut state = state.borrow_mut();
    if let Some(list) = state.list.clone() {
        let target = list.wheel(x, y, delta, &|node| state.predicted.get(&node).copied());
        if let Some((node, to)) = target {
            state.predict(node, to);
            window.refresh();
        }
    }
    if let Some(tell) = &state.tell {
        tell(ToRendererInput::Wheel {
            pointer: Pointer {
                x,
                y,
                ..Pointer::default()
            },
            dx: delta[0],
            dy: delta[1],
            target: None::<NodeId>,
        });
    }
}
