//! What a page does when the main process's `webContents` reaches into it: runs a script and
//! answers with what it gave, adds or takes away a sheet, edits what is selected, and takes the
//! person's pointer and keys that `sendInputEvent` (or the shell) hands it.
//!
//! The page answers on its own outbox, in the order things happened, so the main process hears a
//! script's console lines before the script's result.

use boa_engine::object::builtins::{JsFunction, JsPromise};
use boa_engine::{Context, JsError, JsResult, JsValue};
use url::Url;
use verve::Runtime;
use verve::clone::Cloned;
use verve::native::{self, arg};
use verve_dom::{Document, Dom, NodeId, Pointer};

use crate::api::define;
use crate::api::ipc_renderer::outbox;
use crate::api::web_frame::{SCRIPT_FAILED, evaluate};
use crate::ipc;
use crate::msg::CompositionEvent;
use crate::msg::{ContextMenuParams, EditCommand, FromRenderer, PointerKind, ToRendererInput};
use crate::preload::world_numbered;

const NOT_CLONED: &str = "An object could not be cloned.";

/// Runs `code` in `world` (`0` is the page's own) and tells the main process, as `call`, what it
/// gave: a promise is waited for, a value that cannot be cloned or a script that throws is the
/// failure `executeJavaScript` rejects with.
pub(crate) fn run(runtime: &mut Runtime, code: &str, world: i32) -> Result<JsValue, JsError> {
    let context = runtime.context();
    let world = world_numbered(i64::from(world), context)?;
    evaluate(code, world, context)
}

/// Tells the main process, as `call`, how the script went: what it gave once a promise it gave is
/// settled, or the failure. Called after what the script asked of the window (`window.open`) was
/// queued, so the main process hears that first, as it does in Electron.
pub(crate) fn settle(runtime: &mut Runtime, call: u64, ran: Result<JsValue, JsError>) {
    let context = runtime.context();
    let waiting = match ran {
        Ok(value) => wait_for(context, call, &value),
        Err(error) => Err(error),
    };
    if waiting.is_err() {
        let failure = error_clone(context, SCRIPT_FAILED);
        reply(context, call, Err(failure));
    }
    runtime.run_microtasks();
}

/// Replies to `call` with what `value` settles to.
fn wait_for(context: &mut Context, call: u64, value: &JsValue) -> JsResult<()> {
    let promise = JsPromise::resolve(value.clone(), context)?;
    let kept = native::closure(
        context,
        "",
        1,
        |_, args, call: &u64, cx| {
            let result =
                Cloned::from_value(&arg(args, 0), cx).map_err(|_| error_clone(cx, NOT_CLONED));
            reply(cx, *call, result);
            Ok(JsValue::undefined())
        },
        call,
    );
    let broken = native::closure(
        context,
        "",
        1,
        |_, args, call: &u64, cx| {
            let reason = Cloned::from_value(&arg(args, 0), cx)
                .unwrap_or_else(|_| error_clone(cx, NOT_CLONED));
            reply(cx, *call, Err(reason));
            Ok(JsValue::undefined())
        },
        call,
    );
    let function = |object| JsFunction::from_object(object).expect("a closure is a function");
    promise.then(Some(function(kept)), Some(function(broken)), context)?;
    Ok(())
}

fn reply(context: &mut Context, call: u64, result: Result<Cloned, Cloned>) {
    let message = FromRenderer::ExecuteJavaScriptReply { call, result };
    outbox(context).borrow_mut().push(message);
}

/// An `Error` of `message`, cloned.
fn error_clone(context: &mut Context, message: &str) -> Cloned {
    ipc::error_value(message, context)
        .and_then(|error| {
            Cloned::from_value(&error, context).map_err(|_| ipc::clone_error(context))
        })
        .unwrap_or_else(|_| Cloned::undefined())
}

/// Adds the sheet `key`, the user's own when `user`.
pub(crate) fn insert_css(runtime: &mut Runtime, key: &str, css: &str, user: bool) {
    verve_dom::insert_stylesheet(runtime.context(), key, css, user);
}

/// Takes the sheet `key` away.
pub(crate) fn remove_css(runtime: &mut Runtime, key: &str) {
    verve_dom::remove_stylesheet(runtime.context(), key);
}

/// The menu's `execCommand` name for the commands the page's editing does.
fn command_name(command: &EditCommand) -> Option<&'static str> {
    Some(match command {
        EditCommand::Undo => "undo",
        EditCommand::Redo => "redo",
        EditCommand::Cut => "cut",
        EditCommand::Copy => "copy",
        EditCommand::Delete => "delete",
        EditCommand::SelectAll => "selectAll",
        _ => return None,
    })
}

/// Does an editing command to what the page has selected or focused. The commands the page's
/// editing knows go through the document's `exec_command`, as a person's would: `beforeinput` is
/// fired first (P24).
pub(crate) fn edit(runtime: &mut Runtime, document: &Document, command: EditCommand) {
    if let Some(name) = command_name(&command) {
        document.exec_command(runtime, name, "");
        return;
    }
    let context = runtime.context();
    let done = match command {
        EditCommand::InsertText(text) => return document.insert_text(runtime, &text),
        EditCommand::ScrollToTop => scroll_to(context, 0.),
        EditCommand::ScrollToBottom => scroll_to(context, 1e9),
        EditCommand::Unselect => selection(context)
            .and_then(|selection| native::invoke(&selection, "removeAllRanges", &[], context)),
        _ => return,
    };
    define::report(context, done);
    runtime.run_microtasks();
}

fn scroll_to(context: &mut Context, top: f64) -> JsResult<JsValue> {
    let window = context.global_object();
    native::invoke(&window.into(), "scrollTo", &[0.into(), top.into()], context)
}

fn selection(context: &mut Context) -> JsResult<JsValue> {
    let window = context.global_object();
    native::invoke(&window.into(), "getSelection", &[], context)
}

/// What a person did to the page. The pointer, the wheel, the keys and typed text act on the
/// document; a secondary button let go where the page did not prevent `contextmenu` is a context
/// menu asked for, which the main process is told of.
pub(crate) fn person(
    runtime: &mut Runtime,
    document: &Document,
    input: ToRendererInput,
) -> Option<FromRenderer> {
    match input {
        ToRendererInput::Pointer {
            kind: PointerKind::Down,
            pointer,
            target,
        } => {
            match live(document, target) {
                Some(node) => document.pointer_down_on(runtime, node, &pointer),
                None => document.pointer_down(runtime, &pointer),
            };
        }
        ToRendererInput::Pointer {
            kind: PointerKind::Up,
            pointer,
            target,
        } => {
            let allowed = match live(document, target) {
                Some(node) => document.pointer_up_on(runtime, node, &pointer),
                None => document.pointer_up(runtime, &pointer),
            };
            if pointer.button == 2 && allowed {
                let params = menu_params(runtime, document, &pointer, target);
                return Some(FromRenderer::ContextMenu(params));
            }
        }
        ToRendererInput::Pointer {
            kind: PointerKind::Move,
            pointer,
            target,
        } => match live(document, target) {
            Some(node) => document.pointer_move_on(runtime, node, &pointer),
            None => document.pointer_move(runtime, &pointer),
        },
        ToRendererInput::Pointer {
            kind: PointerKind::Leave,
            ..
        } => document.pointer_leave(runtime),
        ToRendererInput::Key { down: true, key } => {
            // The keydown is delivered here, once; the answer says whether the menu may have it.
            let unhandled = document.key_down_unhandled(runtime, &key);
            return unhandled.then_some(FromRenderer::KeyUnhandled(key));
        }
        ToRendererInput::Key { down: false, key } => {
            document.key_up(runtime, &key);
        }
        ToRendererInput::Text(text) => document.type_text(runtime, &text),
        ToRendererInput::Composition(event) => match event {
            CompositionEvent::Update { text, selection } => {
                document.compose(runtime, &text, selection)
            }
            CompositionEvent::Commit(text) => document.commit_composition(runtime, &text),
            CompositionEvent::Cancel => document.cancel_composition(runtime),
        },
        _ => {}
    }
    None
}

/// Where a context menu was asked for, as the page knows it.
fn menu_params(
    runtime: &mut Runtime,
    document: &Document,
    pointer: &Pointer,
    target: Option<NodeId>,
) -> ContextMenuParams {
    let node = target
        .or_else(|| document.hit_test(pointer.x, pointer.y))
        .unwrap_or_else(|| document.body());
    let base = Url::parse(&document.url()).ok();
    let mut params = describe(&document.dom(), node, base.as_ref());
    params.x = pointer.x;
    params.y = pointer.y;
    let context = runtime.context();
    let selected = selection(context)
        .and_then(|selection| native::invoke(&selection, "toString", &[], context))
        .and_then(|text| native::text_of(&text, context));
    params.selection_text = define::report(context, selected).unwrap_or_default();
    params
}

/// What the element `node` and the ones around it say of a context menu there: the link it is in,
/// the picture or media it is, whether it takes text.
fn describe(dom: &Dom, node: NodeId, base: Option<&Url>) -> ContextMenuParams {
    let resolve = |address: &str| match base {
        Some(base) => base
            .join(address)
            .map_or_else(|_| address.to_owned(), String::from),
        None => address.to_owned(),
    };
    let mut params = ContextMenuParams {
        media_type: "none".into(),
        ..ContextMenuParams::default()
    };
    let own = dom.get(node);
    let attr = |name: &str| {
        own.and_then(|n| n.attr(name))
            .unwrap_or_default()
            .to_owned()
    };
    params.title_text = attr("title");
    params.alt_text = attr("alt");
    match own.and_then(|n| n.tag()) {
        Some("img") => {
            params.media_type = "image".into();
            params.src_url = resolve(&attr("src"));
        }
        Some(kind @ ("audio" | "video")) => {
            params.media_type = kind.into();
            params.src_url = resolve(&attr("src"));
        }
        Some("canvas") => params.media_type = "canvas".into(),
        _ => {}
    }
    let mut at = Some(node);
    while let Some(id) = at {
        let ancestor = dom.get(id);
        if params.link_url.is_empty()
            && let Some(href) = ancestor
                .filter(|n| n.tag() == Some("a"))
                .and_then(|n| n.attr("href"))
        {
            params.link_url = resolve(href);
            params.link_text = dom.text_content(id);
        }
        params.is_editable |= ancestor.is_some_and(is_editable_field);
        at = dom.parent(id);
    }
    params.is_editable |= dom.is_editable(node);
    params
}

/// Whether the element is a text field a person can type in.
fn is_editable_field(node: &verve_dom::Node) -> bool {
    matches!(node.tag(), Some("input" | "textarea"))
        && node.attr("readonly").is_none()
        && node.attr("disabled").is_none()
}

/// The node the shell found under the pointer, if it is still in the document: a node a
/// re-render just took out must not take a click, so the page's own hit test answers instead.
pub(crate) fn live(document: &Document, target: Option<NodeId>) -> Option<NodeId> {
    let dom = document.dom();
    target.filter(|node| dom.is_connected(*node))
}
