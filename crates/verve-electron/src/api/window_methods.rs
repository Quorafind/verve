//! The members of `BrowserWindow.prototype`: every method and property that is a state of the window
//! or a message to the shell.
//!
//! They come from tables, as the many that only remember a flag or send one message would be a
//! function each otherwise: [`ACTIONS`] are the methods that send an operation and nothing else,
//! [`FLAGS`] the setter, getter and property of each on/off member, [`QUERIES`] the yes/no
//! questions the cache answers. Those three hold an index into their table in the function that
//! is made for them ([`install`]); the rest are plain tables of functions. The size and place are
//! [`window_geometry`](super::window_geometry)'s.
//!
//! Every method first asks for the window ([`record_of`]): it throws `Object has been destroyed`
//! once the window is gone, as Electron's native methods do. Members that exist on one system
//! only — the macOS document and represented file — do nothing on the others and answer their
//! default, as Electron 39 does on Windows.

use std::path::PathBuf;
use std::rc::Rc;

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue};
use verve::native::{self, arg, js_text};

use super::browser_window::{WINDOW, window_id_of};
use super::define::{self, Args, Fields};
use super::shared;
use super::window_events::flag_changed;
use super::window_geometry;
use super::window_lifecycle::destroy;
use super::window_options::parse_color;
use super::windows::WindowRecord;
use crate::msg::{ToShell, WindowOp};
use crate::window_state::{Flag, WindowState};

/// The window `this` stands for, while a script can use it: `Object has been destroyed` once it
/// is gone, and `Illegal invocation` for an instance of another class.
pub(crate) fn record_of(this: &JsValue, context: &mut Context) -> JsResult<Rc<WindowRecord>> {
    let state = shared(context);
    define::live(this, WINDOW, context, |window| {
        state.page(window)?;
        state.windows.live(window)
    })
    .map(|(_, record)| record)
}

/// Does `op` to the window: the cached state takes its effect at once, the shell is told.
pub(crate) fn act(context: &mut Context, record: &WindowRecord, op: WindowOp) {
    let state = shared(context);
    record.state.borrow_mut().predict(&op);
    state.follow_modal(record);
    send(context, record, op);
}

/// Tells the shell to do `op`, which has no effect on the cache.
fn send(context: &mut Context, record: &WindowRecord, op: WindowOp) {
    shared(context).hub.to_shell(ToShell::Window {
        window: record.id,
        op,
    });
}

/// Methods that do one thing to the window and take nothing.
type Action = (&'static str, fn() -> WindowOp);

const ACTIONS: &[Action] = &[
    ("show", || WindowOp::Show),
    ("showInactive", || WindowOp::ShowInactive),
    ("hide", || WindowOp::Hide),
    ("focus", || WindowOp::Focus),
    ("blur", || WindowOp::Blur),
    ("maximize", || WindowOp::Maximize),
    ("unmaximize", || WindowOp::Unmaximize),
    ("minimize", || WindowOp::Minimize),
    ("restore", || WindowOp::Restore),
    ("center", || WindowOp::Center),
    ("moveTop", || WindowOp::MoveTop),
];

/// One on/off member: its setter, its getter, its property, the message it sends and the event
/// it says when it changes.
struct FlagSpec {
    flag: Flag,
    set: &'static str,
    get: Option<&'static str>,
    property: Option<&'static str>,
    op: Option<fn(bool) -> WindowOp>,
    changed: Option<&'static str>,
    /// Whether the member is macOS's: elsewhere it is not remembered.
    mac_only: bool,
}

/// A [`FlagSpec`] with only its flag and setter, over which the rest is written.
const fn flag(flag: Flag, set: &'static str) -> FlagSpec {
    FlagSpec {
        flag,
        set,
        get: None,
        property: None,
        op: None,
        changed: None,
        mac_only: false,
    }
}

const FLAGS: &[FlagSpec] = &[
    FlagSpec {
        get: Some("isResizable"),
        property: Some("resizable"),
        op: Some(WindowOp::SetResizable),
        ..flag(Flag::Resizable, "setResizable")
    },
    FlagSpec {
        get: Some("isMovable"),
        property: Some("movable"),
        op: Some(WindowOp::SetMovable),
        ..flag(Flag::Movable, "setMovable")
    },
    FlagSpec {
        get: Some("isMinimizable"),
        property: Some("minimizable"),
        op: Some(WindowOp::SetMinimizable),
        ..flag(Flag::Minimizable, "setMinimizable")
    },
    FlagSpec {
        get: Some("isMaximizable"),
        property: Some("maximizable"),
        op: Some(WindowOp::SetMaximizable),
        ..flag(Flag::Maximizable, "setMaximizable")
    },
    FlagSpec {
        get: Some("isFullScreenable"),
        property: Some("fullScreenable"),
        ..flag(Flag::FullScreenable, "setFullScreenable")
    },
    FlagSpec {
        get: Some("isClosable"),
        property: Some("closable"),
        op: Some(WindowOp::SetClosable),
        ..flag(Flag::Closable, "setClosable")
    },
    FlagSpec {
        get: Some("isFocusable"),
        property: Some("focusable"),
        op: Some(WindowOp::SetFocusable),
        ..flag(Flag::Focusable, "setFocusable")
    },
    FlagSpec {
        get: Some("isAlwaysOnTop"),
        op: Some(WindowOp::SetAlwaysOnTop),
        changed: Some("always-on-top-changed"),
        ..flag(Flag::AlwaysOnTop, "setAlwaysOnTop")
    },
    FlagSpec {
        get: Some("isSimpleFullScreen"),
        property: Some("simpleFullScreen"),
        mac_only: true,
        ..flag(Flag::SimpleFullScreen, "setSimpleFullScreen")
    },
    FlagSpec {
        op: Some(WindowOp::SetSkipTaskbar),
        ..flag(Flag::SkipTaskbar, "setSkipTaskbar")
    },
    FlagSpec {
        get: Some("isContentProtected"),
        ..flag(Flag::ContentProtected, "setContentProtection")
    },
    FlagSpec {
        get: Some("isVisibleOnAllWorkspaces"),
        property: Some("visibleOnAllWorkspaces"),
        ..flag(Flag::VisibleOnAllWorkspaces, "setVisibleOnAllWorkspaces")
    },
    FlagSpec {
        get: Some("hasShadow"),
        property: Some("shadow"),
        ..flag(Flag::HasShadow, "setHasShadow")
    },
    FlagSpec {
        get: Some("isMenuBarAutoHide"),
        property: Some("autoHideMenuBar"),
        ..flag(Flag::MenuBarAutoHide, "setAutoHideMenuBar")
    },
    FlagSpec {
        get: Some("isMenuBarVisible"),
        property: Some("menuBarVisible"),
        ..flag(Flag::MenuBarVisible, "setMenuBarVisibility")
    },
    FlagSpec {
        get: Some("isDocumentEdited"),
        property: Some("documentEdited"),
        mac_only: true,
        ..flag(Flag::DocumentEdited, "setDocumentEdited")
    },
    FlagSpec {
        get: Some("isEnabled"),
        op: Some(WindowOp::SetEnabled),
        ..flag(Flag::Enabled, "setEnabled")
    },
    FlagSpec {
        mac_only: true,
        ..flag(Flag::HiddenInMissionControl, "setHiddenInMissionControl")
    },
];

/// The yes/no questions the cached state answers.
type Query = (&'static str, fn(&WindowState) -> bool);

const QUERIES: &[Query] = &[
    ("isFocused", |s| s.focused),
    ("isVisible", WindowState::is_visible),
    ("isMinimized", |s| s.minimized),
    ("isMaximized", |s| s.maximized),
    ("isNormal", |s| {
        !s.minimized && !s.maximized && !s.fullscreen
    }),
    ("isFullScreen", |s| s.fullscreen),
    // On Windows a kiosk window is a full-screen one.
    ("isKiosk", |s| s.fullscreen),
    ("isTabletMode", |_| false),
    ("isSnapped", |_| false),
];

/// Makes the members of the tables into the prototype's own.
pub(crate) fn install(context: &mut Context, prototype: &JsObject) -> JsResult<()> {
    for (index, (name, _)) in ACTIONS.iter().enumerate() {
        let method = native::closure(context, name, 0, run_action, index);
        native::hidden(prototype, JsString::from(*name), method, context)?;
    }
    for (index, spec) in FLAGS.iter().enumerate() {
        let setter = native::closure(context, spec.set, 1, set_flag, index);
        native::hidden(prototype, JsString::from(spec.set), setter, context)?;
        if let Some(name) = spec.get {
            let getter = native::closure(context, name, 0, get_flag, index);
            native::hidden(prototype, JsString::from(name), getter, context)?;
        }
        if let Some(name) = spec.property {
            let get = native::closure(context, name, 0, get_flag, index);
            let set = native::closure(context, name, 1, set_flag, index);
            let property = PropertyDescriptor::builder()
                .get(get)
                .set(set)
                .enumerable(false)
                .configurable(true);
            prototype.define_property_or_throw(JsString::from(name), property, context)?;
        }
    }
    for (index, (name, _)) in QUERIES.iter().enumerate() {
        let query = native::closure(context, name, 0, ask, index);
        native::hidden(prototype, JsString::from(*name), query, context)?;
    }
    native::methods(context, prototype, METHODS)?;
    native::methods(context, prototype, window_geometry::METHODS)?;
    native::methods(context, prototype, NOTHING)?;
    native::accessors(context, prototype, ACCESSORS)
}

fn run_action(
    this: &JsValue,
    _: &[JsValue],
    index: &usize,
    context: &mut Context,
) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    act(context, &record, ACTIONS[*index].1());
    Ok(JsValue::undefined())
}

fn set_flag(
    this: &JsValue,
    args: &[JsValue],
    index: &usize,
    context: &mut Context,
) -> JsResult<JsValue> {
    let spec = &FLAGS[*index];
    let record = record_of(this, context)?;
    let on = arg(args, 0).to_boolean();
    if spec.mac_only && !cfg!(target_os = "macos") {
        return Ok(JsValue::undefined());
    }
    let was = {
        let mut state = record.state.borrow_mut();
        let was = state.flag(spec.flag);
        state.set_flag(spec.flag, on);
        was
    };
    if let Some(op) = spec.op {
        send(context, &record, op(on));
    }
    if let (Some(name), true) = (spec.changed, was != on) {
        flag_changed(context, &record, name, on);
    }
    Ok(JsValue::undefined())
}

fn get_flag(
    this: &JsValue,
    _: &[JsValue],
    index: &usize,
    context: &mut Context,
) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let on = record.state.borrow().flag(FLAGS[*index].flag);
    Ok(on.into())
}

fn ask(this: &JsValue, _: &[JsValue], index: &usize, context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let answer = QUERIES[*index].1(&record.state.borrow());
    Ok(answer.into())
}

/// Methods that are worth a window of their own state, or a message with something in it.
const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("destroy", 0, |this, _, cx| {
        if let Some(window) = define::id_of(this, WINDOW, cx) {
            destroy(cx, window);
        }
        Ok(JsValue::undefined())
    }),
    ("close", 0, |this, _, cx| {
        let record = record_of(this, cx)?;
        send(cx, &record, WindowOp::Close);
        Ok(JsValue::undefined())
    }),
    ("isDestroyed", 0, |this, _, cx| {
        let window = define::id_of(this, WINDOW, cx);
        Ok(shared(cx).windows.is_gone(window).into())
    }),
    ("setFullScreen", 1, set_full_screen),
    ("setKiosk", 1, set_full_screen),
    ("setTitle", 1, set_title),
    ("getTitle", 0, get_title),
    ("setOpacity", 1, |this, args, cx| {
        let opacity = Args::new(args).f64(0, "opacity", cx)?;
        let record = record_of(this, cx)?;
        act(cx, &record, WindowOp::SetOpacity(opacity.clamp(0., 1.)));
        Ok(JsValue::undefined())
    }),
    ("getOpacity", 0, |this, _, cx| {
        Ok(record_of(this, cx)?.state.borrow().opacity.into())
    }),
    ("setBackgroundColor", 1, |this, args, cx| {
        let text = Args::new(args).string(0, "color", cx)?;
        let record = record_of(this, cx)?;
        let color = parse_color(&text).unwrap_or([255; 4]);
        act(cx, &record, WindowOp::SetBackgroundColor(color));
        Ok(JsValue::undefined())
    }),
    ("getBackgroundColor", 0, |this, _, cx| {
        Ok(js_text(
            &record_of(this, cx)?.state.borrow().background_text(),
        ))
    }),
    ("setIcon", 1, set_icon),
    ("setProgressBar", 1, set_progress_bar),
    ("flashFrame", 1, |this, args, cx| {
        let record = record_of(this, cx)?;
        send(cx, &record, WindowOp::FlashFrame(arg(args, 0).to_boolean()));
        Ok(JsValue::undefined())
    }),
    ("setIgnoreMouseEvents", 1, |this, args, cx| {
        let record = record_of(this, cx)?;
        let ignore = arg(args, 0).to_boolean();
        send(cx, &record, WindowOp::SetIgnoreMouseEvents(ignore));
        Ok(JsValue::undefined())
    }),
    ("setParentWindow", 1, set_parent),
    ("getParentWindow", 0, get_parent),
    ("getChildWindows", 0, |this, _, cx| {
        let record = record_of(this, cx)?;
        let children = shared(cx).windows.children_of(record.id);
        let objects = children.iter().filter(|c| !c.dead.get());
        let objects = objects.map(|c| c.object.clone().into()).collect();
        Ok(native::array(cx, objects))
    }),
    ("isModal", 0, |this, _, cx| {
        Ok(record_of(this, cx)?.modal.into())
    }),
    ("getNativeWindowHandle", 0, |this, _, cx| {
        let handle = record_of(this, cx)?.state.borrow().handle;
        let bytes = handle.to_le_bytes().map(JsValue::from).to_vec();
        let bytes = native::array(cx, bytes);
        let buffer = native::global(cx, "Buffer")?;
        native::invoke(&buffer, "from", &[bytes], cx)
    }),
    ("getMediaSourceId", 0, |this, _, cx| {
        let handle = record_of(this, cx)?.state.borrow().handle;
        Ok(js_text(&format!("window:{handle}:1")))
    }),
    ("hookWindowMessage", 2, |this, args, cx| {
        let message = Args::new(args).f64(0, "message", cx)?;
        record_of(this, cx)?
            .state
            .borrow_mut()
            .hooked
            .insert(message as u32);
        Ok(JsValue::undefined())
    }),
    ("unhookWindowMessage", 1, |this, args, cx| {
        let message = Args::new(args).f64(0, "message", cx)?;
        record_of(this, cx)?
            .state
            .borrow_mut()
            .hooked
            .remove(&(message as u32));
        Ok(JsValue::undefined())
    }),
    ("isWindowMessageHooked", 1, |this, args, cx| {
        let message = Args::new(args).f64(0, "message", cx)?;
        let hooked = record_of(this, cx)?
            .state
            .borrow()
            .hooked
            .contains(&(message as u32));
        Ok(hooked.into())
    }),
    ("setThumbarButtons", 1, |this, _, cx| {
        record_of(this, cx)?;
        Ok(true.into())
    }),
    ("setRepresentedFilename", 1, |this, args, cx| {
        let path = Args::new(args).string(0, "filename", cx)?;
        let record = record_of(this, cx)?;
        if cfg!(target_os = "macos") {
            record.state.borrow_mut().represented_filename = path;
        }
        Ok(JsValue::undefined())
    }),
    ("getRepresentedFilename", 0, |this, _, cx| {
        Ok(js_text(
            &record_of(this, cx)?.state.borrow().represented_filename,
        ))
    }),
];

/// Members of other systems and of the menu, which a window remembers nothing of: they do
/// nothing, once the window has been asked for.
const NOTHING: &[(&str, usize, NativeFunctionPointer)] = &[
    ("setAspectRatio", 1, nothing),
    ("moveAbove", 1, nothing),
    ("focusOnWebView", 0, nothing),
    ("blurWebView", 0, nothing),
    ("invalidateShadow", 0, nothing),
    ("setVibrancy", 1, nothing),
    ("setBackgroundMaterial", 1, nothing),
    ("setTitleBarOverlay", 1, nothing),
    ("setShape", 1, nothing),
    ("setOverlayIcon", 2, nothing),
    ("setAutoHideCursor", 1, nothing),
    ("setWindowButtonVisibility", 1, nothing),
    ("setWindowButtonPosition", 1, nothing),
    ("setSheetOffset", 1, nothing),
    ("previewFile", 1, nothing),
    ("closeFilePreview", 0, nothing),
    ("showDefinitionForSelection", 0, nothing),
    ("selectPreviousTab", 0, nothing),
    ("selectNextTab", 0, nothing),
    ("showAllTabs", 0, nothing),
    ("mergeAllWindows", 0, nothing),
    ("moveTabToNewWindow", 0, nothing),
    ("toggleTabBar", 0, nothing),
    ("addTabbedWindow", 1, nothing),
    ("setTouchBar", 1, nothing),
    ("setThumbnailClip", 1, nothing),
    ("setThumbnailToolTip", 1, nothing),
    ("setAppDetails", 1, nothing),
    ("unhookAllWindowMessages", 0, nothing),
    // P15 gives the window a menu bar; until then there is none to set or remove.
    ("setMenu", 1, nothing),
    ("removeMenu", 0, nothing),
];

fn nothing(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    record_of(this, context)?;
    Ok(JsValue::undefined())
}

/// Properties that are more than a flag: the full-screen and kiosk states, the title.
const ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    ("fullScreen", is_full_screen, Some(set_full_screen)),
    ("kiosk", is_full_screen, Some(set_full_screen)),
    ("title", get_title, Some(set_title)),
    (
        "accessibleTitle",
        get_accessible_title,
        Some(set_accessible_title),
    ),
    (
        "snapped",
        |this, _, cx| {
            record_of(this, cx)?;
            Ok(false.into())
        },
        None,
    ),
    (
        "representedFilename",
        |this, _, cx| {
            Ok(js_text(
                &record_of(this, cx)?.state.borrow().represented_filename,
            ))
        },
        None,
    ),
];

fn is_full_screen(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(record_of(this, context)?.state.borrow().fullscreen.into())
}

fn set_full_screen(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let on = arg(args, 0).to_boolean();
    let allowed = record.state.borrow().flag(Flag::FullScreenable) || !on;
    if allowed {
        act(context, &record, WindowOp::SetFullScreen(on));
    }
    Ok(JsValue::undefined())
}

fn set_title(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let title = Args::new(args).string(0, "title", context)?;
    let record = record_of(this, context)?;
    act(context, &record, WindowOp::SetTitle(title));
    Ok(JsValue::undefined())
}

fn get_title(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(js_text(&record_of(this, context)?.state.borrow().title))
}

/// `accessibleTitle`: the window's own title until one is given.
fn get_accessible_title(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let given = record.accessible_title.borrow().clone();
    Ok(js_text(
        &given.unwrap_or_else(|| record.state.borrow().title.clone()),
    ))
}

fn set_accessible_title(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let title = Args::new(args).string(0, "title", context)?;
    *record_of(this, context)?.accessible_title.borrow_mut() = Some(title);
    Ok(JsValue::undefined())
}

/// `setIcon(path | image)`: a path that names no file is an `Error`; an image is P14's.
fn set_icon(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    if let Some(path) = arg(args, 0).as_string() {
        let path = PathBuf::from(path.to_std_string_escaped());
        if !path.is_file() {
            let message = format!("Failed to load image from path '{}'", path.display());
            return Err(define::error(&message));
        }
        send(context, &record, WindowOp::SetIcon(path));
    }
    Ok(JsValue::undefined())
}

/// `setProgressBar(progress[, { mode }])`: `-1` removes the bar and a `mode` of `'indeterminate'`
/// is the shell's `2`.
fn set_progress_bar(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let progress = Args::new(args).f64(0, "progress", context)?;
    let record = record_of(this, context)?;
    let options = arg(args, 1).as_object();
    let mode = Fields::new(options.as_ref()).string("mode", context)?;
    let shown = match mode.as_deref() {
        Some("none") => -1.,
        Some("indeterminate") => 2.,
        _ => progress,
    };
    send(context, &record, WindowOp::SetProgressBar(shown));
    Ok(JsValue::undefined())
}

/// `setParentWindow(parent | null)`. As in Electron 39.8.10, a modal window keeps its parent
/// (`Can not be called for modal window`), `undefined` is `null`, and anything else that is no
/// window is `Must pass BaseWindow instance or null`. A window is not its own ancestor: that is
/// Verve's rule, as Electron has no check.
fn set_parent(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    if record.modal {
        return Err(define::error("Can not be called for modal window"));
    }
    let Some(value) = args.first() else {
        return Err(native::type_error("Insufficient number of arguments."));
    };
    let parent = if value.is_null_or_undefined() {
        None
    } else if window_id_of(value, context).is_none() {
        return Err(define::error("Must pass BaseWindow instance or null"));
    } else {
        Some(record_of(value, context)?.id)
    };
    if parent.is_some_and(|parent| shared(context).windows.descends_from(parent, record.id)) {
        return Err(define::error("A window cannot be its own ancestor"));
    }
    record.parent.set(parent);
    send(context, &record, WindowOp::SetParent(parent));
    Ok(JsValue::undefined())
}

fn get_parent(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let state = shared(context);
    let parent = record.parent.get().and_then(|id| state.windows.live(id));
    Ok(parent.map_or(JsValue::null(), |parent| parent.object.clone().into()))
}
