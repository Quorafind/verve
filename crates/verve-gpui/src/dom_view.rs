//! One window's page: the display list its renderer published, drawn by [`DomElement`], and
//! scrolled ahead of the renderer by the wheel.
//!
//! The renderer lays the page out on its own thread (`verve-layout`) and says what to draw as a
//! [`DisplayList`]. The view keeps the last one it was given. A wheel turned over it moves the
//! nearest container that can move at once (the offset kept here is the *prediction*, and the
//! frames of the list are moved by it), and is told to the renderer as
//! [`ToRendererInput::Wheel`]; the renderer's own offsets arrive with its next list and win.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    KeyUpEvent, ModifiersChangedEvent, ParentElement, Render, Rgba, Styled, Window, div, rgb,
};
use verve_dom::{NodeId, Rect, Snapshot};
use verve_electron::msg::ToRendererInput;
use verve_layout::{Cursor, DisplayList, ImageTable};

use crate::app_region::{self, AppRegion};
use crate::dom_element::DomElement;
use crate::frame_stats::FrameStats;
use crate::ime::{self, ImeBridge};
use crate::input::{self, Input};
use crate::inspector::model::Highlight;
use crate::inspector::overlay::{self, Picker};
use crate::pictures::Pictures;
use crate::plan::{self, Plan};

/// What is called as each frame of a window is built.
type Drawn = Rc<dyn Fn(&Window)>;

/// What a window tells the page under it: the wheel, for now.
pub(crate) type Tell = Rc<dyn Fn(ToRendererInput)>;

/// What the view and its element share.
#[derive(Default)]
pub(crate) struct DomState {
    pub list: Option<Arc<DisplayList>>,
    /// The offsets the wheel scrolled containers to since the list came.
    pub predicted: HashMap<NodeId, [f32; 2]>,
    /// Counts the changes of `predicted`, so a plan is made again only when it must be.
    generation: u64,
    /// What the list comes to under the predicted offsets, for the list and the generation it was
    /// made for.
    plan: Option<(Arc<DisplayList>, u64, Rc<Plan>)>,
    pub pictures: Pictures,
    /// What the page has decoded.
    pub images: Arc<ImageTable>,
    /// How big the page was drawn, in CSS pixels.
    pub viewport: (f32, f32),
    pub tell: Option<Tell>,
    pub stats: Option<FrameStats>,
    /// The page as its renderer last published it: what a link, a field and a cursor are read from.
    pub snapshot: Option<Snapshot>,
    /// Where a candidate window goes: the caret's box in the page.
    pub ime_rect: Option<Rect>,
    /// Whether the inspector's element picker has the pointer: the page hears nothing then.
    pub picking: bool,
    /// What takes the keyboard for the page: the window's input goes to its element.
    pub focus: Option<FocusHandle>,
    /// The platform's text input into the page.
    pub ime: Option<Entity<ImeBridge>>,
    /// The pointer, the keys and what they reported.
    pub input: Input,
    /// The cursor an `auto` box shows, by node, for the snapshot it was worked out from.
    auto_cursors: HashMap<NodeId, Cursor>,
}

impl DomState {
    /// The wheel moved `node` to `to`.
    pub(crate) fn predict(&mut self, node: NodeId, to: [f32; 2]) {
        self.predicted.insert(node, to);
        self.generation += 1;
    }

    /// The cursor a pointer over `node` shows, given what its box asks for: `auto` is worked out
    /// from the page (a link, a field) once per snapshot.
    pub(crate) fn cursor_of(&mut self, node: NodeId, asked: Cursor) -> Cursor {
        if asked != Cursor::Auto {
            return asked;
        }
        if let Some(known) = self.auto_cursors.get(&node) {
            return *known;
        }
        let shown = crate::cursor::resolve(self.snapshot.as_ref(), node, asked);
        self.auto_cursors.insert(node, shown);
        shown
    }

    /// What the list comes to now: kept from the frame before while the list and the predicted
    /// offsets are the same, since the frame is built from it twice (hitboxes, shapes).
    pub(crate) fn plan(&mut self) -> Option<Rc<Plan>> {
        let list = self.list.clone()?;
        if let Some((seen, generation, plan)) = &self.plan
            && Arc::ptr_eq(seen, &list)
            && *generation == self.generation
        {
            return Some(Rc::clone(plan));
        }
        let predicted = &self.predicted;
        let made = Rc::new(plan::plan(&list, &|node| predicted.get(&node).copied()));
        self.plan = Some((list, self.generation, Rc::clone(&made)));
        Some(made)
    }
}

/// What showing a list changed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Shown {
    /// The list was the one already shown, and no prediction was waiting to be undone.
    Unchanged,
    /// The list was the same, and the renderer's offsets took the place of a prediction.
    Redraw,
    /// A list that was not shown before.
    New,
}

/// What a window draws: the page as it was last published.
pub struct DomView {
    state: Rc<RefCell<DomState>>,
    /// What shows where the page has no background of its own: `None` is a transparent window.
    background: Option<Rgba>,
    regions: Vec<AppRegion>,
    /// Called as each frame is built: how the window knows its first one.
    drawn: Option<Drawn>,
    /// The node the inspector points at, drawn over the page.
    highlight: Option<Highlight>,
    /// Takes the pointer while the inspector picks a node.
    picker: Option<Picker>,
}

impl DomView {
    /// A view with nothing to draw yet, over `background` (`None` for a window that is transparent).
    pub fn new(background: Option<Rgba>) -> Self {
        let state = DomState {
            stats: FrameStats::from_env(),
            ..DomState::default()
        };
        Self {
            state: Rc::new(RefCell::new(state)),
            background,
            regions: Vec::new(),
            drawn: None,
            highlight: None,
            picker: None,
        }
    }

    /// Draws the box of the node `highlight` names over the page, or nothing.
    pub(crate) fn set_highlight(&mut self, highlight: Option<Highlight>) {
        self.highlight = highlight;
    }

    /// Gives the view the keyboard handle and the text input its window needs; called once, as
    /// the view is made.
    pub(crate) fn attach(&mut self, cx: &mut Context<Self>) {
        let ime = cx.new(|_| ImeBridge::new(Rc::clone(&self.state)));
        let mut state = self.state.borrow_mut();
        state.focus = Some(cx.focus_handle());
        state.ime = Some(ime);
    }

    /// The keyboard handle of the page, for the window to give the focus to.
    pub(crate) fn focus_handle(&self) -> Option<FocusHandle> {
        self.state.borrow().focus.clone()
    }

    /// Keeps the page as its renderer published it, and where the caret is. Answers whether the
    /// field with the keyboard or the caret moved, which the platform's text input must hear of.
    pub(crate) fn set_page(&mut self, snapshot: Snapshot, caret: Option<Rect>) -> bool {
        let mut state = self.state.borrow_mut();
        let before = state.snapshot.as_ref().map(ime::editable_focus);
        let after = ime::editable_focus(&snapshot);
        if state
            .snapshot
            .as_ref()
            .is_none_or(|seen| seen.revision != snapshot.revision)
        {
            state.auto_cursors.clear();
        }
        if before != Some(after) {
            state.input.clear_typed();
        }
        state.snapshot = Some(snapshot);
        let moved = state.ime_rect != caret;
        state.ime_rect = caret;
        moved || before != Some(after)
    }

    /// Where the keys and what the pointer shows go.
    pub(crate) fn on_keys(&mut self, keys: Rc<dyn Fn(bool, verve_dom::Key)>) {
        self.state.borrow_mut().input.keys = Some(keys);
    }

    /// Where the cursor and the link under the pointer are reported.
    pub(crate) fn on_report(&mut self, report: Rc<dyn Fn(verve_electron::msg::PageEvent)>) {
        self.state.borrow_mut().input.report = Some(report);
    }

    /// Takes the pointer for `picker` (the inspector's element picker), or gives it back.
    pub(crate) fn set_picker(&mut self, picker: Option<Picker>) {
        self.state.borrow_mut().picking = picker.is_some();
        self.picker = picker;
    }

    /// The color under the page, `None` for none.
    pub fn set_background(&mut self, background: Option<Rgba>) {
        self.background = background;
    }

    /// The parts of the window that drag it: they come from the display list's hitboxes.
    pub fn set_app_regions(&mut self, regions: Vec<AppRegion>) {
        self.regions = regions;
    }

    /// Calls `drawn` each time a frame is built.
    pub fn on_draw(&mut self, drawn: Drawn) {
        self.drawn = Some(drawn);
    }

    /// Tells the page what a person does to the window with `tell`.
    pub(crate) fn on_input(&mut self, tell: Tell) {
        self.state.borrow_mut().tell = Some(tell);
    }

    /// Draws `list` from the next frame on. The renderer's offsets win over the ones the wheel
    /// predicted, so every list that arrives ends the prediction, also the one that is already
    /// shown: that is what a wheel the page prevented, or one that moved nothing, publishes.
    pub fn show(&mut self, list: Arc<DisplayList>) -> Shown {
        let mut state = self.state.borrow_mut();
        let predicted = !state.predicted.is_empty();
        if predicted {
            state.predicted.clear();
            state.generation += 1;
        }
        // Lists are numbered across the process, so one number is one list.
        if state
            .list
            .as_ref()
            .is_some_and(|seen| seen.revision == list.revision)
        {
            return if predicted {
                Shown::Redraw
            } else {
                Shown::Unchanged
            };
        }
        state.list = Some(list);
        Shown::New
    }

    /// Draws the pictures in `table` from the next frame on. Answers whether it is not the table
    /// already shown.
    pub fn show_images(&mut self, table: Arc<ImageTable>) -> bool {
        let mut state = self.state.borrow_mut();
        if Arc::ptr_eq(&state.images, &table) {
            return false;
        }
        state.images = Arc::clone(&table);
        state.pictures.set_table(table);
        true
    }

    /// The pictures the window draws.
    pub fn images(&self) -> Arc<ImageTable> {
        Arc::clone(&self.state.borrow().images)
    }

    /// The list the window draws.
    pub fn list(&self) -> Option<Arc<DisplayList>> {
        self.state.borrow().list.clone()
    }

    /// How big the page was drawn last, in CSS pixels; zero before the first frame.
    pub fn viewport(&self) -> (f32, f32) {
        self.state.borrow().viewport
    }

    /// The color under the page.
    pub fn background(&self) -> Option<Rgba> {
        self.background
    }
}

/// Electron's white: what a window shows until its page or its `backgroundColor` says otherwise.
pub fn white() -> Rgba {
    rgb(0x00ff_ffff)
}

impl Render for DomView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(drawn) = &self.drawn {
            drawn(window);
        }
        let mut page = div().size_full();
        // The keys go to the page through the window's keyboard handle, whose element this is.
        if let Some(focus) = self.focus_handle() {
            page = page
                .track_focus(&focus)
                .key_context("VervePage")
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, _| {
                    let kind = (true, event.is_held, event.prefer_character_input);
                    input::key(&this.state, &event.keystroke, kind, &window.modifiers());
                }))
                .on_key_up(cx.listener(|this, event: &KeyUpEvent, window, _| {
                    let kind = (false, false, false);
                    input::key(&this.state, &event.keystroke, kind, &window.modifiers());
                }))
                .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, _| {
                    input::modifiers_changed(&this.state, event);
                }));
        }
        if let Some(background) = self.background {
            page = page.bg(background);
        }
        let mut page = page
            .child(DomElement::new(Rc::clone(&self.state)))
            .child(app_region::layer(&self.regions));
        if let Some(highlight) = self.highlight {
            page = page.child(overlay::highlight_layer(Rc::clone(&self.state), highlight));
        }
        if let Some(picker) = &self.picker {
            page = page.child(overlay::pick_layer(
                Rc::clone(&self.state),
                Rc::clone(picker),
            ));
        }
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(revision: u64) -> Arc<DisplayList> {
        Arc::new(DisplayList {
            revision,
            ..DisplayList::default()
        })
    }

    #[test]
    fn a_list_replaces_the_one_before_it_and_the_same_list_changes_nothing() {
        let mut view = DomView::new(None);
        assert_eq!(view.show(list(1)), Shown::New);
        assert_eq!(view.show(list(1)), Shown::Unchanged);
        assert_eq!(view.show(list(2)), Shown::New);
        assert_eq!(view.list().map(|list| list.revision), Some(2));
    }

    #[test]
    fn every_list_that_arrives_ends_the_prediction() {
        let mut view = DomView::new(None);
        view.show(list(1));
        view.state.borrow_mut().predict(7, [0., 40.]);
        // The renderer prevented the wheel and published the list it had: the view is not left
        // scrolled by a prediction nobody confirmed.
        assert_eq!(view.show(list(1)), Shown::Redraw);
        assert!(view.state.borrow().predicted.is_empty());
        view.state.borrow_mut().predict(7, [0., 40.]);
        assert_eq!(view.show(list(2)), Shown::New);
        assert!(view.state.borrow().predicted.is_empty());
    }

    #[test]
    fn the_plan_is_made_again_only_when_the_list_or_the_prediction_changed() {
        let mut view = DomView::new(None);
        view.show(list(1));
        let first = view.state.borrow_mut().plan().expect("a plan");
        let again = view.state.borrow_mut().plan().expect("a plan");
        assert!(Rc::ptr_eq(&first, &again));
        view.state.borrow_mut().predict(7, [0., 40.]);
        let moved = view.state.borrow_mut().plan().expect("a plan");
        assert!(!Rc::ptr_eq(&first, &moved));
    }
}
