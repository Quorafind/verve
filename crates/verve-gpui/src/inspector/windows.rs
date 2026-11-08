//! The inspector windows the shell has open, one for each page whose DevTools are open, and what
//! the shell remembers of the pages so that a window opened late has something to show.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AppContext as _, AsyncApp, Bounds, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions,
    px, size,
};
use verve_dom::{Changes, Snapshot, Update};
use verve_electron::Hub;
use verve_electron::ids::{ContentsId, WindowId};
use verve_electron::log::{Category, Severity};
use verve_electron::msg::{FromShell, InspectorEvent};
use verve_layout::DisplayList;

use super::model::Highlight;
use super::overlay::{PickEvent, Picker};
use super::view::InspectorView;
use crate::dom_view::DomView;
use crate::shell::{Inbox, Item};
use crate::window::Window as PageWindow;

/// The size an inspector window opens at.
const WIDTH: f32 = 980.;
const HEIGHT: f32 = 640.;

/// The last document and layout a page published.
struct Latest {
    /// The document the page was showing when this was published ([`Hub::page_generation`]).
    generation: u64,
    snapshot: Option<Snapshot>,
    list: Option<Arc<DisplayList>>,
}

/// An inspector window that is open, and which one it is: a message from a window that was closed
/// before another was opened for the same page is not the new one's.
struct Open {
    serial: u64,
    handle: WindowHandle<InspectorView>,
}

/// The inspector windows, and the last view of every page.
#[derive(Default)]
pub(crate) struct Inspectors {
    open: BTreeMap<ContentsId, Open>,
    latest: BTreeMap<ContentsId, Latest>,
    /// The number of the last window opened.
    opened: u64,
}

/// Says `text` in the `shell` category of the log, which a test reads to know what the shell did;
/// an error is on standard error whether the category is on or not.
fn note(hub: &Hub, severity: Severity, text: &str) {
    let log = hub.log();
    if log.enabled(Category::Shell, severity) {
        log.write(Category::Shell, severity, format_args!("{text}"));
    } else if severity == Severity::Error {
        eprintln!("electron: {text}");
    }
}

impl Inspectors {
    pub fn new() -> Self {
        Self::default()
    }

    /// Page `contents` published `update` and `list`. The inspector window, if there is one, shows
    /// them; whichever way, they are kept for a window that opens later.
    pub fn page(
        &mut self,
        contents: ContentsId,
        generation: u64,
        update: Option<Update>,
        list: Option<Arc<DisplayList>>,
        cx: &mut AsyncApp,
    ) {
        let kept = self.latest.entry(contents).or_insert(Latest {
            generation,
            snapshot: None,
            list: None,
        });
        if generation > kept.generation {
            // Another document: nothing of the old one is kept for a window that opens later.
            *kept = Latest {
                generation,
                snapshot: None,
                list: None,
            };
        } else if generation < kept.generation {
            return;
        }
        if let Some(update) = &update {
            kept.snapshot = Some(update.snapshot.clone());
        }
        if list.is_some() {
            kept.list.clone_from(&list);
        }
        if let Some(open) = self.open.get(&contents) {
            // An error is a window that is gone; its own close tells the shell.
            let _ = open
                .handle
                .update(cx, |view, _, cx| view.page(generation, update, list, cx));
        }
    }

    /// Opens the inspector window of page `contents`, with what the page last published.
    pub fn open(&mut self, contents: ContentsId, hub: &Arc<Hub>, inbox: &Inbox, cx: &mut AsyncApp) {
        if self.open.contains_key(&contents) {
            return;
        }
        let kept = self.latest.get(&contents);
        let url = kept
            .and_then(|kept| kept.snapshot.as_ref())
            .map_or_else(String::new, |snapshot| snapshot.url.clone());
        let title = format!("DevTools - {url}");
        let at = cx.update(|cx| Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(at)),
            titlebar: Some(TitlebarOptions {
                title: Some(title.clone().into()),
                ..TitlebarOptions::default()
            }),
            window_min_size: Some(size(px(480.), px(320.))),
            focus: true,
            show: true,
            ..WindowOptions::default()
        };
        let (view_hub, view_inbox) = (Arc::clone(hub), inbox.clone());
        let opened = cx.open_window(options, move |_, cx| {
            cx.new(|cx| InspectorView::new(contents, view_hub, view_inbox, title, cx))
        });
        let handle = match opened {
            Ok(handle) => handle,
            Err(error) => {
                let text = format!("no inspector window for page {contents}: {error}");
                note(hub, Severity::Error, &text);
                hub.from_shell(FromShell::Inspector {
                    contents,
                    event: InspectorEvent::Closed,
                });
                return;
            }
        };
        let (generation, list, snapshot) = kept.map_or((0, None, None), |k| {
            (k.generation, k.list.clone(), k.snapshot.clone())
        });
        let update = snapshot.map(|snapshot| Update {
            snapshot,
            changes: Changes::default(),
        });
        self.opened += 1;
        let serial = self.opened;
        let inbox = inbox.clone();
        // An error is a window that is gone before it was set up, and it needs nothing then.
        let _ = handle.update(cx, move |view, window, cx| {
            view.page(generation, update, list, cx);
            cx.observe_window_activation(window, |view, window, _| {
                view.activation(window.is_window_active());
            })
            .detach();
            // The window may be active already: the first activation was before this.
            view.activation(window.is_window_active());
            // The shell is told once it has looked at which window this was (see [`Self::gone`]).
            window.on_window_should_close(cx, move |_, _| {
                inbox.push_item(Item::InspectorGone(contents, serial));
                true
            });
        });
        note(
            hub,
            Severity::Info,
            &format!("inspector window of page {contents} opened"),
        );
        self.open.insert(contents, Open { serial, handle });
    }

    /// Closes the inspector window of page `contents`, if it is open.
    pub fn close(&mut self, contents: ContentsId, hub: &Hub, cx: &mut AsyncApp) {
        if let Some(open) = self.open.remove(&contents) {
            // An error is a window GPUI already dropped: it is gone, as was asked.
            let _ = open
                .handle
                .update(cx, |_, window, _| window.remove_window());
            note(
                hub,
                Severity::Info,
                &format!("inspector window of page {contents} closed"),
            );
        }
    }

    /// The page's own window is gone: its inspector goes with it, and the main process is told.
    pub fn close_with_page(&mut self, contents: ContentsId, hub: &Hub, cx: &mut AsyncApp) {
        self.latest.remove(&contents);
        if self.open.contains_key(&contents) {
            self.close(contents, hub, cx);
            hub.from_shell(FromShell::Inspector {
                contents,
                event: InspectorEvent::Closed,
            });
        }
    }

    /// A person closed window `serial` of the inspector of page `contents`, and the main process
    /// is told. A window that was closed (by the DevTools) before another was opened for the page
    /// is not the one open now, and says nothing.
    pub fn gone(&mut self, contents: ContentsId, serial: u64, hub: &Hub) -> bool {
        if self
            .open
            .get(&contents)
            .is_none_or(|open| open.serial != serial)
        {
            return false;
        }
        self.open.remove(&contents);
        note(
            hub,
            Severity::Info,
            &format!("inspector window of page {contents} closed"),
        );
        hub.from_shell(FromShell::Inspector {
            contents,
            event: InspectorEvent::Closed,
        });
        true
    }

    /// The page answered a line of the REPL.
    pub fn reply(
        &mut self,
        contents: ContentsId,
        (call, text, is_error): (u64, String, bool),
        cx: &mut AsyncApp,
    ) {
        if let Some(open) = self.open.get(&contents) {
            let _ = open
                .handle
                .update(cx, |view, _, cx| view.reply(call, text, is_error, cx));
        }
    }

    /// The pointer did something over a page that is being picked from.
    pub fn picked(&mut self, contents: ContentsId, event: PickEvent, cx: &mut AsyncApp) {
        if let Some(open) = self.open.get(&contents) {
            let _ = open.handle.update(cx, |view, _, cx| view.picked(event, cx));
        }
    }

    /// Hands each open window the console lines its page wrote since the last look.
    pub fn pump(&mut self, cx: &mut AsyncApp) {
        for open in self.open.values() {
            let _ = open.handle.update(cx, |view, _, cx| view.pump_console(cx));
        }
    }
}

/// What takes the pointer over a page being picked from: it queues what the pointer did.
pub(crate) fn picker(inbox: &Inbox, contents: ContentsId) -> Picker {
    let inbox = inbox.clone();
    Rc::new(move |event| inbox.push_item(Item::Picked(contents, event)))
}

/// Does `change` to the view of the window that shows page `contents`.
pub(crate) fn on_page_window(
    windows: &BTreeMap<WindowId, PageWindow>,
    contents: ContentsId,
    cx: &mut AsyncApp,
    change: impl FnOnce(&mut DomView) + 'static,
) {
    if let Some(window) = windows.values().find(|w| w.contents == contents) {
        window.show(change, cx);
    }
}

/// The inspector of page `contents` is gone: its window draws nothing over the page, and does not
/// pick.
pub(crate) fn clear_overlay(
    windows: &BTreeMap<WindowId, PageWindow>,
    contents: ContentsId,
    cx: &mut AsyncApp,
) {
    on_page_window(windows, contents, cx, |view| {
        view.set_highlight(None::<Highlight>);
        view.set_picker(None);
    });
}
