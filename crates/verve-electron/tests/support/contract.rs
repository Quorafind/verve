//! What every shell does with a window, as a test that knows only [`ShellPort`]: the headless
//! shell and the GPUI shell are held to the same steps, and to the events Electron 39.8.10 sent
//! for them (the `window-state` fixture's `expected.json`, which P00's state machine keeps).
//!
//! A test makes a [`Hub`] with no main process, puts a shell under it, calls [`watch`] and runs
//! [`window_state`]; the messages the main process would send go to the shell through
//! [`Hub::to_shell`] and what the shell says is read from the observer.
#![allow(dead_code)] // Each test file runs some of the contract.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use verve_electron::Hub;
use verve_electron::ids::WindowId;
use verve_electron::msg::{FromShell, ToShell, WindowEvent, WindowOp, WindowOptions};

/// What a shell has said of its windows, in order.
#[derive(Clone, Default)]
pub struct Said {
    events: Arc<Mutex<Vec<(WindowId, WindowEvent)>>>,
    displays: Arc<Mutex<Option<Vec<verve_electron::msg::Display>>>>,
}

/// Starts keeping what the shell under `hub` says of its windows.
pub fn watch(hub: &Arc<Hub>) -> Said {
    let said = Said::default();
    let kept = said.clone();
    hub.observe_shell(move |msg| match msg {
        FromShell::Window { window, event } => {
            let mut events = kept.events.lock().expect("the events");
            events.push((*window, event.clone()));
        }
        FromShell::Displays(displays) => {
            *kept.displays.lock().expect("the displays") = Some(displays.clone());
        }
        _ => {}
    });
    said
}

impl Said {
    /// The displays the shell last reported.
    pub fn displays(&self) -> Option<Vec<verve_electron::msg::Display>> {
        self.displays.lock().expect("the displays").clone()
    }

    /// The events not yet taken, by name, and forgotten.
    pub fn take(&self) -> Vec<(WindowId, WindowEvent)> {
        std::mem::take(&mut *self.events.lock().expect("the events"))
    }

    /// Waits until nothing has been said for `quiet`, or `limit` has passed, and takes what was said.
    pub fn settle(&self, quiet: Duration, limit: Duration) -> Vec<(WindowId, WindowEvent)> {
        let (start, mut last_count, mut since) = (Instant::now(), 0, Instant::now());
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let count = self.events.lock().expect("the events").len();
            if count != last_count {
                (last_count, since) = (count, Instant::now());
            }
            if since.elapsed() >= quiet || start.elapsed() >= limit {
                return self.take();
            }
        }
    }
}

/// The variant of `event`, without its fields.
pub fn name(event: &WindowEvent) -> String {
    let text = format!("{event:?}");
    text.split([' ', '(']).next().unwrap_or("").to_owned()
}

/// What a window did, as the names of its events.
fn names(events: &[(WindowId, WindowEvent)]) -> Vec<String> {
    events.iter().map(|(_, event)| name(event)).collect()
}

/// How a shell is held to the contract.
pub struct Rig<'a> {
    pub hub: &'a Arc<Hub>,
    pub said: &'a Said,
    /// Whether the focus events are the shell's to promise: a real window cannot take the focus
    /// from the person using the machine, so its `Focus` and `Blur` are left out of the comparison.
    pub focus_exact: bool,
    /// How long a step may take to say all it has to.
    pub quiet: Duration,
    /// Whether the shell sends `NativeHandle` and `FirstPaint` (a real one does, once each).
    pub real: bool,
    handles: std::cell::Cell<u32>,
    paints: std::cell::Cell<u32>,
}

impl<'a> Rig<'a> {
    /// A rig for the shell under `hub`, which says what it says to `said`.
    pub fn new(hub: &'a Arc<Hub>, said: &'a Said, real: bool) -> Self {
        Self {
            hub,
            said,
            focus_exact: !real,
            quiet: Duration::from_millis(if real { 250 } else { 30 }),
            real,
            handles: std::cell::Cell::new(0),
            paints: std::cell::Cell::new(0),
        }
    }

    /// Takes what the shell said, setting aside the events a real shell adds to a window's story.
    fn gather(&self, events: Vec<(WindowId, WindowEvent)>) -> Vec<(WindowId, WindowEvent)> {
        let (extra, events): (Vec<_>, Vec<_>) = events.into_iter().partition(|(_, event)| {
            matches!(
                event,
                WindowEvent::NativeHandle(_) | WindowEvent::FirstPaint
            )
        });
        for (_, event) in extra {
            let count = if matches!(event, WindowEvent::FirstPaint) {
                &self.paints
            } else {
                &self.handles
            };
            count.set(count.get() + 1);
        }
        events
    }

    fn step(&self, window: WindowId, op: WindowOp) -> Vec<(WindowId, WindowEvent)> {
        self.hub.to_shell(ToShell::Window { window, op });
        self.gather(self.said.settle(self.quiet, Duration::from_secs(5)))
    }

    /// The events of a step as names. A real window cannot take the focus from the person using
    /// the machine, so its `Focus` and `Blur` are not promised: they are taken out, after being
    /// checked to come last in the step, as they do in every recording.
    fn seen(&self, events: &[(WindowId, WindowEvent)]) -> Vec<String> {
        let mut seen = names(events);
        if !self.focus_exact {
            let focus = |name: &String| name == "Focus" || name == "Blur";
            let first = seen.iter().position(focus).unwrap_or(seen.len());
            assert!(seen[first..].iter().all(focus), "focus is last: {events:?}");
            seen.truncate(first);
        }
        seen
    }

    fn expect(&self, label: &str, window: WindowId, op: WindowOp, expected: &[&str]) {
        let events = self.step(window, op);
        let mut want: Vec<&str> = expected.to_vec();
        if !self.focus_exact {
            want.retain(|name| *name != "Focus" && *name != "Blur");
        }
        assert_eq!(self.seen(&events), want, "{label}: {events:?}");
    }

    /// A real shell has sent the window's handle and its first paint `handles` and `paints` times in all.
    pub fn sent_extras(&self, handles: u32, paints: u32) {
        if self.real {
            assert_eq!((self.handles.get(), self.paints.get()), (handles, paints));
        }
    }

    /// Makes window `id` with `options` and says nothing happened: creating sends no event.
    pub fn create(&self, id: WindowId, options: WindowOptions) {
        let handles = self.handles.get();
        self.hub.to_shell(ToShell::CreateWindow {
            window: id,
            contents: id,
            options,
        });
        // The first window of a real shell takes a while to come up.
        let first = self.said.settle(self.quiet, Duration::from_secs(10));
        let mut events = self.gather(first);
        while self.real && self.handles.get() == handles {
            events.extend(self.gather(self.said.settle(self.quiet, Duration::from_secs(10))));
        }
        assert_eq!(
            self.seen(&events),
            Vec::<String>::new(),
            "creating {id}: {events:?}"
        );
    }
}

/// `value` is within a pixel of `wanted`.
fn near(value: f32, wanted: f32) -> bool {
    (value - wanted).abs() <= 1.
}

/// The `window-state` fixture's steps, on window `id`: each is checked against the events Electron
/// sent for it, and the last closes the window by the user's way (asked, then answered).
pub fn window_state(rig: &Rig, id: WindowId) {
    let options = WindowOptions {
        show: false,
        ..WindowOptions::default()
    };
    rig.create(id, options);
    rig.expect("show", id, WindowOp::Show, &["Shown", "Focus"]);
    rig.expect("hide", id, WindowOp::Hide, &["Hidden", "Blur"]);
    rig.expect("showInactive", id, WindowOp::ShowInactive, &["Shown"]);
    let bounds = WindowOp::SetBounds {
        x: Some(50.),
        y: Some(60.),
        width: Some(500.),
        height: Some(400.),
        animate: false,
    };
    let events = rig.step(id, bounds);
    assert_eq!(
        rig.seen(&events),
        ["Resized", "Moved"],
        "setBounds: {events:?}"
    );
    let (resized, moved) = (&events[0].1, &events[1].1);
    assert!(
        matches!(resized, WindowEvent::Resized { width, height } if near(*width, 500.) && near(*height, 400.)),
        "{resized:?}"
    );
    assert!(
        matches!(moved, WindowEvent::Moved { x, y } if near(*x, 50.) && near(*y, 60.)),
        "{moved:?}"
    );
    rig.expect(
        "maximize",
        id,
        WindowOp::Maximize,
        &["Resized", "Moved", "Maximized"],
    );
    rig.expect(
        "unmaximize",
        id,
        WindowOp::Unmaximize,
        &["Resized", "Unmaximized"],
    );
    // Electron's window was the active one when it was minimized (it said `blur`): the recording's
    // `showInactive` window had been made so by the system, which a shell is made so by asking.
    rig.expect("focus", id, WindowOp::Focus, &["Focus"]);
    rig.expect("minimize", id, WindowOp::Minimize, &["Minimized", "Blur"]);
    rig.expect("restore", id, WindowOp::Restore, &["Restored", "Focus"]);
    rig.expect("blur", id, WindowOp::Blur, &["Blur"]);
    rig.expect("focus", id, WindowOp::Focus, &["Focus"]);
    let full = WindowOp::SetFullScreen(true);
    rig.expect(
        "setFullScreen(true)",
        id,
        full,
        &["EnterFullScreen", "Resized", "Moved"],
    );
    let leave = WindowOp::SetFullScreen(false);
    rig.expect(
        "setFullScreen(false)",
        id,
        leave,
        &["LeaveFullScreen", "Resized", "Moved"],
    );
    rig.expect("close", id, WindowOp::Close, &["CloseRequested"]);
    rig.expect(
        "close answered",
        id,
        WindowOp::CloseAnswer { prevented: false },
        &["Closed"],
    );
    rig.expect("after closing", id, WindowOp::Show, &[]);
    rig.sent_extras(1, 1);
}

/// The operations that change a window and say nothing, a close that is kept, and a destroy that
/// asks nobody; none of them says `Closed` twice.
pub fn quiet_and_closing(rig: &Rig, id: WindowId) {
    let options = WindowOptions {
        show: false,
        ..WindowOptions::default()
    };
    rig.create(id, options);
    let quiet = [
        WindowOp::SetTitle("another".into()),
        WindowOp::SetOpacity(0.5),
        WindowOp::SetOpacity(1.),
        WindowOp::SetAlwaysOnTop(true),
        WindowOp::SetAlwaysOnTop(false),
        WindowOp::SetBackgroundColor([30, 30, 30, 255]),
        WindowOp::SetProgressBar(0.5),
        WindowOp::SetProgressBar(-1.),
        WindowOp::SetSkipTaskbar(true),
        WindowOp::SetSkipTaskbar(false),
        WindowOp::SetIgnoreMouseEvents(true),
        WindowOp::SetIgnoreMouseEvents(false),
        WindowOp::SetResizable(false),
        WindowOp::SetResizable(true),
        WindowOp::FlashFrame(true),
        WindowOp::FlashFrame(false),
    ];
    for op in quiet {
        rig.expect(&format!("{op:?}"), id, op.clone(), &[]);
    }
    rig.expect("close", id, WindowOp::Close, &["CloseRequested"]);
    rig.expect("kept", id, WindowOp::CloseAnswer { prevented: true }, &[]);
    rig.expect("still there", id, WindowOp::ShowInactive, &["Shown"]);
    rig.expect("destroy", id, WindowOp::Destroy, &["Closed"]);
    rig.expect("destroyed twice", id, WindowOp::Destroy, &[]);
    rig.expect(
        "answered after",
        id,
        WindowOp::CloseAnswer { prevented: false },
        &[],
    );
}

/// Limits hold: a window never gets smaller than its minimum or bigger than its maximum, whether
/// the app set them or a person dragged, and says `Resized` with the size it has.
pub fn limits(rig: &Rig, id: WindowId) {
    let options = WindowOptions {
        show: false,
        ..WindowOptions::default()
    };
    rig.create(id, options);
    rig.expect("show", id, WindowOp::ShowInactive, &["Shown"]);
    rig.step(id, WindowOp::SetMinSize(300., 200.));
    rig.step(id, WindowOp::SetMaxSize(1000., 0.));
    let wanted = WindowOp::SetBounds {
        x: None,
        y: None,
        width: Some(2000.),
        height: Some(100.),
        animate: false,
    };
    let events = rig.step(id, wanted);
    let sized = events.iter().find_map(|(_, event)| match event {
        WindowEvent::Resized { width, height } => Some((*width, *height)),
        _ => None,
    });
    let (width, height) = sized.expect("a window that was resized says so");
    assert!(
        near(width, 1000.) && near(height, 200.),
        "{width} x {height}"
    );
    rig.step(id, WindowOp::Destroy);
}
