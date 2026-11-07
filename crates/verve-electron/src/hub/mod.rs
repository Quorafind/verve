//! The router: what the app's threads — the main process's isolate, each page's, the shell —
//! say to each other, and which of them is still there to hear it.
//!
//! The Hub owns every isolate it started, so no thread ever holds another's handle; and it keeps
//! the two registries everything else is looked up in, the windows that are open (each with the
//! page it shows) and the pages' isolates. Messages are jobs sent to the isolate that runs the
//! JavaScript they end up in — never a borrow of it, and never a wait on it: the rule that only a
//! page may wait for the main process is what keeps the whole thing from deadlocking.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use verve::isolate::{Health, Isolate, Options as IsolateOptions};
use verve::vfs::Overlay;

use crate::ids::{ContentsId, Ids, WindowId};
use crate::launcher::Start;
use crate::msg::{AppEvent, Display, FromShell, PageEvent, ToShell};
use crate::{ElectronOptions, MainGuest};

mod records;
mod renderers;
mod shell;
mod text;
mod windows;

pub use records::ConsoleLine;
pub use renderers::RendererHandle;
use renderers::{RendererSlot, stop};
pub use shell::ShellPort;
use shell::{ShellObserver, Theme};

/// The stack each JS thread starts with: Boa's interpreter recurses deeply, and a page's bundle
/// is parsed on the thread that runs it.
const STACK: usize = 16 * 1024 * 1024;

/// The longest an isolate with nothing due sleeps, so a guest's slow work still gets its beat.
const IDLE: Duration = Duration::from_millis(500);

/// The most messages of a page the main process hears in one job.
const PER_JOB: usize = 1000;

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

/// What the main process's isolate runs first.
#[derive(Clone)]
pub enum Program {
    /// The app's own script: an absolute path, or one resolved against the app's directory.
    Script(PathBuf),
    /// Electron's default app: a window on `url`, or on the welcome page.
    DefaultApp(Option<String>),
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
    /// Every console line, once someone asked to keep them: a long-running app is not made to.
    console: Mutex<Option<Vec<ConsoleLine>>>,
    /// Every lifecycle event of every page, once someone asked to keep them.
    page_events: Mutex<Option<Vec<(ContentsId, PageEvent)>>>,
    /// Numbers the asks of every page's loader for the main process.
    protocol_calls: Arc<AtomicU64>,
    launch: Mutex<Option<Arc<Start>>>,
    /// Whether [`Hub::set_launch`] was called: an app that was not launched has a profile of
    /// its own to clean up.
    told: AtomicBool,
    /// Whether an error box with no window may be the system's own; only a real shell says so.
    system_boxes: AtomicBool,
    /// How many times the main process's thread has run a beat.
    beats: AtomicU64,
    overlay: Mutex<Option<Arc<dyn Overlay>>>,
    /// Told of every [`FromShell`] before the main process is: how a test watches what a shell says.
    observer: Mutex<Option<ShellObserver>>,
    /// The displays the shell last reported: kept here so that what it says before the main
    /// process is up is still there when `screen` is first asked.
    displays: Mutex<Vec<Display>>,
    theme: Mutex<Theme>,
    /// What the pages' text is laid out with: the shell's, once it is up.
    measurer: Mutex<Option<Arc<dyn verve_layout::TextMeasurer>>>,
    /// A page has been given its measurer (whichever it was).
    measurer_taken: AtomicBool,
}

impl Hub {
    /// A Hub for one app; nothing runs until [`Hub::start_main`].
    pub fn new(options: ElectronOptions) -> Arc<Hub> {
        let theme = Theme::new(options.dark);
        Arc::new(Self {
            options,
            ids: Mutex::new(Ids::default()),
            windows: Mutex::new(BTreeMap::new()),
            renderers: Mutex::new(BTreeMap::new()),
            main: Mutex::new(None),
            shell: Mutex::new(None),
            exit: Mutex::new(None),
            console: Mutex::new(None),
            page_events: Mutex::new(None),
            protocol_calls: Arc::default(),
            launch: Mutex::new(None),
            told: AtomicBool::new(false),
            system_boxes: AtomicBool::new(false),
            beats: AtomicU64::new(0),
            overlay: Mutex::new(None),
            observer: Mutex::new(None),
            displays: Mutex::new(Vec::new()),
            theme: Mutex::new(theme),
            measurer: Mutex::new(None),
            measurer_taken: AtomicBool::new(false),
        })
    }

    /// Lets [`Hub::error_box`] show the system's own box when no window exists. The real shell
    /// says so; a headless one never does, so a test cannot put a box on the desktop.
    pub fn allow_system_boxes(&self) {
        self.system_boxes.store(true, Ordering::Relaxed);
    }

    /// An error box: the shell's over a window; with none, the system's own on this thread, the
    /// main process's, as Electron shows it before `ready`, when the shell allows it.
    pub fn error_box(&self, title: String, content: String) {
        if self.windows().is_empty() && self.system_boxes.load(Ordering::Relaxed) {
            crate::os::error_box(&title, &content);
        } else {
            self.to_shell(ToShell::ErrorBox { title, content });
        }
    }

    /// Says how the app was started: the command line, and the app it named. Before
    /// [`Hub::start_main`]; an app that was not told is a development app in `options.app_path`.
    pub fn set_launch(&self, start: Start) {
        *lock(&self.launch) = Some(Arc::new(start));
        self.told.store(true, Ordering::SeqCst);
    }

    /// How the app was started.
    pub fn launch(&self) -> Arc<Start> {
        let mut launch = lock(&self.launch);
        Arc::clone(
            launch
                .get_or_insert_with(|| Arc::new(Start::development(self.options.app_path.clone()))),
        )
    }

    /// The files packed into the app's `app.asar`, which every isolate reads through: one
    /// overlay, so an archive is opened once. Before [`Hub::start_main`].
    pub fn set_overlay(&self, overlay: Arc<dyn Overlay>) {
        *lock(&self.overlay) = Some(overlay);
    }

    /// The text of the file at `path`, as the app sees it: from the overlay when it holds the
    /// file, else from the disk.
    ///
    /// # Errors
    ///
    /// Why it could not be read.
    pub fn read_text(&self, path: &Path) -> std::io::Result<String> {
        let packed = self.overlay().and_then(|overlay| overlay.read(path));
        match packed {
            Some(bytes) => Ok(String::from_utf8_lossy(&bytes?).into_owned()),
            None => std::fs::read_to_string(path),
        }
    }

    /// The overlay [`Hub::set_overlay`] set, if any.
    pub fn overlay(&self) -> Option<Arc<dyn Overlay>> {
        lock(&self.overlay).clone()
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
        self.start(Program::Script(entry));
    }

    /// [`Hub::start_main`] for any [`Program`]. When a shell is registered the app is ready as
    /// soon as the program has run: its [`AppEvent::Ready`] queues behind it.
    pub fn start(self: &Arc<Self>, program: Program) {
        let program = match program {
            Program::Script(entry) if !entry.is_absolute() => {
                Program::Script(self.options.app_path.join(entry))
            }
            other => other,
        };
        let hub = Arc::clone(self);
        let weak = Arc::downgrade(self);
        let wake = move || {
            if let Some(hub) = weak.upgrade() {
                hub.main_woke();
            }
        };
        let isolate = Isolate::spawn(
            isolate_options("electron-main".into()),
            move || MainGuest::new(hub),
            wake,
        );
        match isolate {
            Ok(isolate) => {
                // The Hub holds the isolate before its first job runs: that job may send the main
                // process a job of its own, as `app.quit()` does.
                let poke = isolate.poker();
                *lock(&self.main) = Some(isolate);
                self.to_main(move |guest| guest.set_waker(poke));
                self.to_main(move |guest| guest.run(program));
                if lock(&self.shell).is_some() {
                    self.from_shell(FromShell::App(AppEvent::Ready));
                }
            }
            Err(error) => {
                // No thread to run the app's script on: the app cannot start at all.
                eprintln!("electron: no thread for the browser process: {error}");
                self.shutdown(1);
            }
        }
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
        if !self.told.load(Ordering::SeqCst)
            && let Some(profile) = self.launch().user_data_dir()
        {
            // The profile of an app nobody launched (a test's) is its own, made for it by
            // `Start::development`, and not the user's to keep. It was made, if at all, at `ready`.
            let _ = std::fs::remove_dir_all(profile);
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

    /// The counter a page's loader numbers its asks of the main process from: one for the app,
    /// so a page's ask is never mistaken for the one the page before it made.
    pub fn protocol_calls(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.protocol_calls)
    }

    /// The code the app is ending with, once it is.
    pub fn exit_code(&self) -> Option<i32> {
        *lock(&self.exit)
    }

    /// How many times the main process's thread has run a beat — taken a turn of jobs, timers and
    /// ports — since the app began: what a benchmark counts wake-ups with.
    pub fn main_beats(&self) -> u64 {
        self.beats.load(Ordering::Relaxed)
    }

    pub(crate) fn count_beat(&self) {
        self.beats.fetch_add(1, Ordering::Relaxed);
    }

    /// Runs `job` on the main process's thread, in turn, as [`Hub::on_renderer`] does.
    pub fn to_main(&self, job: impl FnOnce(&mut MainGuest) + Send + 'static) {
        if let Some(isolate) = lock(&self.main).as_ref() {
            isolate.send(move |guest, _| job(guest));
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::{HeadlessOptions, HeadlessShell};

    pub(super) fn wait(mut cond: impl FnMut() -> bool) -> bool {
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
    pub(super) fn app() -> (Arc<Hub>, Arc<HeadlessShell>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("verve-electron-hub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory for the app");
        let main = dir.join(format!("main{}.js", Instant::now().elapsed().as_nanos()));
        std::fs::write(&main, "new (require('electron').BrowserWindow)();").expect("the script");
        let hub = Hub::new(ElectronOptions {
            app_path: dir,
            dark: false,
        });
        let shell = HeadlessShell::new(&hub, HeadlessOptions::default());
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
}
