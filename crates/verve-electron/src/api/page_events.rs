//! What a page says of its life, heard as the events of its `webContents` — and, for the title and
//! `ready-to-show`, of its window — in Electron 39.8.10's order.
//!
//! A first load says `frame-created`, `did-start-loading`, `did-start-navigation`,
//! `did-frame-navigate`, `did-navigate`, `page-title-updated`, `dom-ready`, `did-frame-finish-load`,
//! `did-finish-load`, `did-stop-loading`. A load that fails says `did-fail-provisional-load` and
//! `did-fail-load`, and then — Chromium commits an error page — the finish events all the same.
//! A move within the page (`location.hash`) says `did-start-loading`, `did-start-navigation`,
//! `did-navigate-in-page`, `did-stop-loading`. `did-stop-loading` is not the page's to say: it
//! follows `DidFinishLoad` and `DidFailLoad`.

use std::rc::Rc;

use boa_engine::{Context, JsObject, JsValue};
use verve::native::{self, js_text};

use super::navigation::{emit_navigation, settle_load};
use super::page_state::Intent;
use super::windows::WindowRecord;
use super::{contents_ipc, define, shared, window_events};
use crate::ids::ContentsId;
use crate::ipc::{self, MAIN_FRAME};
use crate::msg::PageEvent;

/// The status Electron reports for a document it committed: 200 `OK`, for files and data too.
const OK: (i32, &str) = (200, "OK");

/// What the page of `contents` says about its life.
pub(crate) fn page_event(context: &mut Context, contents: ContentsId, event: PageEvent) {
    let Some(record) = shared(context).showing(contents) else {
        return;
    };
    let Ok(object) = contents_ipc::contents_object(context, contents) else {
        return;
    };
    let page = Page {
        record: &record,
        object: &object,
        contents,
    };
    match event {
        PageEvent::DidStartLoading => page.started(context),
        PageEvent::DidNavigate { url } => page.committed(context, &url),
        PageEvent::DidNavigateInPage { url } => page.moved_within(context, &url),
        PageEvent::DomReady => page.say(context, "dom-ready", &[]),
        PageEvent::DidFinishLoad => page.finished(context, false),
        PageEvent::DidFailLoad {
            code,
            description,
            url,
        } => {
            page.failed(context, code, description, url);
        }
        PageEvent::TitleUpdated(title) => {
            record.page_state.title.replace(title.clone());
            record.page_state.history.borrow_mut().set_title(&title);
            window_events::page_title(context, &record, &title);
        }
        PageEvent::FaviconUpdated(icons) => {
            let icons = icons.iter().map(|icon| js_text(icon)).collect();
            let icons = native::array(context, icons);
            page.say(context, "page-favicon-updated", &[icons]);
        }
        PageEvent::FirstPaintable => {
            window_events::update_ready(&record, |ready| ready.paintable = true)
        }
    }
    window_events::check_ready(context, &record);
}

/// The page of one window and the object that stands for it.
struct Page<'a> {
    record: &'a Rc<WindowRecord>,
    object: &'a JsObject,
    contents: ContentsId,
}

impl Page<'_> {
    fn say(&self, context: &mut Context, name: &str, args: &[JsValue]) {
        define::emit_reporting(context, self.object, name, args);
    }

    /// `(isMainFrame, processId, routingId)`, the tail of the frame events.
    fn frame_tail(&self) -> [JsValue; 3] {
        [
            true.into(),
            ipc::process_id(self.contents).into(),
            MAIN_FRAME.into(),
        ]
    }

    fn started(&self, context: &mut Context) {
        let page = &self.record.page_state;
        page.loading.set(true);
        page.crashed.set(false);
        if !page.frame_created.replace(true) {
            let frame = contents_ipc::page(context, self.contents).ok().flatten();
            if let Some(frame) = frame.map(|page| page.frame) {
                let details = define::record(context, &[("frame", frame.into())]);
                self.say(
                    context,
                    "frame-created",
                    &[details.map_or_else(|_| JsValue::undefined(), Into::into)],
                );
            }
        }
        self.say(context, "did-start-loading", &[]);
        if !page.announced.replace(true) {
            let target = page.target.borrow().clone();
            emit_navigation(
                context,
                self.object,
                "did-start-navigation",
                (&target, false),
            );
        }
    }

    fn committed(&self, context: &mut Context, url: &str) {
        let page = &self.record.page_state;
        page.url.replace(url.to_owned());
        page.title.take();
        page.target.take();
        page.announced.set(false);
        page.history.borrow_mut().commit(page.intent.take(), url);
        let [main, process, routing] = self.frame_tail();
        let status = [js_text(url), OK.0.into(), js_text(OK.1)];
        let framed = [status.clone().to_vec(), vec![main, process, routing]].concat();
        self.say(context, "did-frame-navigate", &framed);
        self.say(context, "did-navigate", &status);
    }

    fn moved_within(&self, context: &mut Context, url: &str) {
        let page = &self.record.page_state;
        page.url.replace(url.to_owned());
        page.history.borrow_mut().commit(Intent::Push, url);
        self.say(context, "did-start-loading", &[]);
        emit_navigation(context, self.object, "did-start-navigation", (url, true));
        let [_, process, routing] = self.frame_tail();
        self.say(
            context,
            "did-navigate-in-page",
            &[js_text(url), true.into(), process, routing],
        );
        self.say(context, "did-stop-loading", &[]);
    }

    /// The load is over, however it went: the finish events, and `ready-to-show` may follow.
    /// `after_failure` says the error page's `dom-ready` comes first.
    fn finished(&self, context: &mut Context, after_failure: bool) {
        let [main, process, routing] = self.frame_tail();
        if after_failure {
            self.say(context, "dom-ready", &[]);
        }
        self.say(context, "did-frame-finish-load", &[main, process, routing]);
        self.say(context, "did-finish-load", &[]);
        self.record.page_state.loading.set(false);
        self.say(context, "did-stop-loading", &[]);
        window_events::update_ready(self.record, |ready| ready.finished = true);
        settle_load(context, self.record, Ok(()));
    }

    fn failed(&self, context: &mut Context, code: i32, description: String, url: String) {
        let page = &self.record.page_state;
        page.url.replace(url.clone());
        page.title.take();
        page.target.take();
        page.announced.set(false);
        page.history.borrow_mut().commit(page.intent.take(), &url);
        let [main, process, routing] = self.frame_tail();
        let args = [
            code.into(),
            js_text(&description),
            js_text(&url),
            main,
            process,
            routing,
        ];
        for name in ["did-fail-provisional-load", "did-fail-load"] {
            self.say(context, name, &args);
        }
        settle_load(context, self.record, Err((code, description, url)));
        self.finished(context, true);
    }
}
