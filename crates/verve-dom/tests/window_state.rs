//! What the window tells the document (visibility, focus, where lines were painted) and what
//! the document asks of it (`scrollIntoView`).

mod common;

use common::{Page, on_page};
use verve_dom::{Align, Options, Rect, Request, ScrollRequest};

#[test]
fn visibility_and_window_focus_are_told_and_heard() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"window.heard = [];
               document.addEventListener('visibilitychange', () => heard.push(document.visibilityState));
               window.addEventListener('blur', () => heard.push('blur:' + document.hasFocus()));
               window.addEventListener('focus', () => heard.push('focus:' + document.hasFocus()));"#,
        );
        page.document.set_visibility(&mut page.runtime, true);
        page.document.set_visibility(&mut page.runtime, true);
        page.document.set_window_focus(&mut page.runtime, false);
        page.document.set_window_focus(&mut page.runtime, true);
        page.document.set_visibility(&mut page.runtime, false);
        assert_eq!(
            page.eval("heard.join(' ') + ' ' + document.hidden"),
            "hidden blur:false focus:true visible false"
        );
    });
}

#[test]
fn scroll_into_view_says_where_to_line_the_box_up() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("window.el = document.body.appendChild(document.createElement('div'))");
        page.eval("el.scrollIntoView(); el.scrollIntoView(false); el.scrollIntoViewIfNeeded()");
        page.eval("el.scrollIntoView({ block: 'center', inline: 'end', behavior: 'smooth' })");
        let revealed: Vec<_> = page
            .document
            .take_requests()
            .into_iter()
            .filter_map(|request| match request {
                Request::Scroll(ScrollRequest::Reveal {
                    block,
                    inline,
                    smooth,
                    ..
                }) => Some((block, inline, smooth)),
                _ => None,
            })
            .collect();
        assert_eq!(
            revealed,
            [
                (Align::Start, Align::Nearest, false),
                (Align::End, Align::Nearest, false),
                (Align::Center, Align::Nearest, false),
                (Align::Center, Align::End, true),
            ]
        );
    });
}

#[test]
fn an_inline_box_has_a_rectangle_per_line() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("window.span = document.body.appendChild(document.createElement('span'))");
        let span = page.find("span");
        let line = |y| Rect::new(0., y, 50., 10.);
        page.document
            .set_layout(&mut page.runtime, [(span, Rect::new(0., 0., 50., 30.))]);
        assert_eq!(page.eval("span.getClientRects().length"), "1");
        page.document
            .set_fragments([(span, vec![line(0.), line(10.), line(20.)])]);
        assert_eq!(
            page.eval("[...span.getClientRects()].map(r => r.top).join()"),
            "0,10,20"
        );
    });
}

#[test]
fn a_page_keeps_its_window_in_each_way_beforeunload_allows() {
    // Each script, and whether the page lets the window close after it.
    let cases = [
        ("", true),
        ("addEventListener('beforeunload', () => {})", true),
        (
            "addEventListener('beforeunload', (e) => e.preventDefault())",
            false,
        ),
        (
            "addEventListener('beforeunload', (e) => { e.returnValue = false; })",
            false,
        ),
        (
            "addEventListener('beforeunload', (e) => { e.returnValue = 'stay'; })",
            false,
        ),
        (
            "addEventListener('beforeunload', (e) => { e.returnValue = ''; })",
            true,
        ),
        ("onbeforeunload = () => 'stay'", false),
        ("onbeforeunload = () => false", false),
        // Any answer but `null` and `undefined` sets the canceled flag, the empty string too.
        ("onbeforeunload = () => ''", false),
        ("onbeforeunload = () => undefined", true),
        ("onbeforeunload = () => null", true),
    ];
    for (script, proceeds) in cases {
        on_page(move || {
            let mut page = Page::new(Options::default());
            page.eval(script);
            let proceed = page.document.before_unload(&mut page.runtime);
            assert_eq!(proceed, proceeds, "{script}");
        });
    }
}

#[test]
fn the_return_value_of_a_beforeunload_event_is_text() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"window.seen = [];
               addEventListener('beforeunload', (e) => {
                 seen.push(JSON.stringify(e.returnValue));
                 e.returnValue = false;
                 seen.push(JSON.stringify(e.returnValue));
               });"#,
        );
        page.document.before_unload(&mut page.runtime);
        assert_eq!(page.eval("seen.join(' ')"), r#""" "false""#);
    });
}
