//! `<script>` elements: which run, when, and what they see — HTML's "prepare the script
//! element", for those the parser stops at and those a page inserts.

use std::path::Path;

use boa_engine::{Context, JsError, JsValue, Source};
use url::Url;
use verve::native::js_text;

use super::{Destination, Document, Flow, LoadResult, Script, Status, Wait, net};
use crate::bind::node::with_dom;
use crate::bind::{Accessor, events, extend_prototype, node_of};
use crate::dom::NodeId;
use crate::html::Step;
use crate::state::{State, shared};

/// What a `<script>` says of itself.
struct Prepared {
    src: Option<Url>,
    /// `src` was given and is no address.
    bad_src: bool,
    text: String,
    is_async: bool,
    defer: bool,
}

/// Whether `type` makes the `<script>` a classic script: the others are data blocks.
fn is_classic(state: &State, node: NodeId) -> bool {
    let kind = state.dom.get_attr(node, "type").unwrap_or_default();
    matches!(
        kind.trim().to_ascii_lowercase().as_str(),
        "" | "text/javascript"
            | "application/javascript"
            | "text/ecmascript"
            | "application/ecmascript"
            | "application/x-javascript"
            | "text/x-javascript"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
    )
}

fn read(state: &State, node: NodeId) -> Option<Prepared> {
    if !is_classic(state, node) {
        return None;
    }
    let src = state
        .dom
        .get_attr(node, "src")
        .filter(|s| !s.trim().is_empty());
    let resolved = src
        .as_ref()
        .and_then(|s| state.base_url().join(s.trim()).ok());
    let has = |name: &str| state.dom.get_attr(node, name).is_some();
    Some(Prepared {
        bad_src: src.is_some() && resolved.is_none(),
        src: resolved,
        text: state.dom.text_content(node),
        is_async: has("async"),
        defer: has("defer"),
    })
}

/// A `<script>` the parser has just completed.
pub(super) fn parsed(doc: &Document, runtime: &mut verve::Runtime, node: NodeId) {
    let prepared = read(&doc.state.borrow(), node);
    let Some(prepared) = prepared else { return };
    schedule(runtime.context(), node, prepared, true);
    // HTML's "clean up after running script": the microtasks of a script the parser ran are done
    // before the next one starts.
    runtime.run_microtasks();
}

/// What a batch of subtrees that joined the document holds for the page to load.
#[derive(Default)]
struct Found {
    scripts: Vec<NodeId>,
}

/// The roots none of whose ancestors is also among them: a subtree is walked once.
fn outermost(state: &State, roots: &[NodeId]) -> Vec<NodeId> {
    let set: std::collections::HashSet<NodeId> = roots.iter().copied().collect();
    let inside = |root: NodeId| {
        let mut at = state.dom.parent(root);
        while let Some(node) = at {
            if set.contains(&node) {
                return true;
            }
            at = state.dom.parent(node);
        }
        false
    };
    let mut seen = std::collections::HashSet::new();
    roots
        .iter()
        .copied()
        .filter(|root| !inside(*root) && seen.insert(*root))
        .collect()
}

fn collect(state: &State, roots: &[NodeId]) -> Found {
    let mut found = Found::default();
    let nodes = outermost(state, roots)
        .into_iter()
        .flat_map(|root| state.dom.shadow_including_tree(root))
        .filter(|id| !state.dom.in_template(*id));
    for id in nodes {
        match state.dom.get(id).and_then(|n| n.tag()) {
            Some("script") if !state.dom.is_started(id) && state.dom.is_connected(id) => {
                found.scripts.push(id)
            }
            _ => {}
        }
    }
    found
}

/// Subtrees that joined the document: the `<script>`s in them that no parser read are the page's
/// to load now.
pub(crate) fn connected(context: &mut Context, roots: &[NodeId]) {
    super::handlers::connected(context, roots);
    let found = {
        let state = shared(context);
        let state = state.borrow();
        if !state.load.active {
            return;
        }
        collect(&state, roots)
    };
    for node in found.scripts {
        prepare(context, node);
    }
}

/// HTML's "prepare the script element", for a script the page put in: one with nothing to run
/// is left for when it has (`s.src = …`, `s.text = …`), and one that runs is started once.
fn prepare(context: &mut Context, node: NodeId) {
    let prepared = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        if state.dom.is_started(node) {
            return;
        }
        let prepared = read(&state, node);
        let runs = prepared
            .as_ref()
            .is_some_and(|p| p.src.is_some() || p.bad_src || !p.text.is_empty());
        if !runs {
            return;
        }
        state.dom.mark_started(node);
        state.load.forced.insert(node);
        prepared
    };
    if let Some(prepared) = prepared {
        schedule(context, node, prepared, false);
    }
}

/// Puts a script where it runs: now, after the parse, or when it has arrived.
fn schedule(context: &mut Context, node: NodeId, prepared: Prepared, parser: bool) {
    let Some(url) = prepared.src else {
        if prepared.bad_src {
            return fire_on(context, node, "error");
        }
        if prepared.text.is_empty() {
            return;
        }
        // An inline script runs where it stands: as it is put in, or where the parser is.
        let name = shared(context).borrow().url.to_string();
        return run_classic(context, node, &prepared.text, &name, parser);
    };
    let flow = match (parser, prepared.is_async, prepared.defer) {
        (_, true, _) => Flow::Async,
        (true, false, false) => Flow::Blocking,
        (true, ..) => Flow::Defer,
        (false, ..) if shared(context).borrow().load.ordered.contains(&node) => Flow::Ordered,
        (false, ..) => Flow::Async,
    };
    push(context, node, flow, url.clone());
    net::start(context, url, Destination::Script, Wait::Script(node));
}

fn push(context: &mut Context, node: NodeId, flow: Flow, url: Url) {
    shared(context).borrow_mut().load.scripts.push(Script {
        node,
        flow,
        url: Some(url),
        text: String::new(),
        status: Status::Fetching,
    });
}

/// A classic script's source has come, or has not.
pub(super) fn fetched(doc: &Document, node: NodeId, result: LoadResult) {
    let mut state = doc.state.borrow_mut();
    let Some(script) = state.load.scripts.iter_mut().find(|s| s.node == node) else {
        return;
    };
    match result.body {
        Ok(bytes) if result.status < 400 => {
            script.text = net::text(&bytes);
            script.status = Status::Ready;
        }
        _ => script.status = Status::Failed,
    }
}

/// Runs a script that is ready, and tells its element how it went.
pub(super) fn execute(doc: &Document, runtime: &mut verve::Runtime, script: Script) {
    let context = runtime.context();
    if script.status == Status::Failed {
        return fire_on(context, script.node, "error");
    }
    let name = script
        .url
        .as_ref()
        .map_or_else(|| doc.state.borrow().url.to_string(), |url| url.to_string());
    let parser = script.flow == Flow::Blocking;
    let apart = std::mem::replace(&mut shared(context).borrow_mut().load.apart, !parser);
    run_classic(context, script.node, &script.text, &name, parser);
    shared(context).borrow_mut().load.apart = apart;
    // Its microtasks are done before `load` is heard.
    runtime.run_microtasks();
    let context = runtime.context();
    if script.url.is_some() {
        fire_on(context, script.node, "load");
    }
    runtime.run_microtasks();
}

/// Runs `source` as a classic script of `node`, which `document.currentScript` is meanwhile.
/// A script the parser stopped at (`parser`) is where `document.write` goes in meanwhile, and
/// so is any script it runs in turn.
fn run_classic(context: &mut Context, node: NodeId, source: &str, name: &str, parser: bool) {
    let (before, writing, point) = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let load = &mut state.load;
        let before = load.current_script.replace(node);
        let writing = load.writing;
        load.writing |= parser;
        let point = match load.parser.as_mut() {
            Some(reader) if parser => Some(reader.enter_script()),
            _ => None,
        };
        (before, writing, point)
    };
    let result = context.eval(Source::from_bytes(source).with_path(Path::new(name)));
    {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.load.current_script = before;
        state.load.writing = writing;
        if let (Some(point), Some(reader)) = (point, state.load.parser.as_mut()) {
            reader.leave_script(point);
        }
    }
    if let Err(error) = result {
        uncaught(context, &error, name);
    }
}

/// A script threw: `window` hears an `error` event, and unless a listener took it, the console
/// does.
pub(super) fn uncaught(context: &mut Context, error: &JsError, file: &str) {
    let message = format!("Uncaught {}", verve::describe(context, error));
    let value = error.clone().into_opaque(context).unwrap_or_default();
    let window: JsValue = context.global_object().into();
    let init = [
        ("message", js_text(&message)),
        ("filename", js_text(file)),
        ("error", value),
    ];
    let outcome = events::fire(context, &window, "ErrorEvent", "error", &init);
    let allowed = heard(context, outcome);
    if allowed {
        verve::report(context, error);
    }
}

/// Fires `kind` at `node`, as a page's own doing.
pub(super) fn fire_on(context: &mut Context, node: NodeId, kind: &str) {
    let outcome = events::fire_at(context, node, "Event", kind, &[], true);
    heard(context, outcome);
}

pub(super) fn fire_on_document(context: &mut Context, kind: &str, bubbles: bool) {
    let document = shared(context).borrow().dom.document;
    let init = [("bubbles", bubbles.into())];
    let outcome = events::fire_at(context, document, "Event", kind, &init, true);
    heard(context, outcome);
}

pub(super) fn fire_on_window(context: &mut Context, kind: &str) {
    let window: JsValue = context.global_object().into();
    let outcome = events::fire(context, &window, "Event", kind, &[]);
    heard(context, outcome);
}

/// Whether the event was left alone; a listener that threw has it reported, as the page's own.
pub(super) fn heard(context: &mut Context, outcome: boa_engine::JsResult<bool>) -> bool {
    outcome.unwrap_or_else(|error| {
        verve::report(context, &error);
        true
    })
}

/// `script.async`: setting it false asks for the order the scripts were put in the page in.
pub(crate) fn install(context: &mut Context) -> boa_engine::JsResult<()> {
    let prototype = shared(context).borrow().prototype("HTMLScriptElement");
    let accessors: &[Accessor] = &[("async", Some(async_get), Some(async_set))];
    extend_prototype(context, &prototype, &[], accessors)
}

fn async_get(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> boa_engine::JsResult<JsValue> {
    let node = node_of(this)?;
    let state = shared(context);
    let state = state.borrow();
    // A script made by a page is "force async" until it says otherwise.
    let forced = !state.load.ordered.contains(&node)
        && (state.load.forced.contains(&node) || !state.dom.is_started(node));
    Ok((forced || state.dom.get_attr(node, "async").is_some()).into())
}

fn async_set(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> boa_engine::JsResult<JsValue> {
    let node = node_of(this)?;
    let on = args.first().is_some_and(JsValue::to_boolean);
    if on {
        shared(context).borrow_mut().load.ordered.remove(&node);
        with_dom(context, |dom| dom.set_attr(node, "async", ""));
    } else {
        with_dom(context, |dom| dom.remove_attr(node, "async"));
        shared(context).borrow_mut().load.ordered.insert(node);
    }
    Ok(JsValue::undefined())
}

/// What `document.write` makes of `markup`. While a parser-inserted script runs, it is read at
/// the insertion point, and the scripts in it run before the call returns. From a script that
/// does not hold the parser (`async`, `defer`, a module) it is ignored, as Chromium ignores it,
/// and so it is from a timer while the page is read. Answers whether the page was being read.
pub(crate) fn write(context: &mut Context, markup: &str) -> bool {
    let state = shared(context);
    let writing = {
        let mut state = state.borrow_mut();
        let apart = state.load.apart;
        if !state.load.active || (state.load.parser.is_none() && !apart) {
            return false;
        }
        let writing = state.load.writing && !apart;
        if let (true, Some(parser)) = (writing, state.load.parser.as_mut()) {
            parser.insert(markup);
        }
        writing
    };
    if writing {
        write_now(context);
    } else {
        let message = "Failed to execute 'write' on 'Document': It isn't possible to write into a document from an asynchronously-loaded external script unless it is explicitly opened.";
        verve::console_line(context, verve::Level::Warn, message.to_owned());
    }
    true
}

/// Reads what was written, up to a script that has to be waited for.
fn write_now(context: &mut Context) {
    loop {
        let waits = {
            let state = shared(context);
            let state = state.borrow();
            state.load.scripts.iter().any(|s| s.flow == Flow::Blocking)
        };
        if waits {
            return;
        }
        let Some(mut parser) = shared(context).borrow_mut().load.parser.take() else {
            return;
        };
        let step = with_dom(context, |dom| parser.step_inserted(dom));
        shared(context).borrow_mut().load.parser = Some(parser);
        let Step::Script(node) = step else { return };
        let prepared = read(&shared(context).borrow(), node);
        if let Some(prepared) = prepared {
            schedule(context, node, prepared, true);
        }
    }
}
