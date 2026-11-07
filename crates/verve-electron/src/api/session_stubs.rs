//! The methods of `Session` that have nothing to act on here, so an app that calls them still
//! runs: the caches, the storage, the connections and the resolver are not Verve's to clear (the
//! network stack keeps none of them between requests that an app could see), and the spell
//! checker, the SSL settings and the download folder are stored and read back but do nothing. An
//! app's promise resolves as Electron's does; `getCacheSize` says 0. Which of them do nothing is
//! P16's 「状态」 list.

use boa_engine::JsString;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, arg, hidden, js_text};

use super::session::session_of;

/// What the session keeps of a call that has no effect, as a hidden property of its object.
const KEPT: &str = "__kept_";

/// Puts the stubs on the prototype of `Session`.
pub(crate) fn install(context: &mut Context, prototype: &JsObject) -> JsResult<()> {
    native::methods(
        context,
        prototype,
        &[
            ("clearCache", 0, resolves),
            ("clearStorageData", 0, resolves),
            ("clearData", 0, resolves),
            ("clearAuthCache", 0, resolves),
            ("clearHostResolverCache", 0, resolves),
            ("closeAllConnections", 0, resolves),
            ("forceReloadProxyConfig", 0, resolves),
            ("flushStorageData", 0, returns_nothing),
            ("getCacheSize", 0, |this, _, cx| {
                session_of(this, cx)?;
                native::settled(Ok(0.into()), cx)
            }),
            ("setDownloadPath", 1, |this, args, cx| {
                keep(this, "downloadPath", args, cx)
            }),
            ("setSSLConfig", 1, |this, args, cx| {
                keep(this, "sslConfig", args, cx)
            }),
            ("setCertificateVerifyProc", 1, |this, args, cx| {
                keep(this, "certificateVerifyProc", args, cx)
            }),
            ("setSpellCheckerEnabled", 1, |this, args, cx| {
                keep(this, "spellCheckerEnabled", args, cx)
            }),
            ("isSpellCheckerEnabled", 0, |this, _, cx| {
                Ok(kept(this, "spellCheckerEnabled", cx)?.to_boolean().into())
            }),
            ("setSpellCheckerLanguages", 1, |this, args, cx| {
                keep(this, "spellCheckerLanguages", args, cx)
            }),
            ("getSpellCheckerLanguages", 0, |this, _, cx| {
                let languages = kept(this, "spellCheckerLanguages", cx)?;
                if languages.is_object() {
                    return Ok(languages);
                }
                Ok(native::array(cx, Vec::new()))
            }),
            (
                "setSpellCheckerDictionaryDownloadURL",
                1,
                |this, args, cx| keep(this, "spellCheckerDictionaryDownloadURL", args, cx),
            ),
            ("listWordsInSpellCheckerDictionary", 0, |this, _, cx| {
                session_of(this, cx)?;
                let none = native::array(cx, Vec::new());
                native::settled(Ok(none), cx)
            }),
            ("addWordToSpellCheckerDictionary", 1, |_, _, _| {
                Ok(true.into())
            }),
            ("removeWordFromSpellCheckerDictionary", 1, |_, _, _| {
                Ok(true.into())
            }),
        ],
    )
}

/// `netLog`: a log that cannot be started, as a session's `netLog` object.
pub(crate) fn net_log(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    session_of(this, context)?;
    let made = kept(this, "netLog", context)?;
    if made.is_object() {
        return Ok(made);
    }
    let log = native::object(context);
    native::methods(
        context,
        &log,
        &[
            ("startLogging", 1, resolves),
            ("stopLogging", 0, |_, _, cx| {
                native::settled(Ok(js_text("")), cx)
            }),
        ],
    )?;
    log.set(JsString::from("currentlyLogging"), false, false, context)?;
    if let Some(target) = this.as_object() {
        hidden(
            &target,
            JsString::from("__kept_netLog"),
            log.clone(),
            context,
        )?;
    }
    Ok(log.into())
}

/// A promise that resolves with nothing.
fn resolves(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    session_of(this, context)?;
    native::settled(Ok(JsValue::undefined()), context)
}

fn returns_nothing(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    session_of(this, context)?;
    Ok(JsValue::undefined())
}

/// Stores the first argument of a call that has no effect.
fn keep(this: &JsValue, name: &str, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    session_of(this, context)?;
    if let Some(target) = this.as_object() {
        let key = JsString::from(format!("{KEPT}{name}"));
        hidden(&target, key, arg(args, 0), context)?;
    }
    Ok(JsValue::undefined())
}

/// What [`keep`] stored under `name`, `undefined` before.
fn kept(this: &JsValue, name: &str, context: &mut Context) -> JsResult<JsValue> {
    native::get(this, &format!("{KEPT}{name}"), context)
}
