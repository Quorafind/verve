//! turndown 7 (`tests/fixtures/node_modules/turndown`, unmodified) turning HTML into Markdown
//! through the document's parsers — as `dsh-tool-web` bundles it.

mod common;

use std::path::PathBuf;

use common::{Page, on_page};
use verve_dom::Options;

const HTML: &str = r#"
<h1>Release notes</h1>
<p>Verve now has a <strong>document</strong>, with <em>events</em> and <a href="https://example.com/docs">docs</a>.</p>
<ul><li>React 18</li><li>turndown <code>7</code></li></ul>
<blockquote><p>Quoted &amp; kept</p></blockquote>
<pre><code class="language-rust">fn main() {}
</code></pre>
<script>ignored()</script>
<p>Last<br>line</p>
"#;

/// What turndown 7.2.0 answers for `HTML` under Node, with domino as its parser.
const MARKDOWN: &str = "Release notes\n=============\n\nVerve now has a **document**, with _events_ and [docs](https://example.com/docs).\n\n*   React 18\n*   turndown `7`\n\n> Quoted & kept\n\n    fn main() {}\n    \n\nLast  \nline";

fn convert(build: &str, setup: &str) -> String {
    let mut page = Page::new(Options::default());
    page.eval(setup);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/node_modules/turndown/lib")
        .join(build);
    let source = std::fs::read_to_string(&path).unwrap();
    let exports = page
        .runtime
        .load_module(&path, &source)
        .unwrap_or_else(|e| panic!("turndown failed to load: {e}"));
    page.runtime
        .context()
        .global_object()
        .set(
            verve::boa_engine::js_string!("TurndownService"),
            exports,
            false,
            page.runtime.context(),
        )
        .unwrap();
    let html = serde_json::to_string(HTML).unwrap();
    page.eval(&format!(
        "const service = new TurndownService({{ codeBlockStyle: 'indented' }}); service.remove('script'); service.turndown({html})"
    ))
}

#[test]
fn turndown_converts_html_to_markdown_through_dom_parser() {
    on_page(|| {
        assert_eq!(convert("turndown.cjs.js", ""), MARKDOWN);
    });
}

#[test]
fn turndown_converts_through_create_html_document_without_dom_parser() {
    on_page(|| {
        // The browser build, on a window without `DOMParser`: its other way in.
        assert_eq!(
            convert(
                "turndown.browser.cjs.js",
                "delete globalThis.DOMParser; typeof DOMParser"
            ),
            MARKDOWN
        );
    });
}

#[test]
fn turndown_converts_a_node_it_is_handed() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/node_modules/turndown/lib/turndown.cjs.js");
        let source = std::fs::read_to_string(&path).unwrap();
        let exports = page.runtime.load_module(&path, &source).unwrap();
        page.runtime
            .context()
            .global_object()
            .set(
                verve::boa_engine::js_string!("TurndownService"),
                exports,
                false,
                page.runtime.context(),
            )
            .unwrap();
        let out = page.eval(
            r#"
            const div = document.createElement('div');
            div.innerHTML = '<h2>Title</h2><p>A <a href="/x">link</a> and <s>gone</s></p><table><tr><td>1</td></tr></table>';
            new TurndownService({ headingStyle: 'atx' }).turndown(div)
            "#,
        );
        // turndown copies what it is handed: the element itself is as it was.
        assert_eq!(out, "## Title\n\nA [link](/x) and gone\n\n1");
        assert_eq!(page.eval("document.body.childNodes.length"), "0");
    });
}
