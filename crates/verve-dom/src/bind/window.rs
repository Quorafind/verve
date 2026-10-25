//! The window: the global object made a `Window` (an `EventTarget` at the top of every
//! event's path), with what a page reads of it — its size, `location` and `history`,
//! `localStorage` and `sessionStorage`, `requestAnimationFrame` and `requestIdleCallback`,
//! `matchMedia`, `getComputedStyle`, `scrollTo`, `screen`, `postMessage`, `alert`…
//!
//! What only the embedder can do — navigate, open a window, show an alert — is a
//! [`crate::Request`] it reads with [`crate::Document::take_requests`].

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, js_string,
    object::builtins::JsProxyBuilder,
    property::{Attribute, PropertyDescriptor},
};

use super::node::{read_dom, wrap};
use super::{
    Accessor, ClassSpec, Kind, Method, arg, arg_string, array, class, closure, dom_error,
    extend_prototype, function, get, handle_of, illegal_constructor, native_instance, node_of,
    opt_string, plain_object, set, string, text,
};
use crate::css::MediaQuery;
use crate::state::{FrameClock, shared};

// ─── Location and history ───────────────────────────────────────────────────────────────────

/// Goes to `href`: within the page (only the fragment differs) the address changes and
/// `hashchange` is heard; anywhere else, the embedder is asked.
pub fn navigate(context: &mut Context, href: &str, replace: bool) -> JsResult<()> {
    let (old, new) = {
        let state = shared(context);
        let state = state.borrow();
        let new = state.url.join(href).ok();
        (state.url.clone(), new)
    };
    let Some(new) = new else {
        return Err(dom_error(
            context,
            &format!(
                "Failed to set the 'href' property on 'Location': '{href}' is not a valid URL."
            ),
            "SyntaxError",
        ));
    };
    let same_page = {
        let mut a = old.clone();
        let mut b = new.clone();
        a.set_fragment(None);
        b.set_fragment(None);
        a == b && new.fragment().is_some()
    };
    if !same_page {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.requests.push(crate::Request::Navigate {
            url: new.to_string(),
            replace,
        });
        state.touch();
        return Ok(());
    }
    {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.url = new.clone();
        if replace {
            let at = state.history_at;
            state.history[at] = (JsValue::null(), new.to_string());
        } else {
            let at = state.history_at + 1;
            state.history.truncate(at);
            state.history.push((JsValue::null(), new.to_string()));
            state.history_at = at;
        }
        state.touch();
    }
    if old != new {
        let global: JsValue = context.global_object().into();
        super::events::fire(
            context,
            &global,
            "HashChangeEvent",
            "hashchange",
            &[
                ("oldURL", text(old.as_str())),
                ("newURL", text(new.as_str())),
                ("bubbles", false.into()),
            ],
        )?;
    }
    Ok(())
}

fn location_part(part: &str, context: &Context) -> String {
    let url = shared(context).borrow().url.clone();
    super::element::url_parts()
        .iter()
        .find(|(name, _)| *name == part)
        .map(|(_, f)| f(&url))
        .unwrap_or_default()
}

macro_rules! location_accessor {
    ($name:literal, $set:expr) => {
        (
            $name,
            Some(|_, _, cx| Ok(text(&location_part($name, cx)))),
            Some(|_, args, cx| {
                let value = arg_string(args, 0, cx)?;
                let mut url = shared(cx).borrow().url.clone();
                let change: fn(&mut url::Url, &str) = $set;
                change(&mut url, &value);
                navigate(cx, url.as_str(), false)?;
                Ok(JsValue::undefined())
            }),
        )
    };
}

const LOCATION_ACCESSORS: &[Accessor] = &[
    (
        "href",
        Some(|_, _, cx| Ok(text(shared(cx).borrow().url.as_str()))),
        Some(|_, args, cx| {
            let href = arg_string(args, 0, cx)?;
            navigate(cx, &href, false)?;
            Ok(JsValue::undefined())
        }),
    ),
    location_accessor!("protocol", |u, v| {
        let _ = u.set_scheme(v.trim_end_matches(':'));
    }),
    location_accessor!("host", |u, v| {
        let (h, p) = v
            .split_once(':')
            .map_or((v, None), |(h, p)| (h, p.parse().ok()));
        let _ = u.set_host(Some(h));
        let _ = u.set_port(p);
    }),
    location_accessor!("hostname", |u, v| {
        let _ = u.set_host(Some(v));
    }),
    location_accessor!("port", |u, v| {
        let _ = u.set_port(v.parse().ok());
    }),
    location_accessor!("pathname", |u, v| u.set_path(v)),
    location_accessor!("search", |u, v| u
        .set_query(Some(v.trim_start_matches('?')).filter(|q| !q.is_empty()))),
    location_accessor!("hash", |u, v| u
        .set_fragment(Some(v.trim_start_matches('#')))),
    (
        "origin",
        Some(|_, _, cx| Ok(text(&location_part("origin", cx)))),
        None,
    ),
    (
        "ancestorOrigins",
        Some(|_, _, cx| Ok(array(Vec::new(), cx))),
        None,
    ),
];

const LOCATION_METHODS: &[Method] = &[
    ("assign", 1, |_, args, cx| {
        let href = arg_string(args, 0, cx)?;
        navigate(cx, &href, false)?;
        Ok(JsValue::undefined())
    }),
    ("replace", 1, |_, args, cx| {
        let href = arg_string(args, 0, cx)?;
        navigate(cx, &href, true)?;
        Ok(JsValue::undefined())
    }),
    ("reload", 0, |_, _, cx| {
        shared(cx)
            .borrow_mut()
            .requests
            .push(crate::Request::Reload);
        Ok(JsValue::undefined())
    }),
    ("toString", 0, |_, _, cx| {
        Ok(text(shared(cx).borrow().url.as_str()))
    }),
];

fn change_history(context: &mut Context, args: &[JsValue], replace: bool) -> JsResult<JsValue> {
    let state_value = arg(args, 0);
    let url = opt_string(args, 2, context)?;
    let state = shared(context);
    let mut state = state.borrow_mut();
    let new = match url {
        Some(url) => match state.url.join(&url) {
            Ok(new) if new.origin() == state.url.origin() => new,
            _ => {
                drop(state);
                return Err(dom_error(
                    context,
                    "Failed to execute 'pushState' on 'History': A history state object with URL cannot be created in a document with a different origin.",
                    "SecurityError",
                ));
            }
        },
        None => state.url.clone(),
    };
    state.url = new.clone();
    if replace {
        let at = state.history_at;
        state.history[at] = (state_value, new.to_string());
    } else {
        let at = state.history_at + 1;
        state.history.truncate(at);
        state.history.push((state_value, new.to_string()));
        state.history_at = at;
    }
    state.touch();
    Ok(JsValue::undefined())
}

/// Moves `delta` through the history, as `history.go` does: the address and state of the entry
/// gone to, and `popstate`, a task later.
fn traverse(context: &mut Context, delta: i64) -> JsResult<JsValue> {
    let moved = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let to = state.history_at as i64 + delta;
        if delta == 0 || to < 0 || to as usize >= state.history.len() {
            None
        } else {
            state.history_at = to as usize;
            let (value, url) = state.history[to as usize].clone();
            if let Ok(url) = url::Url::parse(&url) {
                state.url = url;
            }
            state.touch();
            Some(value)
        }
    };
    if let Some(value) = moved {
        let fire = closure(
            context,
            "popstate",
            |_, _, value: &JsValue, cx| {
                let global: JsValue = cx.global_object().into();
                super::events::fire(
                    cx,
                    &global,
                    "PopStateEvent",
                    "popstate",
                    &[("state", value.clone()), ("bubbles", false.into())],
                )?;
                Ok(JsValue::undefined())
            },
            value,
        );
        verve::set_timeout(context, fire.into(), 0., false, Vec::new());
    }
    Ok(JsValue::undefined())
}

const HISTORY_METHODS: &[Method] = &[
    ("pushState", 3, |_, args, cx| {
        change_history(cx, args, false)
    }),
    ("replaceState", 3, |_, args, cx| {
        change_history(cx, args, true)
    }),
    ("back", 0, |_, _, cx| traverse(cx, -1)),
    ("forward", 0, |_, _, cx| traverse(cx, 1)),
    ("go", 1, |_, args, cx| {
        let delta = arg(args, 0).to_number(cx)? as i64;
        traverse(cx, delta)
    }),
];

const HISTORY_ACCESSORS: &[Accessor] = &[
    (
        "length",
        Some(|_, _, cx| Ok(shared(cx).borrow().history.len().into())),
        None,
    ),
    (
        "state",
        Some(|_, _, cx| {
            let state = shared(cx);
            let state = state.borrow();
            Ok(state.history[state.history_at].0.clone())
        }),
        None,
    ),
    (
        "scrollRestoration",
        Some(|_, _, _| Ok(text("auto"))),
        Some(|_, _, _| Ok(JsValue::undefined())),
    ),
];

// ─── Storage ────────────────────────────────────────────────────────────────────────────────

/// Which storage a `Storage` object is: 0 local, 1 session.
fn storage_of(this: &JsValue) -> JsResult<u32> {
    handle_of(this, Kind::Plain)
        .ok_or_else(|| super::type_error("Illegal invocation: not a Storage"))
}

fn with_storage<R>(
    context: &Context,
    which: u32,
    f: impl FnOnce(&mut std::collections::BTreeMap<String, String>) -> R,
) -> R {
    let state = shared(context);
    let mut state = state.borrow_mut();
    let result = if which == 0 {
        f(&mut state.local)
    } else {
        f(&mut state.session)
    };
    if which == 0 {
        state.persist_storage();
    }
    result
}

fn read_storage<R>(
    context: &Context,
    which: u32,
    f: impl FnOnce(&std::collections::BTreeMap<String, String>) -> R,
) -> R {
    let state = shared(context);
    let state = state.borrow();
    if which == 0 {
        f(&state.local)
    } else {
        f(&state.session)
    }
}

const STORAGE_METHODS: &[Method] = &[
    ("getItem", 1, |this, args, cx| {
        let which = storage_of(this)?;
        let key = arg_string(args, 0, cx)?;
        Ok(read_storage(cx, which, |s| s.get(&key).cloned()).map_or(JsValue::null(), |v| text(&v)))
    }),
    ("setItem", 2, |this, args, cx| {
        let which = storage_of(this)?;
        let key = arg_string(args, 0, cx)?;
        let value = arg_string(args, 1, cx)?;
        with_storage(cx, which, |s| s.insert(key, value));
        Ok(JsValue::undefined())
    }),
    ("removeItem", 1, |this, args, cx| {
        let which = storage_of(this)?;
        let key = arg_string(args, 0, cx)?;
        with_storage(cx, which, |s| s.remove(&key));
        Ok(JsValue::undefined())
    }),
    ("clear", 0, |this, _, cx| {
        let which = storage_of(this)?;
        with_storage(cx, which, |s| s.clear());
        Ok(JsValue::undefined())
    }),
    ("key", 1, |this, args, cx| {
        let which = storage_of(this)?;
        let at = arg(args, 0).to_number(cx)? as usize;
        Ok(read_storage(cx, which, |s| s.keys().nth(at).cloned())
            .map_or(JsValue::null(), |k| text(&k)))
    }),
];

const STORAGE_ACCESSORS: &[Accessor] = &[(
    "length",
    Some(|this, _, cx| {
        let which = storage_of(this)?;
        Ok(read_storage(cx, which, |s| s.len()).into())
    }),
    None,
)];

/// A storage, its items readable as properties too (`localStorage.theme`).
fn storage(context: &mut Context, which: u32) -> JsResult<JsValue> {
    let target = native_instance(context, "Storage", Kind::Plain, which);
    let proxy = JsProxyBuilder::new(target)
        .get(|_, args, cx| {
            let target = arg(args, 0);
            let key = arg(args, 1);
            let own = match target.as_object() {
                Some(object) => object.get(key.to_property_key(cx)?, cx)?,
                None => JsValue::undefined(),
            };
            if !own.is_undefined() || key.as_string().is_none() {
                if own.is_callable() {
                    return super::call_method(&own, "bind", &[target], cx);
                }
                return Ok(own);
            }
            let which = storage_of(&target)?;
            let name = string(&key, cx)?;
            Ok(read_storage(cx, which, |s| s.get(&name).cloned())
                .map_or(JsValue::undefined(), |v| text(&v)))
        })
        .set(|_, args, cx| {
            let target = arg(args, 0);
            let which = storage_of(&target)?;
            let name = string(&arg(args, 1), cx)?;
            let value = string(&arg(args, 2), cx)?;
            with_storage(cx, which, |s| s.insert(name, value));
            Ok(true.into())
        })
        .has(|_, args, cx| {
            let target = arg(args, 0);
            let which = storage_of(&target)?;
            let name = string(&arg(args, 1), cx)?;
            let known = target
                .as_object()
                .map(|o| o.has_property(JsString::from(name.as_str()), cx))
                .transpose()?
                .unwrap_or(false);
            Ok((known || read_storage(cx, which, |s| s.contains_key(&name))).into())
        })
        .delete_property(|_, args, cx| {
            let target = arg(args, 0);
            let which = storage_of(&target)?;
            let name = string(&arg(args, 1), cx)?;
            with_storage(cx, which, |s| s.remove(&name));
            Ok(true.into())
        })
        .own_keys(|_, args, cx| {
            let which = storage_of(&arg(args, 0))?;
            let keys: Vec<JsValue> =
                read_storage(cx, which, |s| s.keys().map(|k| text(k)).collect());
            Ok(array(keys, cx))
        })
        .get_own_property_descriptor(|_, args, cx| {
            let which = storage_of(&arg(args, 0))?;
            let name = string(&arg(args, 1), cx)?;
            match read_storage(cx, which, |s| s.get(&name).cloned()) {
                Some(value) => {
                    let descriptor = plain_object(cx);
                    set(&descriptor, "value", text(&value), cx)?;
                    set(&descriptor, "writable", true, cx)?;
                    set(&descriptor, "enumerable", true, cx)?;
                    set(&descriptor, "configurable", true, cx)?;
                    Ok(descriptor.into())
                }
                None => Ok(JsValue::undefined()),
            }
        })
        .build(context)?;
    Ok(JsObject::from(proxy).into())
}

// ─── Frames and idle callbacks ──────────────────────────────────────────────────────────────

/// Runs the frame's callbacks — every one queued before it began — with the frame's time.
pub fn run_frame(context: &mut Context, now: f64) -> usize {
    let callbacks = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.frame_timer = None;
        std::mem::take(&mut state.frames)
    };
    let count = callbacks.len();
    for (_, callback) in callbacks {
        if let Err(error) = verve::call(context, &callback, &JsValue::undefined(), &[now.into()]) {
            verve::report(context, &error);
        }
    }
    count
}

fn schedule_frame(context: &mut Context) {
    let due = {
        let state = shared(context);
        let state = state.borrow();
        state.frame_clock == FrameClock::Timer && state.frame_timer.is_none()
    };
    if !due {
        if shared(context).borrow().frame_clock == FrameClock::Host {
            shared(context)
                .borrow_mut()
                .requests
                .push(crate::Request::Frame);
        }
        return;
    }
    // The next frame on a 60 Hz beat.
    let now = verve::now(context);
    let delay = 16. - now.rem_euclid(16.);
    let tick = function(context, "frame", 0, |_, _, cx| {
        let now = verve::now(cx);
        run_frame(cx, now);
        Ok(JsValue::undefined())
    });
    let timer = verve::set_timeout(context, tick.into(), delay, false, Vec::new());
    shared(context).borrow_mut().frame_timer = Some(timer);
}

const FRAME_FUNCTIONS: &[Method] = &[
    ("requestAnimationFrame", 1, |_, args, cx| {
        let callback = arg(args, 0);
        if !callback.is_callable() {
            return Err(super::type_error(
                "Failed to execute 'requestAnimationFrame' on 'Window': The callback provided as parameter 1 is not a function.",
            ));
        }
        let id = {
            let state = shared(cx);
            let mut state = state.borrow_mut();
            let id = state.id();
            state.frames.push((id, callback));
            id
        };
        schedule_frame(cx);
        Ok(id.into())
    }),
    ("cancelAnimationFrame", 1, |_, args, cx| {
        let id = arg(args, 0).to_number(cx)? as u32;
        let state = shared(cx);
        let mut state = state.borrow_mut();
        state.frames.retain(|(f, _)| *f != id);
        if state.frames.is_empty()
            && let Some(timer) = state.frame_timer.take()
        {
            drop(state);
            verve::clear_timer(cx, timer);
        }
        Ok(JsValue::undefined())
    }),
    // Idle time is a task later, with a deadline of a frame's worth.
    ("requestIdleCallback", 2, |_, args, cx| {
        let callback = arg(args, 0);
        let run = closure(
            cx,
            "idle",
            |_, _, callback: &JsValue, cx| {
                let deadline = plain_object(cx);
                let start = verve::now(cx);
                set(&deadline, "didTimeout", false, cx)?;
                let remaining = closure(
                    cx,
                    "timeRemaining",
                    |_, _, start: &f64, cx| Ok((50. - (verve::now(cx) - start)).max(0.).into()),
                    start,
                );
                set(&deadline, "timeRemaining", remaining, cx)?;
                verve::call(cx, callback, &JsValue::undefined(), &[deadline.into()])
            },
            callback,
        );
        Ok(verve::set_timeout(cx, run.into(), 1., false, Vec::new()).into())
    }),
    ("cancelIdleCallback", 1, |_, args, cx| {
        if let Some(id) = arg(args, 0).as_number() {
            verve::clear_timer(cx, id as u32);
        }
        Ok(JsValue::undefined())
    }),
];

// ─── matchMedia ─────────────────────────────────────────────────────────────────────────────

fn media_of(this: &JsValue, context: &mut Context) -> JsResult<String> {
    string(&get(this, "media", context)?, context)
}

const MEDIA_QUERY_ACCESSORS: &[Accessor] = &[(
    "matches",
    Some(|this, _, cx| {
        let media = media_of(this, cx)?;
        let env = shared(cx).borrow().env.clone();
        Ok(MediaQuery::new(&media).matches(&env).into())
    }),
    None,
)];

const MEDIA_QUERY_METHODS: &[Method] = &[
    ("addListener", 1, |this, args, cx| {
        let callback = arg(args, 0);
        if callback.is_callable() {
            super::call_method(this, "addEventListener", &[text("change"), callback], cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("removeListener", 1, |this, args, cx| {
        super::call_method(
            this,
            "removeEventListener",
            &[text("change"), arg(args, 0)],
            cx,
        )
    }),
];

/// `matchMedia(query)`: a list whose `matches` follows the viewport and colour scheme, and
/// which hears `change` when they move it.
fn match_media(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let query = arg_string(args, 0, context)?;
    let (target, list) = {
        let state = shared(context);
        let state = state.borrow();
        (
            state.class("EventTarget").expect("made").0,
            state.class("MediaQueryList").expect("made").0,
        )
    };
    let object = target.construct(&[], Some(&list), context)?;
    object.define_property_or_throw(
        js_string!("media"),
        PropertyDescriptor::builder()
            .value(text(&query))
            .writable(false)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    let env = shared(context).borrow().env.clone();
    let media = MediaQuery::new(&query);
    let matches = media.matches(&env);
    shared(context)
        .borrow_mut()
        .media_lists
        .push((object.downgrade(), media, matches));
    Ok(object.into())
}

/// The lists whose answer the environment changed hear `change`.
pub fn media_changed(context: &mut Context) -> JsResult<()> {
    let changed: Vec<(JsObject, String, bool)> = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let env = state.env.clone();
        state.media_lists.retain(|(weak, ..)| weak.is_alive());
        state
            .media_lists
            .iter_mut()
            .filter_map(|(weak, media, last)| {
                let now = media.matches(&env);
                if now == *last {
                    return None;
                }
                *last = now;
                weak.upgrade().map(|o| (o, media.text().to_owned(), now))
            })
            .collect()
    };
    for (list, media, matches) in changed {
        super::events::fire(
            context,
            &list.into(),
            "MediaQueryListEvent",
            "change",
            &[
                ("media", text(&media)),
                ("matches", matches.into()),
                ("bubbles", false.into()),
                ("cancelable", false.into()),
            ],
        )?;
    }
    Ok(())
}

// ─── The window's functions ─────────────────────────────────────────────────────────────────

fn post_message(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let clone = context
        .global_object()
        .get(js_string!("structuredClone"), context)?;
    let data = verve::call(context, &clone, &JsValue::undefined(), &[arg(args, 0)])?;
    let origin = location_part("origin", context);
    let deliver = closure(
        context,
        "message",
        |_, _, (data, origin): &(JsValue, JsString), cx| {
            let global: JsValue = cx.global_object().into();
            let event = verve::dom_events::classes(cx)?;
            let _ = event;
            let class = cx.global_object().get(js_string!("MessageEvent"), cx)?;
            let Some(class) = class.as_object() else {
                return Ok(JsValue::undefined());
            };
            let init = super::dictionary(
                cx,
                &[
                    ("data", data.clone()),
                    ("origin", origin.clone().into()),
                    ("source", global.clone()),
                ],
            )?;
            let message = class.construct(&[text("message"), init.into()], None, cx)?;
            verve::dom_events::dispatch_value(&global, &message, cx)?;
            Ok(JsValue::undefined())
        },
        (data, JsString::from(origin)),
    );
    verve::set_timeout(context, deliver.into(), 0., false, Vec::new());
    Ok(JsValue::undefined())
}

const WINDOW_FUNCTIONS: &[Method] = &[
    ("matchMedia", 1, match_media),
    ("getComputedStyle", 2, |_, args, cx| {
        let id = super::node_arg(args, 0, "getComputedStyle", "Window")?;
        super::platform::computed_style(cx, id)
    }),
    ("scrollTo", 2, |_, args, cx| {
        let root = read_dom(cx, |dom| dom.html);
        super::element::scroll_to(root, args, cx, false)
    }),
    ("scroll", 2, |_, args, cx| {
        let root = read_dom(cx, |dom| dom.html);
        super::element::scroll_to(root, args, cx, false)
    }),
    ("scrollBy", 2, |_, args, cx| {
        let root = read_dom(cx, |dom| dom.html);
        super::element::scroll_to(root, args, cx, true)
    }),
    ("getSelection", 0, |_, _, cx| super::platform::selection(cx)),
    ("postMessage", 2, post_message),
    ("alert", 1, |_, args, cx| {
        let message = opt_string(args, 0, cx)?.unwrap_or_default();
        shared(cx)
            .borrow_mut()
            .requests
            .push(crate::Request::Alert(message));
        Ok(JsValue::undefined())
    }),
    // A page nothing can answer for: what a browser that blocked the dialog answers.
    ("confirm", 1, |_, args, cx| {
        let message = opt_string(args, 0, cx)?.unwrap_or_default();
        shared(cx)
            .borrow_mut()
            .requests
            .push(crate::Request::Alert(message));
        Ok(false.into())
    }),
    ("prompt", 2, |_, args, cx| {
        let message = opt_string(args, 0, cx)?.unwrap_or_default();
        shared(cx)
            .borrow_mut()
            .requests
            .push(crate::Request::Alert(message));
        Ok(JsValue::null())
    }),
    ("open", 3, |_, args, cx| {
        let url = opt_string(args, 0, cx)?.unwrap_or_default();
        let resolved = shared(cx)
            .borrow()
            .url
            .join(&url)
            .map(|u| u.to_string())
            .unwrap_or(url);
        shared(cx)
            .borrow_mut()
            .requests
            .push(crate::Request::Open(resolved));
        Ok(JsValue::null())
    }),
    ("close", 0, |_, _, _| Ok(JsValue::undefined())),
    ("stop", 0, |_, _, _| Ok(JsValue::undefined())),
    ("focus", 0, |_, _, _| Ok(JsValue::undefined())),
    ("blur", 0, |_, _, _| Ok(JsValue::undefined())),
    ("print", 0, |_, _, _| Ok(JsValue::undefined())),
    ("moveTo", 2, |_, _, _| Ok(JsValue::undefined())),
    ("moveBy", 2, |_, _, _| Ok(JsValue::undefined())),
    ("resizeTo", 2, |_, _, _| Ok(JsValue::undefined())),
    ("resizeBy", 2, |_, _, _| Ok(JsValue::undefined())),
    ("captureEvents", 0, |_, _, _| Ok(JsValue::undefined())),
    ("releaseEvents", 0, |_, _, _| Ok(JsValue::undefined())),
];

/// The window's sizes and scroll, read from the environment each time.
const WINDOW_ACCESSORS: &[Accessor] = &[
    (
        "innerWidth",
        Some(|_, _, cx| Ok(f64::from(shared(cx).borrow().env.width).into())),
        None,
    ),
    (
        "innerHeight",
        Some(|_, _, cx| Ok(f64::from(shared(cx).borrow().env.height).into())),
        None,
    ),
    (
        "outerWidth",
        Some(|_, _, cx| Ok(f64::from(shared(cx).borrow().env.width).into())),
        None,
    ),
    (
        "outerHeight",
        Some(|_, _, cx| Ok(f64::from(shared(cx).borrow().env.height).into())),
        None,
    ),
    (
        "devicePixelRatio",
        Some(|_, _, cx| Ok(f64::from(shared(cx).borrow().env.device_pixel_ratio).into())),
        None,
    ),
    (
        "scrollX",
        Some(|_, _, cx| Ok(window_scroll(cx, 0).into())),
        None,
    ),
    (
        "scrollY",
        Some(|_, _, cx| Ok(window_scroll(cx, 1).into())),
        None,
    ),
    (
        "pageXOffset",
        Some(|_, _, cx| Ok(window_scroll(cx, 0).into())),
        None,
    ),
    (
        "pageYOffset",
        Some(|_, _, cx| Ok(window_scroll(cx, 1).into())),
        None,
    ),
    ("screenX", Some(|_, _, _| Ok(0.into())), None),
    ("screenY", Some(|_, _, _| Ok(0.into())), None),
    ("screenLeft", Some(|_, _, _| Ok(0.into())), None),
    ("screenTop", Some(|_, _, _| Ok(0.into())), None),
    (
        "origin",
        Some(|_, _, cx| Ok(text(&location_part("origin", cx)))),
        None,
    ),
];

fn window_scroll(context: &Context, axis: usize) -> f64 {
    let state = shared(context);
    let state = state.borrow();
    let html = state.dom.html;
    f64::from(state.scroll_offsets.get(&html).map_or(0., |o| o[axis]))
}

fn install_screen(context: &mut Context) -> JsResult<()> {
    let screen = plain_object(context);
    let (width, height) = {
        let state = shared(context);
        let state = state.borrow();
        (
            f64::from(state.env.width.max(1920.)),
            f64::from(state.env.height.max(1080.)),
        )
    };
    for (key, value) in [
        ("width", width),
        ("height", height),
        ("availWidth", width),
        ("availHeight", height - 40.),
        ("availLeft", 0.),
        ("availTop", 0.),
        ("colorDepth", 24.),
        ("pixelDepth", 24.),
    ] {
        set(&screen, key, value, context)?;
    }
    set(&screen, "isExtended", false, context)?;
    let target = shared(context)
        .borrow()
        .class("EventTarget")
        .expect("made")
        .0;
    let orientation = target.construct(&[], None, context)?;
    set(&orientation, "type", text("landscape-primary"), context)?;
    set(&orientation, "angle", 0, context)?;
    let lock = function(context, "lock", 1, |_, _, cx| {
        super::rejected(
            super::type_error("screen.orientation.lock() is not available on this device."),
            cx,
        )
    });
    set(&orientation, "lock", lock, context)?;
    let unlock = function(context, "unlock", 0, |_, _, _| Ok(JsValue::undefined()));
    set(&orientation, "unlock", unlock, context)?;
    set(&screen, "orientation", orientation, context)?;
    context.register_global_property(js_string!("screen"), screen, Attribute::all())?;
    // The visual viewport: the window's, unscaled.
    let viewport = target.construct(&[], None, context)?;
    let accessors: &[Accessor] = &[
        (
            "width",
            Some(|_, _, cx| Ok(f64::from(shared(cx).borrow().env.width).into())),
            None,
        ),
        (
            "height",
            Some(|_, _, cx| Ok(f64::from(shared(cx).borrow().env.height).into())),
            None,
        ),
        ("offsetLeft", Some(|_, _, _| Ok(0.into())), None),
        ("offsetTop", Some(|_, _, _| Ok(0.into())), None),
        (
            "pageLeft",
            Some(|_, _, cx| Ok(window_scroll(cx, 0).into())),
            None,
        ),
        (
            "pageTop",
            Some(|_, _, cx| Ok(window_scroll(cx, 1).into())),
            None,
        ),
        ("scale", Some(|_, _, _| Ok(1.into())), None),
    ];
    extend_prototype(context, &viewport, &[], accessors)?;
    context.register_global_property(js_string!("visualViewport"), viewport, Attribute::all())?;
    Ok(())
}

/// Makes `navigator` the window's: the user agent and language the embedder gave, and a
/// clipboard whose writes the embedder carries out.
fn install_navigator(context: &mut Context, options: &crate::Options) -> JsResult<()> {
    let navigator = context
        .global_object()
        .get(js_string!("navigator"), context)?;
    let Some(navigator) = navigator.as_object() else {
        return Ok(());
    };
    if let Some(agent) = &options.user_agent {
        for (key, value) in [
            ("userAgent", agent.as_str()),
            ("appVersion", agent.trim_start_matches("Mozilla/")),
        ] {
            navigator.define_property_or_throw(
                JsString::from(key),
                PropertyDescriptor::builder()
                    .value(text(value))
                    .writable(true)
                    .enumerable(true)
                    .configurable(true),
                context,
            )?;
        }
    }
    if let Some(language) = &options.language {
        navigator.set(js_string!("language"), text(language), false, context)?;
        let languages = array(vec![text(language)], context);
        navigator.set(js_string!("languages"), languages, false, context)?;
    }
    let clipboard = plain_object(context);
    let methods: &[Method] = &[
        ("writeText", 1, |_, args, cx| {
            let content = arg_string(args, 0, cx)?;
            shared(cx)
                .borrow_mut()
                .requests
                .push(crate::Request::CopyText(content));
            super::resolved(JsValue::undefined(), cx)
        }),
        ("readText", 0, |_, _, cx| super::resolved(text(""), cx)),
        ("write", 1, |_, _, cx| {
            super::resolved(JsValue::undefined(), cx)
        }),
        ("read", 0, |_, _, cx| {
            let empty = array(Vec::new(), cx);
            super::resolved(empty, cx)
        }),
    ];
    for (name, length, f) in methods {
        let made = function(context, name, *length, *f);
        set(&clipboard, name, made, context)?;
    }
    navigator.set(js_string!("clipboard"), clipboard, false, context)?;
    Ok(())
}

/// `new FormData(form, submitter)`: Verve's `FormData`, filled from the form's controls as a
/// submission would send them.
fn install_form_data(context: &mut Context) -> JsResult<()> {
    let original = context
        .global_object()
        .get(js_string!("FormData"), context)?;
    let Some(original) = original.as_object() else {
        return Ok(());
    };
    let proxy = JsProxyBuilder::new(original)
        .construct(|_, args, cx| {
            let target = arg(args, 0)
                .as_object()
                .ok_or_else(|| super::type_error("no FormData"))?;
            let given = super::list(&arg(args, 1), cx)?;
            let new_target = arg(args, 2);
            let form = given.first().and_then(super::node_id);
            let submitter = given.get(1).and_then(super::node_id);
            let made = target.construct(&[], new_target.as_object().as_ref(), cx)?;
            if let Some(form) = form {
                for (name, value) in form_entries(cx, form, submitter) {
                    super::call_method(
                        &made.clone().into(),
                        "append",
                        &[text(&name), text(&value)],
                        cx,
                    )?;
                }
            }
            Ok(made.into())
        })
        .build(context)?;
    context.register_global_property(
        js_string!("FormData"),
        JsObject::from(proxy),
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    Ok(())
}

/// What a form submits: each named, enabled control's value — boxes only when ticked, a
/// select's chosen options, the submitter when it has a name.
pub fn form_entries(
    context: &Context,
    form: crate::dom::NodeId,
    submitter: Option<crate::dom::NodeId>,
) -> Vec<(String, String)> {
    read_dom(context, |dom| {
        if dom.get(form).and_then(|n| n.tag()) != Some("form") {
            return Vec::new();
        }
        let mut out = Vec::new();
        for control in dom.descendants(form) {
            let Some(node) = dom.get(control) else {
                continue;
            };
            let Some(name) = node.attr("name").filter(|n| !n.is_empty()) else {
                continue;
            };
            if node.attr("disabled").is_some() {
                continue;
            }
            match node.tag() {
                Some("input") => {
                    let kind = super::element::input_type(dom, control);
                    match kind.as_str() {
                        "checkbox" | "radio" => {
                            if node.checked {
                                out.push((
                                    name.to_owned(),
                                    node.attr("value").unwrap_or("on").to_owned(),
                                ));
                            }
                        }
                        "submit" | "image" | "button" | "reset" => {
                            if submitter == Some(control) {
                                out.push((name.to_owned(), dom.value(control)));
                            }
                        }
                        "file" => {}
                        _ => out.push((name.to_owned(), dom.value(control))),
                    }
                }
                Some("button") if submitter == Some(control) => {
                    out.push((name.to_owned(), node.attr("value").unwrap_or("").to_owned()));
                }
                Some("textarea") => out.push((name.to_owned(), dom.value(control))),
                Some("select") => {
                    for option in dom.options(control) {
                        if super::element::option_selected(dom, option) {
                            out.push((name.to_owned(), dom.option_value(option)));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    })
}

pub fn install(context: &mut Context, options: &crate::Options) -> JsResult<()> {
    // `Window`, the window's class: `window instanceof Window`, and `EventTarget`'s methods
    // reached through it as a browser's window reaches them.
    let (_, window) = class(
        context,
        ClassSpec {
            name: "Window",
            length: 0,
            construct: illegal_constructor,
            parent: Some("EventTarget"),
            methods: WINDOW_FUNCTIONS,
            accessors: WINDOW_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    extend_prototype(context, &window, FRAME_FUNCTIONS, &[])?;
    verve::dom_events::install_window_target(context)?;
    let global = context.global_object();
    global.set_prototype(Some(window));
    for name in ["window", "self", "top", "parent", "frames"] {
        global.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder()
                .value(global.clone())
                .writable(true)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    for (name, value) in [
        ("opener", JsValue::null()),
        ("frameElement", JsValue::null()),
        ("length", 0.into()),
        ("closed", false.into()),
        ("isSecureContext", true.into()),
        ("crossOriginIsolated", false.into()),
        ("name", text("")),
        ("status", text("")),
        ("event", JsValue::undefined()),
    ] {
        global.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder()
                .value(value)
                .writable(true)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    class(
        context,
        ClassSpec {
            name: "Location",
            length: 0,
            construct: illegal_constructor,
            parent: None,
            methods: LOCATION_METHODS,
            accessors: LOCATION_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    let location = native_instance(context, "Location", Kind::Plain, 0);
    context.register_global_property(
        js_string!("location"),
        location,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    class(
        context,
        ClassSpec {
            name: "History",
            length: 0,
            construct: illegal_constructor,
            parent: None,
            methods: HISTORY_METHODS,
            accessors: HISTORY_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    let history = native_instance(context, "History", Kind::Plain, 0);
    context.register_global_property(js_string!("history"), history, Attribute::all())?;
    class(
        context,
        ClassSpec {
            name: "Storage",
            length: 0,
            construct: illegal_constructor,
            parent: None,
            methods: STORAGE_METHODS,
            accessors: STORAGE_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    let local = storage(context, 0)?;
    context.register_global_property(js_string!("localStorage"), local, Attribute::all())?;
    let session = storage(context, 1)?;
    context.register_global_property(js_string!("sessionStorage"), session, Attribute::all())?;
    class(
        context,
        ClassSpec {
            name: "MediaQueryList",
            length: 0,
            construct: illegal_constructor,
            parent: Some("EventTarget"),
            methods: MEDIA_QUERY_METHODS,
            accessors: MEDIA_QUERY_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    let media_proto = shared(context).borrow().prototype("MediaQueryList");
    verve::dom_events::handler_accessors(context, &media_proto, &["change"])?;
    install_screen(context)?;
    install_navigator(context, options)?;
    install_form_data(context)?;
    let _ = node_of;
    let _ = wrap;
    Ok(())
}
