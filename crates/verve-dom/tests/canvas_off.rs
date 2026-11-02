//! Built without the `canvas` feature a page has no 2D context, but `<img>` still reports the
//! size its header gives. Run with `--no-default-features`.

#![cfg(not(feature = "canvas"))]

mod common;

use common::{PNG_1X1, Page, on_page};
use verve_dom::Options;

#[test]
fn no_context_but_images_keep_their_size() {
    on_page(|| {
        let mut p = Page::new(Options {
            url: "https://app.test/".into(),
            ..Options::default()
        });
        p.eval("globalThis.c = document.createElement('canvas'); c.width = 20;");
        assert_eq!(p.eval("c.getContext('2d')"), "null");
        assert_eq!(p.eval("c.width + ',' + c.height"), "20,150");
        let size = p.eval(&format!(
            "const i = new Image(); i.src = '{PNG_1X1}'; i.complete + ',' + i.naturalWidth"
        ));
        assert_eq!(size, "true,1");
    });
}
