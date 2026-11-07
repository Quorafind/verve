//! A link with a `download` attribute asks the embedder to save its address, and goes nowhere.

mod common;

use common::Page;
use verve_dom::{Options, Request};

#[test]
fn pressing_a_download_link_asks_for_a_download_and_does_not_navigate() {
    let mut page = Page::new(Options {
        url: "http://127.0.0.1:7/dir/p".into(),
        ..Options::default()
    });
    page.eval(
        "document.body.innerHTML = '<a id=a href=\"f.bin\" download=\"x.bin\">a</a><a id=b href=\"g\" download>b</a><a id=c href=\"h\">c</a>';
         document.getElementById('a').click(); document.getElementById('b').click();
         document.getElementById('c').click();",
    );
    assert_eq!(
        page.document.take_requests(),
        [
            Request::Download {
                url: "http://127.0.0.1:7/dir/f.bin".into(),
                filename: Some("x.bin".into()),
            },
            Request::Download {
                url: "http://127.0.0.1:7/dir/g".into(),
                filename: None,
            },
            Request::Navigate {
                url: "http://127.0.0.1:7/dir/h".into(),
                replace: false,
            },
        ]
    );
}
