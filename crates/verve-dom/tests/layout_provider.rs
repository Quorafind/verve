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

#[test]
fn an_inline_elements_client_rects_follow_a_change_with_no_range_call_before_them() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            "window.span = document.body.appendChild(document.createElement('span'));
             span.textContent = 'x'",
        );
        let span = page.find("span");
        // One box for each child of the body: a change to the document moves what the span has.
        page.document
            .set_layout_provider(Some(Box::new(move |update| {
                let dom = &update.snapshot.dom;
                let count = dom.children(dom.body).len();
                let lines = (0..count)
                    .map(|line| Rect::new(0., line as f32 * 10., 10., 10.))
                    .collect();
                Layout {
                    rects: vec![(span, Rect::new(0., 0., 10., 10.))],
                    fragments: vec![(span, lines)],
                    ..Layout::default()
                }
            })));
        assert_eq!(page.eval("span.getClientRects().length"), "1");
        page.eval("document.body.appendChild(document.createElement('b'))");
        assert_eq!(page.eval("span.getClientRects().length"), "2");
    });
}

#[test]
fn the_text_position_under_a_point_comes_from_the_position_provider() {
    on_page(|| {
        let page = Page::new(Options::default());
        assert_eq!(page.document.text_position_at(1., 2.), None);
        let body = page.find("body");
        page.document
            .set_position_provider(Some(Box::new(move |x, y| {
                (x < y).then_some((body, x as usize))
            })));
        assert_eq!(page.document.text_position_at(1., 2.), Some((body, 1)));
        assert_eq!(page.document.text_position_at(3., 2.), None);
    });
}

#[test]
fn caret_range_and_position_from_a_point_name_the_text_the_provider_finds() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("document.body.innerHTML = '<p id=p>hello</p>'");
        let text = page.eval(
            "document.getElementById('p').firstChild === document.body.firstChild.firstChild",
        );
        assert_eq!(text, "true");
        assert_eq!(
            page.eval(
                "String(document.caretRangeFromPoint(3, 4)) + document.caretPositionFromPoint(3, 4)"
            ),
            "nullnull",
            "no provider, no text to be near"
        );
        let node = page.find("p");
        let first = page.document.update().snapshot.dom.children(node)[0];
        page.document
            .set_position_provider(Some(Box::new(move |x, _| Some((first, x as usize)))));
        assert_eq!(
            page.eval(
                r#"const r = document.caretRangeFromPoint(3, 4);
                   const c = document.caretPositionFromPoint(2, 4);
                   [r.collapsed, r.startContainer.nodeValue, r.startOffset,
                    c.offsetNode === r.startContainer, c.offset].join()"#
            ),
            "true,hello,3,true,2"
        );
    });
}

#[test]
fn the_selection_is_readable_by_the_host_as_two_boundaries() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("document.body.innerHTML = '<p id=p>hello</p>'");
        assert_eq!(page.document.selection_boundaries(&mut page.runtime), None);
        page.eval(
            "const s = document.getSelection(); const t = document.getElementById('p').firstChild; s.setBaseAndExtent(t, 1, t, 4)",
        );
        let text = page.document.update().snapshot.dom.children(page.find("p"))[0];
        assert_eq!(
            page.document.selection_boundaries(&mut page.runtime),
            Some(((text, 1), (text, 4)))
        );
    });
}

#[test]
fn get_bbox_is_the_box_the_provider_measured_in_the_drawings_own_units() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            "document.body.innerHTML = '<svg width=\"200\" height=\"100\"><rect id=\"r\" width=\"30\" height=\"20\"/></svg>'; window.r = document.getElementById('r')",
        );
        let rect = page.find("rect");
        page.document
            .set_layout_provider(Some(Box::new(move |_| Layout {
                // On the page the rectangle is at (30, 30), 60 by 40; in the drawing's units it
                // is at (10, 5), 30 by 20.
                rects: vec![(rect, Rect::new(30., 30., 60., 40.))],
                svg_boxes: vec![(rect, Rect::new(10., 5., 30., 20.))],
                ..Layout::default()
            })));
        let said = page.eval(
            "(() => { const b = r.getBBox(), c = r.getBoundingClientRect(); return [b.x, b.y, b.width, b.height, c.x, c.y, c.width, c.height].join(' '); })()",
        );
        assert_eq!(said, "10 5 30 20 30 30 60 40");
    });
}
