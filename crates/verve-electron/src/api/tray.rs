//! `Tray`: an icon in the notification area (Windows), the menu bar (macOS) or the system tray.
//!
//! Electron 39.8.10 (observed on Windows): `new Tray()` with no argument is `TypeError:
//! Insufficient number of arguments.`, checked before anything else; before `ready` it is
//! `Error: Cannot create Tray before app is ready`; an argument that is neither a path nor a
//! `NativeImage` is `Error: Argument must be a file path or a NativeImage`. As in Electron a
//! `Tray` nothing refers to any more is collected and its icon goes with it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::object::WeakJsObject;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};
use verve::events::emitter_init;
use verve::native::{self, ClassSpec, NewOnly, arg, js_text};

use super::define::{self, Args, Fields, Kind};
use super::menu_state::{self, menu_of};
use super::native_image::image_arg;
use super::shared;
use crate::keyed::Keyed;
use crate::msg::{MenuOp, TrayEvent, TrayId, TrayOp};
use crate::pixels::NativeImage;

/// The kind of object a `Tray` is, in [`define`]'s table.
const TRAY: Kind = Kind("Tray");

/// The key a tray keeps the menu it was given under, so the menu lives as long as the icon.
const MENU_KEY: &str = "__contextMenu";

#[derive(Default)]
struct Trays {
    next: TrayId,
    live: Keyed<Record>,
}

struct Record {
    object: WeakJsObject,
    bounds: crate::msg::Rect,
    ignore_double_click: bool,
    title: String,
    guid: Option<String>,
}

fn trays(context: &mut Context) -> Rc<RefCell<Trays>> {
    if let Some(found) = context.get_data::<Rc<RefCell<Trays>>>() {
        return Rc::clone(found);
    }
    let made = Rc::<RefCell<Trays>>::default();
    context.insert_data(Rc::clone(&made));
    made
}

/// The `Tray` class, extending `EventEmitter`.
///
/// # Errors
///
/// The class could not be made.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    let emitter = define::emitter(context)?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "Tray",
            length: 2,
            init,
            parent: Some(&emitter),
            methods: &[
                ("destroy", 0, destroy),
                ("isDestroyed", 0, is_destroyed),
                ("setImage", 1, |t, a, c| image_op(t, a, c, TrayOp::Image)),
                ("setPressedImage", 1, set_pressed_image),
                ("setToolTip", 1, set_tool_tip),
                ("setTitle", 1, set_title),
                ("getTitle", 0, get_title),
                ("setIgnoreDoubleClickEvents", 1, set_ignore_double_click),
                ("getIgnoreDoubleClickEvents", 0, get_ignore_double_click),
                ("displayBalloon", 1, display_balloon),
                ("removeBalloon", 0, |t, _, c| {
                    plain_op(t, c, TrayOp::RemoveBalloon)
                }),
                ("focus", 0, |t, _, c| plain_op(t, c, TrayOp::Focus)),
                ("popUpContextMenu", 0, pop_up_context_menu),
                ("closeContextMenu", 0, |t, _, c| {
                    plain_op(t, c, TrayOp::CloseContextMenu)
                }),
                ("setContextMenu", 1, set_context_menu),
                ("getBounds", 0, get_bounds),
                ("getGUID", 0, get_guid),
            ],
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    Ok(class)
}

fn init(this: &JsObject, values: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    Args::new(values).require(1)?;
    let state = shared(context);
    if !state.app.is_ready() {
        return Err(define::error("Cannot create Tray before app is ready"));
    }
    let image = image_arg(&arg(values, 0))
        .ok_or_else(|| define::error("Argument must be a file path or a NativeImage"))?;
    let guid = arg(values, 1)
        .as_string()
        .map(|g| g.to_std_string_escaped());
    let registry = trays(context);
    let id = {
        let mut registry = registry.borrow_mut();
        registry.next += 1;
        let id = registry.next;
        registry.live.insert(
            id,
            Record {
                object: this.downgrade(),
                bounds: crate::msg::Rect::default(),
                ignore_double_click: false,
                title: String::new(),
                guid: guid.clone(),
            },
        );
        id
    };
    define::bind(this, TRAY, id, context);
    menu_state::send(
        context,
        MenuOp::TrayCreate {
            tray: id,
            image,
            guid,
        },
    );
    Ok(())
}

/// The id of the tray `this` is, while its icon exists. The text is in `electron.exe` beside the
/// names of `Tray`'s methods (39.8.10); its error class was not observed, since a destroyed tray
/// cannot be made without putting an icon on the desktop.
fn live_id(this: &JsValue, context: &mut Context) -> JsResult<TrayId> {
    let id = define::id_of(this, TRAY, context)
        .ok_or_else(|| native::type_error("Illegal invocation"))?;
    if trays(context).borrow().live.contains_key(&id) {
        Ok(id)
    } else {
        Err(define::error("Tray is destroyed"))
    }
}

fn destroy(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = define::id_of(this, TRAY, context)
        .ok_or_else(|| native::type_error("Illegal invocation"))?;
    if trays(context).borrow_mut().live.remove(&id).is_some() {
        menu_state::send(context, MenuOp::TrayDestroy { tray: id });
    }
    Ok(JsValue::undefined())
}

fn is_destroyed(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let registry = trays(context);
    let alive = define::id_of(this, TRAY, context)
        .is_some_and(|id| registry.borrow().live.contains_key(&id));
    Ok((!alive).into())
}

fn set(context: &mut Context, tray: TrayId, op: TrayOp) {
    menu_state::send(context, MenuOp::TraySet { tray, op });
}

fn plain_op(this: &JsValue, context: &mut Context, op: TrayOp) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    set(context, id, op);
    Ok(JsValue::undefined())
}

fn image_op(
    this: &JsValue,
    values: &[JsValue],
    context: &mut Context,
    op: fn(Arc<NativeImage>) -> TrayOp,
) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let image = image_arg(&arg(values, 0))
        .ok_or_else(|| define::error("Argument must be a file path or a NativeImage"))?;
    set(context, id, op(image));
    Ok(JsValue::undefined())
}

fn set_pressed_image(
    this: &JsValue,
    values: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let image = image_arg(&arg(values, 0));
    set(context, id, TrayOp::PressedImage(image));
    Ok(JsValue::undefined())
}

fn set_tool_tip(this: &JsValue, values: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let text = Args::new(values).string(0, "toolTip", context)?;
    set(context, id, TrayOp::ToolTip(text));
    Ok(JsValue::undefined())
}

fn set_title(this: &JsValue, values: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let title = Args::new(values).string(0, "title", context)?;
    if let Some(record) = trays(context).borrow_mut().live.get_mut(&id) {
        record.title.clone_from(&title);
    }
    set(context, id, TrayOp::Title(title));
    Ok(JsValue::undefined())
}

fn get_title(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let title = trays(context)
        .borrow()
        .live
        .get(&id)
        .map(|r| r.title.clone());
    Ok(js_text(&title.unwrap_or_default()))
}

fn set_ignore_double_click(
    this: &JsValue,
    values: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let ignore = arg(values, 0).to_boolean();
    if let Some(record) = trays(context).borrow_mut().live.get_mut(&id) {
        record.ignore_double_click = ignore;
    }
    set(context, id, TrayOp::IgnoreDoubleClick(ignore));
    Ok(JsValue::undefined())
}

fn get_ignore_double_click(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let ignore = trays(context)
        .borrow()
        .live
        .get(&id)
        .is_some_and(|r| r.ignore_double_click);
    Ok(ignore.into())
}

fn get_guid(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let guid = trays(context)
        .borrow()
        .live
        .get(&id)
        .and_then(|r| r.guid.clone());
    Ok(guid.map_or(JsValue::undefined(), |g| js_text(&g)))
}

fn display_balloon(this: &JsValue, values: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let options = Args::new(values).object_opt(0, "options", context)?;
    let fields = Fields::new(options.as_ref());
    let present = |name: &str, context: &mut Context| -> JsResult<bool> {
        Ok(fields.string(name, context)?.is_some())
    };
    if !present("title", context)? || !present("content", context)? {
        return Err(define::error("'title' and 'content' must be defined"));
    }
    let icon = options
        .as_ref()
        .map(|o| o.get(js_string!("icon"), context))
        .transpose()?
        .and_then(|icon| image_arg(&icon));
    let op = TrayOp::DisplayBalloon {
        icon,
        icon_type: fields
            .string("iconType", context)?
            .unwrap_or_else(|| "custom".into()),
        title: fields.string("title", context)?.unwrap_or_default(),
        content: fields.string("content", context)?.unwrap_or_default(),
        large_icon: fields.bool("largeIcon", true, context)?,
        no_sound: fields.bool("noSound", false, context)?,
        respect_quiet_time: fields.bool("respectQuietTime", false, context)?,
    };
    set(context, id, op);
    Ok(JsValue::undefined())
}

fn set_context_menu(
    this: &JsValue,
    values: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let menu = arg(values, 0);
    // `undefined` is not `null`: the message is in electron.exe (39.8.10); its class was not observed.
    let menu_id = if menu.is_null() {
        None
    } else if menu_of(&menu, context).is_some() {
        Some(menu_state::attach(&menu, context)?)
    } else {
        return Err(define::error("Must pass Menu or null."));
    };
    if let Some(object) = this.as_object() {
        native::hidden(&object, JsString::from(MENU_KEY), menu, context)?;
    }
    set(context, id, TrayOp::ContextMenu(menu_id));
    Ok(JsValue::undefined())
}

fn pop_up_context_menu(
    this: &JsValue,
    values: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let first = arg(values, 0);
    let (menu, position) = if menu_of(&first, context).is_some() {
        (Some(menu_state::attach(&first, context)?), arg(values, 1))
    } else {
        (None, first)
    };
    let position = position.as_object();
    let fields = Fields::new(position.as_ref());
    let at = match (fields.f64_opt("x", context)?, fields.f64_opt("y", context)?) {
        (Some(x), Some(y)) => Some((x as f32, y as f32)),
        _ => None,
    };
    set(context, id, TrayOp::PopUpContextMenu { menu, at });
    Ok(JsValue::undefined())
}

fn get_bounds(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = live_id(this, context)?;
    let bounds = trays(context)
        .borrow()
        .live
        .get(&id)
        .map(|r| r.bounds)
        .unwrap_or_default();
    Ok(rectangle(context, bounds)?.into())
}

fn rectangle(context: &mut Context, bounds: crate::msg::Rect) -> JsResult<JsObject> {
    define::record(
        context,
        &[
            ("x", bounds.x.round().into()),
            ("y", bounds.y.round().into()),
            ("width", bounds.width.round().into()),
            ("height", bounds.height.round().into()),
        ],
    )
}

/// The shell moved or resized the icon.
pub(crate) fn bounds_changed(context: &mut Context, tray: TrayId, bounds: crate::msg::Rect) {
    if let Some(record) = trays(context).borrow_mut().live.get_mut(&tray) {
        record.bounds = bounds;
    }
}

/// A person did something to the icon: the tray's event, with the arguments Electron gives it.
pub(crate) fn happened(
    context: &mut Context,
    tray: TrayId,
    event: TrayEvent,
    (bounds, position): (crate::msg::Rect, Option<(f32, f32)>),
    modifiers: verve_dom::Modifiers,
) {
    bounds_changed(context, tray, bounds);
    let object = trays(context)
        .borrow()
        .live
        .get(&tray)
        .and_then(|r| r.object.upgrade());
    let Some(object) = object else {
        return;
    };
    let emitted = emit(context, &object, event, bounds, position, modifiers);
    define::report(context, emitted);
}

fn emit(
    context: &mut Context,
    object: &JsObject,
    event: TrayEvent,
    bounds: crate::msg::Rect,
    position: Option<(f32, f32)>,
    modifiers: verve_dom::Modifiers,
) -> JsResult<()> {
    let keys = define::record(
        context,
        &[
            ("shiftKey", modifiers.shift.into()),
            ("ctrlKey", modifiers.ctrl.into()),
            ("altKey", modifiers.alt.into()),
            ("metaKey", modifiers.meta.into()),
        ],
    )?;
    let point = |context: &mut Context| -> JsResult<JsValue> {
        let (x, y) = position.unwrap_or_default();
        Ok(define::record(context, &[("x", x.round().into()), ("y", y.round().into())])?.into())
    };
    let rect = rectangle(context, bounds)?;
    let (name, args): (&str, Vec<JsValue>) = match event {
        TrayEvent::Click => ("click", vec![keys.into(), rect.into(), point(context)?]),
        TrayEvent::RightClick => ("right-click", vec![keys.into(), rect.into()]),
        TrayEvent::DoubleClick => ("double-click", vec![keys.into(), rect.into()]),
        TrayEvent::MiddleClick => ("middle-click", vec![keys.into(), rect.into()]),
        TrayEvent::BalloonShow => ("balloon-show", Vec::new()),
        TrayEvent::BalloonClick => ("balloon-click", Vec::new()),
        TrayEvent::BalloonClosed => ("balloon-closed", Vec::new()),
        TrayEvent::MouseEnter => ("mouse-enter", vec![keys.into(), point(context)?]),
        TrayEvent::MouseLeave => ("mouse-leave", vec![keys.into(), point(context)?]),
        TrayEvent::MouseMove => ("mouse-move", vec![keys.into(), point(context)?]),
        TrayEvent::DropFiles(files) => {
            let files = files.iter().map(|f| js_text(f)).collect();
            (
                "drop-files",
                vec![keys.into(), native::array(context, files)],
            )
        }
    };
    define::emit(context, object, name, &args)
}

/// Ends a turn: the icon of a `Tray` nothing refers to any more is removed.
pub(crate) fn sync(context: &mut Context) {
    let registry = trays(context);
    let gone: Vec<TrayId> = registry
        .borrow()
        .live
        .iter()
        .filter(|(_, record)| record.object.upgrade().is_none())
        .map(|(id, _)| *id)
        .collect();
    for id in gone {
        registry.borrow_mut().live.remove(&id);
        menu_state::send(context, MenuOp::TrayDestroy { tray: id });
    }
}
