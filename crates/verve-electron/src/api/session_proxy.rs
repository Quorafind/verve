//! `session.setProxy` and `session.resolveProxy` (`session.cc` of Electron 39.8.10; the answers
//! and the refusal are observed, see P16's contract). The reading of the rules is
//! [`crate::hub::Proxy`]; this is the script's side of it.

use boa_engine::{Context, JsResult, JsValue};
use verve::Level;
use verve::native::{self, arg, js_text};

use super::define::{self, Args, Fields};
use super::session::session_of;
use crate::hub::{Proxy, ProxyConfig, system_proxy};

/// `setProxy(config)`: the session's requests go as `config` says. Answers a promise.
pub(crate) fn set_proxy(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    Args::new(args).require(1)?;
    let config = arg(args, 0);
    let object = config.as_object();
    let fields = Fields::new(object.as_ref());
    let pac = fields.string("pacScript", context)?;
    // A `pacScript` with no `mode` is the `pac_script` mode, as in Electron.
    let mode = fields.string("mode", context)?.or_else(|| {
        pac.filter(|pac| !pac.is_empty())
            .map(|_| "pac_script".to_owned())
    });
    let rules = fields.string("proxyRules", context)?;
    let bypass = fields.string("proxyBypassRules", context)?;
    let read = ProxyConfig {
        mode: mode.as_deref(),
        rules: rules.as_deref(),
        bypass: bypass.as_deref(),
    };
    let outcome = Proxy::from_config(&read, system_proxy::read);
    if matches!(mode.as_deref(), Some("auto_detect" | "pac_script"))
        && !super::shared(context).net.proxy_warned.replace(true)
    {
        let text = format!(
            "session.setProxy: mode '{}' is not supported; requests connect directly",
            mode.unwrap_or_default()
        );
        verve::console_line(context, Level::Warn, text);
    }
    let outcome = outcome
        .map(|proxy| session.set_proxy(proxy))
        .map(|()| JsValue::undefined())
        .map_err(|message| define::error(&message));
    native::settled(outcome, context)
}

/// `resolveProxy(url)`: `DIRECT`, or the proxy the session's rules give it. Answers a promise;
/// a call with no address, or one that is none, is `DIRECT`.
pub(crate) fn resolve_proxy(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    let given = arg(args, 0);
    let text = match given.as_string() {
        Some(text) => text.to_std_string_escaped(),
        None => String::new(),
    };
    let answer = url::Url::parse(&text)
        .map_or_else(|_| "DIRECT".to_owned(), |url| session.resolve_proxy(&url));
    native::settled(Ok(js_text(&answer)), context)
}
