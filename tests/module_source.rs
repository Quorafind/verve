//! A host's own addresses as module addresses: `app://` and `https://` modules from a source
//! hook, relative imports against them, and the import map that rewrites specifiers first.

use std::collections::HashMap;
use std::rc::Rc;

use url::Url;
use verve::{ImportMap, Runtime};

/// A realm whose hook answers from `files`, as a host answers from what it has fetched.
fn realm(files: &[(&str, &str)]) -> Runtime {
    let files: HashMap<String, String> = files
        .iter()
        .map(|(url, text)| ((*url).to_owned(), (*text).to_owned()))
        .collect();
    let mut rt = Runtime::new();
    rt.set_module_source(Some(Rc::new(move |url: &Url| {
        files.get(url.as_str()).map(|text| Rc::from(text.as_str()))
    })));
    rt
}

fn import(rt: &mut Runtime, url: &str) {
    rt.import_module(url, None).expect("the import starts");
    rt.run_microtasks();
}

fn global(rt: &mut Runtime, name: &str) -> String {
    let value = rt.eval(&format!("String(globalThis.{name})")).unwrap();
    value
        .to_string(rt.context())
        .unwrap()
        .to_std_string_escaped()
}

#[test]
fn a_module_graph_loads_from_the_hosts_addresses() {
    let mut rt = realm(&[
        (
            "app://page/assets/main.js",
            "import { two } from './dep.js'; import { root } from '/root.js';
             globalThis.out = [two, root, import.meta.url].join();",
        ),
        ("app://page/assets/dep.js", "export const two = 2;"),
        ("app://page/root.js", "export const root = 'r';"),
    ]);
    import(&mut rt, "app://page/assets/main.js");
    assert_eq!(global(&mut rt, "out"), "2,r,app://page/assets/main.js");
}

#[test]
fn a_dynamic_import_resolves_against_the_importing_module() {
    let mut rt = realm(&[
        (
            "https://example.test/a/main.js",
            "import('./late.js').then(m => { globalThis.out = m.value });",
        ),
        (
            "https://example.test/a/late.js",
            "export const value = 'late';",
        ),
    ]);
    import(&mut rt, "https://example.test/a/main.js");
    assert_eq!(global(&mut rt, "out"), "late");
}

#[test]
fn an_address_the_host_has_nothing_for_is_not_found() {
    let mut rt = realm(&[(
        "app://page/main.js",
        "import('./gone.js').catch(e => { globalThis.out = e.code });",
    )]);
    import(&mut rt, "app://page/main.js");
    assert_eq!(global(&mut rt, "out"), "ERR_MODULE_NOT_FOUND");
    rt.eval("import('app://page/other.js').catch(e => { globalThis.err = e.code })")
        .unwrap();
    assert_eq!(global(&mut rt, "err"), "ERR_MODULE_NOT_FOUND");
}

#[test]
fn an_import_map_rewrites_bare_specifiers_and_prefixes() {
    let mut rt = realm(&[
        (
            "app://page/main.js",
            "import a from 'lib'; import b from 'pkg/inner.js';
             globalThis.out = [a, b].join();",
        ),
        ("app://page/vendor/lib.js", "export default 'lib';"),
        ("app://page/vendor/pkg/inner.js", "export default 'inner';"),
    ]);
    let base = Url::parse("app://page/").unwrap();
    let map = ImportMap::parse(
        r#"{"imports": {"lib": "./vendor/lib.js", "pkg/": "./vendor/pkg/"}}"#,
        &base,
    )
    .unwrap();
    rt.set_import_map(Some(map));
    import(&mut rt, "app://page/main.js");
    assert_eq!(global(&mut rt, "out"), "lib,inner");
}

#[test]
fn a_scope_beats_the_top_level_for_the_modules_inside_it() {
    let base = Url::parse("app://page/").unwrap();
    let map = ImportMap::parse(
        r#"{"imports": {"x": "/one.js"}, "scopes": {"/deep/": {"x": "/two.js"}}}"#,
        &base,
    )
    .unwrap();
    let shallow = Url::parse("app://page/main.js").unwrap();
    let deep = Url::parse("app://page/deep/main.js").unwrap();
    assert_eq!(
        map.resolve("x", &shallow).unwrap().unwrap().path(),
        "/one.js"
    );
    assert_eq!(map.resolve("x", &deep).unwrap().unwrap().path(), "/two.js");
    assert!(map.resolve("y", &deep).unwrap().is_none());
}

#[test]
fn a_map_that_is_not_one_is_refused() {
    let base = Url::parse("app://page/").unwrap();
    assert!(ImportMap::parse("[]", &base).is_err());
    assert!(ImportMap::parse("not json", &base).is_err());
    assert!(ImportMap::parse(r#"{"imports": []}"#, &base).is_err());
}

#[test]
fn an_address_that_is_no_string_blocks_only_its_specifier() {
    let base = Url::parse("app://page/").unwrap();
    let map = ImportMap::parse(r#"{"imports": {"a": 1, "b": "/b.js"}}"#, &base).unwrap();
    let referrer = Url::parse("app://page/main.js").unwrap();
    assert_eq!(map.warnings().len(), 1);
    assert!(map.resolve("a", &referrer).is_err());
    assert_eq!(
        map.resolve("b", &referrer).unwrap().unwrap().as_str(),
        "app://page/b.js"
    );
}

#[test]
fn a_prefix_may_not_be_backed_out_of_and_stays_off_urls_that_are_not_special() {
    let base = Url::parse("app://page/").unwrap();
    let text = r#"{"imports": {"pkg/": "/pkg/", "data:text/": "/d/"}}"#;
    let map = ImportMap::parse(text, &base).unwrap();
    let referrer = Url::parse("app://page/main.js").unwrap();
    assert_eq!(
        map.resolve("pkg/x.js", &referrer)
            .unwrap()
            .unwrap()
            .as_str(),
        "app://page/pkg/x.js"
    );
    assert!(map.resolve("pkg/../secret.js", &referrer).is_err());
    // `data:` is no special scheme: only an exact entry maps it.
    assert_eq!(map.resolve("data:text/javascript,1", &referrer), Ok(None));
}

#[test]
fn a_prefix_must_map_to_a_prefix() {
    let base = Url::parse("app://page/").unwrap();
    let map = ImportMap::parse(r#"{"imports": {"a/": "/b.js"}}"#, &base).unwrap();
    let referrer = Url::parse("app://page/main.js").unwrap();
    assert!(map.resolve("a/x", &referrer).is_err());
}

/// What importing `specifier` from a module at `entry` says, in a realm with or without Node.
fn import_error(entry: &str, specifier: &str, web: bool) -> String {
    let mut rt = Runtime::new();
    rt.set_web_modules(web);
    rt.set_module_source(Some(Rc::new(|_: &Url| None)));
    let source = format!(
        "import('{specifier}').then(m => {{ globalThis.out = 'ok ' + typeof (m.default || m).readFileSync }}, e => {{ globalThis.out = e.name + ': ' + e.message }});"
    );
    // A folder of this call's own: the tests of this file run side by side.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("verve-web-modules-{}-{call}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = if entry == "file" {
        let path = dir.join("entry.mjs");
        std::fs::write(&path, &source).unwrap();
        Url::from_file_path(&path).unwrap().to_string()
    } else {
        format!("data:text/javascript,{}", source.replace(' ', "%20"))
    };
    rt.import_module(&url, None).expect("the import starts");
    rt.run_microtasks();
    let out = global(&mut rt, "out");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_page_without_node_resolves_the_webs_way_whoever_imports() {
    for entry in ["file", "data"] {
        assert_eq!(
            import_error(entry, "fs", true),
            "TypeError: Failed to resolve module specifier 'fs'",
            "{entry}"
        );
        assert_eq!(
            import_error(entry, "x", true),
            "TypeError: Failed to resolve module specifier 'x'",
            "{entry}"
        );
        assert_eq!(
            import_error(entry, "node:fs", true),
            "TypeError: Failed to fetch dynamically imported module: node:fs",
            "{entry}"
        );
    }
}

#[test]
fn the_runtime_without_the_flag_still_resolves_node_builtins() {
    assert_eq!(import_error("file", "fs", false), "ok function");
    assert_eq!(import_error("data", "node:fs", false), "ok function");
}

/// What a module at `referrer` gets from importing the local file `specifier` in a host's realm.
fn file_import(referrer_url: &str, static_import: bool, web: bool) -> String {
    let dir = std::env::temp_dir().join(format!("verve-file-refusal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let json = dir.join("x.json");
    std::fs::write(&json, r#"{"a":1}"#).unwrap();
    let target = Url::from_file_path(&json).unwrap().to_string();
    let entry = if static_import {
        format!("import x from '{target}' with {{ type: 'json' }}; globalThis.out = 'read';")
    } else {
        format!(
            "import('{target}', {{ with: {{ type: 'json' }} }}).then(() => {{ globalThis.out = 'read' }}, e => {{ globalThis.out = e.name + ': ' + e.message.replace(/^(.*module: ).*$/, '$1') }});"
        )
    };
    let mut rt = realm(&[(referrer_url, &entry)]);
    rt.set_web_modules(web);
    rt.set_local_modules(false);
    let _ = rt.import_module(referrer_url, None);
    rt.run_microtasks();
    let out = global(&mut rt, "out");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_hosted_page_may_not_import_a_local_file() {
    for web in [false, true] {
        for referrer in ["https://example.test/main.js", "app://page/main.js"] {
            assert_eq!(
                file_import(referrer, false, web),
                "TypeError: Failed to fetch dynamically imported module: ",
                "{referrer} {web}"
            );
            assert_eq!(
                file_import(referrer, true, web),
                "undefined",
                "{referrer} {web}"
            );
        }
    }
}

#[test]
fn a_second_map_adds_to_the_first_and_a_scope_ignores_the_fragment() {
    let base = Url::parse("app://page/").unwrap();
    let mut first = ImportMap::parse(r#"{"imports": {"a": "/a1.js"}}"#, &base).unwrap();
    let second = r#"{"imports": {"a": "/a2.js", "b": "/b.js"}, "scopes": {"app://page/doc.html": {"c": "/c.js"}}}"#;
    first.merge(ImportMap::parse(second, &base).unwrap());
    let referrer = Url::parse("app://page/main.js").unwrap();
    let url = |name: &str, from: &Url| first.resolve(name, from).unwrap().map(|u| u.to_string());
    assert_eq!(url("a", &referrer).as_deref(), Some("app://page/a1.js"));
    assert_eq!(url("b", &referrer).as_deref(), Some("app://page/b.js"));
    let inline = Url::parse("app://page/doc.html#module-7").unwrap();
    assert_eq!(url("c", &inline).as_deref(), Some("app://page/c.js"));
}

/// A temp folder of this call's own, with a module in it that sets `globalThis.ran`.
fn local_module(name: &str) -> (std::path::PathBuf, String) {
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("verve-local-{name}-{}-{call}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.mjs");
    std::fs::write(&path, "globalThis.ran = 'yes'; export default 1;").unwrap();
    let url = Url::from_file_path(&path).unwrap().to_string();
    (dir, url)
}

/// The ways a `file:` module is asked for, each in a realm of its own: whether it ran.
fn ran_by_way(allowed: bool) -> Vec<(&'static str, String)> {
    let mut seen = Vec::new();
    for way in ["entry", "static", "dynamic", "map"] {
        let (dir, target) = local_module(way);
        let referrer = "app://page/main.js";
        let main = match way {
            "static" => format!("import '{target}';"),
            "dynamic" => format!("import('{target}').catch(() => {{}});"),
            "map" => "import('local').catch(() => {});".to_owned(),
            _ => String::new(),
        };
        let mut rt = realm(&[(referrer, &main)]);
        rt.set_local_modules(allowed);
        if way == "map" {
            let base = Url::parse("app://page/").unwrap();
            let map = format!(r#"{{"imports": {{"local": "{target}"}}}}"#);
            rt.set_import_map(Some(ImportMap::parse(&map, &base).unwrap()));
        }
        // With no parent, the importer is the working folder's own: no referrer to go by.
        let _ = rt.import_module(if way == "entry" { &target } else { referrer }, None);
        rt.run_microtasks();
        seen.push((way, global(&mut rt, "ran")));
        let _ = std::fs::remove_dir_all(&dir);
    }
    seen
}

#[test]
fn a_page_refused_local_files_loads_none_whoever_imports() {
    for (way, ran) in ran_by_way(false) {
        assert_eq!(ran, "undefined", "{way}");
    }
}

#[test]
fn a_page_allowed_local_files_loads_them_as_before() {
    for (way, ran) in ran_by_way(true) {
        assert_eq!(ran, "yes", "{way}");
    }
}

#[test]
fn an_entry_refused_with_no_parent_is_the_browsers_type_error() {
    let (dir, target) = local_module("error");
    let mut rt = Runtime::new();
    rt.set_local_modules(false);
    let promise = rt.import_module(&target, None).expect("the import starts");
    rt.run_microtasks();
    match promise.state() {
        verve::boa_engine::builtins::promise::PromiseState::Rejected(error) => {
            let text = error
                .to_string(rt.context())
                .unwrap()
                .to_std_string_escaped();
            assert!(
                text.contains("Failed to fetch dynamically imported module"),
                "{text}"
            );
        }
        _ => panic!("the entry was not refused"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
