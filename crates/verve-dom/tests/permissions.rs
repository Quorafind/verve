//! `Notification`, `navigator.permissions.query` and `clipboard.readText` ask the embedder.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::Page;
use verve_dom::{Options, PermissionBackend};

/// Answers at once, or says it will answer later, and remembers each question.
struct Answers {
    now: Option<bool>,
    known: Option<bool>,
    asked: Rc<RefCell<Vec<String>>>,
}

impl PermissionBackend for Answers {
    fn ask(&self, ask: u64, permission: &str, url: &str, check_only: bool) -> Option<bool> {
        self.asked
            .borrow_mut()
            .push(format!("{ask} {permission} {url} {check_only}"));
        self.now
    }

    fn known(&self, _: &str) -> Option<bool> {
        self.known
    }
}

fn page_with(now: Option<bool>, known: Option<bool>) -> (Page, Rc<RefCell<Vec<String>>>) {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let page = Page::new(Options {
        url: "http://127.0.0.1:7/p".into(),
        permissions: Some(Box::new(Answers {
            now,
            known,
            asked: Rc::clone(&asked),
        })),
        ..Options::default()
    });
    (page, asked)
}

#[test]
fn a_denied_notification_request_answers_denied() {
    let (mut page, asked) = page_with(Some(false), None);
    page.eval("var out; Notification.requestPermission().then((r) => { out = r; })");
    page.settle();
    assert_eq!(page.eval("out"), "denied");
    assert_eq!(
        *asked.borrow(),
        ["1 notifications http://127.0.0.1:7/p false"]
    );
    // A request answer does not feed `Notification.permission` (Electron 39.8.10, probe7
    // `denyRequest`: the request was denied and the property still read from the check side).
    // With nothing known from a check, the property is `default`.
    assert_eq!(page.eval("Notification.permission"), "default");
}

#[test]
fn notification_permission_is_what_the_backend_says_is_known() {
    // Electron 39.8.10, probe7 `denyRequest`: with only a request handler that denies, the check
    // side defaults to granted, so the request answers `denied` while the property reads `granted`.
    let (mut page, _) = page_with(Some(false), Some(true));
    page.eval("var out; Notification.requestPermission().then((r) => { out = r; })");
    page.settle();
    assert_eq!(page.eval("out"), "denied");
    assert_eq!(page.eval("Notification.permission"), "granted");
}

#[test]
fn an_answer_that_comes_later_settles_the_promise_then() {
    let (mut page, asked) = page_with(None, Some(true));
    page.eval("var out = 'waiting'; navigator.permissions.query({ name: 'notifications' }).then((s) => { out = s.name + ' ' + s.state; })");
    page.settle();
    assert_eq!(page.eval("out"), "waiting");
    assert_eq!(
        *asked.borrow(),
        ["1 notifications http://127.0.0.1:7/p true"]
    );
    page.document.settle_permission(&mut page.runtime, 1, true);
    assert_eq!(page.eval("out"), "notifications granted");
    assert_eq!(page.eval("Notification.permission"), "granted");
}

#[test]
fn reading_the_clipboard_needs_the_permission() {
    let (mut page, _) = page_with(Some(false), None);
    page.eval("var out; navigator.clipboard.readText().catch((e) => { out = e.name; })");
    page.settle();
    assert_eq!(page.eval("out"), "NotAllowedError");
}

#[test]
fn a_page_with_no_backend_is_refused() {
    let mut page = Page::new(Options::default());
    page.eval("var out; Notification.requestPermission().then((r) => { out = r; })");
    page.settle();
    assert_eq!(page.eval("out"), "denied");
}
