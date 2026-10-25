//! DeepSeek Harness's client halves on the document: `@deepseek-ai/dsh-client-ui-primitives`'
//! components, and the web-search provider's settings page (`dsh-client-ui-settings-web-search`'s
//! `lib/client.js`) loaded through `window.__ModuleLoader__` and rendered with stand-ins for
//! the context DSH's web client injects — all from npm, unmodified.
//!
//! Ignored by default: the packages (about 30 MB with what the primitives import — shiki,
//! katex, simple-icons…) are installed with `verve::npm` the first time, into `VERVE_DOM_DSH`
//! or, without it, the test target's own scratch directory. Run with
//! `cargo test -p verve-dom --test dsh -- --ignored`.

mod common;

use std::path::{Path, PathBuf};

use common::{Page, on_page};
use verve_dom::{Options, Rect};

const PRIMITIVES: &str = "0.1.7-alpha.1";

/// The project the packages are installed in, installed if it is not yet.
fn prepared() -> PathBuf {
    let dir = std::env::var_os("VERVE_DOM_DSH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("dsh"));
    // The primitives and the page, and what the packages import but leave to the web client
    // to bundle: their own devDependencies, at the versions they name.
    let package = serde_json::json!({
            "name": "verve-dom-dsh-check",
            "private": true,
            "dependencies": {
                "@deepseek-ai/dsh-client-ui-primitives": PRIMITIVES,
                "@deepseek-ai/dsh-client-ui-settings-web-search": "0.1.7-alpha.1",
                "@deepseek-ai/dsh-client-store": "0.1.7-alpha.1",
                "@deepseek-ai/dsh-util-workspace-path": "0.1.7-alpha.1",
                "@deepseek-ai/cordis": "^4.0.3",
                "react": "18.3.1",
                "react-dom": "18.3.1",
                "clsx": "^2.0.0",
                "diff": "^9.0.0",
                "anser": "^2.3.5",
                "katex": "^0.16.47",
                "micromark-core-commonmark": "^2.0.3",
                "micromark-util-character": "^2.1.1",
                "micromark-util-classify-character": "^2.0.1",
                "micromark-util-symbol": "^2.0.1",
                "micromark-util-types": "^2.0.2",
                "micromark-factory-space": "^2.0.1",
                "micromark-extension-math": "^3.1.0",
                "micromark-extension-gfm": "^3.0.0",
                "micromark-util-sanitize-uri": "^2.0.1",
                "mdast-util-from-markdown": "^2.0.3",
                "mdast-util-gfm": "^3.1.0",
                "mdast-util-math": "^3.0.0",
                "shiki": "^4.3.1",
                "@shikijs/langs": "^4.3.1",
                "simple-icons": "16.31.0",
                "zustand": "~4.4.7",
                "immer": "^10.1.1"
            }
        });
    let manifest = serde_json::to_string_pretty(&package).unwrap();
    let installed = std::fs::read_to_string(dir.join("package.json")).ok();
    let marker = dir.join("node_modules/@deepseek-ai/dsh-client-ui-primitives/package.json");
    if !marker.exists() || installed.as_deref() != Some(manifest.as_str()) {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), &manifest).unwrap();
        let options = verve::npm::InstallOptions {
            cache_dir: Some(dir.join(".tarballs")),
            ..verve::npm::InstallOptions::default()
        };
        verve::npm::install(&dir, &options).unwrap_or_else(|e| panic!("npm install failed: {e}"));
    }
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dsh-page.mjs"),
        dir.join("dsh-page.mjs"),
    )
    .unwrap();
    dir
}

/// CSS modules as DSH's bundler leaves them to the page: the sheet put in the document, the
/// module's class names as they are written.
const CSS_MODULES: &str = r#"
globalThis.require = __require;
process.env.NODE_ENV = 'production';
require('module').registerHooks({
    load(url, context, nextLoad) {
        if (!url.endsWith('.css')) return nextLoad(url, context);
        const text = require('fs').readFileSync(require('url').fileURLToPath(url), 'utf8');
        return {
            format: 'module',
            shortCircuit: true,
            source: 'const style = document.createElement("style"); style.textContent = ' + JSON.stringify(text) +
                '; document.head.appendChild(style); export default new Proxy({}, { get: (_, name) => typeof name === "string" ? name : undefined });',
        };
    },
});
"#;

fn page(dir: &Path) -> Page {
    let mut page = Page::new(Options::default());
    let require = page.runtime.global_require();
    page.runtime.context().global_object().set(
        verve::boa_engine::js_string!("__require"),
        require,
        false,
        page.runtime.context(),
    ).unwrap();
    page.eval(CSS_MODULES);
    let started = std::time::Instant::now();
    let promise = page
        .runtime
        .import_module("./dsh-page.mjs", Some(&dir.join("page.js")))
        .unwrap_or_else(|e| panic!("the page failed: {e}"));
    page.settle();
    if let verve::boa_engine::builtins::promise::PromiseState::Rejected(error) = promise.state() {
        panic!("the page failed: {}\n{}", verve::print(&error), page.console());
    }
    eprintln!("the primitives loaded in {:?}", started.elapsed());
    page
}

#[test]
#[ignore = "installs DSH's packages from npm"]
fn the_primitives_render_and_answer_presses() {
    on_page(|| {
        let dir = prepared();
        let mut page = page(&dir);
        page.eval(
            r#"
            const { Button, SettingsValueField } = dsh.primitives;
            const h = dsh.h;
            function Demo() {
                const [presses, setPresses] = React.useState(0);
                const [text, setText] = React.useState('');
                return h('div', { id: 'demo' },
                    h(Button, { id: 'save', variant: 'primary', onClick: () => setPresses(p => p + 1) }, 'Pressed ' + presses),
                    h(SettingsValueField, { id: 'endpoint', label: 'Endpoint', hint: 'Leave blank', text, overridden: text !== '', overriddenLabel: 'Overridden', resetLabel: 'Reset', onEdit: setText, onReset: () => setText('') }));
            }
            const React = dsh.React;
            dsh.render(h(Demo));
            "#,
        );
        page.settle();
        let button = page.find("#save");
        assert_eq!(page.document.dom().text_content(button), "Pressed 0");
        // The Button's CSS module, in the document's cascade.
        assert!(page.eval("document.querySelectorAll('style').length").parse::<u32>().unwrap() > 0);
        assert!(page.eval("document.getElementById('save').className").contains("button"), "{}", page.eval("document.getElementById('save').outerHTML"));
        page.document.press(&mut page.runtime, button);
        page.settle();
        assert_eq!(page.document.dom().text_content(button), "Pressed 1");
        // The settings field: its label for its input, typed into.
        assert_eq!(page.eval("document.querySelector('label[for=endpoint]').textContent"), "Endpoint");
        let field = page.find("#endpoint");
        page.document.press(&mut page.runtime, field);
        page.document.type_text(&mut page.runtime, "https://x");
        page.settle();
        assert_eq!(page.eval("document.getElementById('endpoint').value"), "https://x");
        assert!(page.eval("document.getElementById('demo').textContent").contains("Overridden"));
        let errors: Vec<String> = page.console().lines().filter(|l| l.starts_with("Error")).map(str::to_owned).collect();
        assert!(errors.is_empty(), "{errors:?}");
    });
}

#[test]
#[ignore = "installs DSH's packages from npm"]
fn the_web_search_settings_page_runs_through_the_module_loader() {
    on_page(|| {
        let dir = prepared();
        let mut page = page(&dir);
        let pages = page.eval("(async () => { globalThis.pages = await dsh.loadClient(); })()");
        let _ = pages;
        page.settle();
        assert_eq!(page.eval("pages"), "web-search");
        // Its one-liner, as the Plugins page lists it, and the form itself.
        page.eval("dsh.renderPage('web-search', 'summary'); dsh.renderPage('web-search', 'detail')");
        page.settle();
        assert!(page.eval("document.body.textContent").contains("Set up the DeepSeek search provider."));
        assert_eq!(
            page.eval("[...document.querySelectorAll('label')].map(l => l.textContent).join(' | ')"),
            "API key | Endpoint | Max searches per request"
        );
        assert_eq!(page.eval("document.getElementById('plugin-config-web-search-max-uses').value"), "5");
        assert_eq!(page.eval("document.querySelector('button[disabled]') !== null"), "true");
        // Staging an endpoint: the form is dirty, and Save writes it.
        let endpoint = page.find("#plugin-config-web-search-endpoint");
        page.document.set_layout(&mut page.runtime, [(endpoint, Rect::new(0., 0., 200., 20.))]);
        page.document.press(&mut page.runtime, endpoint);
        page.document.type_text(&mut page.runtime, "https://search.test");
        page.settle();
        assert_eq!(page.eval("document.getElementById('plugin-config-web-search-endpoint').value"), "https://search.test");
        let save = page.eval("(() => { const b = [...document.querySelectorAll('button')].find(b => b.textContent === 'Save'); b.id = 'save'; return b.disabled })()");
        assert_eq!(save, "false");
        let save = page.find("#save");
        page.document.press(&mut page.runtime, save);
        page.settle();
        assert!(
            page.eval("log.join('\\n')").contains(r#"mutate [{"op":"set","path":["baseURL"],"value":"https://search.test"}] at 1"#),
            "{}",
            page.eval("log.join('\\n')")
        );
        // Written and read back: overridden now, with its reset.
        assert!(page.eval("document.body.textContent").contains("Overridden"));
        let errors: Vec<String> = page.console().lines().filter(|l| l.starts_with("Error")).map(str::to_owned).collect();
        assert!(errors.is_empty(), "{errors:?}");
    });
}
