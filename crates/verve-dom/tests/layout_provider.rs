//! The layout provider: geometry a script reads is worked out for the document as it is, at
//! the read, and the drawer still hears of what the provider took on the way.

mod common;

use std::cell::Cell;
use std::rc::Rc;

use common::{Page, on_page};
use verve_dom::{Layout, Options, Rect};

/// A provider that gives the first `div` a box as wide as ten pixels for each child of the body,
/// and counts how often it is asked.
fn count_children(page: &Page, asked: &Rc<Cell<u32>>) {
    let asked = Rc::clone(asked);
    let div = page.find("div");
    page.document
        .set_layout_provider(Some(Box::new(move |update| {
            asked.set(asked.get() + 1);
            let dom = &update.snapshot.dom;
            let width = dom.children(dom.body).len() as f32 * 10.;
            Layout {
                rects: vec![(div, Rect::new(0., 0., width, 5.))],
                ..Layout::default()
            }
        })));
}

#[test]
fn a_read_after_a_change_is_laid_out_once() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("window.el = document.body.appendChild(document.createElement('div'))");
        let asked = Rc::new(Cell::new(0));
        count_children(&page, &asked);
        assert_eq!(page.eval("el.getBoundingClientRect().width"), "10");
        assert_eq!(page.eval("el.offsetHeight"), "5");
        assert_eq!(asked.get(), 1, "the second read found the layout current");
        page.eval("document.body.appendChild(document.createElement('div'))");
        assert_eq!(page.eval("el.getBoundingClientRect().width"), "20");
        assert_eq!(asked.get(), 2);
    });
}

#[test]
fn what_the_layout_took_is_still_in_the_next_update() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("window.el = document.body.appendChild(document.createElement('div'))");
        let asked = Rc::new(Cell::new(0));
        count_children(&page, &asked);
        page.eval("el.style.width = '30px'; el.getBoundingClientRect().width");
        assert_eq!(asked.get(), 1);
        let update = page.document.update();
        let div = page.find("div");
        assert!(update.changes.nodes.contains(&div));
        assert!(page.document.update().changes.is_empty());
    });
}

#[test]
fn a_page_without_a_provider_answers_from_the_boxes_it_was_given() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("window.el = document.body.appendChild(document.createElement('div'))");
        let div = page.find("div");
        page.document
            .set_layout(&mut page.runtime, [(div, Rect::new(0., 0., 40., 6.))]);
        assert_eq!(page.eval("el.getBoundingClientRect().width"), "40");
        page.eval("el.style.width = '99px'");
        assert_eq!(page.eval("el.getBoundingClientRect().width"), "40");
    });
}

#[test]
fn a_change_after_an_early_layout_and_what_the_layout_took_both_reach_the_update() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("window.el = document.body.appendChild(document.createElement('div'))");
        page.document.update();
        let asked = Rc::new(Cell::new(0));
        count_children(&page, &asked);
        page.eval("el.id = 'early'; el.getBoundingClientRect().width");
        page.eval("document.body.appendChild(document.createElement('span'))");
        let update = page.document.update();
        let (div, span) = (page.find("div"), page.find("span"));
        assert!(update.changes.nodes.contains(&div), "what the layout took");
        assert!(update.changes.nodes.contains(&span), "what came after it");
    });
}

#[test]
fn a_viewport_change_alone_asks_for_a_layout_that_a_resize_handler_can_read() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("window.el = document.body.appendChild(document.createElement('div'))");
        let div = page.find("div");
        page.document
            .set_layout_provider(Some(Box::new(move |update| Layout {
                rects: vec![(div, Rect::new(0., 0., update.snapshot.viewport.0, 5.))],
                ..Layout::default()
            })));
        page.eval(
            "window.seen = []; addEventListener('resize', () => seen.push(el.getBoundingClientRect().width))",
        );
        page.document.set_viewport(&mut page.runtime, 300., 200.);
        assert_eq!(page.eval("seen.join()"), "300");
    });
}

#[test]
fn no_observer_hears_of_a_layout_a_read_made_until_the_turn_ends() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("window.el = document.body.appendChild(document.createElement('div'))");
        let asked = Rc::new(Cell::new(0));
        count_children(&page, &asked);
        page.eval("window.heard = 0; new ResizeObserver(() => heard++).observe(el)");
        page.eval("el.getBoundingClientRect().width");
        assert_eq!(page.eval("heard"), "0", "a read runs no callback");
        page.document.ensure_layout();
        page.document.deliver_layout(&mut page.runtime);
        assert_eq!(page.eval("heard"), "1");
    });
}

#[test]
fn a_scroll_reads_back_clamped_and_by_is_not_doubled() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("window.el = document.body.appendChild(document.createElement('div'))");
        let div = page.find("div");
        // A box with 100 px to scroll and 300 px of content.
        page.document
            .set_layout_provider(Some(Box::new(move |update| {
                let wanted = update.snapshot.scroll_offsets.get(&div).copied();
                Layout {
                    scroll_offsets: vec![(div, [0., wanted.unwrap_or_default()[1].min(100.)])],
                    scroll_extents: vec![(div, [50., 300.])],
                    ..Layout::default()
                }
            })));
        assert_eq!(page.eval("el.scrollHeight"), "300");
        page.eval("el.scrollBy(0, 40); el.scrollBy(0, 40)");
        assert_eq!(page.eval("el.scrollTop"), "80");
        page.eval("el.scrollTop = 9999");
        assert_eq!(page.eval("el.scrollTop"), "100", "clamped at once");
        assert_eq!(
            page.eval("el.scrollHeight"),
            "300",
            "not by what is scrolled"
        );
    });
}

#[test]
fn what_an_animation_restyled_makes_the_layout_stale() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.document
            .set_stylesheet("test", "#box { transition: opacity 100ms linear }");
        page.eval(
            "document.body.innerHTML = '<div id=box></div>'; window.box = document.getElementById('box');
             box.style.opacity = '1';",
        );
        page.document.update();
        let asked = Rc::new(Cell::new(0));
        count_children(&page, &asked);
        page.eval("box.getBoundingClientRect()");
        page.eval("box.style.opacity = '0'; box.getBoundingClientRect()");
        let before = asked.get();
        // The transition moves on a frame, and nothing in the document changes.
        for now in [0., 50.] {
            page.document.frame(&mut page.runtime, now);
        }
        page.eval("box.getBoundingClientRect()");
        assert!(
            asked.get() > before,
            "a frame of the transition laid out again"
        );
    });
}
