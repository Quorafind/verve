//! A page loaded through a `Loader`: the order scripts run in, what blocks what, and the events
//! a page hears — against pages in `tests/fixtures/load/` whose order was observed in Chromium
//! (Electron 39.8.10, Chromium 142.0.7444.265, `loadFile` in a hidden window), which the
//! expected logs below are copied from.

mod common;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use common::Page;
use url::Url;
use verve_dom::{
    Destination, LoadError, LoadEvent, LoadRequest, LoadResult, Loader, Options, Ready,
};

/// Answers `app://page/<name>` from the fixture folder, or from `extra`, at once.
struct Fixtures {
    extra: HashMap<String, Vec<u8>>,
}

thread_local! {
    /// The documents that asked for something, per request, on this test's thread.
    static INITIATORS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

impl Loader for Fixtures {
    fn fetch(&self, request: LoadRequest, done: Box<dyn FnOnce(LoadResult)>) {
        if let Some(initiator) = &request.initiator {
            INITIATORS.with(|seen| seen.borrow_mut().push(initiator.to_string()));
        }
        let mut name = request.url.path().trim_start_matches('/').to_owned();
        let mut url = request.url.clone();
        // `go/x` is answered from `x`, as if it had redirected to `dir/x`.
        if let Some(target) = name.strip_prefix("go/").map(str::to_owned) {
            url = Url::parse(&format!("app://page/dir/{target}")).expect("a URL");
            name = target;
        }
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/load");
        let bytes = self
            .extra
            .get(&name)
            .or_else(|| self.extra.get(url.path().trim_start_matches('/')))
            .cloned()
            .or_else(|| std::fs::read(folder.join(&name)).ok());
        let mime = request.destination == Destination::Fetch;
        done(match bytes {
            Some(body) => LoadResult {
                status: 200,
                url,
                mime: mime.then(|| "application/json".to_owned()),
                headers: Vec::new(),
                body: Ok(body),
            },
            None => LoadResult {
                status: 404,
                url: request.url,
                mime: None,
                headers: Vec::new(),
                body: Err(LoadError {
                    code: -6,
                    description: "ERR_FILE_NOT_FOUND".into(),
                }),
            },
        });
    }

    fn handles(&self, scheme: &str) -> bool {
        scheme == "app"
    }
}

fn page(extra: &[(&str, &str)]) -> Page {
    let extra = extra
        .iter()
        .map(|(name, text)| ((*name).to_owned(), text.as_bytes().to_vec()))
        .collect();
    Page::new(Options {
        loader: Some(Box::new(Fixtures { extra })),
        ..Options::default()
    })
}

impl Page {
    /// Pumps the load until nothing is left.
    fn finish_loading(&mut self) {
        for _ in 0..200 {
            self.document.pump_loads(&mut self.runtime);
            if !self.document.is_loading() {
                return;
            }
        }
        panic!("still loading: {}", self.console());
    }

    fn initiators(&self) -> Vec<String> {
        INITIATORS.with(|seen| seen.borrow().clone())
    }

    fn log(&mut self) -> Vec<String> {
        let json = self.eval("JSON.stringify(window.__log)");
        serde_json::from_str(&json).expect("a log")
    }
}

/// Loads fixture `name` and answers what the page logged.
fn logged(name: &str) -> Vec<String> {
    let mut page = page(&[]);
    page.document
        .navigate_initial(&mut page.runtime, &format!("app://page/{name}"));
    page.finish_loading();
    page.log()
}

#[test]
fn scripts_run_in_the_order_chromium_runs_them() {
    assert_eq!(
        logged("order.html"),
        [
            "inline-head:loading",
            "sync-a:loading",
            "inline-body:loading",
            "sync-b:loading",
            "sees-after:true",
            "rsc:interactive",
            "defer-1:interactive",
            "defer-2:interactive",
            "DCL",
            "rsc:complete",
            "load:complete"
        ]
    );
}

#[test]
fn an_async_script_does_not_hold_the_parser_and_runs_before_load() {
    assert_eq!(
        logged("async.html"),
        ["inline", "async-1:interactive", "load"]
    );
}

#[test]
fn a_script_waits_for_the_sheet_before_it_and_the_sheets_load_comes_after_it() {
    assert_eq!(
        logged("styles.html"),
        ["script-sees:rgb(1, 2, 3)", "link-load"]
    );
}

#[test]
fn document_write_goes_in_where_the_parser_is() {
    assert_eq!(
        logged("write.html"),
        ["seen", "after-write", "next:written"]
    );
}

#[test]
fn current_script_is_the_running_classic_script() {
    assert_eq!(
        logged("current.html"),
        ["inline:one", "external:two", "after:null"]
    );
}

#[test]
fn scripts_put_in_the_page_run_as_chromium_runs_them() {
    assert_eq!(
        logged("dynamic.html"),
        [
            "before",
            "dynamic-inline",
            "after",
            "error:missing",
            "ordered-a:interactive",
            "ordered-b:interactive",
            "onload-b",
            "load"
        ]
    );
}

#[test]
fn a_script_that_throws_is_an_error_event_and_the_parse_goes_on() {
    assert_eq!(
        logged("onerror.html"),
        ["error-event:Uncaught Error: boom", "after-error"]
    );
}

#[test]
fn an_imported_sheet_comes_before_the_sheet_that_imports_it() {
    assert_eq!(logged("cascade.html"), ["p:rgb(255, 0, 0):rgb(0, 128, 0)"]);
}

#[test]
fn the_page_reports_its_stages_in_order() {
    let mut page = page(&[]);
    page.document
        .navigate_initial(&mut page.runtime, "app://page/order.html");
    page.finish_loading();
    assert_eq!(
        page.document.take_load_events(),
        [
            LoadEvent::Started,
            LoadEvent::Committed("app://page/order.html".into()),
            LoadEvent::FirstPaintable,
            LoadEvent::Title("order".into()),
            LoadEvent::DomContentLoaded,
            LoadEvent::Loaded,
        ]
    );
    assert_eq!(page.eval("document.readyState"), "complete");
}

#[test]
fn a_page_that_cannot_be_fetched_fails_with_the_loaders_code() {
    let mut page = page(&[]);
    page.document
        .navigate_initial(&mut page.runtime, "app://page/nowhere.html");
    page.finish_loading();
    assert_eq!(
        page.document.take_load_events(),
        [
            LoadEvent::Started,
            LoadEvent::Failed {
                code: -6,
                description: "ERR_FILE_NOT_FOUND".into(),
                url: "app://page/nowhere.html".into()
            }
        ]
    );
}

#[test]
fn markup_in_hand_loads_without_a_loader() {
    let mut page = Page::new(Options::default());
    let html =
        "<title>t</title><p id=a>x</p><script>document.getElementById('a').id = 'b'</script>";
    page.document
        .load_html(&mut page.runtime, "https://example.test/", html);
    assert_eq!(page.eval("document.getElementById('b') !== null"), "true");
    assert_eq!(page.eval("document.readyState"), "complete");
    assert!(
        page.document
            .take_load_events()
            .contains(&LoadEvent::Loaded)
    );
}

#[test]
fn a_page_without_a_loader_is_complete_and_loads_nothing() {
    let mut page = Page::new(Options::default());
    assert_eq!(page.eval("document.readyState"), "complete");
    page.eval("document.body.innerHTML = '<script>window.ran = 1</script>'; var s = document.createElement('script'); s.textContent = 'window.ran = 2'; document.body.appendChild(s)");
    assert_eq!(page.eval("typeof window.ran"), "undefined");
    assert!(!page.document.is_loading());
}

#[test]
fn a_page_in_the_document_never_runs_what_innerhtml_made() {
    let mut page = page(&[]);
    page.document
        .load_html(&mut page.runtime, "app://page/x.html", "<div id=d></div>");
    page.eval("document.getElementById('d').innerHTML = '<script>window.ran = 1</script>'");
    page.finish_loading();
    assert_eq!(page.eval("typeof window.ran"), "undefined");
}

#[test]
fn the_title_is_reported_once_the_head_is_read() {
    let mut page = page(&[]);
    let html = "<title> A  page </title><p>x";
    page.document
        .load_html(&mut page.runtime, "app://page/dir/p.html", html);
    page.finish_loading();
    let events = page.document.take_load_events();
    assert!(
        events.contains(&LoadEvent::Title("A page".into())),
        "{events:?}"
    );
}

#[test]
fn a_base_element_moves_what_relative_addresses_resolve_against() {
    let mut page = page(&[("sub/a.js", "window.__log = ['from-base']")]);
    let html = "<base href='sub/'><script src='a.js'></script>";
    page.document
        .load_html(&mut page.runtime, "app://page/p.html", html);
    page.finish_loading();
    assert_eq!(page.log(), ["from-base"]);
    assert_eq!(page.eval("document.baseURI"), "app://page/sub/");
}

#[test]
fn what_the_loader_does_not_claim_goes_through_the_realms_own_fetch() {
    let mut page = page(&[]);
    let html = "<script>window.__log = [];</script>
        <script src='data:text/javascript,__log.push(%22from-data%22)'></script>
        <script>__log.push('after')</script>";
    page.document
        .load_html(&mut page.runtime, "app://page/d.html", html);
    page.finish_loading();
    assert_eq!(page.log(), ["from-data", "after"]);
}

#[test]
fn window_open_asks_the_embedder_with_its_name_and_features() {
    let mut page = page(&[]);
    page.document
        .load_html(&mut page.runtime, "https://example.test/", "<p>x");
    page.eval("window.open('/other', 'side', 'width=300')");
    assert_eq!(
        page.document.take_requests(),
        [verve_dom::Request::Open {
            url: "https://example.test/other".into(),
            name: "side".into(),
            features: "width=300".into()
        }]
    );
}

#[test]
fn the_ready_state_names() {
    let names = [
        (Ready::Loading, "loading"),
        (Ready::Interactive, "interactive"),
        (Ready::Complete, "complete"),
    ];
    for (ready, name) in names {
        assert_eq!(ready.name(), name);
    }
    assert_eq!(Ready::default(), Ready::Complete);
}

type Waiting = Vec<(LoadRequest, Box<dyn FnOnce(LoadResult)>)>;

/// A loader that holds every request until the test answers them, in the order it likes.
#[derive(Clone, Default)]
struct Held {
    files: Rc<HashMap<String, String>>,
    queue: Rc<RefCell<Waiting>>,
}

impl Loader for Held {
    fn fetch(&self, request: LoadRequest, done: Box<dyn FnOnce(LoadResult)>) {
        self.queue.borrow_mut().push((request, done));
    }

    fn handles(&self, scheme: &str) -> bool {
        scheme == "app"
    }
}

impl Held {
    /// Answers what is held, last asked first. Answers how many.
    fn release_reversed(&self) -> usize {
        let held: Vec<_> = self.queue.borrow_mut().drain(..).collect();
        self.answer(held.into_iter().rev().collect())
    }

    /// Answers the held request for `name` alone, if there is one.
    fn release(&self, name: &str) -> usize {
        let mut queue = self.queue.borrow_mut();
        let at = queue
            .iter()
            .position(|(request, _)| request.url.path().trim_start_matches('/') == name);
        let held: Vec<_> = at.map(|at| queue.remove(at)).into_iter().collect();
        drop(queue);
        self.answer(held)
    }

    fn answer(&self, held: Waiting) -> usize {
        let count = held.len();
        for (request, done) in held {
            let name = request.url.path().trim_start_matches('/').to_owned();
            let body = self.files.get(&name).map(|text| text.as_bytes().to_vec());
            done(LoadResult {
                status: 200,
                url: request.url,
                mime: None,
                headers: Vec::new(),
                body: body.ok_or(LoadError {
                    code: -6,
                    description: "ERR_FILE_NOT_FOUND".into(),
                }),
            });
        }
        count
    }
}

fn held_page(files: &[(&str, &str)], html: &str) -> (Page, Held) {
    let files = files
        .iter()
        .map(|(n, t)| ((*n).to_owned(), (*t).to_owned()))
        .collect();
    let held = Held {
        files: Rc::new(files),
        ..Held::default()
    };
    let mut page = Page::new(Options {
        loader: Some(Box::new(held.clone())),
        ..Options::default()
    });
    page.document
        .load_html(&mut page.runtime, "app://page/h.html", html);
    (page, held)
}

/// Pumps and answers, last asked first, until the page is loaded.
fn settle_reversed(page: &mut Page, held: &Held) {
    for _ in 0..50 {
        page.document.pump_loads(&mut page.runtime);
        if held.release_reversed() == 0 && !page.document.is_loading() {
            return;
        }
    }
    panic!("still loading: {}", page.console());
}

#[test]
fn defer_scripts_keep_their_order_when_they_arrive_out_of_order() {
    let files = [
        ("a.js", "(window.__log ||= []).push('a')"),
        ("b.js", "(window.__log ||= []).push('b')"),
        ("c.js", "(window.__log ||= []).push('c')"),
    ];
    let html = "<script defer src=a.js></script><script defer src=b.js></script><script defer src=c.js></script>";
    let (mut page, held) = held_page(&files, html);
    settle_reversed(&mut page, &held);
    assert_eq!(page.log(), ["a", "b", "c"]);
}

#[test]
fn what_is_inside_a_template_neither_runs_nor_loads() {
    let mut page = page(&[]);
    let html = "<script>window.__log = [];</script><template><script>__log.push('ran')</script>
        <link rel=stylesheet href=s.css><img src=dot.png onerror=\"__log.push('img')\"></template><p>x";
    page.document
        .load_html(&mut page.runtime, "app://page/t.html", html);
    page.finish_loading();
    page.settle_for(10.);
    assert_eq!(page.log(), Vec::<String>::new());
    assert!(page.initiators().is_empty(), "{:?}", page.initiators());
}

/// Loads `html` as a page at `app://page/p.html` and finishes loading it.
fn loaded(extra: &[(&str, &str)], html: &str) -> Page {
    let mut page = page(extra);
    page.document
        .load_html(&mut page.runtime, "app://page/p.html", html);
    page.finish_loading();
    page
}

const LOG: &str = "<script>window.__log = [];</script>";

fn body_order(page: &mut Page) -> String {
    page.eval("[...document.body.children].map(e => e.id || e.tagName).join()")
}

#[test]
fn two_writes_join_before_they_are_read() {
    let html = format!(
        "{LOG}<script>document.write('<div class=\"'); document.write('a\">x</div>');</script>"
    );
    let mut page = loaded(&[], &html);
    assert_eq!(
        page.eval("document.querySelector('div.a').textContent"),
        "x"
    );
}

#[test]
fn a_write_after_a_script_that_blocks_waits_for_it() {
    let files = [("a.js", "__log.push('a')")];
    let html = format!(
        "{LOG}<script>document.write('<script src=a.js><\\/script><p id=one></p>'); \
         document.write('<p id=two></p>'); __log.push('writer-done');</script>\
         <script>__log.push('after')</script>"
    );
    let mut page = loaded(&files, &html);
    assert_eq!(page.log(), ["writer-done", "a", "after"]);
    let order = body_order(&mut page);
    assert!(order.contains("one,two"), "{order}");
}

#[test]
fn a_write_from_a_script_inside_a_write_goes_after_that_script() {
    let html = format!(
        "{LOG}<script>document.write('<script>document.write(\"<i id=in></i>\")<\\/script><b id=out></b>');</script>"
    );
    let mut page = loaded(&[], &html);
    let order = body_order(&mut page);
    assert!(order.contains("in,out"), "{order}");
}

#[test]
fn an_async_script_cannot_write_into_the_page() {
    let files = [
        (
            "w.js",
            "document.write('<p id=late></p>'); __log.push('ran')",
        ),
        ("slow.js", ""),
    ];
    let html = format!(
        "{LOG}<script async src=w.js></script><script src=slow.js></script><p id=early></p>"
    );
    let (mut page, held) = held_page(&files, &html);
    page.document.pump_loads(&mut page.runtime);
    // The async script is here while the parser waits for the one before `<p>`.
    held.release("w.js");
    page.document.pump_loads(&mut page.runtime);
    held.release("slow.js");
    settle_reversed(&mut page, &held);
    assert_eq!(page.log(), ["ran"]);
    assert_eq!(page.eval("String(document.getElementById('late'))"), "null");
    assert!(
        page.console()
            .contains("asynchronously-loaded external script")
    );
}

#[test]
fn a_body_handler_attribute_is_read_and_goes_where_html_says() {
    let html = format!(
        "{LOG}<body onload=\"__log.push('load')\" onclick=\"__log.push('click')\" \
         onmessage=\"__log.push('message')\"><p id=p onclick=\"__log.push('p')\">"
    );
    let mut page = loaded(&[], &html);
    assert_eq!(page.log(), ["load"]);
    assert_eq!(page.eval("typeof document.body.onclick"), "function");
    assert_eq!(page.eval("typeof window.onclick"), "object");
    assert_eq!(page.eval("typeof window.onmessage"), "function");
}

#[test]
fn moving_an_element_leaves_the_handler_a_script_set() {
    let html = format!("{LOG}<p id=p onclick=\"__log.push('attr')\">x</p><div id=d></div>");
    let mut page = loaded(&[], &html);
    page.eval("var p = document.getElementById('p'), d = document.getElementById('d'); p.onclick = () => __log.push('set'); d.appendChild(p); p.click()");
    assert_eq!(page.log(), ["set"]);
    page.eval("var p = document.getElementById('p'); p.setAttribute('onclick', \"__log.push('again')\"); p.click()");
    assert_eq!(page.log(), ["set", "again"]);
}

#[test]
fn a_redirected_page_resolves_what_it_names_against_where_it_ended() {
    let files = [
        ("r.html", "<script src=s.js></script>"),
        ("dir/s.js", "window.found = document.currentScript.src"),
    ];
    let mut page = page(&files);
    page.document
        .navigate_initial(&mut page.runtime, "app://page/go/r.html");
    page.finish_loading();
    assert_eq!(page.document.url(), "app://page/dir/r.html");
    assert_eq!(page.eval("window.found"), "app://page/dir/s.js");
}

#[test]
fn a_page_that_wraps_fetch_does_not_see_its_own_subresources() {
    let html = "<script>window.seen = 0; const f = window.fetch; \
        window.fetch = (...a) => { seen++; return f(...a); };</script>\
        <script src=\"data:text/javascript,window.ran=1\"></script>";
    let mut page = loaded(&[], html);
    assert_eq!(page.eval("String(window.ran)"), "1");
    assert_eq!(page.eval("String(window.seen)"), "0");
}

#[test]
fn a_script_with_nothing_to_run_runs_when_it_has_something() {
    let html = format!("{LOG}<div id=d></div>");
    let mut page = loaded(&[("x.js", "__log.push('src')")], &html);
    page.eval(
        "var d = document.getElementById('d'); var s = document.createElement('script'); d.appendChild(s); s.text = '__log.push(\"text\")'; \
         var t = document.createElement('script'); d.appendChild(t); t.src = 'x.js';",
    );
    page.finish_loading();
    assert_eq!(page.log(), ["text", "src"]);
}

#[test]
fn a_script_a_page_makes_is_async_until_it_says_otherwise() {
    let mut page = loaded(&[], "<p>x");
    assert_eq!(
        page.eval("String(document.createElement('script').async)"),
        "true"
    );
    assert_eq!(
        page.eval("var s = document.createElement('script'); s.async = false; String(s.async)"),
        "false"
    );
}

#[test]
fn a_contextual_fragment_runs_its_scripts_once_in_the_page() {
    let html = format!("{LOG}<div id=d></div>");
    let mut page = loaded(&[], &html);
    page.eval(
        "var d = document.getElementById('d'); var f = document.createRange().createContextualFragment('<script>__log.push(\"frag\")</script>'); \
         d.appendChild(f)",
    );
    assert_eq!(page.log(), ["frag"]);
}

#[test]
fn a_script_in_an_inserted_subtree_runs_once() {
    let html = format!("{LOG}<div id=d></div>");
    let mut page = loaded(&[], &html);
    page.eval(
        "var d = document.getElementById('d'); var o = document.createElement('div'); var s = document.createElement('section'); \
         var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); \
         var n = document.createElementNS('http://www.w3.org/2000/svg', 'script'); \
         n.textContent = '__log.push(\"svg\")'; svg.appendChild(n); s.appendChild(svg); \
         o.appendChild(s); d.appendChild(o)",
    );
    assert_eq!(page.log(), ["svg"]);
}

#[test]
fn fetch_and_xhr_follow_the_base_element() {
    let files = [("sub/x.json", "{\"a\": 1}")];
    let html = "<base href=\"app://page/sub/\"><p>x";
    let mut page = loaded(&files, html);
    page.eval("fetch('x.json').then(r => r.json()).then(j => { window.got = j.a; })");
    page.finish_loading();
    page.settle_for(10.);
    assert_eq!(page.eval("String(window.got)"), "1");
}

fn margin(page: &mut Page) -> String {
    page.eval("getComputedStyle(document.getElementById('p')).marginTop")
}

#[test]
fn imports_go_in_the_order_they_are_written_before_the_sheet_that_imports_them() {
    let files = [
        ("s.css", "@import \"a.css\"; @import \"b.css\";"),
        ("a.css", "p { margin-top: 1px }"),
        ("b.css", "p { margin-top: 2px }"),
    ];
    let html = "<link rel=stylesheet href=s.css><p id=p>x";
    let (mut page, held) = held_page(&files, html);
    settle_reversed(&mut page, &held);
    assert_eq!(margin(&mut page), "2px");
}

#[test]
fn a_sheet_that_comes_late_keeps_the_place_of_its_element() {
    let files = [("l.css", "p { margin-top: 9px }")];
    let html = "<link rel=stylesheet href=l.css><style>p { margin-top: 3px }</style><p id=p>x";
    let (mut page, held) = held_page(&files, html);
    page.document.pump_loads(&mut page.runtime);
    // The `<style>` is read and drawn with before the link has come.
    assert_eq!(margin(&mut page), "3px");
    settle_reversed(&mut page, &held);
    assert_eq!(margin(&mut page), "3px");
}

#[test]
fn a_sheet_for_print_is_not_one_for_the_screen() {
    let files = [("pr.css", "p { margin-top: 7px }")];
    let html = "<link rel=stylesheet media=print href=pr.css><p id=p>x";
    let (mut page, held) = held_page(&files, html);
    settle_reversed(&mut page, &held);
    assert_eq!(margin(&mut page), "1em");
}

#[test]
fn only_a_sheet_in_the_head_holds_back_the_scripts_after_it() {
    let files = [("slow.css", "p { margin-top: 5px }")];
    let head =
        format!("{LOG}<link rel=stylesheet href=slow.css><script>__log.push('head')</script>");
    let (mut page, held) = held_page(&files, &head);
    page.document.pump_loads(&mut page.runtime);
    assert_eq!(page.log(), Vec::<String>::new());
    settle_reversed(&mut page, &held);
    assert_eq!(page.log(), ["head"]);

    let body = format!(
        "{LOG}<body><link rel=stylesheet href=slow.css><script>__log.push('body')</script>"
    );
    let (mut page, held) = held_page(&files, &body);
    page.document.pump_loads(&mut page.runtime);
    assert_eq!(page.log(), ["body"]);
    settle_reversed(&mut page, &held);
}

#[test]
fn a_document_that_is_not_the_windows_has_no_handlers() {
    let html = format!("{LOG}<template id=t><p id=tp onclick=\"__log.push('t')\"></p></template>");
    let mut page = loaded(&[], &html);
    page.eval(
        "var d = new DOMParser().parseFromString('<body onfocus=\"__log.push(1)\" onmessage=\"__log.push(2)\"><p id=q onclick=\"__log.push(3)\">', 'text/html'); \
         var e = document.implementation.createHTMLDocument(''); e.body.setAttribute('onblur', \"__log.push(4)\");",
    );
    assert_eq!(page.eval("String(window.onfocus)"), "null");
    assert_eq!(page.eval("String(window.onmessage)"), "null");
    assert_eq!(page.eval("String(window.onblur)"), "null");
    assert_eq!(
        page.eval("typeof new DOMParser().parseFromString('<p onclick=x>', 'text/html').body.firstChild.onclick"),
        "object"
    );
    page.eval("document.getElementById('t').content.getElementById('tp').setAttribute('onclick', \"__log.push('t2')\")");
    assert_eq!(
        page.eval("typeof document.getElementById('t').content.getElementById('tp').onclick"),
        "object"
    );
}

#[test]
fn removing_a_handler_attribute_removes_the_handler() {
    let html =
        format!("{LOG}<body onfocus=\"__log.push('f')\"><p id=p onclick=\"__log.push('p')\">");
    let mut page = loaded(&[], &html);
    assert_eq!(page.eval("typeof window.onfocus"), "function");
    page.eval("document.body.removeAttribute('onfocus'); var p = document.getElementById('p'); p.setAttribute('onclick', ''); ");
    assert_eq!(page.eval("String(window.onfocus)"), "null");
    assert_eq!(
        page.eval("String(document.getElementById('p').onclick)"),
        "null"
    );
    page.eval("document.body.setAttribute('onpageswap', \"1\"); document.body.setAttribute('onpagereveal', \"1\")");
    assert_eq!(
        page.eval("typeof window.onpageswap + typeof window.onpagereveal"),
        "functionfunction"
    );
}

#[test]
fn writes_of_a_script_in_a_write_keep_to_their_place() {
    // Two writes per level: the second goes after the first, before the rest of the outer text.
    let html = format!(
        r#"{LOG}<script>document.write('<script>document.write("<i id=a></i>");document.write("<i id=b></i>")<\/script><b id=c></b>');</script>"#
    );
    let mut page = loaded(&[], &html);
    assert!(body_order(&mut page).contains("a,b,c"));
    // A script in the outer text does not run inside the inner script's write.
    let html = format!(
        r#"{LOG}<script>document.write('<script>document.write("x");__log.push("in1");document.write("y");__log.push("in2")<\/script><script>__log.push("outer")<\/script>');</script>"#
    );
    let mut page = loaded(&[], &html);
    assert_eq!(page.log(), ["in1", "in2", "outer"]);
}

#[test]
fn writes_three_levels_deep_do_not_panic() {
    let html = format!(
        r#"{LOG}<script>
        var s3 = 'document.write("<i id=a></i>");document.write("<i id=b></i>")';
        var s2 = 'document.write(' + JSON.stringify('<script>' + s3 + '<\/script><i id=c></i>').replace(/<\//g, '<\\/') + ');document.write("<i id=d></i>")';
        document.write('<script>' + s2 + '<\/script><i id=e></i>');
        </script><p id=z>"#
    );
    let mut page = loaded(&[], &html);
    let order = body_order(&mut page);
    assert!(order.contains("a,b,c,d,e,z"), "{order}");
}

#[test]
fn a_written_external_script_may_write_twice() {
    let files = [(
        "w.js",
        "document.write('<i id=a></i>'); document.write('<i id=b></i>')",
    )];
    let html = format!(
        r#"{LOG}<script>document.write('<script src=w.js><\/script><i id=c></i>');</script><i id=d></i>"#
    );
    let mut page = loaded(&files, &html);
    let order = body_order(&mut page);
    assert!(order.contains("a,b,c,d"), "{order}");
}

#[test]
fn a_write_to_another_document_is_not_the_parsers() {
    let html = format!(
        "{LOG}<script>var d = document.implementation.createHTMLDocument(''); d.write('<p id=other></p>'); \
         __log.push(String(!!d.getElementById('other')));</script><p id=after></p>"
    );
    let mut page = loaded(&[], &html);
    assert_eq!(page.log(), ["true"]);
    assert_eq!(
        page.eval("String(document.getElementById('other'))"),
        "null"
    );
    assert_eq!(
        page.eval("String(!!document.getElementById('after'))"),
        "true"
    );
    assert!(!page.console().contains("asynchronously-loaded"));
}

#[test]
fn a_scripts_microtasks_run_before_the_next_script_and_before_its_load() {
    let files = [(
        "e.js",
        "Promise.resolve().then(() => __log.push('e-micro'))",
    )];
    let html = format!(
        "{LOG}<script>Promise.resolve().then(() => __log.push('micro'))</script>\
         <script>__log.push('second')</script>\
         <script src=e.js onload=\"__log.push('e-load')\"></script>"
    );
    let mut page = loaded(&files, &html);
    assert_eq!(page.log(), ["micro", "second", "e-micro", "e-load"]);
}

#[test]
fn a_page_that_breaks_the_responses_cannot_stall_its_own_scripts() {
    // The runtime's own `fetch` answers a network address, as the loader asks it to.
    let clock = Rc::new(std::cell::Cell::new(0.));
    let mut runtime = verve::Runtime::with_clock(move || clock.get());
    runtime
        .eval("globalThis.fetch = () => Promise.resolve(new Response('window.__log.push(\"remote\")'))")
        .expect("a fetch");
    let document = verve_dom::install(
        &mut runtime,
        Options {
            loader: Some(Box::new(Fixtures {
                extra: HashMap::new(),
            })),
            ..Options::default()
        },
    )
    .expect("a document");
    let mut page = Page {
        runtime,
        document,
        clock: Rc::new(std::cell::Cell::new(0.)),
    };
    let html = format!(
        "{LOG}<script src=\"https://example.test/x.js\"></script>\
         <script>Response.prototype.arrayBuffer = function () {{ throw new Error('broken'); }}</script>\
         <script src=\"https://example.test/y.js\" onerror=\"__log.push('error')\"></script>\
         <script>__log.push('after')</script>"
    );
    page.document
        .load_html(&mut page.runtime, "app://page/p.html", &html);
    page.finish_loading();
    assert_eq!(page.log(), ["remote", "error", "after"]);
}

#[test]
fn a_dynamic_import_that_cannot_be_fetched_fails_where_the_page_can_catch_it() {
    let files = [("chunk.js", "export const v = 'chunk'")];
    let html = format!(
        "{LOG}<script type=module>\
         const web = /^https?:\\/\\//.test('x'); __log.push('ran');\
         import('./opt.js').catch(() => __log.push('caught'));\
         import('./chunk.js').then(m => __log.push(m.v));\
         </script>"
    );
    let mut page = loaded(&files, &html);
    page.finish_loading();
    let log = page.log();
    assert_eq!(log[0], "ran");
    assert!(log.contains(&"caught".to_owned()), "{log:?}");
    assert!(log.contains(&"chunk".to_owned()), "{log:?}");
}

#[test]
fn a_deferred_or_module_script_cannot_write_into_the_page() {
    let files = [
        (
            "d.js",
            "document.write('<p id=late-defer></p>'); __log.push('defer')",
        ),
        (
            "m.js",
            "document.write('<p id=late-module></p>'); __log.push('module')",
        ),
    ];
    let html = format!(
        "{LOG}<script defer src=d.js></script><script type=module src=m.js></script><p id=early></p>"
    );
    let mut page = loaded(&files, &html);
    assert_eq!(page.log(), ["defer", "module"]);
    assert_eq!(
        page.eval("String(document.getElementById('late-defer'))"),
        "null"
    );
    assert_eq!(
        page.eval("String(document.getElementById('late-module'))"),
        "null"
    );
    let console = page.console();
    assert_eq!(
        console
            .matches("asynchronously-loaded external script unless it is explicitly opened.")
            .count(),
        2,
        "{console}"
    );
    assert!(!console.contains("  "), "{console}");
}

#[test]
fn a_script_in_a_template_runs_when_its_content_is_put_in_the_page() {
    // Observed in Electron 39: the copy and the content itself both run.
    let html = format!(
        "{LOG}<body><template id=t><script>__log.push('tpl')</script></template>\
         <script>var c = document.getElementById('t').content; document.body.append(c.cloneNode(true)); document.body.append(c);</script>"
    );
    let mut page = loaded(&[], &html);
    assert_eq!(page.log(), ["tpl", "tpl"]);
    let copy = format!(
        "{LOG}<script>var s = document.createElement('script'); s.text = `__log.push(\"1\")`; document.body.append(s); document.body.append(s.cloneNode(true));</script>"
    );
    let mut page = loaded(&[], &copy);
    assert_eq!(page.log(), ["1"]);
}

#[test]
fn nested_imports_come_before_their_importer_and_the_links_load_waits_for_them() {
    let files = [
        ("s.css", "@import \"a.css\";"),
        ("a.css", "@import \"c.css\"; p { margin-top: 2px }"),
        ("c.css", "p { margin-top: 1px }"),
    ];
    let html = format!(
        "{LOG}<p id=p>x</p><link rel=stylesheet href=s.css \
         onload=\"__log.push(getComputedStyle(document.getElementById('p')).marginTop)\">"
    );
    let (mut page, held) = held_page(&files, &html);
    settle_reversed(&mut page, &held);
    assert_eq!(margin(&mut page), "2px");
    assert_eq!(page.log(), ["2px"]);
}
