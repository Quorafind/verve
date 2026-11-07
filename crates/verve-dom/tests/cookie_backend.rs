//! `document.cookie` goes to the embedder's jar when the page has one.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::Page;
use verve_dom::{CookieBackend, Options};

/// A jar that remembers what it was asked and answers with a fixed string.
struct Recorder(Rc<RefCell<Vec<String>>>);

impl CookieBackend for Recorder {
    fn get(&self, url: &str) -> String {
        self.0.borrow_mut().push(format!("get {url}"));
        "a=1; b=2".into()
    }

    fn set(&self, url: &str, line: &str) {
        self.0.borrow_mut().push(format!("set {url} {line}"));
    }
}

#[test]
fn document_cookie_reads_and_writes_the_embedders_jar() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut page = Page::new(Options {
        url: "http://127.0.0.1:7/p".into(),
        cookies: Some(Box::new(Recorder(Rc::clone(&seen)))),
        ..Options::default()
    });
    assert_eq!(page.eval("document.cookie"), "a=1; b=2");
    page.eval("document.cookie = 'c=3; path=/'");
    assert_eq!(
        *seen.borrow(),
        [
            "get http://127.0.0.1:7/p",
            "set http://127.0.0.1:7/p c=3; path=/"
        ]
    );
}

#[test]
fn without_a_jar_the_page_keeps_its_cookies_in_the_window() {
    let mut page = Page::new(Options::default());
    page.eval("document.cookie = 'k=v'");
    assert_eq!(page.eval("document.cookie"), "k=v");
}
