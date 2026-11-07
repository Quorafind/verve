//! One window's page: the display list its renderer published, drawn by [`DomElement`].
//!
//! The renderer lays the page out on its own thread (`verve-layout`) and says what to draw as a
//! [`DisplayList`]. The view keeps the last one it was given.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Context, IntoElement, ParentElement, Render, Rgba, Styled, Window, div, rgb};
use verve_layout::DisplayList;

use crate::app_region::{self, AppRegion};
use crate::dom_element::DomElement;
use crate::plan::{self, Plan};

/// What is called as each frame of a window is built.
type Drawn = Rc<dyn Fn(&Window)>;

/// What the view and its element share.
#[derive(Default)]
pub(crate) struct DomState {
    pub list: Option<Arc<DisplayList>>,
    /// What the list comes to, for the list it was made for.
    plan: Option<(Arc<DisplayList>, Rc<Plan>)>,
}

impl DomState {
    /// What the list comes to: kept from the frame before while the list is the same, since the
    /// frame is built from it twice (hitboxes, shapes).
    pub(crate) fn plan(&mut self) -> Option<Rc<Plan>> {
        let list = self.list.clone()?;
        if let Some((seen, plan)) = &self.plan
            && Arc::ptr_eq(seen, &list)
        {
            return Some(Rc::clone(plan));
        }
        let made = Rc::new(plan::plan(&list, &|_| None));
        self.plan = Some((list, Rc::clone(&made)));
        Some(made)
    }
}

/// What showing a list changed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Shown {
    /// The list was the one already shown.
    Unchanged,
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
}

impl DomView {
    /// A view with nothing to draw yet, over `background` (`None` for a window that is transparent).
    pub fn new(background: Option<Rgba>) -> Self {
        let state = DomState::default();
        Self {
            state: Rc::new(RefCell::new(state)),
            background,
            regions: Vec::new(),
            drawn: None,
        }
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

    /// Draws `list` from the next frame on.
    pub fn show(&mut self, list: Arc<DisplayList>) -> Shown {
        let mut state = self.state.borrow_mut();
        // Lists are numbered across the process, so one number is one list.
        if state
            .list
            .as_ref()
            .is_some_and(|seen| seen.revision == list.revision)
        {
            return Shown::Unchanged;
        }
        state.list = Some(list);
        Shown::New
    }
}

/// Electron's white: what a window shows until its page or its `backgroundColor` says otherwise.
pub fn white() -> Rgba {
    rgb(0x00ff_ffff)
}

impl Render for DomView {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if let Some(drawn) = &self.drawn {
            drawn(window);
        }
        let mut page = div().size_full();
        if let Some(background) = self.background {
            page = page.bg(background);
        }
        page.child(DomElement::new(Rc::clone(&self.state)))
            .child(app_region::layer(&self.regions))
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
        assert_eq!(
            view.state.borrow().list.as_ref().map(|list| list.revision),
            Some(2)
        );
    }

    #[test]
    fn the_plan_is_made_again_only_when_the_list_changed() {
        let mut view = DomView::new(None);
        view.show(list(1));
        let first = view.state.borrow_mut().plan().expect("a plan");
        let again = view.state.borrow_mut().plan().expect("a plan");
        assert!(Rc::ptr_eq(&first, &again));
        view.show(list(2));
        let next = view.state.borrow_mut().plan().expect("a plan");
        assert!(!Rc::ptr_eq(&first, &next));
    }
}
