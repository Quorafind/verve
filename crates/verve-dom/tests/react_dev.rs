//! React's development builds — with their checks of the DOM's nesting, their warnings and
//! their extra reads of the document — running the same app as `tests/react.rs`, with nothing
//! said on the console.
//!
//! Ignored by default: react and react-dom 18.3.1 are installed with `verve::npm` the first
//! time, into `VERVE_DOM_REACT_DEV` or the test target's scratch directory. Run with
//! `cargo test -p verve-dom --test react_dev -- --ignored`.

mod common;

use std::path::PathBuf;

use common::{Page, on_page};
use verve_dom::{Key, Options};

fn prepared() -> PathBuf {
    let dir = std::env::var_os("VERVE_DOM_REACT_DEV")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("react-dev"));
    if !dir
        .join("node_modules/react-dom/cjs/react-dom.development.js")
        .exists()
    {
        std::fs::create_dir_all(&dir).unwrap();
        let package = r#"{ "name": "verve-dom-react-dev", "private": true, "dependencies": { "react": "18.3.1", "react-dom": "18.3.1" } }"#;
        std::fs::write(dir.join("package.json"), package).unwrap();
        verve::npm::install(&dir, &verve::npm::InstallOptions::default())
            .unwrap_or_else(|e| panic!("npm install failed: {e}"));
    }
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/react-app.js"),
        dir.join("react-app.js"),
    )
    .unwrap();
    dir
}

#[test]
#[ignore = "installs React from npm"]
fn the_development_builds_render_type_and_unmount_without_a_warning() {
    on_page(|| {
        let dir = prepared();
        let mut page = Page::new(Options::default());
        page.eval("process.env.NODE_ENV = 'development'");
        let path = dir.join("react-app.js");
        let source = std::fs::read_to_string(&path).unwrap();
        if let Err(error) = page.runtime.load_module(&path, &source) {
            panic!("the app failed to load: {error}\n{}", page.console());
        }
        page.settle();
        assert_eq!(
            page.eval("document.getElementById('inc').textContent"),
            "count: 0"
        );
        let inc = page.find("#inc");
        page.document.press(&mut page.runtime, inc);
        page.settle();
        assert_eq!(
            page.eval("document.getElementById('inc').textContent"),
            "count: 1"
        );
        let field = page.find("#field");
        page.document.press(&mut page.runtime, field);
        page.document.type_text(&mut page.runtime, "dev");
        page.document
            .key_down(&mut page.runtime, &Key::named("Backspace"));
        page.settle();
        assert_eq!(
            page.eval("document.getElementById('echo').textContent"),
            "DE"
        );
        let check = page.find("#check");
        page.document.press(&mut page.runtime, check);
        page.settle();
        assert_eq!(
            page.eval("document.getElementById('check').checked"),
            "true"
        );
        page.eval("unmount()");
        page.settle();
        assert_eq!(
            page.eval("document.getElementById('root').childNodes.length"),
            "0"
        );
        // React's development build says on the console what it finds wrong with the DOM.
        let console = page.console();
        let complaints: Vec<&str> = console
            .lines()
            .filter(|line| !line.contains("Download the React DevTools"))
            .collect();
        assert!(complaints.is_empty(), "{complaints:#?}");
    });
}
