//! `navigator`, as Obsidian's desktop app has it: Electron's Chromium, saying so in its user
//! agent, on the machine it runs on.
//!
//! What plugins read of it is mostly what kind of browser and machine they are in —
//! `userAgent`, `platform`, `maxTouchPoints`, `language`, `hardwareConcurrency` — and that is
//! what is here, made the first time it is read. What a page's `navigator` does beyond that
//! (`clipboard`, `geolocation`, `share`…) belongs to the host's window, which may put its own
//! `navigator` in place of this one.

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};

use crate::native::{array, exports, js_text, object};
use crate::process::{CHROME_VERSION, ELECTRON_VERSION};

/// What the user agent says the machine is, and what `navigator.platform` answers.
fn machine() -> (&'static str, &'static str) {
    match std::env::consts::OS {
        "windows" => ("Windows NT 10.0; Win64; x64", "Win32"),
        "macos" => ("Macintosh; Intel Mac OS X 10_15_7", "MacIntel"),
        _ => ("X11; Linux x86_64", "Linux x86_64"),
    }
}

/// What the user agent Electron's renderer sends. An app that has a name and a version puts them
/// in front of Chrome's (`MyApp/1.0 Chrome/…`), as `app.userAgentFallback` does.
pub fn electron_user_agent(app: Option<(&str, &str)>) -> String {
    let app = app.map_or(String::new(), |(name, version)| {
        format!("{name}/{version} ")
    });
    format!(
        "Mozilla/5.0 ({}) AppleWebKit/537.36 (KHTML, like Gecko) {app}Chrome/{CHROME_VERSION} Electron/{ELECTRON_VERSION} Safari/537.36",
        machine().0
    )
}

pub(crate) fn user_agent() -> String {
    electron_user_agent(None)
}

/// `navigator.appVersion`, from the whole user agent: what follows `Mozilla/`, as browsers
/// answer it.
fn app_version(agent: &str) -> &str {
    agent.trim_start_matches("Mozilla/")
}

/// The user agent `navigator` answers: the embedder's when it set one, else Electron's.
fn agent_of(context: &Context) -> String {
    let set = crate::shared(context).user_agent.borrow().clone();
    set.unwrap_or_else(user_agent)
}

/// Puts `agent`, and the `appVersion` that follows from it, on `navigator`.
fn set_agent(navigator: &JsObject, agent: &str, context: &mut Context) -> JsResult<()> {
    navigator.set(js_string!("userAgent"), js_text(agent), false, context)?;
    navigator.set(
        js_string!("appVersion"),
        js_text(app_version(agent)),
        false,
        context,
    )?;
    Ok(())
}

/// The user agent `navigator` answers from now on, whole. The realm's own record is replaced,
/// and a `navigator` the scripts already read is mended in place; one they never read is made
/// from the record when they do, so this makes nothing they do not ask for.
pub(crate) fn set_user_agent(context: &mut Context, agent: String) {
    crate::shared(context)
        .user_agent
        .borrow_mut()
        .replace(agent);
    let Some(navigator) = crate::native::lazy_value(context, "navigator") else {
        return;
    };
    let Some(navigator) = navigator.as_object() else {
        return;
    };
    let agent = agent_of(context);
    // The scripts' own `navigator` can have been frozen; it then keeps the agent it had, and the
    // record above is what a `navigator` made later answers.
    let _ = set_agent(&navigator, &agent, context);
}

/// `navigator.userAgentData`: Chromium's client hints.
fn client_hints(context: &mut Context) -> JsResult<JsObject> {
    let major = CHROME_VERSION.split('.').next().unwrap_or("138");
    let data = object(context);
    let mut brands = Vec::new();
    for (brand, version) in [("Not)A;Brand", "8"), ("Chromium", major)] {
        let entry = object(context);
        entry.set(js_string!("brand"), js_text(brand), false, context)?;
        entry.set(js_string!("version"), js_text(version), false, context)?;
        brands.push(entry.into());
    }
    let brands = array(context, brands);
    data.set(js_string!("brands"), brands, false, context)?;
    data.set(js_string!("mobile"), false, false, context)?;
    let hint_platform = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        _ => "Linux",
    };
    data.set(
        js_string!("platform"),
        js_text(hint_platform),
        false,
        context,
    )?;
    exports(
        context,
        &data,
        &[
            ("getHighEntropyValues", 1, |this, _, cx| {
                let values = object(cx);
                for key in ["brands", "mobile", "platform"] {
                    let value = crate::native::get(this, key, cx)?;
                    values.set(JsString::from(key), value, false, cx)?;
                }
                let arch = if std::env::consts::ARCH == "aarch64" {
                    "arm"
                } else {
                    "x86"
                };
                values.set(js_string!("architecture"), js_text(arch), false, cx)?;
                values.set(js_string!("bitness"), js_text("64"), false, cx)?;
                values.set(
                    js_string!("uaFullVersion"),
                    js_text(CHROME_VERSION),
                    false,
                    cx,
                )?;
                values.set(js_string!("platformVersion"), js_text(""), false, cx)?;
                values.set(js_string!("model"), js_text(""), false, cx)?;
                crate::native::settled(Ok(values.into()), cx)
            }),
            ("toJSON", 0, |this, _, cx| {
                let values = object(cx);
                for key in ["brands", "mobile", "platform"] {
                    let value = crate::native::get(this, key, cx)?;
                    values.set(JsString::from(key), value, false, cx)?;
                }
                Ok(values.into())
            }),
        ],
    )?;
    Ok(data)
}

fn make(context: &mut Context) -> JsResult<JsValue> {
    let navigator = object(context);
    let agent = agent_of(context);
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let platform = machine().1;
    let values: [(&str, JsValue); 18] = [
        ("userAgent", js_text(&agent)),
        ("appVersion", js_text(app_version(&agent))),
        ("appName", js_text("Netscape")),
        ("appCodeName", js_text("Mozilla")),
        ("product", js_text("Gecko")),
        ("productSub", js_text("20030107")),
        ("vendor", js_text("Google Inc.")),
        ("vendorSub", js_text("")),
        ("platform", js_text(platform)),
        ("language", js_text("en-US")),
        ("hardwareConcurrency", cores.into()),
        ("deviceMemory", 8.into()),
        ("maxTouchPoints", 0.into()),
        ("onLine", true.into()),
        ("cookieEnabled", true.into()),
        ("pdfViewerEnabled", true.into()),
        ("webdriver", false.into()),
        ("doNotTrack", JsValue::null()),
    ];
    for (key, value) in values {
        navigator.set(JsString::from(key), value, false, context)?;
    }
    let languages = array(context, vec![js_text("en-US"), js_text("en")]);
    navigator.set(js_string!("languages"), languages, false, context)?;

    let data = client_hints(context)?;
    navigator.set(js_string!("userAgentData"), data, false, context)?;
    exports(
        context,
        &navigator,
        &[
            ("javaEnabled", 0, |_, _, _| Ok(false.into())),
            ("vibrate", 1, |_, _, _| Ok(false.into())),
            ("sendBeacon", 2, |_, _, _| Ok(false.into())),
        ],
    )?;
    Ok(navigator.into())
}

pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    crate::native::lazy_global(context, "navigator", make)
}
