//! A shell for a machine with no display: it does with the messages it is sent what the GPUI
//! shell does, keeps the window state they add up to, and can be told that a person did
//! something. P00 builds its end-to-end assertions on this, and the state machine here — the
//! rules are in [`window`] — is the one the real shell runs: P34 keeps them in step.
//!
//! Pages are laid out by a [`FakeLayout`] on every view they publish, so `getBoundingClientRect`
//! and the observers answer without a drawer. The shell never waits for a page or the main
//! process: it is called on their threads.

mod layout;
mod menu;
mod window;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use verve_dom::{NodeId, Snapshot};

pub use layout::FakeLayout;
pub use menu::{HeadlessPopup, HeadlessTray};
pub use window::HeadlessWindow;

use crate::accelerator::Accelerator;
use crate::hub::{Hub, ShellPort};
use crate::ids::{ContentsId, WindowId};
use crate::msg::{
    AppEvent, DialogReply, DialogResult, DialogSpec, Display, FromShell, MenuId, MenuSpec, Rect,
    ToRendererInput, ToShell, TrayEvent, TrayId, WindowEvent, WindowOp,
};
use window::Window;

/// What a headless shell is told about the machine it pretends to be.
#[derive(Clone, Debug)]
pub struct HeadlessOptions {
    /// How the pages' boxes are worked out.
    pub layout: FakeLayout,
    /// The displays; the first is the primary one that windows open on.
    pub displays: Vec<Display>,
    /// Whether the system theme is dark: what each page's `prefers-color-scheme` is told.
    pub dark: bool,
    /// Whether each message to the shell is also written to standard error as a line, like
    /// `[window 1] create 800x600 "title"`, for a person watching an app run.
    pub trace: bool,
}

impl Default for HeadlessOptions {
    /// One 1920 x 1080 display at 1x, a light theme, block-stack layout.
    fn default() -> Self {
        Self {
            layout: FakeLayout::default(),
            displays: vec![Display::default()],
            dark: false,
            trace: false,
        }
    }
}

/// The shell of a machine with no display.
pub struct HeadlessShell {
    /// Held weakly: the Hub holds the shell, and neither keeps the other alive.
    hub: Weak<Hub>,
    layout: FakeLayout,
    displays: Mutex<Vec<Display>>,
    trace: bool,
    state: Mutex<State>,
    desk: Mutex<DialogDesk>,
}

/// The dialogs a headless shell was asked for and how they are answered: from the answers a test
/// queued, else at once with what a person who changes nothing and presses Enter answers (a
/// cancel, the default button), else not at all while the test holds them.
#[derive(Default)]
struct DialogDesk {
    presets: VecDeque<DialogResult>,
    hold: bool,
    asked: Vec<(u64, DialogSpec)>,
    waiting: BTreeMap<u64, DialogReply>,
}

#[derive(Default)]
struct State {
    received: Vec<ToShell>,
    windows: BTreeMap<WindowId, Window>,
    pages: BTreeMap<ContentsId, Page>,
    menus: menu::Desk,
    dark: bool,
    exit: Option<i32>,
}

/// What the shell has of a page: the last document it published and the boxes it was given.
#[derive(Default)]
struct Page {
    snapshot: Option<Snapshot>,
    boxes: Vec<(NodeId, verve_dom::Rect)>,
}

/// What a message did, to be said to the app once the shell's own state is let go.
#[derive(Default)]
struct Outcome {
    events: Vec<FromShell>,
    inputs: Vec<(ContentsId, ToRendererInput)>,
}

impl Outcome {
    /// `events` of `window`, and what each tells its page.
    fn window(&mut self, window: &Window, events: Vec<WindowEvent>, scale: f32) {
        let shown = &window.shown;
        for event in events {
            let input = match &event {
                WindowEvent::Resized { width, height } => Some(ToRendererInput::Viewport {
                    width: *width,
                    height: *height,
                    scale,
                }),
                WindowEvent::Focus => Some(ToRendererInput::Focus(true)),
                WindowEvent::Blur => Some(ToRendererInput::Focus(false)),
                _ => None,
            };
            self.inputs
                .extend(input.map(|input| (shown.contents, input)));
            self.events.push(FromShell::Window {
                window: shown.id,
                event,
            });
        }
    }
}

impl State {
    /// What the shell does with a message, and what it says back.
    fn apply(&mut self, msg: ToShell, primary: &Display) -> Outcome {
        let mut out = Outcome::default();
        match &msg {
            ToShell::CreateWindow {
                window,
                contents,
                options,
            } => {
                let created = Window::new(*window, *contents, options, primary);
                let bounds = created.shown.bounds;
                out.inputs.push((
                    *contents,
                    ToRendererInput::Viewport {
                        width: bounds.width,
                        height: bounds.height,
                        scale: primary.scale_factor,
                    },
                ));
                out.inputs
                    .push((*contents, ToRendererInput::ColorScheme { dark: self.dark }));
                if created.shown.focused {
                    out.inputs.push((*contents, ToRendererInput::Focus(true)));
                }
                self.windows.insert(*window, created);
            }
            ToShell::Window { window, op } => self.apply_op(*window, op, primary, &mut out),
            ToShell::Menu(op) => self.menus.apply(op, &mut out),
            ToShell::ErrorBox { .. } | ToShell::Dialog { .. } | ToShell::SetThemeSource(_) => {}
            ToShell::Quit { exit_code } => {
                self.exit = Some(*exit_code);
                self.windows.values_mut().for_each(|window| {
                    window.close();
                });
            }
        }
        self.received.push(msg);
        out
    }

    fn apply_op(&mut self, id: WindowId, op: &WindowOp, primary: &Display, out: &mut Outcome) {
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        let events = match op {
            // Closing is the page's to cancel, so it is asked first.
            WindowOp::Close => {
                out.events.push(FromShell::Window {
                    window: id,
                    event: WindowEvent::CloseRequested,
                });
                return;
            }
            WindowOp::Destroy | WindowOp::CloseAnswer { prevented: false } => window.close(),
            _ => window.apply(op, primary),
        };
        out.window(window, events, primary.scale_factor);
        // A window shown with its page already drawn gets its first paint now.
        let drawn = |pages: &BTreeMap<ContentsId, Page>| {
            pages
                .get(&window.shown.contents)
                .is_some_and(|page| page.snapshot.is_some())
        };
        if window.shown.visible && !window.painted && drawn(&self.pages) {
            window.painted = true;
            out.events.push(FromShell::Window {
                window: id,
                event: WindowEvent::FirstPaint,
            });
        }
    }

    /// Takes a view the page published and lays it out; answers the boxes if they are new.
    fn settle(
        &mut self,
        contents: ContentsId,
        snapshot: Snapshot,
        layout: FakeLayout,
    ) -> Option<Vec<(NodeId, verve_dom::Rect)>> {
        let boxes = layout.boxes(&snapshot);
        let page = self.pages.entry(contents).or_default();
        page.snapshot = Some(snapshot);
        (page.boxes != boxes).then(|| {
            page.boxes.clone_from(&boxes);
            boxes
        })
    }
}

impl HeadlessShell {
    /// A shell that takes this app's messages: from now on the Hub sends them here, and keeps
    /// every console line the app writes.
    pub fn new(hub: &Arc<Hub>, options: HeadlessOptions) -> Arc<Self> {
        let shell = Arc::new(Self {
            hub: Arc::downgrade(hub),
            layout: options.layout,
            displays: Mutex::new(options.displays.clone()),
            trace: options.trace,
            state: Mutex::new(State {
                dark: options.dark,
                ..State::default()
            }),
            desk: Mutex::new(DialogDesk::default()),
        });
        // `screen` is right from the first line: the main process is not up to hear it said.
        hub.set_displays(options.displays.clone());
        hub.record_console();
        hub.record_page_events();
        hub.set_shell(shell.clone());
        shell
    }

    /// Every window this app made, open or closed.
    pub fn windows(&self) -> Vec<HeadlessWindow> {
        self.state()
            .windows
            .values()
            .map(|window| window.shown.clone())
            .collect()
    }

    /// Everything the main process asked of the shell, in order.
    pub fn received(&self) -> Vec<ToShell> {
        self.state().received.clone()
    }

    /// The code the app ended with, once it has.
    pub fn exit_code(&self) -> Option<i32> {
        self.state().exit
    }

    /// The document `window`'s page last published, as a drawer would have it.
    pub fn snapshot(&self, window: WindowId) -> Option<Snapshot> {
        self.pump();
        let state = self.state();
        let contents = state.windows.get(&window)?.shown.contents;
        state.pages.get(&contents)?.snapshot.clone()
    }

    /// A person clicked `window`'s close button: the main process is asked, a `'close'`
    /// listener may keep the window, and the answer closes it.
    pub fn user_close(&self, window: WindowId) {
        self.say(FromShell::Window {
            window,
            event: WindowEvent::CloseRequested,
        });
    }

    /// A person dragged `window`'s edge to `width` x `height`; a window that cannot be resized
    /// stays as it is.
    pub fn resize(&self, window: WindowId, width: f32, height: f32) {
        let scale = self.primary().scale_factor;
        let mut out = Outcome::default();
        if let Some(found) = self.state().windows.get_mut(&window) {
            let events = found.resize(width, height);
            out.window(found, events, scale);
        }
        self.deliver(out);
    }

    /// A person clicked the first element of `window`'s page that `selector` matches.
    pub fn click(&self, window: WindowId, selector: &str) {
        let selector = selector.to_owned();
        self.on_page(window, move |page| {
            if !page.click(&selector) {
                eprintln!("headless: no element matches {selector}");
            }
        });
    }

    /// A person typed `text` into whatever has the focus in `window`.
    pub fn type_text(&self, window: WindowId, text: &str) {
        let text = text.to_owned();
        self.on_page(window, move |page| page.type_text(&text));
    }

    /// A person chose menu item `item`.
    pub fn menu_click(&self, item: u32) {
        self.say(FromShell::MenuClicked {
            menu_item: item,
            window: None,
            triggered_by_accelerator: false,
            modifiers: verve_dom::Modifiers::default(),
        });
    }

    /// A person chose menu item `item` of `window`'s menu, holding `modifiers`.
    pub fn menu_click_in(&self, window: WindowId, item: u32, modifiers: verve_dom::Modifiers) {
        self.say(FromShell::MenuClicked {
            menu_item: item,
            window: Some(window),
            triggered_by_accelerator: false,
            modifiers,
        });
    }

    /// The application menu, as the main process last sent it.
    pub fn application_menu(&self) -> Option<MenuSpec> {
        self.state().menus.application_menu()
    }

    /// The menu `window` shows: its own, else (on macOS) the application menu.
    pub fn menu_of(&self, window: WindowId) -> Option<MenuSpec> {
        self.state().menus.book.menu_of(window).cloned()
    }

    /// The menu the main process last sent as `id`.
    pub fn menu(&self, id: MenuId) -> Option<MenuSpec> {
        self.state().menus.book.menu(id).cloned()
    }

    /// Every popup the main process asked for, in order. Each opened and closed at once, with
    /// nothing chosen.
    pub fn popups(&self) -> Vec<HeadlessPopup> {
        self.state().menus.popups.clone()
    }

    /// The tray icons the app made, by number.
    pub fn trays(&self) -> BTreeMap<TrayId, HeadlessTray> {
        self.state().menus.trays.clone()
    }

    /// A person did `event` to tray icon `tray`.
    pub fn tray_event(&self, tray: TrayId, event: TrayEvent) {
        self.say(FromShell::Tray {
            tray,
            event,
            bounds: Rect {
                x: 1800.,
                y: 1040.,
                width: 16.,
                height: 16.,
            },
            position: Some((1808., 1048.)),
            modifiers: verve_dom::Modifiers::default(),
        });
    }

    /// The global shortcuts the app registered, by the number it was registered under.
    pub fn shortcuts(&self) -> BTreeMap<u32, Accelerator> {
        self.state().menus.shortcuts.clone()
    }

    /// Whether the system takes the global shortcuts the app registers from now on. It does,
    /// until a test says it does not.
    pub fn accept_shortcuts(&self, accept: bool) {
        self.state().menus.refuse_shortcuts = !accept;
    }

    /// The system hands the app global shortcut `id`.
    pub fn press_shortcut(&self, id: u32) {
        self.say(FromShell::Shortcut { id });
    }

    /// A person pressed `key` in `window` and let go. A key `'before-input-event'` listeners are
    /// asked of goes to the main process first; otherwise the menu's accelerators are looked up
    /// here, and a key one chooses is a click on that item and not a key for the page.
    pub fn press_key(&self, window: WindowId, key: verve_dom::Key) {
        self.key_event(window, true, key.clone());
        self.key_event(window, false, key);
    }

    /// A person held `key` in `window`: it went down, auto-repeated `repeats` more times
    /// (`repeat: true` each) and came up once.
    pub fn hold_key(&self, window: WindowId, key: verve_dom::Key, repeats: usize) {
        self.key_event(window, true, key.clone());
        for _ in 0..repeats {
            let again = verve_dom::Key {
                repeat: true,
                ..key.clone()
            };
            self.key_event(window, true, again);
        }
        self.key_event(window, false, key);
    }

    fn key_event(&self, window: WindowId, down: bool, key: verve_dom::Key) {
        let Some(contents) = self.state().windows.get(&window).map(|w| w.shown.contents) else {
            return;
        };
        let intercepted = self
            .hub
            .upgrade()
            .is_some_and(|hub| hub.intercepts_keys(contents));
        if intercepted {
            self.say(FromShell::KeyForContents {
                contents,
                down,
                key,
            });
            return;
        }
        let chosen = {
            let mut state = self.state();
            let chosen = down
                .then(|| state.menus.book.item_for(window, &key))
                .flatten();
            match (chosen, down) {
                (Some(_), _) => {
                    if !state.menus.swallowed.contains(&key.code) {
                        state.menus.swallowed.push(key.code.clone());
                    }
                }
                (None, false) => {
                    let at = state
                        .menus
                        .swallowed
                        .iter()
                        .position(|code| *code == key.code);
                    if let Some(at) = at {
                        state.menus.swallowed.remove(at);
                        return;
                    }
                }
                (None, true) => state.menus.swallowed.retain(|code| *code != key.code),
            }
            chosen
        };
        match chosen {
            Some(item) => self.say(FromShell::MenuClicked {
                menu_item: item,
                window: Some(window),
                triggered_by_accelerator: true,
                modifiers: key.modifiers,
            }),
            None => self.input(contents, ToRendererInput::Key { down, key }),
        }
    }

    /// The system theme changed.
    pub fn set_dark(&self, dark: bool) {
        let contents: Vec<_> = {
            let mut state = self.state();
            state.dark = dark;
            state.pages.keys().copied().collect()
        };
        for contents in contents {
            self.input(contents, ToRendererInput::ColorScheme { dark });
        }
        self.say(FromShell::App(AppEvent::ThemeChanged { dark }));
    }

    /// The displays change: the main process hears `display-added`, `display-removed` and
    /// `display-metrics-changed` for what differs.
    pub fn set_displays(&self, displays: Vec<Display>) {
        *self.displays.lock().unwrap_or_else(|p| p.into_inner()) = displays.clone();
        self.say(FromShell::Displays(displays));
    }

    /// Answers the next dialog with `result`, before any held or default answer.
    pub fn queue_dialog_answer(&self, result: DialogResult) {
        self.desk().presets.push_back(result);
    }

    /// Holds dialogs until [`HeadlessShell::answer_dialog`] answers them, as a person takes their
    /// time; `false` goes back to answering at once.
    pub fn hold_dialogs(&self, hold: bool) {
        self.desk().hold = hold;
    }

    /// Every dialog asked for so far, `(call, spec)`, in order.
    pub fn dialogs(&self) -> Vec<(u64, DialogSpec)> {
        self.desk().asked.clone()
    }

    /// Answers the held dialog `call`; `false` if none is waiting under that number.
    pub fn answer_dialog(&self, call: u64, result: DialogResult) -> bool {
        let waiting = self.desk().waiting.remove(&call);
        waiting.is_some_and(|reply| self.reply(call, reply, result))
    }

    fn desk(&self) -> MutexGuard<'_, DialogDesk> {
        self.desk.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A dialog: answered from a preset, at once, or when the test says.
    fn dialog(&self, call: u64, spec: &DialogSpec, reply: &DialogReply) {
        let result = {
            let mut desk = self.desk();
            desk.asked.push((call, spec.clone()));
            let preset = desk.presets.pop_front();
            match preset {
                Some(result) => Some(result),
                None if desk.hold => {
                    desk.waiting.insert(call, reply.clone());
                    None
                }
                None => Some(default_answer(spec)),
            }
        };
        if let Some(result) = result {
            self.reply(call, reply.clone(), result);
        }
    }

    /// Says a dialog's answer where the dialog asked for it. `false` when the main process
    /// stopped waiting on it.
    fn reply(&self, call: u64, reply: DialogReply, result: DialogResult) -> bool {
        match reply {
            DialogReply::Sync(answer) => answer.send(result).is_ok(),
            DialogReply::Async => {
                self.say(FromShell::DialogResult { call, result });
                true
            }
        }
    }

    fn primary(&self) -> Display {
        let displays = self.displays.lock().unwrap_or_else(|p| p.into_inner());
        (displays.iter().find(|display| display.primary))
            .or(displays.first())
            .cloned()
            .expect("a headless shell has a display")
    }

    fn say(&self, msg: FromShell) {
        if let Some(hub) = self.hub.upgrade() {
            hub.from_shell(msg);
        }
    }

    fn input(&self, contents: ContentsId, msg: ToRendererInput) {
        if let Some(hub) = self.hub.upgrade() {
            hub.input(contents, msg);
        }
    }

    fn deliver(&self, out: Outcome) {
        for (contents, input) in out.inputs {
            self.input(contents, input);
        }
        for event in out.events {
            self.say(event);
        }
    }

    /// Runs `job` on the page `window` shows, if it is open.
    fn on_page(
        &self,
        window: WindowId,
        job: impl FnOnce(&mut crate::RendererGuest) + Send + 'static,
    ) {
        let contents = self.state().windows.get(&window).map(|w| w.shown.contents);
        if let (Some(hub), Some(contents)) = (self.hub.upgrade(), contents) {
            hub.on_renderer(contents, job);
        }
    }

    /// Takes the views the pages published and gives each page the boxes of its new document. A
    /// layout that comes out as the last one is not sent again, which is what ends the loop of a
    /// page that publishes a view for having been laid out.
    fn pump(&self) {
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        let mut state = self.state();
        let mut painted = Vec::new();
        let open: Vec<_> = state
            .windows
            .values()
            .filter(|window| !window.shown.closed)
            .map(|window| window.shown.contents)
            .collect();
        for contents in open {
            let Some(update) = hub.renderer_view(contents).and_then(|view| view.update) else {
                continue;
            };
            if let Some(boxes) = state.settle(contents, update.snapshot, self.layout) {
                hub.input(contents, ToRendererInput::Layout(boxes));
            }
            // The first snapshot of a window that is shown is its first paint.
            let shown = state.windows.values_mut().find(|window| {
                window.shown.contents == contents && window.shown.visible && !window.painted
            });
            if let Some(window) = shown {
                window.painted = true;
                painted.push(window.shown.id);
            }
        }
        drop(state);
        for window in painted {
            self.say(FromShell::Window {
                window,
                event: WindowEvent::FirstPaint,
            });
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Runs the app whose script is `entry` on a headless shell that traces to standard error, and
/// answers the code it ends with: `electron --headless`.
pub fn run(hub: &Arc<Hub>, program: crate::hub::Program) -> i32 {
    let options = HeadlessOptions {
        trace: true,
        ..HeadlessOptions::default()
    };
    let shell = HeadlessShell::new(hub, options);
    hub.start(program);
    loop {
        if let Some(code) = shell.exit_code() {
            return code;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A message to the shell as one line of the trace.
fn describe(msg: &ToShell) -> String {
    match msg {
        ToShell::CreateWindow {
            window, options, ..
        } => format!(
            "[window {window}] create {}x{} {:?}",
            options.width, options.height, options.title
        ),
        ToShell::Window { window, op } => format!("[window {window}] {op:?}"),
        ToShell::Menu(op) => format!("[menu] {}", menu::describe(op)),
        ToShell::Quit { exit_code } => format!("[app] quit {exit_code}"),
        ToShell::ErrorBox { title, content } => format!("[app] error box {title:?} {content:?}"),
        ToShell::Dialog { call, spec, .. } => format!("[dialog {call}] {spec:?}"),
        ToShell::SetThemeSource(source) => format!("[app] theme source {source:?}"),
    }
}

/// What a person who changes nothing and presses Enter answers.
fn default_answer(spec: &DialogSpec) -> DialogResult {
    match spec {
        DialogSpec::Open { .. } => DialogResult::Open {
            canceled: true,
            file_paths: Vec::new(),
        },
        DialogSpec::Save { .. } => DialogResult::Save {
            canceled: true,
            file_path: None,
        },
        DialogSpec::MessageBox {
            default_id,
            checkbox_checked,
            ..
        } => DialogResult::MessageBox {
            response: usize::try_from(*default_id).unwrap_or(0),
            checkbox_checked: *checkbox_checked,
        },
    }
}

impl ShellPort for HeadlessShell {
    fn send(&self, msg: ToShell) {
        if self.trace {
            eprintln!("{}", describe(&msg));
        }
        if let ToShell::Dialog {
            call, spec, reply, ..
        } = &msg
        {
            self.dialog(*call, spec, reply);
        }
        // Nothing is said back to the main process while the shell's own state is held: a window
        // that closed is reported once this message is done with.
        let out = {
            let primary = self.primary();
            self.state().apply(msg, &primary)
        };
        self.deliver(out);
    }

    fn wake(&self) {
        self.pump();
    }
}
