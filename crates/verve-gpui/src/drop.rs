//! Files dragged in from the system: GPUI's drag of `ExternalPaths` over the window becomes the
//! page's `dragenter`, `dragover`, `drop` and `dragleave` (P24's [`DragInput`]).
//!
//! GPUI turns a file drag into a mouse move with the left button held while it carries the
//! paths (`FileDropEvent::Entered` and `Pending`), a left button release (`Submit`), and a
//! `FileDropEvent::Exited` of its own. The element that listens to those hears them as an
//! `ExternalPaths` drag; the functions here are what such a listener calls, so a test can call
//! them with no window and no drag of the system's. The point is turned into the page's pixels
//! as the pointer's is ([`input::local`]); the node under it is found by the renderer.

use std::path::PathBuf;

use gpui::{Pixels, Point};
use verve_dom::{ExternalDrag, Pointer};
use verve_electron::msg::{DragInput, ToRendererInput};

use crate::dom_view::DomState;
use crate::input;

fn pointer_at(state: &DomState, position: Point<Pixels>) -> Pointer {
    let (x, y) = input::local(position, state.input.origin);
    Pointer {
        x,
        y,
        ..Pointer::default()
    }
}

fn tell(state: &DomState, drag: DragInput) {
    if let Some(tell) = &state.tell {
        tell(ToRendererInput::Drag(drag));
    }
}

/// Files are over the window at `position`: the first time is the page's `dragenter` with the
/// files, and each time after it a `dragover`.
pub(crate) fn moved(state: &mut DomState, position: Point<Pixels>, files: &[PathBuf]) {
    let pointer = pointer_at(state, position);
    let drag = if std::mem::replace(&mut state.input.file_drag, true) {
        DragInput::Over { pointer }
    } else {
        let data = ExternalDrag {
            files: files.to_vec(),
            ..ExternalDrag::default()
        };
        DragInput::Enter { data, pointer }
    };
    tell(state, drag);
}

/// The files were let go at `position`.
pub(crate) fn dropped(state: &mut DomState, position: Point<Pixels>) {
    if std::mem::take(&mut state.input.file_drag) {
        let pointer = pointer_at(state, position);
        tell(state, DragInput::Drop { pointer });
    }
}

/// The files left the window, or the drag ended: the page's `dragleave`, if it was told they
/// came.
pub(crate) fn left(state: &mut DomState) {
    if std::mem::take(&mut state.input.file_drag) {
        tell(state, DragInput::Leave);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{point, px};

    use super::*;

    fn recording() -> (DomState, Rc<RefCell<Vec<ToRendererInput>>>) {
        let heard = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&heard);
        let mut state = DomState::default();
        state.input.origin = point(px(10.), px(20.));
        state.tell = Some(Rc::new(move |input| sink.borrow_mut().push(input)));
        (state, heard)
    }

    fn drags(heard: &Rc<RefCell<Vec<ToRendererInput>>>) -> Vec<String> {
        heard
            .borrow()
            .iter()
            .map(|input| match input {
                ToRendererInput::Drag(DragInput::Enter { data, pointer }) => {
                    format!("enter {} {} {}", data.files.len(), pointer.x, pointer.y)
                }
                ToRendererInput::Drag(DragInput::Over { pointer }) => {
                    format!("over {} {}", pointer.x, pointer.y)
                }
                ToRendererInput::Drag(DragInput::Drop { pointer }) => {
                    format!("drop {} {}", pointer.x, pointer.y)
                }
                ToRendererInput::Drag(DragInput::Leave) => "leave".to_owned(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn files_coming_in_are_an_enter_then_overs_in_the_pages_pixels_and_a_drop_ends_it() {
        let (mut state, heard) = recording();
        let files = [PathBuf::from("a.txt"), PathBuf::from("b.txt")];
        moved(&mut state, point(px(15.), px(30.)), &files);
        moved(&mut state, point(px(16.), px(31.)), &files);
        dropped(&mut state, point(px(17.), px(32.)));
        // The system's own `Exited` after a drop says nothing more.
        left(&mut state);
        assert_eq!(
            drags(&heard),
            ["enter 2 5 10", "over 6 11", "drop 7 12"].map(String::from)
        );
    }

    #[test]
    fn files_that_leave_are_a_leave_once_and_a_drop_with_none_over_is_nothing() {
        let (mut state, heard) = recording();
        dropped(&mut state, point(px(1.), px(1.)));
        left(&mut state);
        moved(&mut state, point(px(11.), px(21.)), &[PathBuf::from("a")]);
        left(&mut state);
        left(&mut state);
        assert_eq!(drags(&heard), ["enter 1 1 1", "leave"].map(String::from));
        // A second drag starts with an enter again.
        moved(&mut state, point(px(11.), px(21.)), &[PathBuf::from("a")]);
        assert_eq!(
            drags(&heard).last().map(String::as_str),
            Some("enter 1 1 1")
        );
    }
}
