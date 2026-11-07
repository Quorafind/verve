//! `HTMLImageElement`: what an `<img>` reports about its load (`naturalWidth`, `complete`,
//! `currentSrc`), `decode()`, and the `load` and `error` events (HTML, "images").
//!
//! The renderer never decodes an `<img>` to show it: it reads the header for the size and hands
//! the drawer the bytes. A `src` that changes is a load — a `data:` address is read here, any
//! other is asked of the embedder as [`Request::Image`] and answered by
//! [`Document::set_image_bytes`](crate::Document::set_image_bytes).

use std::sync::Arc;

use boa_engine::object::WeakJsObject;
use boa_engine::{Context, JsObject, JsResult, JsValue, builtins::promise::ResolvingFunctions};
use verve::native::{self, arg, closure, js_text, text_of};

use crate::Request;
use crate::bind::element::{Reflect, absolute_url};
use crate::bind::node::{read_dom, with_dom};
use crate::bind::{self, Accessor, Method, node_of};
use crate::canvas::image::{ElementImage, Keep, Status, image_size};
use crate::canvas::{Axis, set_dimension};
use crate::dom::NodeId;
use crate::state::{State, shared};

pub const IMAGE_ACCESSORS: &[Accessor] = &[
    (
        "naturalWidth",
        Some(|this, _, cx| Ok(report(cx, node_of(this)?).natural.0.into())),
        None,
    ),
    (
        "naturalHeight",
        Some(|this, _, cx| Ok(report(cx, node_of(this)?).natural.1.into())),
        None,
    ),
    (
        "complete",
        Some(|this, _, cx| Ok(report(cx, node_of(this)?).status.is_complete().into())),
        None,
    ),
    (
        "currentSrc",
        Some(|this, _, cx| Ok(js_text(&report(cx, node_of(this)?).url.unwrap_or_default()))),
        None,
    ),
    (
        "src",
        Some(|this, _, cx| source(this, cx)),
        Some(set_source),
    ),
    (
        "width",
        Some(|this, _, cx| Ok(rendered(cx, node_of(this)?, Axis::Width).into())),
        Some(|this, args, cx| set_dimension(this, args, cx, Axis::Width)),
    ),
    (
        "height",
        Some(|this, _, cx| Ok(rendered(cx, node_of(this)?, Axis::Height).into())),
        Some(|this, args, cx| set_dimension(this, args, cx, Axis::Height)),
    ),
];

pub const IMAGE_METHODS: &[Method] = &[("decode", 0, |this, _, cx| {
    decode_promise(cx, node_of(this)?)
})];

pub const IMAGE_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("srcset", "srcset", Reflect::Text),
    ("sizes", "sizes", Reflect::Text),
    ("alt", "alt", Reflect::Text),
    ("loading", "loading", Reflect::Keyword("eager")),
    ("decoding", "decoding", Reflect::Keyword("auto")),
    ("crossOrigin", "crossorigin", Reflect::Text),
    ("useMap", "usemap", Reflect::Text),
    ("isMap", "ismap", Reflect::Flag),
    ("referrerPolicy", "referrerpolicy", Reflect::Text),
];

fn source(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let value = read_dom(context, |dom| dom.get_attr(id, "src")).unwrap_or_default();
    Ok(js_text(&absolute_url(context, value)))
}

/// Setting the source starts the load at once, as in a browser, so its events are due.
fn set_source(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let value = text_of(&arg(args, 0), context)?;
    with_dom(context, |dom| dom.set_attr(id, "src", &value));
    report(context, id);
    Ok(JsValue::undefined())
}

// ─── Loading ────────────────────────────────────────────────────────────────────────────────

/// Reads what `node` names now, for the sources that changed: a new address is a load — a
/// `data:` one decoded here, anything else asked of the embedder. Called for the nodes a frame
/// found changed, so a page adds no cost for the images it does not touch.
pub(crate) fn sync(state: &mut State, node: NodeId) {
    let chosen = super::sources::current(state, node);
    let density = chosen.as_ref().map_or(1., |(_, density)| *density);
    let wanted = chosen.and_then(|(source, _)| resolve(&state.base_url(), &source));
    let named = state
        .images
        .nodes
        .get(&node)
        .and_then(|img| img.url.as_deref());
    if named == wanted.as_deref() {
        if let Some(img) = state.images.nodes.get_mut(&node) {
            img.density = density;
        }
        return;
    }
    let status = match &wanted {
        Some(url) => start_load(state, url, Keep::WhileNamed),
        None => Status::Idle,
    };
    if matches!(status, Status::Loaded | Status::Broken) {
        state.images.events.push((node, status));
    }
    let img = ElementImage {
        url: wanted,
        status,
        waiting: Vec::new(),
        density,
    };
    state.images.nodes.insert(node, img);
}

/// Where a load of `url` stands as it starts: already known, readable here, or asked of the
/// embedder.
pub(crate) fn start_load(state: &mut State, url: &str, keep: Keep) -> Status {
    if state.images.has(url) {
        return Status::Loaded;
    }
    if state.images.failed.contains(url) {
        return Status::Broken;
    }
    let data = url::Url::parse(url)
        .ok()
        .filter(|parsed| parsed.scheme() == "data")
        .map(|parsed| verve::encoding::data_url(&parsed));
    match data {
        Some(Ok((mime, bytes))) if image_size(&bytes).is_some() => {
            state.images.insert(url, Some(mime), Arc::from(bytes), keep);
            Status::Loaded
        }
        Some(_) => {
            state.images.mark_failed(url);
            Status::Broken
        }
        None => {
            state.requests.push(Request::Image(url.to_owned()));
            Status::Loading
        }
    }
}

/// Whether bytes from `url` taint the canvas they are drawn on: a cross-origin fetch, which
/// `file:`, `data:` and `blob:` images are not.
#[cfg(feature = "canvas")]
pub(crate) fn is_cross_origin(document: &url::Url, url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return true;
    };
    match url.scheme() {
        "file" | "data" | "blob" | "about" => false,
        _ => url.origin() != document.origin(),
    }
}

/// The address an `<img src>` names, if it is one to fetch.
pub(crate) fn resolve(document: &url::Url, source: &str) -> Option<String> {
    let source = source.trim();
    if source.is_empty() {
        return None;
    }
    let url = document.join(source).ok()?;
    matches!(
        url.scheme(),
        "http" | "https" | "file" | "data" | "blob" | "app"
    )
    .then(|| url.to_string())
}

/// A `decode()` promise waiting for its image.
type Waiting = Vec<(NodeId, WeakJsObject)>;

/// The loader's answer for `url`: the elements naming it are loaded or broken, the events they
/// are owed are queued, and the `decode()` promises waiting on them come back to be settled.
pub(crate) fn settle(state: &mut State, url: &str, outcome: Status) -> Waiting {
    let mut waiting = Vec::new();
    for (node, img) in state
        .images
        .nodes
        .iter_mut()
        .filter(|(_, img)| img.url.as_deref() == Some(url))
    {
        state.images.events.push((*node, outcome));
        img.status = outcome;
        waiting.extend(img.waiting.drain(..).map(|promise| (*node, promise)));
    }
    waiting
}

/// The message `decode()` rejects with when the image never arrived or is not an image.
/// The message `decode()` rejects with when the loader's answer is no image.
pub(crate) const UNDECODABLE: &str = "The source image cannot be decoded.";
/// The message `decode()` rejects with when the loader could not fetch the address.
pub(crate) const UNFETCHABLE: &str = "The source image cannot be fetched.";

/// Queues the events owed and settles the `decode()` promises: resolved when `failure` is
/// `None`, rejected with an `EncodingError` carrying it otherwise.
pub(crate) fn deliver(context: &mut Context, waiting: Waiting, failure: Option<&str>) {
    flush_events(context);
    for (node, promise) in waiting {
        if let Err(error) = settle_promise(context, node, &promise, failure) {
            verve::report(context, &error);
        }
    }
}

/// Queues the `load` and `error` events the elements are owed, each as a task of its own: a
/// handler set right after `src` still hears it, as in a browser.
pub(crate) fn flush_events(context: &mut Context) {
    let owed = std::mem::take(&mut shared(context).borrow_mut().images.events);
    for (node, status) in owed {
        let fire = closure(context, "image event", 0, fire_event, ());
        let name = match status {
            Status::Loaded => "load",
            _ => "error",
        };
        let args = vec![node.into(), js_text(name)];
        verve::set_timeout(context, fire.into(), 0., false, args);
    }
}

fn fire_event(_: &JsValue, args: &[JsValue], _: &(), context: &mut Context) -> JsResult<JsValue> {
    let node = arg(args, 0).as_number().unwrap_or_default() as NodeId;
    let name = text_of(&arg(args, 1), context)?;
    if shared(context).borrow().dom.exists(node) {
        bind::events::fire_at(context, node, "Event", &name, &[], true)?;
    }
    Ok(JsValue::undefined())
}

// ─── What an `<img>` reports ────────────────────────────────────────────────────────────────

/// What an `<img>` reports now. Its address is read afresh, since a `src` set a moment ago has
/// not reached a frame yet.
pub(crate) struct Report {
    pub url: Option<String>,
    pub status: Status,
    pub natural: (u32, u32),
}

pub(crate) fn report(context: &mut Context, node: NodeId) -> Report {
    let report = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        sync(&mut state, node);
        let img = state.images.nodes.get(&node);
        let url = img.and_then(|img| img.url.clone());
        let status = img.map_or(Status::Idle, |img| img.status);
        let density = img.map_or(1., ElementImage::density);
        let natural = url
            .as_deref()
            .and_then(|url| state.images.natural(url))
            .filter(|_| status == Status::Loaded)
            .map_or((0, 0), |(w, h)| {
                ((w as f32 / density) as u32, (h as f32 / density) as u32)
            });
        Report {
            url,
            status,
            natural,
        }
    };
    flush_events(context);
    report
}

/// `img.width` and `img.height`: the attribute, else the painted box, else the natural size.
fn rendered(context: &mut Context, node: NodeId, axis: Axis) -> u32 {
    let attribute = read_dom(context, |dom| dom.get_attr(node, axis.attribute()));
    if let Some(value) = attribute.and_then(|v| v.trim().parse::<f32>().ok()) {
        return value.max(0.) as u32;
    }
    let natural = report(context, node).natural;
    let painted = crate::layout_provider::fresh(context)
        .borrow()
        .rects
        .get(&node)
        .copied();
    match (painted, axis) {
        (Some(rect), Axis::Width) => rect.width.round() as u32,
        (Some(rect), Axis::Height) => rect.height.round() as u32,
        (None, Axis::Width) => natural.0,
        (None, Axis::Height) => natural.1,
    }
}

// ─── decode() ───────────────────────────────────────────────────────────────────────────────

/// A `decode()` promise waiting for the image: settled by [`deliver`].
#[derive(boa_engine::Trace, boa_engine::Finalize, boa_engine::JsData)]
struct PendingDecode {
    resolvers: ResolvingFunctions,
}

/// `img.decode()`: settled now when the image is, waiting for it otherwise.
fn decode_promise(context: &mut Context, node: NodeId) -> JsResult<JsValue> {
    let report = report(context, node);
    let decodable = report.status == Status::Loaded
        && shared(context)
            .borrow_mut()
            .images
            .decodes(report.url.as_deref());
    match (report.status, decodable) {
        (Status::Loading, _) => wait_for(context, node),
        (_, true) => native::settled(Ok(JsValue::undefined()), context),
        (_, false) => {
            let error = decode_error(context, UNDECODABLE);
            native::settled(Err(error), context)
        }
    }
}

fn wait_for(context: &mut Context, node: NodeId) -> JsResult<JsValue> {
    let (promise, resolvers) = boa_engine::object::builtins::JsPromise::new_pending(context);
    let pending = JsObject::from_proto_and_data(None, PendingDecode { resolvers });
    let weak = pending.downgrade();
    if let Some(wrapper) = bind::node::existing_wrapper(context, node) {
        verve::dom_events::hold(&wrapper, |held| held.push(pending));
    }
    let state = shared(context);
    let mut state = state.borrow_mut();
    state
        .images
        .nodes
        .entry(node)
        .or_default()
        .waiting
        .push(weak);
    Ok(promise.into())
}

fn decode_error(context: &mut Context, reason: &str) -> boa_engine::JsError {
    let message = format!("Failed to execute 'decode' on 'HTMLImageElement': {reason}");
    bind::dom_error(context, &message, "EncodingError")
}

/// Settles one waiting promise, and lets go of the object that kept it alive.
fn settle_promise(
    context: &mut Context,
    node: NodeId,
    promise: &WeakJsObject,
    failure: Option<&str>,
) -> JsResult<()> {
    let Some(pending) = promise.upgrade() else {
        return Ok(());
    };
    if let Some(wrapper) = bind::node::existing_wrapper(context, node) {
        verve::dom_events::hold(&wrapper, |held| held.retain(|object| *object != pending));
    }
    let resolvers = pending
        .downcast_ref::<PendingDecode>()
        .expect("a pending decode")
        .resolvers
        .clone();
    let (settle, value) = match failure {
        None => (resolvers.resolve, JsValue::undefined()),
        Some(message) => {
            let error = decode_error(context, message);
            (resolvers.reject, error.into_opaque(context)?)
        }
    };
    settle.call(&JsValue::undefined(), &[value], context)?;
    Ok(())
}
