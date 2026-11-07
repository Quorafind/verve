//! The pages' isolates: making one, retiring it for a navigation, and what is sent to it and heard
//! from it, checked against the generation of the document it was made for.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use verve::isolate::{Health, Isolate};

use super::{Hub, PER_JOB, isolate_options, lock};
use crate::RendererGuest;
use crate::ids::ContentsId;
use crate::msg::{
    FromRenderer, GoneReason, RendererView, ToRenderer, ToRendererInput, WebPreferences,
};

/// What the main process holds of a page: its id, and the Hub that reaches the thread it runs on.
#[derive(Clone)]
pub struct RendererHandle {
    /// The page.
    pub contents: ContentsId,
    hub: Arc<Hub>,
}

impl RendererHandle {
    /// Sends `msg` to the page.
    pub fn to_renderer(&self, msg: ToRenderer) -> bool {
        self.hub.to_renderer(self.contents, msg)
    }
}

/// One page's isolate, the preferences it was made with, and the last view it published.
pub(super) struct RendererSlot {
    hub: Weak<Hub>,
    contents: ContentsId,
    /// What the page was made with: the isolate P20 makes anew on a navigation is made with these.
    prefs: WebPreferences,
    /// Whether a document has been asked of the page: the next one gets an isolate of its own.
    loaded: AtomicBool,
    /// Which document's isolate is the page's now: it counts up with each one. What an isolate
    /// says, and what is said in its name, is heard only while its number is this one.
    generation: Arc<AtomicU64>,
    isolate: Mutex<Option<Isolate<RendererGuest>>>,
    view: Mutex<Option<RendererView>>,
    /// Whether the main process was told the page's process is gone.
    gone: AtomicBool,
    /// Whether the shell hands the page's keys to the main process first (`before-input-event`).
    intercept_keys: AtomicBool,
    /// When each input sent to the page and not yet taken by it was sent, oldest first. Emptied when
    /// the isolate goes: the jobs it had queued go with it.
    input: Mutex<std::collections::VecDeque<std::time::Instant>>,
}

impl RendererSlot {
    /// Which document's isolate the page has now.
    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Called on the page's own thread, by its isolate, when it has a view, events, or died:
    /// the view waits for the shell, the events go to the main process's thread, and a page
    /// that died is reported to it — once — as `render-process-gone`.
    fn published(&self, generation: u64) {
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        if self.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let (view, events, health) = match lock(&self.isolate).as_ref() {
            Some(isolate) => (isolate.take_view(), isolate.take_events(), isolate.health()),
            None => (None, Vec::new(), Health::Alive),
        };
        if let Some(mut view) = view {
            let mut waiting = lock(&self.view);
            // A view the shell has not read yet is replaced, but what changed in it is not lost.
            if let Some(unread) = waiting.take() {
                carry_changes(unread, &mut view);
            }
            *waiting = Some(view);
        }
        if !events.is_empty() {
            hub.from_renderer_batch(self.contents, generation, events);
        }
        if let Health::Dead(message) = health
            && !self.gone.swap(true, Ordering::SeqCst)
        {
            eprintln!("electron: page {} crashed: {message}", self.contents);
            hub.from_renderer(
                self.contents,
                generation,
                FromRenderer::Gone {
                    reason: GoneReason::Crashed,
                },
            );
        }
    }
}

/// Adds the update of the view `unread`, which `newer` takes the place of, to `newer`'s: a newer
/// view with no update keeps the unread one, and with one it has the changes of both. The nodes
/// the unread view freed are said once, however many views pass before the shell looks.
fn carry_changes(unread: RendererView, newer: &mut RendererView) {
    let Some(old) = unread.update else {
        return;
    };
    let Some(new) = newer.update.as_mut() else {
        newer.update = Some(old);
        return;
    };
    let mut changes = old.changes;
    changes.merge(std::mem::take(&mut new.changes));
    let mut seen = std::collections::HashSet::new();
    changes.removed.retain(|node| seen.insert(*node));
    new.changes = changes;
}

/// Interrupts the script a page is running and lets its isolate go.
pub(super) fn stop(page: &RendererSlot) {
    let isolate = lock(&page.isolate).take();
    if let Some(isolate) = &isolate {
        isolate.retire();
    }
}

impl Hub {
    /// The slot of page `contents`, while it is registered.
    fn slot(&self, contents: ContentsId) -> Option<Arc<RendererSlot>> {
        lock(&self.renderers).get(&contents).cloned()
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
            prefs,
            isolate: Mutex::new(None),
            view: Mutex::new(None),
            gone: AtomicBool::new(false),
            loaded: AtomicBool::new(false),
            intercept_keys: AtomicBool::new(false),
            input: Mutex::new(std::collections::VecDeque::new()),
            generation: Arc::new(AtomicU64::new(0)),
        });
        // Registered first: what the page says while its isolate is made is heard.
        lock(&self.renderers).insert(contents, Arc::clone(&slot));
        self.start_kept_console(contents);
        self.spawn_renderer(&slot);
        RendererHandle {
            contents,
            hub: Arc::clone(self),
        }
    }

    /// Makes the isolate that renders `slot`'s page, with the preferences the page was made with.
    fn spawn_renderer(self: &Arc<Self>, slot: &Arc<RendererSlot>) {
        let (contents, prefs) = (slot.contents, slot.prefs.clone());
        let generation = slot.generation.load(Ordering::SeqCst);
        let hub = Arc::clone(self);
        let make = move || RendererGuest::new(&hub, contents, generation, prefs);
        let published = Arc::downgrade(slot);
        let weak = Arc::downgrade(self);
        let wake = move || {
            if let Some(slot) = published.upgrade() {
                slot.published(generation);
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
                    generation,
                    FromRenderer::Gone {
                        reason: GoneReason::LaunchFailed,
                    },
                );
            }
        }
    }

    /// A page that has loaded something navigates to another document in an isolate of its own,
    /// as Chromium gives a new document a new world: the old one is retired (the one place a
    /// page's isolate is torn down) and the new one is made with the same preferences. The first
    /// load of a page uses the isolate it was made with.
    fn renew_for_navigation(self: &Arc<Self>, contents: ContentsId) -> bool {
        let Some(slot) = self.slot(contents) else {
            return false;
        };
        if !slot.loaded.swap(true, Ordering::SeqCst) {
            return false;
        }
        stop(&slot);
        lock(&slot.input).clear();
        self.clear_kept_console(contents);
        // Nothing the old document said from here on is heard, nor what is said for it.
        slot.generation.fetch_add(1, Ordering::SeqCst);
        *lock(&slot.view) = None;
        // The new page's process may be gone in its turn, and the main process hears of that.
        slot.gone.store(false, Ordering::SeqCst);
        self.spawn_renderer(&slot);
        true
    }

    /// The preferences page `contents` was made with, while it exists.
    pub fn preferences_of(&self, contents: ContentsId) -> Option<WebPreferences> {
        Some(self.slot(contents)?.prefs.clone())
    }

    /// What a page says: a lifecycle step, a request only the window can answer. `generation`
    /// is the document's: what a page that was replaced says is not heard.
    pub fn from_renderer(&self, contents: ContentsId, generation: u64, event: FromRenderer) {
        self.from_renderer_batch(contents, generation, vec![event]);
    }

    /// What a page says in one turn, as one job of the main process: its thread is woken once
    /// for all of it, and hears it in order.
    pub fn from_renderer_batch(
        &self,
        contents: ContentsId,
        generation: u64,
        events: Vec<FromRenderer>,
    ) {
        let Some(current) = self.slot(contents).map(|slot| Arc::clone(&slot.generation)) else {
            return;
        };
        if current.load(Ordering::SeqCst) != generation {
            return;
        }
        for event in &events {
            if let FromRenderer::Console {
                level,
                message,
                line,
                source,
            } = event
            {
                self.log_console(Some(contents), message);
                self.keep_console(contents, *level, (source, *line), message);
                self.forward_console(contents, *level, (source, *line), message);
            }
            if let FromRenderer::Lifecycle(page_event) = event {
                self.log_page_event(contents, page_event);
            }
            self.log_from_renderer(contents, event);
        }
        // The REPL's answers are the inspector window's, and the main process never hears them.
        let events = self.take_inspector_replies(contents, events);
        // At most `PER_JOB` to a job, so what the shell or another page says in the meantime is
        // heard between them.
        let mut events = events.into_iter().peekable();
        while events.peek().is_some() {
            let chunk: Vec<_> = events.by_ref().take(PER_JOB).collect();
            let current = Arc::clone(&current);
            self.to_main(move |guest| {
                for event in chunk {
                    // The document may have been replaced since this was queued.
                    if current.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    guest.from_renderer(contents, event);
                }
            });
        }
    }

    /// Sends `msg` to page `contents`, to run as a job of its isolate.
    /// Answers whether the page's isolate was replaced by this (a `Load` of a page that has loaded):
    /// what the old one was asked will never be answered.
    pub fn to_renderer(self: &Arc<Self>, contents: ContentsId, msg: ToRenderer) -> bool {
        self.log_to_renderer(contents, &msg);
        let replaced =
            matches!(msg, ToRenderer::Load { .. }) && self.renew_for_navigation(contents);
        self.on_renderer(contents, move |guest| guest.apply(msg));
        replaced
    }

    /// What a person did to the window, to the page under it: the hot path, and the only
    /// messages that never touch the main process.
    pub fn input(&self, contents: ContentsId, msg: ToRendererInput) {
        // The shell says what the system's theme is; the page is told what `themeSource` makes
        // of it.
        let msg = match msg {
            ToRendererInput::ColorScheme { dark } => {
                let mut theme = lock(&self.theme);
                theme.set_system(dark);
                let dark = theme.dark();
                ToRendererInput::ColorScheme { dark }
            }
            other => other,
        };
        // Only what a person does is waited for: frames and layouts queued behind a hang are no sign
        // the page is being asked anything.
        let by_person = matches!(
            msg,
            ToRendererInput::Pointer { .. }
                | ToRendererInput::Wheel { .. }
                | ToRendererInput::Key { .. }
                | ToRendererInput::Text(_)
        );
        let slot = self.slot(contents);
        let slot = slot.filter(|_| by_person && self.has_renderer(contents));
        if let Some(slot) = &slot {
            lock(&slot.input).push_back(std::time::Instant::now());
        }
        self.log_input(contents, &msg);
        self.on_renderer(contents, move |guest| {
            guest.input(msg);
            if let Some(slot) = slot {
                lock(&slot.input).pop_front();
            }
        });
    }

    /// Runs `job` on page `contents`'s thread, in turn. It never waits for the page: a caller
    /// that needs an answer has the job send it back, from a thread that may wait.
    pub fn on_renderer(
        &self,
        contents: ContentsId,
        job: impl FnOnce(&mut RendererGuest) + Send + 'static,
    ) {
        let slot = self.slot(contents);
        if let Some(slot) = slot
            && let Some(isolate) = lock(&slot.isolate).as_ref()
        {
            isolate.send(move |guest, _| job(guest));
        }
    }

    /// Whether page `contents` has an isolate to answer: one that was never made, has been
    /// retired or has crashed answers nothing.
    pub fn has_renderer(&self, contents: ContentsId) -> bool {
        let slot = self.slot(contents);
        slot.is_some_and(|slot| !slot.gone.load(Ordering::SeqCst) && lock(&slot.isolate).is_some())
    }

    /// How long the oldest input sent to page `contents` has waited for the page to take it, if
    /// any has: with a script that has run as long, the page is not answering (Chromium's hang rule).
    pub fn input_waiting_for(&self, contents: ContentsId) -> Option<Duration> {
        let slot = self.slot(contents)?;
        let since = lock(&slot.input).front().copied();
        since.map(|since| since.elapsed())
    }

    /// How long the script page `contents` runs has been running, when it runs one: what the main
    /// process's heartbeat reads for `'unresponsive'`.
    pub fn renderer_busy_for(&self, contents: ContentsId) -> Option<Duration> {
        let slot = self.slot(contents)?;
        lock(&slot.isolate).as_ref()?.busy_for()
    }

    /// Ends page `contents`' process as `forcefullyCrashRenderer()` does: the isolate is dropped, the
    /// main process hears it crashed (Electron 39.8.10 says `crashed`, not `killed`), and the next
    /// load makes a new one.
    pub fn kill_renderer(self: &Arc<Self>, contents: ContentsId) {
        let Some(slot) = self.slot(contents) else {
            return;
        };
        slot.loaded.store(true, Ordering::SeqCst);
        if !slot.gone.swap(true, Ordering::SeqCst) {
            stop(&slot);
            lock(&slot.input).clear();
            let generation = slot.generation.load(Ordering::SeqCst);
            let reason = GoneReason::Crashed;
            self.from_renderer(contents, generation, FromRenderer::Gone { reason });
        }
    }

    /// Whether the shell is to hand page `contents`' keys to the main process before the page.
    pub fn set_intercept_keys(&self, contents: ContentsId, on: bool) {
        if let Some(slot) = self.slot(contents) {
            slot.intercept_keys.store(on, Ordering::SeqCst);
        }
    }

    /// Whether keys for page `contents` go to the main process first, as [`crate::msg::FromShell::KeyForContents`].
    pub fn intercepts_keys(&self, contents: ContentsId) -> bool {
        let slot = self.slot(contents);
        slot.is_some_and(|slot| slot.intercept_keys.load(Ordering::SeqCst))
    }

    /// The view the shell has not read yet. Taken: a view is delivered once.
    pub fn renderer_view(&self, contents: ContentsId) -> Option<RendererView> {
        let slot = self.slot(contents)?;
        lock(&slot.view).take()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{app, wait};
    use super::*;
    use crate::msg::LoadOptions;

    /// A view whose document changed at `nodes` since the view before it.
    fn view_changing(nodes: Vec<verve_dom::NodeId>) -> RendererView {
        let mut runtime = verve::Runtime::new();
        let document = verve_dom::install(&mut runtime, verve_dom::Options::default())
            .expect("a document installs");
        let mut update = document.update();
        update.changes = verve_dom::Changes {
            nodes,
            ..verve_dom::Changes::default()
        };
        RendererView {
            applied: 0,
            update: Some(update),
            wants_frame: false,
            cursor: None,
            ime_rect: None,
            display_list: None,
        }
    }

    #[test]
    fn a_view_the_shell_did_not_read_leaves_its_changes_to_the_one_that_replaces_it() {
        let mut unread = view_changing(vec![5, 9]);
        unread.update.as_mut().unwrap().changes.removed = vec![3, 4];
        let mut newer = view_changing(vec![7, 9]);
        newer.update.as_mut().unwrap().changes.removed = vec![4, 6];
        carry_changes(unread, &mut newer);
        let changes = newer.update.expect("an update").changes;
        assert_eq!(changes.nodes, [5, 7, 9]);
        assert_eq!(changes.removed, [3, 4, 6]);
    }

    #[test]
    fn a_newer_view_with_no_update_keeps_the_unread_ones() {
        let unread = view_changing(vec![5]);
        let mut newer = view_changing(vec![]);
        newer.update = None;
        carry_changes(unread, &mut newer);
        assert_eq!(newer.update.expect("the unread update").changes.nodes, [5]);
    }

    #[test]
    fn a_page_that_panics_is_reported_gone_once() {
        let (hub, shell, _) = app();
        assert!(wait(|| !shell.windows().is_empty()));
        let page = shell.windows()[0].contents;
        let slot = hub.slot(page).expect("the page");
        assert!(!slot.gone.load(Ordering::SeqCst));
        hub.on_renderer(page, |_| panic!("the page broke"));
        assert!(wait(|| slot.gone.load(Ordering::SeqCst)));
        // The app is still up, and a second wake does not say it again.
        assert_eq!(shell.exit_code(), None);
        slot.published(0);
        assert!(slot.gone.load(Ordering::SeqCst));
        hub.shutdown(0);
    }

    #[test]
    fn a_document_that_was_replaced_runs_and_says_nothing_more() {
        let (hub, shell, _) = app();
        hub.record_console();
        assert!(wait(|| !shell.windows().is_empty()));
        let page = shell.windows()[0].contents;
        let load = || ToRenderer::Load {
            url: "data:text/html,x".into(),
            options: LoadOptions::default(),
        };
        let say = |text: &'static str| {
            move |guest: &mut RendererGuest| {
                let _ = guest.eval(text);
            }
        };
        hub.to_renderer(page, load());
        // The old document's thread is busy while the page is replaced: what it was sent is
        // still queued, and a timer it set is due.
        hub.on_renderer(page, |_| std::thread::sleep(Duration::from_millis(300)));
        hub.on_renderer(page, say("console.log('OLD-queued')"));
        hub.on_renderer(page, say("setTimeout(() => console.log('OLD-timer'), 10)"));
        hub.to_renderer(page, load());
        hub.on_renderer(page, say("console.log('NEW')"));
        let lines = || -> Vec<String> { hub.console_lines().into_iter().map(|l| l.text).collect() };
        assert!(wait(|| lines().iter().any(|l| l.contains("NEW"))));
        std::thread::sleep(Duration::from_millis(600));
        let lines = lines();
        assert!(!lines.iter().any(|l| l.contains("OLD")), "{lines:?}");
        hub.shutdown(0);
    }

    #[test]
    fn a_page_that_crashed_is_not_asked_anything() {
        let (hub, shell, _) = app();
        assert!(wait(|| !shell.windows().is_empty()));
        let page = shell.windows()[0].contents;
        hub.to_renderer(
            page,
            ToRenderer::Load {
                url: "data:text/html,x".into(),
                options: LoadOptions::default(),
            },
        );
        assert!(hub.has_renderer(page));
        hub.on_renderer(page, |_| panic!("the page broke"));
        assert!(wait(|| !hub.has_renderer(page)));
        hub.shutdown(0);
    }

    #[test]
    fn a_page_that_is_loaded_again_is_reported_gone_again() {
        let (hub, shell, _) = app();
        assert!(wait(|| !shell.windows().is_empty()));
        let page = shell.windows()[0].contents;
        let slot = hub.slot(page).expect("the page");
        let load = || ToRenderer::Load {
            url: "data:text/html,x".into(),
            options: LoadOptions::default(),
        };
        // The first load keeps the page's first isolate; the second gives it a new one.
        hub.to_renderer(page, load());
        hub.to_renderer(page, load());
        hub.on_renderer(page, |_| panic!("the page broke"));
        assert!(wait(|| slot.gone.load(Ordering::SeqCst)));
        hub.to_renderer(page, load());
        assert!(!slot.gone.load(Ordering::SeqCst));
        hub.on_renderer(page, |_| panic!("the page broke again"));
        assert!(wait(|| slot.gone.load(Ordering::SeqCst)));
        hub.shutdown(0);
    }
}
