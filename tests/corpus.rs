//! Real community plugins, loaded: each of these needed something this runtime now has — Node's
//! `stream`, `crypto`, `zlib`, `buffer`, `constants`, `querystring`, `path/posix`; the web's
//! `TransformStream`, `EventTarget`, `Event`, `navigator`, `Intl.Segmenter`. The host's own modules
//! (`obsidian`, `@codemirror/*`, `electron`) and the window's globals are stood in for by a
//! `Proxy` that answers every name with another.
//!
//! The plugins are read from the corpus beside this repository
//! (`../obsidian-corpus/plugins`, or `RUNTIME_X_CORPUS`); without it, there is nothing to load and
//! the test says so and passes. `cargo test --test corpus -- --ignored`: the bundles are large.

use std::path::PathBuf;

use runtime_x::RuntimeX;

/// A module whose every property is a class, whose every property is one too.
const STUB: &str = r#"
(function () {
    const cache = new Map();
    const KNOWN = ["AbstractInputSuggest","Annotation","BaseComponent","ButtonComponent","ColorComponent","Compartment","Component","ConfirmationModal","Decoration","DropdownComponent","EditableFileView","EditorSelection","EditorState","EditorSuggest","EditorView","Events","ExtraButtonComponent","FileSystemAdapter","FileView","FuzzySuggestModal","HoverPopover","ItemView","Keymap","LRLanguage","LRParser","LanguageSupport","MarkdownPreviewRenderer","MarkdownRenderChild","MarkdownRenderer","MarkdownView","Menu","Modal","Notice","Platform","Plugin","PluginSettingTab","Prec","RangeSetBuilder","Scope","SearchComponent","Setting","SettingGroup","SettingPage","StateEffect","StateField","StreamLanguage","SuggestModal","TFile","TFolder","TextAreaComponent","TextComponent","TextFileView","ToggleComponent","Transaction","Vault","View","ViewPlugin","WidgetType","WorkspaceLeaf","addIcon","apiVersion","autocompletion","bracketMatching","closeBrackets","closeBracketsKeymap","completionKeymap","continuedIndent","crosshairCursor","debounce","defaultHighlightStyle","defaultKeymap","drawSelection","dropCursor","editorEditorField","editorInfoField","editorLivePreviewField","ensureSyntaxTree","foldGutter","foldInside","foldKeymap","foldNodeProp","getAllTags","getIcon","getIconIds","getLanguage","getLinkpath","highlightActiveLine","highlightActiveLineGutter","highlightSelectionMatches","highlightSpecialChars","history","historyKeymap","htmlToMarkdown","indentNodeProp","indentOnInput","indentWithTab","iterateRefs","keymap","lineNumbers","lintGutter","lintKeymap","linter","loadMermaid","loadPdfJs","moment","normalizePath","parseFrontMatterAliases","parseFrontMatterStringArray","parseFrontMatterTags","parseYaml","prepareFuzzySearch","prepareSimpleSearch","rectangularSelection","request","requestUrl","requireApiVersion","sanitizeHTMLToDom","search","searchKeymap","setIcon","setTooltip","stringifyYaml","styleTags","syntaxHighlighting","syntaxTree","tags","tokenClassNodeProp","undoDepth"];
    function stub(name) {
        if (cache.has(name)) return cache.get(name);
        const target = function () {};
        Object.defineProperty(target, 'name', { value: name.split('.').pop() });
        const proxy = new Proxy(target, {
            get(t, key) {
                if (key === 'prototype') return t.prototype;
                if (typeof key === 'symbol') {
                    if (key === Symbol.toPrimitive) return () => '';
                    return t[key];
                }
                if (key === 'then') return undefined;
                if (key in t && key !== 'name' && key !== 'length') return t[key];
                if (key === 'name') return t.name;
                return stub(name + '.' + key);
            },
            ownKeys(t) { return Reflect.ownKeys(t).concat(KNOWN.filter(k => !(k in t))); },
            getOwnPropertyDescriptor(t, key) {
                const own = Reflect.getOwnPropertyDescriptor(t, key);
                if (own) return own;
                if (KNOWN.includes(key)) return { value: stub(name + '.' + key), writable: true, enumerable: true, configurable: true };
                return undefined;
            },
            apply() { return stub(name + '()'); },
            construct(t) { return Object.create(t.prototype); },
        });
        cache.set(name, proxy);
        return proxy;
    }
    return stub;
})()
"#;

/// The window's own globals, which the host's DOM gives a plugin.
const WINDOW: &str = "for (const k of ['document','activeDocument','createDiv','createEl','createSpan','createFragment','createSvg','i18next','moment','app','localStorage','sessionStorage','getComputedStyle','matchMedia','requestAnimationFrame','cancelAnimationFrame','HTMLElement','Element','Node','MutationObserver','ResizeObserver','IntersectionObserver','XMLHttpRequest','fetch','Image','DOMParser','CSS','location','history','screen']) if (!(k in globalThis)) globalThis[k] = __stub(k); globalThis.window = globalThis.self = globalThis.activeWindow = globalThis; globalThis.addEventListener = () => {}; globalThis.removeEventListener = () => {};";

const PLUGINS: &[&str] = &[
    "execute-code",
    "obsidian-advanced-slides",
    "obsidian-citation-plugin",
    "obsidian-git",
    "obsidian-livesync",
    "readwise-official",
    "obsidian-importer",
    "novel-word-count",
    "obsidian-pandoc",
    "webpage-html-export",
    "obsidian-markmind",
    "drawio-obsidian",
    "obsidian-memos",
    "obsidian-tagfolder",
];

fn corpus() -> Option<PathBuf> {
    let dir = std::env::var_os("RUNTIME_X_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../obsidian-corpus/plugins")
        });
    dir.is_dir().then_some(dir)
}

/// Loads one plugin's `main.js` as the Obsidian host would, the host's modules stood in for.
fn load(main: PathBuf) -> Result<(), String> {
    let source = std::fs::read_to_string(&main).map_err(|e| e.to_string())?;
    let mut rt = RuntimeX::new();
    let stub = rt.eval(STUB)?;
    rt.context()
        .register_global_property(
            boa_engine::js_string!("__stub"),
            stub.clone(),
            boa_engine::property::Attribute::all(),
        )
        .unwrap();
    rt.eval(WINDOW)?;
    let require = rt.global_require();
    rt.context()
        .register_global_property(
            boa_engine::js_string!("require"),
            require,
            boa_engine::property::Attribute::all(),
        )
        .unwrap();
    for name in [
        "obsidian",
        "electron",
        "@codemirror/state",
        "@codemirror/view",
        "@codemirror/language",
        "@codemirror/commands",
        "@codemirror/search",
        "@codemirror/autocomplete",
        "@codemirror/lint",
        "@lezer/common",
        "@lezer/highlight",
        "@lezer/lr",
    ] {
        let module = rt.call(
            &stub,
            &boa_engine::JsValue::undefined(),
            &[boa_engine::js_string!(name).into()],
        )?;
        rt.define_builtin(name, module);
    }
    rt.load_module(&main, &source)?;
    Ok(())
}

#[test]
#[ignore = "loads large bundles from the corpus beside the repository"]
fn community_plugins_load() {
    let Some(dir) = corpus() else {
        eprintln!("no plugin corpus beside the repository: nothing to load");
        return;
    };
    let mut failed = Vec::new();
    for plugin in PLUGINS {
        let main = dir.join(plugin).join("main.js");
        if !main.is_file() {
            continue;
        }
        // The bundles are parsed on a thread with room for them, as the host runs them.
        let outcome = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(move || load(main))
            .unwrap()
            .join()
            .unwrap_or_else(|_| Err("panicked".into()));
        if let Err(error) = outcome {
            failed.push(format!("{plugin}: {error}"));
        }
    }
    assert!(
        failed.is_empty(),
        "plugins that did not load:
{}",
        failed.join(
            "
"
        )
    );
}
