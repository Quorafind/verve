//! The part of a form control that is neither its box nor its text (P32): what the shell draws
//! of a check, a slider, a caret, a composition.

use verve_dom::Rect;

use super::{Effect, Painter};
use crate::controls;
use crate::display_list::PaintItem;

impl Painter<'_> {
    /// The control item of the box `index`, whose border box is `rect`, if it has one. A control
    /// whose style says `appearance: none` is a plain box and has none, except for a text field:
    /// its text, caret and selection stay.
    pub(super) fn control(&mut self, index: usize, rect: Rect) {
        let snapshot = self.text.snapshot;
        let node = self.tree.boxes[index].node;
        if !controls::draws_itself(snapshot, node)
            || controls::is_text_field(snapshot, node)
            || controls::is_plain(snapshot, node)
        {
            return;
        }
        let Some(kind) = controls::kind_of(snapshot, node, None, [0.; 2]) else {
            return;
        };
        self.items.push(PaintItem::Control {
            node,
            bounds: rect,
            kind,
            look: controls::look(snapshot, node),
        });
    }

    /// The caret and the composition of the text field `index`, inside the clip and the scroll
    /// frame its text is in (`wanted`), so that they move and are cut as the glyphs are.
    pub(super) fn text_field(&mut self, index: usize, wanted: &[Effect]) {
        let snapshot = self.text.snapshot;
        let node = self.tree.boxes[index].node;
        if !controls::is_text_field(snapshot, node) {
            return;
        }
        let rect = self.placed.paint_rect[index];
        let [top, _, _, left] = self.laid.border[index];
        let [pad_top, _, _, pad_left] = self.laid.padding[index];
        let origin = [rect.x + left + pad_left, rect.y + top + pad_top];
        let lines = self.tree.inline[index].as_deref();
        let Some(kind) = controls::kind_of(snapshot, node, lines, origin) else {
            return;
        };
        self.enter(wanted);
        self.items.push(PaintItem::Control {
            node,
            bounds: rect,
            kind,
            look: controls::look(snapshot, node),
        });
    }
}
