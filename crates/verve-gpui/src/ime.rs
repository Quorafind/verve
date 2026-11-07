//! The input method: what the platform asks of the field that has the keyboard, answered from the
//! page's latest snapshot, and what it composes sent to the page.
//!
//! [`ImeBridge`] owns no text. It forwards what the platform does (`replace_and_mark_text_in_range`
//! and the rest) as [`CompositionEvent`]s and reads what it is asked (the text around the caret,
//! the selection, the composed range) from the focused field's [`Draft`] in the snapshot. The
//! candidate window goes where the page says the caret is (`RendererView.ime_rect`).

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, point, px, size,
};
use verve_dom::{Draft, NodeId, Snapshot};
use verve_electron::msg::{CompositionEvent, ToRendererInput};

use crate::cursor;
use crate::dom_view::DomState;

/// The element of the page that takes text from an input method: the focused field or editing
/// host, if there is one.
pub(crate) fn editable_focus(snapshot: &Snapshot) -> Option<NodeId> {
    let dom = &snapshot.dom;
    let focused = dom.focused.filter(|id| dom.is_connected(*id))?;
    cursor::takes_text(snapshot, focused).then_some(focused)
}

/// The draft of the field with the keyboard, as the page last published it.
fn focused_draft(state: &DomState) -> Option<Draft> {
    let snapshot = state.snapshot.as_ref()?;
    let id = editable_focus(snapshot)?;
    snapshot.dom.get(id)?.draft.clone()
}

/// A composition the platform has started: where, and as it was last spelled.
struct Composing {
    field: Option<NodeId>,
    spelled: String,
}

/// The platform's text input into the page.
pub(crate) struct ImeBridge {
    state: Rc<RefCell<DomState>>,
    /// The composition going on: an update has arrived and no commit since.
    composing: Option<Composing>,
}

impl ImeBridge {
    pub fn new(state: Rc<RefCell<DomState>>) -> Self {
        Self {
            state,
            composing: None,
        }
    }

    fn tell(&self, input: ToRendererInput) {
        if let Some(tell) = &self.state.borrow().tell {
            tell(input);
        }
    }

    /// Takes the composition that is going on, if it is in the field that has the keyboard now:
    /// one left behind by a field that lost it is over, or its commit would be typed twice.
    fn take_composing(&mut self) -> Option<Composing> {
        let here = (self.state.borrow().snapshot.as_ref()).and_then(editable_focus);
        self.state.borrow_mut().composing = false;
        self.composing.take().filter(|c| c.field == here)
    }
}

impl EntityInputHandler for ImeBridge {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let draft = focused_draft(&self.state.borrow())?;
        let total = draft.offset_to_utf16(draft.text().len());
        let range = range.start.min(total)..range.end.min(total);
        *adjusted_range = Some(range.clone());
        Some(draft.text()[draft.range_from_utf16(range)].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let draft = focused_draft(&self.state.borrow());
        Some(match draft {
            Some(draft) => UTF16Selection {
                range: draft.range_to_utf16(draft.selection()),
                reversed: draft.is_reversed(),
            },
            None => UTF16Selection {
                range: 0..0,
                reversed: false,
            },
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let draft = focused_draft(&self.state.borrow())?;
        draft.marked().map(|range| draft.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        let Some(composing) = self.take_composing() else {
            return;
        };
        // What is composed stays as it was last spelled.
        self.tell(ToRendererInput::Composition(CompositionEvent::Commit(
            composing.spelled,
        )));
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let composing = self.take_composing();
        // On Windows a typed character arrives here after its key went to the page.
        if composing.is_none() && self.state.borrow_mut().input.is_typed(text) {
            return;
        }
        if text.is_empty() && composing.is_none() {
            return;
        }
        let input = if composing.is_some() {
            ToRendererInput::Composition(CompositionEvent::Commit(text.to_owned()))
        } else {
            ToRendererInput::Text(text.to_owned())
        };
        self.tell(input);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        if new_text.is_empty() {
            // The composition was cleared: abandoned if one was going, nothing otherwise (the
            // platform also clears after a commit).
            if self.take_composing().is_some() {
                self.tell(ToRendererInput::Composition(CompositionEvent::Cancel));
            }
            return;
        }
        let field = (self.state.borrow().snapshot.as_ref()).and_then(editable_focus);
        self.state.borrow_mut().composing = true;
        self.composing = Some(Composing {
            field,
            spelled: new_text.to_owned(),
        });
        self.tell(ToRendererInput::Composition(CompositionEvent::Update {
            text: new_text.to_owned(),
            selection: new_selected_range,
        }));
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let state = self.state.borrow();
        let rect = state.ime_rect?;
        let single_line = (state.snapshot.as_ref())
            .and_then(|snapshot| Some((snapshot, editable_focus(snapshot)?)))
            .is_some_and(|(snapshot, id)| {
                snapshot.dom.get(id).and_then(|n| n.tag()) == Some("input")
            });
        // GPUI puts the candidate window at the middle of the height it is given. For the whole
        // box of a one-line field that is half way down the box, so the field's bottom edge is
        // given instead, as a zero-height rect, which puts it under the field.
        let (y, height) = if single_line {
            (rect.y + rect.height, 0.)
        } else {
            (rect.y, rect.height)
        };
        let origin: Point<Pixels> = element_bounds.origin + point(px(rect.x), px(y));
        Some(Bounds::new(origin, size(px(rect.width), px(height))))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        // Where a point falls in the text is the renderer's to say (`Document::text_position_at`);
        // a question the platform asks synchronously cannot wait for it. P32's 「状态」.
        None
    }
}

/// Whether the field with the keyboard takes an input method: Chromium turns it off for a
/// password field (from its source; hidden windows cannot show it).
pub(crate) fn accepts_ime(snapshot: &Snapshot) -> bool {
    let Some(id) = editable_focus(snapshot) else {
        return false;
    };
    let kind = snapshot.dom.get_attr(id, "type").unwrap_or_default();
    !(snapshot.dom.get(id).and_then(|n| n.tag()) == Some("input")
        && kind.eq_ignore_ascii_case("password"))
}
