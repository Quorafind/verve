//! The router: what the app's threads — the main process's isolate, each page's, the shell —
//! say to each other, and which of them is still there to hear it.
//!
//! The Hub owns every isolate it started, so no thread ever holds another's handle; and it keeps
//! the two registries everything else is looked up in, the windows that are open (each with the
//! page it shows) and the pages' isolates. Messages are jobs sent to the isolate that runs the
//! JavaScript they end up in — never a borrow of it, and never a wait on it: the rule that only a
//! page may wait for the main process is what keeps the whole thing from deadlocking.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use verve::isolate::{Health, Isolate, Options as IsolateOptions};

use crate::ids::{ContentsId, Ids, WindowId};
use crate::msg::{
    FromRenderer, FromShell, GoneReason, RendererView, ToRenderer, ToRendererInput, ToShell,
    WebPreferences,
};
use crate::{ElectronOptions, MainGuest, RendererGuest};

/// The stack each JS thread starts with: Boa's interpreter recurses deeply, and a page's bundle
/// is parsed on the thread that runs it.
const STACK: usize = 16 * 1024 * 1024;

/// The longest an isolate with nothing due sleeps, so a guest's slow work still gets its beat.
const IDLE: Duration = Duration::from_millis(500);

/// A mutex of the Hub's, read even after a panic elsewhere poisoned it: what it guards is a
/// registry of ids, still true after someone else's job threw.
fn lock<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn isolate_options(name: String) -> IsolateOptions {
    IsolateOptions {
        name,
        stack_size: STACK,
        idle: IDLE,
    }
}

/// The shell's end of the Hub: what the OS thread does with what the main process asks of it.
pub trait ShellPort: Send + Sync {
    /// Takes a message. Answers before the Hub wakes the shell.
    fn send(&self, msg: ToShell);

    /// There is news: what was just sent, or a page that published a view. Reached from any
    /// thread, so a shell on another one — GPUI's OS thread — sends itself what it needs.
    fn wake(&self);
}

/// What the main process holds of a page: its id, and the Hub that reaches the thread it runs on.
#[derive(Clone)]
pub struct RendererHandle {
    /// The page.
    pub contents: ContentsId,
    hub: Arc<Hub>,
}

impl RendererHandle {
    /// Sends `msg` to the page.
    pub fn to_renderer(&self, msg: ToRenderer) {
        self.hub.to_renderer(self.contents, msg);
    }
}

/// One page's isolate, the preferences it was made with, and the last view it published.
struct RendererSlot {
    hub: Weak<Hub>,
    contents: ContentsId,
    /// What the page was made with: the isolate P20 makes anew on a navigation is made with these.
    prefs: WebPreferences,
    isolate: Mutex<Option<Isolate<RendererGuest>>>,
    view: Mutex<Option<RendererView>>,
    /// Whether the main process was told the page's process is gone.
    gone: AtomicBool,
}

impl RendererSlot {
    /// Called on the page's own thread, by its isolate, when it has a view, events, or died:
    /// the view waits for the shell, the events go to the main process's thread, and a page
    /// that died is reported to it — once — as `render-process-gone`.
    fn published(&self) {
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        let (view, events, health) = match lock(&self.isolate).as_ref() {
            Some(isolate) => (isolate.take_view(), isolate.take_events(), isolate.health()),
            None => (None, Vec::new(), Health::Alive),
        };
        if let Some(view) = view {
            *lock(&self.view) = Some(view);
        }
        for event in events {
            hub.from_renderer(self.contents, event);
        }
        if let Health::Dead(message) = health
            && !self.gone.swap(true, Ordering::SeqCst)
        {
            eprintln!("electron: page {} crashed: {message}", self.contents);
            hub.from_renderer(
                self.contents,
                FromRenderer::Gone {
                    reason: GoneReason::Crashed,
                },
            );
        }
    }
}

/// The threads of one app, and everything they say. Made once, with [`Hub::new`], and shared
/// as an `Arc`.
pub struct Hub {
    options: ElectronOptions,
    ids: Mutex<Ids>,
    /// The windows open, each with the page it shows.
    windows: Mutex<BTreeMap<WindowId, ContentsId>>,
    renderers: Mutex<BTreeMap<ContentsId, Arc<RendererSlot>>>,
    main: Mutex<Option<Isolate<MainGuest>>>,
    shell: Mutex<Option<Arc<dyn ShellPort>>>,
    exit: Mutex<Option<i32>>,
}

impl Hub {
    /// A Hub for one app; nothing runs until [`Hub::start_main`].
    pub fn new(options: ElectronOptions) -> Arc<Hub> {
        Arc::new(Self {
            options,
            ids: Mutex::new(Ids::default()),
            windows: Mutex::new(BTreeMap::new()),
            renderers: Mutex::new(BTreeMap::new()),
            main: Mutex::new(None),
            shell: Mutex::new(None),
            exit: Mutex::new(None),
        })
    }

    /// What the app was started with.
    pub fn options(&self) -> &ElectronOptions {
        &self.options
    }

    /// Starts the browser process's isolate, running `entry` — an absolute path, or one resolved
    /// against the app's directory. The isolate is made first and the script is its first job, so
    /// a message for the main process sent while the script runs waits its turn instead of being
    /// lost, and a script that never returns can be interrupted.
    pub fn start_main(self: &Arc<Self>, entry: PathBuf) {
        let entry = if entry.is_absolute() {
            entry
        } else {
            self.options.app_path.join(entry)
        };
        let hub = Arc::clone(self);
        let made = entry.clone();
        let weak = Arc::downgrade(self);
        let wake = move || {
            if let Some(hub) = weak.upgrade() {
                hub.main_woke();
            }
        };
        let isolate = Isolate::spawn(
            isolate_options("electron-main".into()),
            move || MainGuest::new(hub, made),
            wake,
        );
        match isolate {
            Ok(isolate) => {
                let poke = isolate.poker();
                isolate.send(move |guest, _| guest.set_waker(poke));
                isolate.send(move |guest, _| guest.run(&entry));
                *lock(&self.main) = Some(isolate);
            }
            Err(error) => {
                // No thread to run the app's script on: the app cannot start at all.
                eprintln!("electron: no thread for the browser process: {error}");
                self.shutdown(1);
            }
        }
    }

    /// Starts the isolate that renders page `contents`, which the app has just given a window,
    /// with `prefs`. A thread that cannot be made leaves the page registered with no isolate:
    /// its window still exists — a window whose page never ran — and the main process is told
    /// the page's process was gone before it started, which is Electron's `launch-failed`.
    pub fn create_renderer(
        self: &Arc<Self>,
        contents: ContentsId,
        prefs: WebPreferences,
    ) -> RendererHandle {
        let slot = Arc::new(RendererSlot {
            hub: Arc::downgrade(self),
            contents,
            prefs: prefs.clone(),
            isolate: Mutex::new(None),
            view: Mutex::new(None),
            gone: AtomicBool::new(false),
        });
        let hub = Arc::clone(self);
        let make = move || RendererGuest::new(&hub, contents, prefs);
        let published = Arc::downgrade(&slot);
        let weak = Arc::downgrade(self);
        let wake = move || {
            if let Some(slot) = published.upgrade() {
                slot.published();
            }
            if let Some(hub) = weak.upgrade() {
                hub.wake();
            }
        };
        match Isolate::spawn(isolate_options(format!("renderer-{contents}")), make, wake) {
            Ok(isolate) => {
                let poke = isolate.poker();
                isolate.send(move |guest, _| guest.set_waker(poke));
                *lock(&slot.isolate) = Some(isolate);
            }
            Err(error) => {
                eprintln!("electron: no thread for page {contents}: {error}");
                slot.gone.store(true, Ordering::SeqCst);
                self.from_renderer(
                    contents,
                    FromRenderer::Gone {
                        reason: GoneReason::LaunchFailed,
                    },
                );
            }
        }
        lock(&self.renderers).insert(contents, slot);
        RendererHandle {
            contents,
            hub: Arc::clone(self),
        }
    }

    /// The preferences page `contents` was made with, while it exists.
    pub fn preferences_of(&self, contents: ContentsId) -> Option<WebPreferences> {
        Some(lock(&self.renderers).get(&contents)?.prefs.clone())
    }

    /// The ids of a window about to be made and of the page it shows.
    pub fn new_window_ids(&self) -> (WindowId, ContentsId) {
        let mut ids = lock(&self.ids);
        (ids.window(), ids.contents())
    }

    /// Records a window the main process made, with the page it shows.
    pub fn add_window(&self, window: WindowId, contents: ContentsId) {
        lock(&self.windows).insert(window, contents);
    }

    /// The windows still open.
    pub fn windows(&self) -> Vec<WindowId> {
        lock(&self.windows).keys().copied().collect()
    }

    /// The page `window` shows, while the window is open.
    pub fn contents_of(&self, window: WindowId) -> Option<ContentsId> {
        lock(&self.windows).get(&window).copied()
    }

    /// The window that shows page `contents`, while it is open.
    pub fn window_of(&self, contents: ContentsId) -> Option<WindowId> {
        lock(&self.windows)
            .iter()
            .find(|(_, shown)| **shown == contents)
            .map(|(window, _)| *window)
    }

    /// Asks the shell for something: a window, a menu, the end of the app.
    pub fn to_shell(&self, msg: ToShell) {
        let shell = lock(&self.shell).clone();
        if let Some(shell) = shell {
            shell.send(msg);
        }
        self.wake();
    }

    /// What the shell saw: a window shown, closed or resized, the displays changed.
    pub fn from_shell(&self, msg: FromShell) {
        self.to_main(move |guest| guest.from_shell(msg));
    }

    /// What a page says: a lifecycle step, a request only the window can answer.
    pub fn from_renderer(&self, contents: ContentsId, event: FromRenderer) {
        self.to_main(move |guest| guest.from_renderer(contents, event));
    }

    /// Sends `msg` to page `contents`, to run as a job of its isolate.
    pub fn to_renderer(&self, contents: ContentsId, msg: ToRenderer) {
        self.on_renderer(contents, move |guest| guest.apply(msg));
    }

    /// What a person did to the window, to the page under it: the hot path, and the only
    /// messages that never touch the main process.
    pub fn input(&self, contents: ContentsId, msg: ToRendererInput) {
        self.on_renderer(contents, move |guest| guest.input(msg));
    }

    fn on_renderer(
        &self,
        contents: ContentsId,
        job: impl FnOnce(&mut RendererGuest) + Send + 'static,
    ) {
        let slot = lock(&self.renderers).get(&contents).cloned();
        if let Some(slot) = slot
            && let Some(isolate) = lock(&slot.isolate).as_ref()
        {
            isolate.send(move |guest, _| job(guest));
        }
    }

    /// The view the shell has not read yet. Taken: a view is delivered once.
    pub fn renderer_view(&self, contents: ContentsId) -> Option<RendererView> {
        let slot = lock(&self.renderers).get(&contents).cloned()?;
        lock(&slot.view).take()
    }

    /// Registers the shell that takes [`ToShell`] messages and the wake that says there are new
    /// ones. A shell that draws on another thread keeps its queue here and is woken there.
    pub fn set_shell(&self, shell: Arc<dyn ShellPort>) {
        *lock(&self.shell) = Some(shell);
    }

    /// The app is ending, with `exit_code`; the first code asked for is the one it leaves with.
    /// Every isolate is stopped — the script it is running interrupted, and its handle dropped,
    /// which ends its thread and lets go of the Hub it holds — and the shell is told to close
    /// and quit. A page waiting on `sendSync` is woken by its reply being dropped unanswered.
    pub fn shutdown(&self, exit_code: i32) {
        {
            let mut exit = lock(&self.exit);
            if exit.is_some() {
                return;
            }
            *exit = Some(exit_code);
        }
        let main = lock(&self.main).take();
        if let Some(main) = &main {
            main.interrupt();
        }
        drop(main);
        let pages: Vec<_> = std::mem::take(&mut *lock(&self.renderers))
            .into_values()
            .collect();
        for page in pages {
            stop(&page);
        }
        self.to_shell(ToShell::Quit { exit_code });
    }

    /// The code the app is ending with, once it is.
    pub fn exit_code(&self) -> Option<i32> {
        *lock(&self.exit)
    }

    fn to_main(&self, job: impl FnOnce(&mut MainGuest) + Send + 'static) {
        if let Some(isolate) = lock(&self.main).as_ref() {
            isolate.send(move |guest, _| job(guest));
        }
    }

    /// A window the main process is done with, and the page it showed: the page's script is
    /// interrupted and its isolate dropped, which ends its thread. Called on the main process's
    /// thread after the window's `'closed'` listeners ran, so a listener that made another
    /// window has made it before the app asks whether any is left.
    pub fn remove_window(&self, window: WindowId) {
        let Some(contents) = lock(&self.windows).remove(&window) else {
            return;
        };
        let page = lock(&self.renderers).remove(&contents);
        if let Some(page) = page {
            stop(&page);
        }
    }

    /// The main process's isolate woke the Hub: a job it ran has news, or it died. A main
    /// process that died takes the app with it, with code 1.
    fn main_woke(&self) {
        let health = lock(&self.main).as_ref().map(Isolate::health);
        if let Some(Health::Dead(message)) = health {
            eprintln!("electron: the browser process crashed: {message}");
            self.shutdown(1);
            return;
        }
        self.wake();
    }

    fn wake(&self) {
        let shell = lock(&self.shell).clone();
        if let Some(shell) = shell {
            shell.wake();
        }
    }
}

/// Interrupts the script a page is running and lets its isolate go.
fn stop(page: &RendererSlot) {
    let isolate = lock(&page.isolate).take();
    if let Some(isolate) = &isolate {
        isolate.interrupt();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::HeadlessShell;

    fn wait(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// An app whose script makes one window, so it stays up until something ends it.
    fn app() -> (Arc<Hub>, Arc<HeadlessShell>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("verve-electron-hub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory for the app");
        let main = dir.join(format!("main{}.js", Instant::now().elapsed().as_nanos()));
        std::fs::write(&main, "new (require('electron').BrowserWindow)();").expect("the script");
        let hub = Hub::new(ElectronOptions {
            app_path: dir,
            dark: false,
        });
        let shell = HeadlessShell::new(&hub);
        hub.start_main(main.clone());
        (hub, shell, main)
    }

    #[test]
    fn a_main_process_that_panics_ends_the_app_with_one() {
        let (hub, shell, _) = app();
        assert!(wait(|| !shell.windows().is_empty()));
        hub.to_main(|_| panic!("the browser process broke"));
        assert!(wait(|| shell.exit_code() == Some(1)));
    }

    #[test]
    fn a_page_that_panics_is_reported_gone_once() {
        let (hub, shell, _) = app();
        assert!(wait(|| !shell.windows().is_empty()));
        let page = shell.windows()[0].contents;
        let slot = lock(&hub.renderers).get(&page).cloned().expect("the page");
        assert!(!slot.gone.load(Ordering::SeqCst));
        hub.on_renderer(page, |_| panic!("the page broke"));
        assert!(wait(|| slot.gone.load(Ordering::SeqCst)));
        // The app is still up, and a second wake does not say it again.
        assert_eq!(shell.exit_code(), None);
        slot.published();
        assert!(slot.gone.load(Ordering::SeqCst));
        hub.shutdown(0);
    }
}
