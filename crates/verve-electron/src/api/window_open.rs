//! `window.open` and `<a target="_blank">`, as the main process answers them: the page's
//! `setWindowOpenHandler` decides, and an allowed one becomes a new `BrowserWindow` that inherits
//! the opener's security settings (a child cannot have more permissions than its parent unless the
//! handler's `overrideBrowserWindowOptions` says so).
//!
//! The details the handler is given, the `disposition`s and `did-create-window` are Electron
//! 39.8.10's. A `window.open` with a features string is `new-window`, one without — and a link
//! with a `target` — is `foreground-tab`. What the opener gets back is `null` (P20: there is no
//! proxy for the new window yet).

use std::rc::Rc;

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};
use verve::native::{self, js_text};

use super::browser_window::window_id_of;
use super::navigation;
use super::page_state::Intent;
use super::windows::WindowRecord;
use super::{contents_ipc, define, ipc_main, shared};
use crate::ids::ContentsId;
use crate::msg::{LoadOptions, ToRenderer};

const BAD_RESPONSE: &str = "The window open handler response must be an object with an 'action' property of 'allow' or 'deny'.";

/// What the handler decided: the options it wants, and whether the new window stays when the
/// opener goes.
struct Allowed {
    overrides: Option<JsObject>,
    outlives_opener: bool,
}

/// A value of a `window.open` features string.
#[derive(Debug, PartialEq)]
enum Feature {
    Number(f64),
    Flag(bool),
    Text(String),
}

/// The `BrowserWindow` options a features string (`width=300,height=200,left=10,resizable=no`)
/// stands for: `left` and `top` are `x` and `y`, the sizes and places are numbers, anything else
/// is a yes/no flag.
fn feature_options(features: &str) -> Vec<(String, Feature)> {
    let mut options = Vec::new();
    for part in features.split([',', ' ']).filter(|part| !part.is_empty()) {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = key.trim().to_ascii_lowercase();
        let number = value.trim().parse::<f64>().ok();
        let option = match (key.as_str(), number) {
            ("left", Some(n)) => ("x".to_owned(), Feature::Number(n)),
            ("top", Some(n)) => ("y".to_owned(), Feature::Number(n)),
            ("width" | "height", Some(n)) => (key, Feature::Number(n)),
            _ => (key, flag_or_text(value.trim())),
        };
        options.push(option);
    }
    options
}

/// A features value: `yes`/`no`, `true`/`false`, `1`/`0` and nothing are flags; anything else (`title=Foo`) is
/// kept as text.
fn flag_or_text(value: &str) -> Feature {
    match value {
        "" | "yes" | "true" | "1" => Feature::Flag(true),
        "no" | "false" | "0" => Feature::Flag(false),
        other => Feature::Text(other.to_owned()),
    }
}

/// Page `contents` asked to open `url` in a window named `name`, with `features`.
pub(crate) fn requested(
    context: &mut Context,
    contents: ContentsId,
    (url, name, features): (String, String, String),
) {
    let Some(opener) = shared(context).showing(contents) else {
        return;
    };
    let disposition = if features.is_empty() {
        "foreground-tab"
    } else {
        "new-window"
    };
    let opened = (|| {
        let handler = opener.page_state.open_handler.borrow().clone();
        let allowed = match handler {
            Some(handler) => {
                let details = details_of(context, &opener, (&url, &name, &features), disposition)?;
                ask(context, &handler, details)
            }
            None => Some(Allowed {
                overrides: None,
                outlives_opener: false,
            }),
        };
        match allowed {
            Some(allowed) => match refusal(&opener.page_state.url.borrow(), &url) {
                None => create(
                    context,
                    &opener,
                    allowed,
                    (url, name, features, disposition),
                ),
                Some(message) => {
                    // The handler was asked, as in Chromium, and the load is refused all the same,
                    // with a line on the opener's console.
                    opener
                        .page
                        .to_renderer(ToRenderer::ConsoleError { message });
                    Ok(())
                }
            },
            None => Ok(()),
        }
    })();
    define::report(context, opened);
}

/// Why a page at `from` may not open `to` in a window, in Chromium's words, if it may not: the rule
/// of the page's own navigations (P20). A `file:` address only from a `file:` page, a `data:` address
/// never as the top of a window, a `blob:` only from its own origin, a `javascript:` address never
/// (verve-dom's `window.open` refuses it too; a link with a target reaches here with it).
fn refusal(from: &str, to: &str) -> Option<String> {
    let target = url::Url::parse(to).ok()?;
    let refused = match target.scheme() {
        "javascript" => format!("Not allowed to open a javascript: URL in a new window: {to}"),
        "data" => format!("Not allowed to navigate top frame to data URL: {to}"),
        "file" if !from.starts_with("file:") => format!("Not allowed to load local resource: {to}"),
        "blob" if url::Url::parse(from).is_ok_and(|from| from.origin() != target.origin()) => {
            format!("Not allowed to load local resource: {to}")
        }
        _ => return None,
    };
    Some(refused)
}

/// The handler's `details`.
fn details_of(
    context: &mut Context,
    opener: &WindowRecord,
    (url, name, features): (&str, &str, &str),
    disposition: &str,
) -> JsResult<JsObject> {
    let referrer = referrer_of(context, opener)?;
    define::record(
        context,
        &[
            ("url", js_text(url)),
            ("frameName", js_text(name)),
            ("features", js_text(features)),
            ("disposition", js_text(disposition)),
            ("referrer", referrer.into()),
        ],
    )
}

/// `{ url, policy }`: the opener's address when it is a web page, else empty.
pub(crate) fn referrer_of(context: &mut Context, opener: &WindowRecord) -> JsResult<JsObject> {
    let from = opener.page_state.url.borrow().clone();
    let from = if from.starts_with("http") {
        from
    } else {
        String::new()
    };
    let policy = js_text("strict-origin-when-cross-origin");
    define::record(context, &[("url", js_text(&from)), ("policy", policy)])
}

/// Calls the handler: `None` for a refusal — `{ action: 'deny' }`, a throw, or an answer that is
/// neither action, which the main process's console is told of.
fn ask(context: &mut Context, handler: &JsObject, details: JsObject) -> Option<Allowed> {
    let called = verve::call(
        context,
        &handler.clone().into(),
        &JsValue::undefined(),
        &[details.into()],
    );
    let answer = define::report(context, called)?;
    let action = native::get(&answer, "action", context)
        .ok()
        .and_then(|a| a.as_string().map(|s| s.to_std_string_escaped()));
    match action.as_deref() {
        Some("deny") => None,
        Some("allow") => {
            let overrides = native::get(&answer, "overrideBrowserWindowOptions", context)
                .ok()?
                .as_object();
            let outlives = native::get(&answer, "outlivesOpener", context)
                .ok()?
                .to_boolean();
            Some(Allowed {
                overrides,
                outlives_opener: outlives,
            })
        }
        _ => {
            let said = ipc_main::console(context, "error", &[js_text(BAD_RESPONSE)]);
            define::report(context, said);
            None
        }
    }
}

/// Makes the window: the features and the handler's options over the opener's preferences, then
/// `did-create-window`, then the load.
fn create(
    context: &mut Context,
    opener: &Rc<WindowRecord>,
    allowed: Allowed,
    (url, name, features, disposition): (String, String, String, &str),
) -> JsResult<()> {
    let state = shared(context);
    let class = state
        .window_class
        .borrow()
        .clone()
        .ok_or_else(define::destroyed)?;
    let options = native::object(context);
    for (key, feature) in feature_options(&features) {
        let value: JsValue = match feature {
            Feature::Number(number) => number.into(),
            Feature::Flag(flag) => flag.into(),
            Feature::Text(text) => js_text(&text),
        };
        options.set(JsString::from(key), value, false, context)?;
    }
    let preferences = inherited_preferences(context, opener.page.contents)?;
    if let Some(overrides) = &allowed.overrides {
        assign(context, &options, overrides)?;
        let given = overrides.get(js_string!("webPreferences"), context)?;
        if let Some(given) = given.as_object() {
            assign(context, &preferences, &given)?;
        }
    }
    options.set(js_string!("webPreferences"), preferences, false, context)?;
    let window = class.construct(&[options.clone().into()], None, context)?;
    let Some(record) =
        window_id_of(&window.clone().into(), context).and_then(|id| state.windows.live(id))
    else {
        return Ok(());
    };
    record.page_state.opener.set(Some(opener.page.contents));
    record
        .page_state
        .outlives_opener
        .set(allowed.outlives_opener);
    let object = contents_ipc::contents_object(context, opener.page.contents)?;
    let referrer = referrer_of(context, opener)?;
    let details = define::record(
        context,
        &[
            ("url", js_text(&url)),
            ("frameName", js_text(&name)),
            ("options", options.into()),
            ("disposition", js_text(disposition)),
            ("referrer", referrer.into()),
            ("postData", JsValue::undefined()),
        ],
    )?;
    // Unlike most events, this one has no event object: `(window, details)`.
    let said = define::emit(
        context,
        &object,
        "did-create-window",
        &[window.into(), details.into()],
    );
    define::report(context, said);
    navigation::start(
        context,
        &record,
        (url, LoadOptions::default()),
        Intent::Push,
        false,
    )?;
    Ok(())
}

/// The preferences a new window starts from, as Electron's `makeWebPreferences`
/// (`guest-window-manager.ts`) takes them: a security setting is copied from the opener only when
/// the opener has its secure value (isolation and sandbox on, Node, webview and
/// WebSQL off); otherwise the new window gets the default, so a window cannot be given more
/// permissions than its opener has by opening it. The handler's options come over these.
fn inherited_preferences(context: &mut Context, opener: ContentsId) -> JsResult<JsObject> {
    let Some(prefs) = shared(context).hub.preferences_of(opener) else {
        return Ok(native::object(context));
    };
    let secure = [
        ("contextIsolation", prefs.context_isolation),
        ("javascript", !prefs.javascript),
        ("nodeIntegration", !prefs.node_integration),
        ("sandbox", prefs.sandbox),
        ("webviewTag", !prefs.webview_tag),
        (
            "nodeIntegrationInSubFrames",
            !prefs.node_integration_in_sub_frames,
        ),
        ("enableWebSQL", !prefs.enable_web_sql),
    ];
    // `secure` says whether the opener holds the secure value; `values` is the value to copy:
    // `contextIsolation` and `sandbox` are secure when true, the rest when false.
    let values = [true, false, false, true, false, false, false];
    let mut copied: Vec<_> = secure
        .iter()
        .zip(values)
        .filter(|((_, holds), _)| *holds)
        .map(|((name, _), value)| (*name, JsValue::from(value)))
        .collect();
    // The session is no permission: a window the page opens is in the page's own session.
    if let Some(partition) = &prefs.partition {
        copied.push(("partition", js_text(partition)));
    }
    define::record(context, &copied)
}

/// `Object.assign(target, source)`.
fn assign(context: &mut Context, target: &JsObject, source: &JsObject) -> JsResult<()> {
    let object = native::global(context, "Object")?;
    native::invoke(
        &object,
        "assign",
        &[target.clone().into(), source.clone().into()],
        context,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_page_cannot_open_a_local_file_or_a_data_window() {
        let refused = |from, to| refusal(from, to).is_some();
        assert!(refused(
            "https://example.test/",
            "file:///C:/Users/me/evil.html"
        ));
        assert!(refused("file:///app/index.html", "data:text/html,x"));
        assert!(refused("file:///app/index.html", "javascript:alert(1)"));
        assert!(refused("https://a.test/", "blob:https://b.test/1"));
        assert!(!refused("https://a.test/", "blob:https://a.test/1"));
        assert!(!refused("file:///app/index.html", "file:///app/b.html"));
        assert!(!refused("https://example.test/", "about:blank"));
    }

    #[test]
    fn a_features_string_is_window_options() {
        let options = feature_options("width=150, height=250,left=10,top=20,resizable=no,popup");
        let names: Vec<_> = options.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["width", "height", "x", "y", "resizable", "popup"]);
        assert_eq!(options[0].1, Feature::Number(150.));
        assert_eq!(options[2].1, Feature::Number(10.));
        assert_eq!(options[4].1, Feature::Flag(false));
        assert_eq!(options[5].1, Feature::Flag(true));
    }
}
