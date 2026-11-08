//! `webContents.navigationHistory`, and the methods `webContents` had before it
//! (`canGoBack()`, `goBack()`…), which Electron 39 keeps and warns about once each.
//!
//! The history itself is [`History`](super::page_state::History), which commits what the page
//! loads; these members read it, and move through it by loading the entry they name.

use std::rc::Rc;

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue};
use verve::native::{self, js_text};

use super::contents_ipc;
use super::define::{self, Args, Kind};
use super::web_contents::record_of;
use super::windows::WindowRecord;
use super::{navigation, shared};
use crate::ids::ContentsId;

/// The kind of object a page's `navigationHistory` is, in [`define`]'s table.
const HISTORY: Kind = Kind("NavigationHistory");

type Op = fn(&Rc<WindowRecord>, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// What the history does: its name on `navigationHistory`, its length and the function.
const OPS: &[(&str, usize, Op)] = &[
    ("canGoBack", 0, |r, _, _| {
        Ok(r.page_state.history.borrow().index_at(-1).is_some().into())
    }),
    ("canGoForward", 0, |r, _, _| {
        Ok(r.page_state.history.borrow().index_at(1).is_some().into())
    }),
    ("canGoToOffset", 1, |r, args, cx| {
        let offset = Args::new(args).f64(0, "offset", cx)?;
        Ok(r.page_state
            .history
            .borrow()
            .index_at(offset as i32)
            .is_some()
            .into())
    }),
    ("goBack", 0, |r, _, cx| go_offset(r, -1, cx)),
    ("goForward", 0, |r, _, cx| go_offset(r, 1, cx)),
    ("goToIndex", 1, |r, args, cx| {
        let index = Args::new(args).f64(0, "index", cx)?;
        navigation::go_to_index(cx, r, whole(index).map_or(-1, |i| i as i64))?;
        Ok(JsValue::undefined())
    }),
    ("goToOffset", 1, |r, args, cx| {
        let offset = Args::new(args).f64(0, "offset", cx)?;
        go_offset(r, offset as i32, cx)
    }),
    ("getActiveIndex", 0, |r, _, _| {
        Ok(r.page_state.history.borrow().active().into())
    }),
    ("length", 0, |r, _, _| {
        Ok((r.page_state.history.borrow().len() as u32).into())
    }),
    ("getEntryAtIndex", 1, |r, args, cx| {
        let index = Args::new(args).f64(0, "index", cx)?;
        let entry = r
            .page_state
            .history
            .borrow()
            .entries()
            .get(whole(index).unwrap_or(usize::MAX))
            .cloned();
        entry.map_or(Ok(JsValue::null()), |entry| {
            entry_object(cx, &entry.url, &entry.title)
        })
    }),
    ("getAllEntries", 0, |r, _, cx| {
        let entries = r.page_state.history.borrow().entries().to_vec();
        let mut all = Vec::new();
        for entry in &entries {
            all.push(entry_object(cx, &entry.url, &entry.title)?);
        }
        Ok(native::array(cx, all))
    }),
    ("clear", 0, |r, _, _| {
        r.page_state.history.borrow_mut().clear();
        Ok(JsValue::undefined())
    }),
    ("removeEntryAtIndex", 1, |r, args, cx| {
        let index = Args::new(args).f64(0, "index", cx)?;
        let removed = r
            .page_state
            .history
            .borrow_mut()
            .remove(whole(index).unwrap_or(usize::MAX));
        Ok(removed.into())
    }),
];

/// The legacy names `webContents` answers, each an entry of [`OPS`] (`clearHistory` is `clear`).
const LEGACY: &[(&str, &str)] = &[
    ("canGoBack", "canGoBack"),
    ("canGoForward", "canGoForward"),
    ("canGoToOffset", "canGoToOffset"),
    ("goBack", "goBack"),
    ("goForward", "goForward"),
    ("goToIndex", "goToIndex"),
    ("goToOffset", "goToOffset"),
    ("clearHistory", "clear"),
    ("getActiveIndex", "getActiveIndex"),
    ("length", "length"),
];

/// `index` as an entry number: a whole number from 0, else none (`-1` and `NaN` name no entry).
fn whole(index: f64) -> Option<usize> {
    (index >= 0. && index.fract() == 0.).then_some(index as usize)
}

fn go_offset(record: &Rc<WindowRecord>, offset: i32, context: &mut Context) -> JsResult<JsValue> {
    let target = record.page_state.history.borrow().index_at(offset);
    if let Some(index) = target {
        navigation::go_to_index(context, record, index as i64)?;
    }
    Ok(JsValue::undefined())
}

/// `{ url, title, pageState }`, an entry as Electron 39.8.10 shows it.
fn entry_object(context: &mut Context, url: &str, title: &str) -> JsResult<JsValue> {
    let fields = [
        ("url", js_text(url)),
        ("title", js_text(title)),
        ("pageState", js_text("")),
    ];
    Ok(define::record(context, &fields)?.into())
}

/// The `navigationHistory` object of page `id`: a plain object with the methods of [`OPS`].
pub(crate) fn make(context: &mut Context, id: ContentsId) -> JsResult<JsObject> {
    let history = native::object(context);
    define::bind(&history, HISTORY, id, context);
    for (index, (name, length, _)) in OPS.iter().enumerate() {
        let method = native::closure(context, name, *length, run, index);
        native::hidden(&history, JsString::from(*name), method, context)?;
    }
    Ok(history)
}

fn run(
    this: &JsValue,
    args: &[JsValue],
    index: &usize,
    context: &mut Context,
) -> JsResult<JsValue> {
    let state = shared(context);
    let (_, record) = define::live(this, HISTORY, context, |id| state.showing(id))?;
    (OPS[*index].2)(&record, args, context)
}

/// `webContents.navigationHistory`.
pub(crate) fn navigation_history(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let page = contents_ipc::page(context, record.page.contents)?;
    page.map_or(Err(define::destroyed()), |page| Ok(page.history.into()))
}

/// Gives `prototype` the legacy history methods.
pub(crate) fn install_legacy(context: &mut Context, prototype: &JsObject) -> JsResult<()> {
    for (index, (name, _)) in LEGACY.iter().enumerate() {
        let method = native::closure(context, name, 0, run_legacy, index);
        native::hidden(prototype, JsString::from(*name), method, context)?;
    }
    Ok(())
}

fn run_legacy(
    this: &JsValue,
    args: &[JsValue],
    index: &usize,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (name, op) = LEGACY[*index];
    let record = record_of(this, context)?;
    let said = format!(
        "(electron) 'webContents.{name}' is deprecated and will be removed. Please use 'webContents.navigationHistory.{op}' instead."
    );
    define::deprecate_once(context, name, &said)?;
    let run = OPS
        .iter()
        .find(|(known, _, _)| *known == op)
        .map(|(_, _, run)| run);
    run.map_or(Ok(JsValue::undefined()), |run| run(&record, args, context))
}
