//! One window of the app: the GPUI window, the system window under it, and what each reports.
//!
//! GPUI makes the window; [`platform`] does to it what Electron's windows are asked and GPUI's
//! are not; [`Reporter`] tells the main process what became of it. A window is made hidden and
//! shown by [`open`] once it has its frame, place and limits, so it never appears in any other.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, AsyncApp, Bounds, PromptButton, PromptLevel, Rgba, TitlebarOptions,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowOptions as GpuiOptions, point, px, rgba, size,
};
use verve_electron::Hub;
use verve_electron::ids::{ContentsId, WindowId};
use verve_electron::msg::{
    Display, FromShell, Rect, TitleBarStyle, ToRendererInput, ToShell, WindowEvent, WindowOp,
    WindowOptions,
};
use verve_layout::{DisplayList, ImageTable};

use crate::app_region::AppRegion;
use crate::dom_view::{DomView, Shown, white};
use crate::input::KeyRoute;
use crate::platform::{self, Native, Target};
use crate::report::Reporter;
use crate::shell::{Inbox, Item};

/// How long the main process may take to answer a close before a second try closes the window
/// without it: the page is not responding.
const UNRESPONSIVE: Duration = Duration::from_secs(5);

/// What a window draws: its display list, the size its page was drawn at, the color under it, and
/// the pictures the page has decoded.
pub type Look = (Arc<DisplayList>, (f32, f32), Option<Rgba>, Arc<ImageTable>);

/// A window the shell has open.
pub(crate) struct Window {
    handle: WindowHandle<DomView>,
    pub contents: ContentsId,
    /// The window this one is a child of: it goes when its parent does.
    pub parent: Option<WindowId>,
    /// Whether the system draws a frame: Electron shows no menu bar in a window without one.
    #[cfg(any(windows, target_os = "macos"))]
    pub framed: bool,
    native: Rc<Native>,
    reporter: Rc<Reporter>,
    asked: Rc<Cell<Option<Instant>>>,
    /// The regions the last display list had, to see when they change.
    regions: RefCell<Vec<AppRegion>>,
}

/// What a window needs to be made.
pub(crate) struct Opening<'a> {
    pub hub: &'a Arc<Hub>,
    pub inbox: &'a Inbox,
    pub id: WindowId,
    pub contents: ContentsId,
    pub options: &'a WindowOptions,
    pub owner: Option<&'a Window>,
}

/// Whether the system draws a title bar on this window.
pub(crate) fn framed(options: &WindowOptions) -> bool {
    options.frame && options.title_bar_style == TitleBarStyle::Default
}

/// The color under a window's page: none for a transparent window, white unless it was given.
fn background(options: &WindowOptions) -> Option<Rgba> {
    let given = options.background_color.map(color_of);
    (!options.transparent).then(|| given.unwrap_or_else(white))
}

/// `[r, g, b, a]` as the color GPUI paints.
fn color_of([r, g, b, a]: [u8; 4]) -> Rgba {
    rgba(u32::from_be_bytes([r, g, b, a]))
}

/// Whether the system draws its windows dark.
fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// Where the window opens: where it was asked to be, else the middle of the work area (the whole
/// display for a full-screen window).
pub(crate) fn opening_bounds(options: &WindowOptions, display: &Display) -> Rect {
    let area = display.work_area;
    if options.fullscreen {
        return display.bounds;
    }
    Rect {
        x: options
            .x
            .unwrap_or(area.x + (area.width - options.width) / 2.),
        y: options
            .y
            .unwrap_or(area.y + (area.height - options.height) / 2.),
        width: options.width,
        height: options.height,
    }
}

/// GPUI's options for a window: the part of Electron's it has a field for, always hidden (the
/// platform layer shows it) and always a normal window (a modal one is the platform layer's).
pub(crate) fn gpui_options(options: &WindowOptions, bounds: Rect) -> GpuiOptions {
    let at = Bounds::new(
        point(px(bounds.x), px(bounds.y)),
        size(px(bounds.width), px(bounds.height)),
    );
    GpuiOptions {
        window_bounds: Some(WindowBounds::Windowed(at)),
        titlebar: Some(TitlebarOptions {
            title: Some(options.title.clone().into()),
            appears_transparent: !framed(options),
            traffic_light_position: (options.title_bar_style == TitleBarStyle::HiddenInset)
                .then(|| point(px(12.), px(12.))),
        }),
        show: false,
        focus: false,
        is_resizable: options.resizable,
        is_movable: options.movable,
        app_owns_titlebar_drag: cfg!(target_os = "macos"),
        is_minimizable: options.minimizable,
        window_background: if options.transparent {
            WindowBackgroundAppearance::Transparent
        } else {
            WindowBackgroundAppearance::Opaque
        },
        window_min_size: options
            .min_size
            .map(|(width, height)| size(px(width), px(height))),
        window_decorations: (!options.frame).then_some(gpui::WindowDecorations::Client),
        ..Default::default()
    }
}

/// Opens the window `opening` asks for, hidden, gives it its frame, place and limits, then shows
/// it as `options.show` says. `None` when the system would not make one.
pub(crate) fn open(opening: Opening, display: &Display, cx: &mut AsyncApp) -> Option<Window> {
    let Opening {
        hub,
        inbox,
        id,
        contents,
        options,
        owner,
    } = opening;
    let bounds = opening_bounds(options, display);
    let color = background(options);
    let opened = cx.open_window(gpui_options(options, bounds), |_, cx| {
        cx.new(|cx| {
            let mut view = DomView::new(color);
            view.attach(cx);
            view
        })
    });
    let handle = match opened {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("electron: no window for {id}: {error}");
            return None;
        }
    };
    let target = handle.update(cx, |_, window, _| Target::of(window)).ok()?;
    let native = Native::attach(target, options, owner.map(|owner| &*owner.native), cx);
    native.place(bounds);
    let reporter = Reporter::new(Arc::clone(hub), id, contents, native.observe());
    let asked = Rc::new(Cell::new(None));
    let window = Window {
        handle,
        contents,
        parent: options.parent,
        #[cfg(any(windows, target_os = "macos"))]
        framed: framed(options),
        native,
        reporter,
        asked,
        regions: RefCell::new(Vec::new()),
    };
    window.guard(cx, options.closable, (hub, inbox), id);
    window.start(options, cx);
    Some(window)
}

impl Window {
    /// Makes the user's close ask the main process, and the first frame report `FirstPaint`.
    fn guard(
        &self,
        cx: &mut AsyncApp,
        closable: bool,
        (hub, inbox): (&Arc<Hub>, &Inbox),
        id: WindowId,
    ) {
        let inbox_for_gone = inbox.clone();
        let (reporter, asked, inbox) = (
            Rc::clone(&self.reporter),
            Rc::clone(&self.asked),
            inbox.clone(),
        );
        let themed = inbox.clone();
        let (native, painted) = (Rc::clone(&self.native), Rc::clone(&self.reporter));
        let told = Rc::clone(&self.reporter);
        let route = KeyRoute::new(Arc::clone(hub), self.contents);
        let (seen, contents) = (Arc::clone(hub), self.contents);
        // An error is a window that is gone before it was set up, and it needs nothing then.
        let _ = self.handle.update(cx, move |view, window, cx| {
            view.on_input(Rc::new(move |input| told.input(input)));
            view.on_keys(Rc::new(move |down, key| route.send(down, key)));
            view.on_report(Rc::new(move |event| {
                seen.from_shell(FromShell::Page { contents, event });
            }));
            // The page has the keyboard from the start; a click on it takes it back.
            if let Some(focus) = view.focus_handle() {
                window.focus(&focus, cx);
            }
            view.on_draw(Rc::new(move |window| {
                if let Some(look) = native.looked(window) {
                    painted.sync(look);
                }
                // The first frame of a shown window is looked for until it is found.
                if !painted.is_painted() && native.observe().visible {
                    painted.painted();
                }
            }));
            window
                .observe_window_appearance(move |window, _| {
                    themed.push_item(Item::Theme(is_dark(window.appearance())));
                })
                .detach();
            // A person asking for the window to close is the page's to answer: the main process
            // asks it, and says back whether a listener kept the window open. One that has not
            // answered in a while is not answering: the next try closes the window.
            window.on_window_should_close(cx, move |_, _| {
                if closable {
                    user_closes(&asked, &reporter, &inbox, id);
                }
                false
            });
        });
        self.watch();
        // The system destroys a window by itself when GPUI's close callback cannot answer (the app
        // is borrowed or quitting) and when GPUI shuts down: the main process hears of it, and
        // the shell forgets the window.
        let (gone, inbox) = (Rc::clone(&self.reporter), inbox_for_gone.clone());
        self.native.on_destroy(Rc::new(move || {
            gone.close();
            inbox.push_item(Item::Gone(id));
        }));
    }

    /// Looks at the window whenever the system says it changed.
    fn watch(&self) {
        let (native, reporter) = (Rc::downgrade(&self.native), Rc::clone(&self.reporter));
        self.native.watch(Rc::new(move || {
            if let Some(native) = native.upgrade() {
                reporter.sync(native.observe());
            }
        }));
    }

    /// The window is complete: it is shown and covers its display as the options say, and the page
    /// and the main process are told its handle.
    fn start(&self, options: &WindowOptions, cx: &mut AsyncApp) {
        if options.always_on_top || options.kiosk {
            self.native.set_always_on_top(true);
        }
        if options.fullscreen || options.kiosk {
            self.native.set_fullscreen(true);
        }
        if let Some(icon) = &options.icon {
            self.native.set_icon(icon);
        }
        if options.show {
            self.show_now(options.focusable, cx);
        }
        let first = self.native.observe();
        self.reporter.begin(first);
        self.reporter
            .say(WindowEvent::NativeHandle(self.native.handle()));
        let dark = cx.update(|cx| is_dark(cx.window_appearance()));
        self.reporter.input(ToRendererInput::ColorScheme { dark });
    }

    /// Does `op` to the window and tells what came of it. Closing is the shell's.
    pub fn apply(&self, op: WindowOp, cx: &mut AsyncApp) {
        let native = &self.native;
        match op {
            WindowOp::Show => self.show_now(true, cx),
            WindowOp::ShowInactive => self.show_now(false, cx),
            WindowOp::Hide => native.hide(),
            WindowOp::Focus => native.focus(),
            WindowOp::Blur => native.blur(),
            WindowOp::Minimize => native.minimize(),
            WindowOp::Maximize => native.maximize(),
            WindowOp::Restore => native.restore(),
            WindowOp::Unmaximize if native.observe().maximized => native.restore(),
            WindowOp::SetFullScreen(on) => native.set_fullscreen(on),
            WindowOp::SetBounds {
                x,
                y,
                width,
                height,
                ..
            } => native.place(merged(native.observe().bounds, x, y, width, height)),
            WindowOp::SetMinSize(width, height) => native.limit(Some((width, height)), None),
            WindowOp::SetMaxSize(width, height) => native.limit(None, Some((width, height))),
            WindowOp::Center => native.place(centred(native.observe().bounds, native.work_area())),
            WindowOp::SetTitle(title) => native.set_title(&title),
            WindowOp::SetResizable(resizable) => native.set_resizable(resizable),
            WindowOp::SetAlwaysOnTop(on) => native.set_always_on_top(on),
            WindowOp::SetBackgroundColor(color) => self.set_background(color, cx),
            WindowOp::SetProgressBar(progress) => native.set_progress(progress),
            WindowOp::SetIcon(path) => native.set_icon(&path),
            WindowOp::FlashFrame(on) => native.flash(on),
            WindowOp::SetOpacity(opacity) => native.set_opacity(opacity),
            WindowOp::SetSkipTaskbar(skip) => native.set_skip_taskbar(skip),
            WindowOp::SetIgnoreMouseEvents(ignore) => native.set_ignore_mouse(ignore),
            WindowOp::SetClosable(closable) => native.set_closable(closable),
            WindowOp::SetEnabled(enabled) => native.set_enabled(enabled),
            // P34: the platform layer acts on these; the main process remembers them and answers
            // for them meanwhile.
            WindowOp::SetMovable(_)
            | WindowOp::SetMinimizable(_)
            | WindowOp::SetMaximizable(_)
            | WindowOp::SetFocusable(_)
            | WindowOp::SetParent(_)
            | WindowOp::MoveTop => {}
            // Closing is the shell's.
            WindowOp::Close | WindowOp::CloseAnswer { .. } | WindowOp::Destroy => return,
            // `Unmaximize` of a window that is not maximized does nothing.
            WindowOp::Unmaximize => {}
        }
        self.reporter.sync(native.observe());
    }

    /// The system's handle of the window: the `HWND`, or the `NSView` on macOS.
    #[cfg(any(windows, target_os = "macos"))]
    pub fn raw_handle(&self) -> isize {
        self.native.handle() as isize
    }

    /// Shows the window and asks for a frame, so that `FirstPaint` comes though no page changed.
    fn show_now(&self, activate: bool, cx: &mut AsyncApp) {
        self.native.show(activate);
        self.show(|_| {}, cx);
    }

    fn set_background(&self, [r, g, b, a]: [u8; 4], cx: &mut AsyncApp) {
        let color = color_of([r, g, b, a]);
        // An error is a window that is gone, and there is nothing left to paint.
        let _ = self.handle.update(cx, |view, _, cx| {
            view.set_background(Some(color));
            cx.notify();
        });
    }

    /// Hands the window what its page published and the regions it drags by.
    pub fn show(&self, update: impl FnOnce(&mut DomView) + 'static, cx: &mut AsyncApp) {
        // An error is a window that is gone, and there is nothing left to hand it.
        let _ = self.handle.update(cx, |view, _, cx| {
            update(view);
            cx.notify();
        });
    }

    /// Shows a prompt over the window; the answer is the index of the button pressed, `None` when
    /// the prompt went away unanswered. `None` at once for a window that is gone.
    pub fn prompt(
        &self,
        level: PromptLevel,
        message: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
        cx: &mut AsyncApp,
    ) -> Option<impl Future<Output = Option<usize>> + use<>> {
        let asked = self.handle.update(cx, |_, window, cx| {
            window.prompt(level, message, detail, answers, cx)
        });
        asked.ok().map(|chosen| async move { chosen.await.ok() })
    }

    /// Hands the window the display list its page published, and the regions it is dragged by if
    /// the list is a new one. The same list again only ends the wheel's prediction.
    pub fn show_list(&self, list: Arc<DisplayList>, cx: &mut AsyncApp) {
        let shown = self.handle.update(cx, |view, _, cx| {
            let shown = view.show(Arc::clone(&list));
            if shown != Shown::Unchanged {
                cx.notify();
            }
            shown
        });
        if !matches!(shown, Ok(Shown::New)) {
            return;
        }
        let regions = crate::app_region::regions_of(&list);
        if *self.regions.borrow() != regions {
            self.regions.replace(regions.clone());
            self.set_app_regions(regions, cx);
        }
    }

    /// Hands the window the page its renderer published and where its caret is: what a link, a
    /// field and a cursor are read from, and where the input method puts its candidate window.
    pub fn show_page(
        &self,
        snapshot: verve_dom::Snapshot,
        caret: Option<verve_dom::Rect>,
        cx: &mut AsyncApp,
    ) {
        let _ = self.handle.update(cx, |view, window, cx| {
            if view.set_page(snapshot, caret) {
                window.invalidate_character_coordinates();
                cx.notify();
            }
        });
    }

    /// Hands the window the pictures its page has decoded.
    pub fn show_images(&self, table: Arc<ImageTable>, cx: &mut AsyncApp) {
        let _ = self.handle.update(cx, |view, _, cx| {
            if view.show_images(table) {
                cx.notify();
            }
        });
    }

    /// What the window draws now: its display list, how big the page was drawn, the color under
    /// it and the pictures the list names. `None` for a window that is gone.
    pub fn look(&self, cx: &mut AsyncApp) -> Option<Look> {
        self.handle
            .update(cx, |view, _, _| {
                Some((
                    view.list()?,
                    view.viewport(),
                    view.background(),
                    view.images(),
                ))
            })
            .ok()
            .flatten()
    }

    /// Sets the regions the window is dragged by.
    pub fn set_app_regions(&self, regions: Vec<AppRegion>, cx: &mut AsyncApp) {
        self.native
            .set_drag_area(crate::app_region::drag_area(&regions));
        self.show(move |view| view.set_app_regions(regions), cx);
    }

    /// The window was asked to close and the main process has not said no: ask it.
    pub fn ask_to_close(&self) {
        self.asked.set(Some(Instant::now()));
        self.reporter.say(WindowEvent::CloseRequested);
    }

    /// The main process answered a close.
    pub fn answered(&self) {
        self.asked.set(None);
    }

    /// The window goes without asking, and the main process is told once.
    pub fn destroy(self, cx: &mut AsyncApp) {
        self.native.release();
        // An error is a window GPUI already dropped: it is gone, as was asked.
        let _ = self
            .handle
            .update(cx, |_, window, _| window.remove_window());
        self.reporter.close();
    }
}

/// A person asks to close window `id`. A close is asked once: more clicks wait for the answer,
/// and only one that has waited too long gives up on it.
fn user_closes(asked: &Cell<Option<Instant>>, reporter: &Reporter, inbox: &Inbox, id: WindowId) {
    match asked.get() {
        Some(since) if since.elapsed() >= UNRESPONSIVE => {
            eprintln!(
                "electron: window {id} did not answer a close for {UNRESPONSIVE:?}; closing it"
            );
            inbox.push(ToShell::Window {
                window: id,
                op: WindowOp::Destroy,
            });
        }
        Some(_) => {}
        None => {
            asked.set(Some(Instant::now()));
            reporter.say(WindowEvent::CloseRequested);
        }
    }
}

/// `bounds` with the sides that were given replaced.
fn merged(
    bounds: Rect,
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
) -> Rect {
    Rect {
        x: x.unwrap_or(bounds.x),
        y: y.unwrap_or(bounds.y),
        width: width.unwrap_or(bounds.width),
        height: height.unwrap_or(bounds.height),
    }
}

/// `bounds` moved to the middle of `area`.
fn centred(bounds: Rect, area: Rect) -> Rect {
    Rect {
        x: area.x + (area.width - bounds.width) / 2.,
        y: area.y + (area.height - bounds.height) / 2.,
        ..bounds
    }
}

/// The display a new window opens on: the primary one.
pub(crate) fn primary(cx: &AsyncApp) -> Option<Display> {
    let displays = platform::displays(cx);
    let index = displays
        .iter()
        .position(|display| display.primary)
        .unwrap_or(0);
    displays.into_iter().nth(index)
}
