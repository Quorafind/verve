//! The rest of `app`'s methods: the user's language, what Windows keeps for an app (protocol
//! clients, login items, the recent documents list), the figures `getAppMetrics` and the GPU
//! calls answer, and the switches that must be thrown before `ready`.
//!
//! Where Electron answers from Chromium or the shell and Verve has no counterpart, the answer is
//! the one a machine without it would give: no GPU, no jump list, no accessibility client.

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsResult, JsValue};
use verve::native::{self, arg, js_text};

use super::{platform, shared};
use crate::api::define::{self, Args};
use crate::api::process;

pub(super) const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("getLocale", 0, |_, _, cx| Ok(js_text(&locale(cx)))),
    ("getSystemLocale", 0, get_system_locale),
    ("getPreferredSystemLanguages", 0, |_, _, cx| {
        let languages = platform::preferred_languages();
        Ok(native::array(
            cx,
            languages.iter().map(|l| js_text(l)).collect(),
        ))
    }),
    ("getLocaleCountryCode", 0, |_, _, _| {
        let tag = platform::system_locale();
        Ok(js_text(
            tag.rsplit('-')
                .next()
                .filter(|r| r.len() == 2)
                .unwrap_or(""),
        ))
    }),
    ("addRecentDocument", 1, add_recent_document),
    ("clearRecentDocuments", 0, |_, _, cx| {
        shared(cx).app.settings.borrow_mut().recent.clear();
        Ok(JsValue::undefined())
    }),
    ("getRecentDocuments", 0, |_, _, cx| {
        let recent = shared(cx).app.settings.borrow().recent.clone();
        Ok(native::array(
            cx,
            recent.iter().map(|p| js_text(p)).collect(),
        ))
    }),
    ("setAsDefaultProtocolClient", 1, protocol::set),
    ("removeAsDefaultProtocolClient", 1, protocol::remove),
    ("isDefaultProtocolClient", 1, protocol::is_default),
    ("getApplicationNameForProtocol", 1, protocol::name_for),
    ("getApplicationInfoForProtocol", 1, protocol::info_for),
    ("setUserTasks", 1, |_, _, _| Ok(true.into())),
    ("getJumpListSettings", 0, |_, _, cx| {
        let removed = native::array(cx, Vec::new());
        Ok(define::record(cx, &[("minItems", 10.into()), ("removedItems", removed)])?.into())
    }),
    ("setJumpList", 1, |_, _, _| Ok(js_text("ok"))),
    ("setAppUserModelId", 1, set_app_user_model_id),
    ("getAppMetrics", 0, app_metrics),
    ("getGPUFeatureStatus", 0, gpu_feature_status),
    ("getGPUInfo", 1, |_, _, cx| {
        let devices = native::array(cx, Vec::new());
        let aux = native::object(cx);
        let info = define::record(cx, &[("auxAttributes", aux.into()), ("gpuDevice", devices)])?;
        native::settled(Ok(info.into()), cx)
    }),
    ("setBadgeCount", 0, |_, args, cx| set_badge(args, cx)),
    ("getBadgeCount", 0, |_, _, cx| {
        Ok(shared(cx).app.settings.borrow().badge.into())
    }),
    ("getLoginItemSettings", 0, login::get),
    ("setLoginItemSettings", 1, login::set),
    ("isAccessibilitySupportEnabled", 0, |_, _, cx| {
        Ok(shared(cx).app.settings.borrow().accessibility.into())
    }),
    ("setAccessibilitySupportEnabled", 1, |_, args, cx| {
        shared(cx).app.settings.borrow_mut().accessibility = arg(args, 0).to_boolean();
        Ok(JsValue::undefined())
    }),
    (
        "setAboutPanelOptions",
        1,
        |_, _, _| Ok(JsValue::undefined()),
    ),
    ("showAboutPanel", 0, |_, _, _| Ok(JsValue::undefined())),
    ("isEmojiPanelSupported", 0, |_, _, _| {
        Ok(cfg!(windows).into())
    }),
    ("showEmojiPanel", 0, |_, _, _| Ok(JsValue::undefined())),
    ("disableHardwareAcceleration", 0, |_, _, cx| {
        before_ready(cx, "disableHardwareAcceleration", |s| {
            s.hardware_acceleration_off = true;
        })
    }),
    ("disableDomainBlockingFor3DAPIs", 0, |_, _, cx| {
        before_ready(cx, "disableDomainBlockingFor3DAPIs", |_| {})
    }),
    ("isHardwareAccelerationEnabled", 0, |_, _, cx| {
        Ok((!shared(cx).app.settings.borrow().hardware_acceleration_off).into())
    }),
    ("enableSandbox", 0, |_, _, cx| {
        before_ready(cx, "enableSandbox", |s| s.sandbox = true)
    }),
    ("getFileIcon", 1, |_, _, cx| {
        define::rejected(cx, "app.getFileIcon")
    }),
    ("setProxy", 1, |_, _, cx| {
        define::rejected(cx, "app.setProxy")
    }),
    ("resolveProxy", 1, |_, _, cx| {
        define::rejected(cx, "app.resolveProxy")
    }),
    ("setClientCertRequestPasswordHandler", 1, |_, _, cx| {
        define::not_supported(cx, "app.setClientCertRequestPasswordHandler")
    }),
    ("importCertificate", 2, |_, _, cx| {
        define::not_supported(cx, "app.importCertificate")
    }),
];

pub(super) const ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    ("userAgentFallback", get_user_agent, Some(set_user_agent)),
    (
        "badgeCount",
        |_, _, cx| Ok(shared(cx).app.settings.borrow().badge.into()),
        Some(|_, args, cx| set_badge(args, cx)),
    ),
    (
        "accessibilitySupportEnabled",
        |_, _, cx| Ok(shared(cx).app.settings.borrow().accessibility.into()),
        Some(|_, args, cx| {
            shared(cx).app.settings.borrow_mut().accessibility = arg(args, 0).to_boolean();
            Ok(JsValue::undefined())
        }),
    ),
    (
        "runningUnderARM64Translation",
        |_, _, _| Ok(false.into()),
        None,
    ),
];

/// The Chromium locales Electron ships translations for; `getLocale` answers one of them.
const LOCALES: &[&str] = &[
    "af", "am", "ar", "bg", "bn", "ca", "cs", "da", "de", "el", "en-GB", "en-US", "es", "es-419",
    "et", "fa", "fi", "fil", "fr", "gu", "he", "hi", "hr", "hu", "id", "it", "ja", "kn", "ko",
    "lt", "lv", "ml", "mr", "ms", "nb", "nl", "pl", "pt-BR", "pt-PT", "ro", "ru", "sk", "sl", "sr",
    "sv", "sw", "ta", "te", "th", "tr", "uk", "ur", "vi", "zh-CN", "zh-TW",
];

/// The locale of the translations that serve `tag`, as Chromium picks one: the tag itself, the
/// Chinese variants by script or region, then its language alone, and English by default.
fn chromium_locale(tag: &str) -> &'static str {
    let tag = tag.replace('_', "-");
    let language = tag.split('-').next().unwrap_or("");
    let found = |wanted: &str| {
        LOCALES
            .iter()
            .find(|l| l.eq_ignore_ascii_case(wanted))
            .copied()
    };
    if language.eq_ignore_ascii_case("zh") {
        let traditional = ["Hant", "TW", "HK", "MO"]
            .iter()
            .any(|part| tag.contains(part));
        return if traditional { "zh-TW" } else { "zh-CN" };
    }
    found(&tag)
        .or_else(|| found(language))
        .or_else(|| language.eq_ignore_ascii_case("pt").then_some("pt-BR"))
        .unwrap_or("en-US")
}

/// `app.getLocale()`: empty until `ready`, then `--lang` or the user's first display language.
fn locale(context: &mut Context) -> String {
    let state = shared(context);
    if !state.app.is_ready() {
        return String::new();
    }
    let lang = state
        .app
        .switches
        .borrow()
        .iter()
        .rev()
        .find(|(name, _)| name == "lang")
        .map(|(_, value)| value.clone());
    let tag = lang.or_else(|| platform::preferred_languages().into_iter().next());
    chromium_locale(&tag.unwrap_or_default()).to_owned()
}

fn get_system_locale(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !shared(context).app.is_ready() {
        return Err(define::error(
            "app.getSystemLocale() can only be called after app is ready",
        ));
    }
    Ok(js_text(&platform::system_locale()))
}

fn add_recent_document(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = Args::new(args).string(0, "path", context)?;
    shared(context).app.settings.borrow_mut().recent.push(path);
    Ok(JsValue::undefined())
}

fn set_badge(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let count = Args::new(args).f64_or(0, "count", 0., context)? as i32;
    shared(context).app.settings.borrow_mut().badge = count;
    Ok(true.into())
}

fn set_app_user_model_id(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = Args::new(args).string(0, "id", context)?;
    #[cfg(windows)]
    platform::set_app_user_model_id(&id);
    shared(context).app.settings.borrow_mut().user_model_id = Some(id);
    Ok(JsValue::undefined())
}

/// What may only be switched before `ready`: the message is Electron's.
fn before_ready(
    context: &mut Context,
    method: &str,
    set: impl FnOnce(&mut super::Settings),
) -> JsResult<JsValue> {
    let state = shared(context);
    if state.app.is_ready() {
        return Err(define::error(&format!(
            "app.{method}() can only be called before app is ready"
        )));
    }
    set(&mut state.app.settings.borrow_mut());
    Ok(JsValue::undefined())
}

fn get_user_agent(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let state = shared(context);
    if let Some(agent) = state.app.settings.borrow().user_agent.clone() {
        return Ok(js_text(&agent));
    }
    let (name, version) = (
        state.app.name().replace(' ', ""),
        state.app.version.borrow().clone(),
    );
    Ok(js_text(&verve::electron_user_agent(Some((
        &name, &version,
    )))))
}

fn set_user_agent(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let agent = native::text_of(&arg(args, 0), context)?;
    shared(context).app.settings.borrow_mut().user_agent = Some(agent);
    Ok(JsValue::undefined())
}

/// `app.getAppMetrics()`: the browser process, and a `Tab` for each window, which shares its
/// pid since Verve's pages are threads of this process.
fn app_metrics(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let windows = shared(context).hub.windows().len();
    let mut metrics = Vec::new();
    for kind in std::iter::once("Browser").chain(std::iter::repeat_n("Tab", windows)) {
        let cpu = process::cpu_usage(context)?;
        let memory = process::process_memory_figures(context)?;
        let entry = define::record(
            context,
            &[
                ("pid", std::process::id().into()),
                ("type", js_text(kind)),
                ("creationTime", process::creation_time().into()),
                ("cpu", cpu.into()),
                ("memory", memory.into()),
            ],
        )?;
        metrics.push(entry.into());
    }
    Ok(native::array(context, metrics))
}

/// `app.getGPUFeatureStatus()`: a machine that draws in software.
fn gpu_feature_status(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    const SOFTWARE: &str = "disabled_software";
    let features = [
        "2d_canvas",
        "gpu_compositing",
        "rasterization",
        "video_decode",
        "video_encode",
    ];
    let off = [
        "opengl",
        "webgl",
        "webgl2",
        "webgpu",
        "webnn",
        "skia_graphite",
        "trees_in_viz",
    ];
    let mut fields: Vec<(&str, JsValue)> =
        features.iter().map(|f| (*f, js_text(SOFTWARE))).collect();
    fields.extend(off.iter().map(|f| (*f, js_text("disabled_off"))));
    fields.push(("multiple_raster_threads", js_text("enabled_on")));
    Ok(define::record(context, &fields)?.into())
}

/// `Software\Classes\<protocol>` in the registry: the default handler of a URL scheme, written
/// and read as Electron's `browser_win.cc` does. Only Windows has it; elsewhere nothing is
/// registered and nothing can be.
#[cfg(windows)]
mod protocol {
    use super::*;
    use platform::{Root, delete_tree, read_string, write_string};

    fn key(protocol: &str) -> String {
        format!(r"Software\Classes\{protocol}")
    }

    fn command_key(protocol: &str) -> String {
        format!(r"{}\shell\open\command", key(protocol))
    }

    /// The protocol argument; an empty one names no key, and Electron answers `false` for it.
    fn protocol_of(args: &[JsValue], context: &mut Context) -> JsResult<Option<String>> {
        let protocol = Args::new(args).string(0, "protocol", context)?;
        Ok((!protocol.is_empty()).then_some(protocol))
    }

    /// The command the registry holds for a client run as `path` with `args`.
    fn command(args: &[JsValue], context: &mut Context) -> JsResult<String> {
        let command = program_command(&arg(args, 1), &arg(args, 2), context)?;
        Ok(command + " \"%1\"")
    }

    pub(super) fn set(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
        let Some(protocol) = protocol_of(args, context)? else {
            return Ok(false.into());
        };
        let (key, command) = (key(&protocol), command(args, context)?);
        let user = Root::CurrentUser;
        let done = write_string(user, &key, "URL Protocol", "")
            && write_string(user, &key, "", &format!("URL:{protocol}"))
            && write_string(user, &command_key(&protocol), "", &command);
        Ok(done.into())
    }

    fn is_ours(protocol: &str, args: &[JsValue], context: &mut Context) -> JsResult<bool> {
        let want = command(args, context)?;
        let have = read_string(Root::CurrentUser, &command_key(protocol), "");
        Ok(have.is_some_and(|have| have.eq_ignore_ascii_case(&want)))
    }

    pub(super) fn is_default(
        _: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> JsResult<JsValue> {
        match protocol_of(args, context)? {
            Some(protocol) => Ok(is_ours(&protocol, args, context)?.into()),
            None => Ok(false.into()),
        }
    }

    /// Takes the client away when it is this program's, as Electron's `browser_win.cc` does:
    /// `<protocol>\shell`, the open command it wrote, then the two values it set, and the key
    /// itself only if nothing else is left in it — icons and other verbs belong to whoever put
    /// them there.
    pub(super) fn remove(
        _: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> JsResult<JsValue> {
        let Some(protocol) = protocol_of(args, context)? else {
            return Ok(false.into());
        };
        if !is_ours(&protocol, args, context)? {
            return Ok(true.into());
        }
        let (user, key) = (Root::CurrentUser, key(&protocol));
        let gone = delete_tree(user, &format!(r"{key}\shell"));
        platform::delete_value(user, &key, "URL Protocol");
        platform::delete_value(user, &key, "");
        if platform::is_key_empty(user, &key) {
            platform::delete_key(user, &key);
        }
        Ok(gone.into())
    }

    /// The program registered for a scheme: its name and path.
    fn handler(url: &str) -> Option<(String, String)> {
        let scheme = url.split(':').next().unwrap_or("");
        let command = read_string(Root::CurrentUser, &command_key(scheme), "")?;
        let path = command
            .trim_start_matches('"')
            .split('"')
            .next()?
            .to_owned();
        let name = std::path::Path::new(&path)
            .file_stem()?
            .to_string_lossy()
            .into_owned();
        Some((name, path))
    }

    pub(super) fn name_for(
        _: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> JsResult<JsValue> {
        let url = Args::new(args).string(0, "url", context)?;
        Ok(js_text(
            &handler(&url).map(|(name, _)| name).unwrap_or_default(),
        ))
    }

    pub(super) fn info_for(
        _: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> JsResult<JsValue> {
        let url = Args::new(args).string(0, "url", context)?;
        let outcome = match handler(&url) {
            Some((name, path)) => Ok(define::record(
                context,
                &[("name", js_text(&name)), ("path", js_text(&path))],
            )?
            .into()),
            None => Err(define::error("Unable to retrieve installation path to app")),
        };
        native::settled(outcome, context)
    }
}

#[cfg(not(windows))]
mod protocol {
    use super::*;

    pub(super) fn set(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
        Ok(false.into())
    }

    pub(super) use self::set as is_default;
    pub(super) use self::set as remove;

    pub(super) fn name_for(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
        Ok(js_text(""))
    }

    pub(super) fn info_for(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
        let error = define::error("Unable to retrieve installation path to app");
        native::settled(Err(error), context)
    }
}

/// `"path" arg arg…`: how the registry holds a program and its arguments, from the `path` (the
/// running executable when it is not a string) and the list of `args` an API was given.
#[cfg(windows)]
fn program_command(path: &JsValue, args: &JsValue, context: &mut Context) -> JsResult<String> {
    let exe = match path.as_string() {
        Some(path) => path.to_std_string_escaped(),
        None => crate::launcher::exe_path().to_string_lossy().into_owned(),
    };
    let mut command = format!("\"{exe}\"");
    for item in native::list_of(args, context)? {
        command.push(' ');
        command.push_str(&native::text_of(&item, context)?);
    }
    Ok(command)
}

/// Login items: a value in `Run` under the current user's key, named as Electron names it — the
/// `name` setting, else the AppUserModelID — so one app never reads or removes another's.
mod login {
    use super::*;

    #[cfg(windows)]
    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn item_name(settings: &JsValue, context: &mut Context) -> JsResult<String> {
        let given = native::get(settings, "name", context)?;
        match given.as_string().map(|n| n.to_std_string_escaped()) {
            Some(name) if !name.is_empty() => Ok(name),
            _ => Ok(shared(context).app.user_model_id()),
        }
    }

    pub(super) fn get(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
        let name = item_name(&arg(args, 0), context)?;
        #[cfg(windows)]
        let open = platform::read_string(platform::Root::CurrentUser, RUN, &name).is_some();
        #[cfg(not(windows))]
        let open = {
            let _ = name;
            false
        };
        let items = native::array(context, Vec::new());
        Ok(define::record(
            context,
            &[
                ("launchItems", items),
                ("executableWillLaunchAtLogin", open.into()),
                ("openAtLogin", open.into()),
                ("openAsHidden", false.into()),
                ("restoreState", false.into()),
                ("wasOpenedAtLogin", false.into()),
                ("wasOpenedAsHidden", false.into()),
            ],
        )?
        .into())
    }

    pub(super) fn set(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
        let settings = arg(args, 0);
        let name = item_name(&settings, context)?;
        let open = native::get(&settings, "openAtLogin", context)?.to_boolean();
        #[cfg(windows)]
        {
            let path = native::get(&settings, "path", context)?;
            let list = native::get(&settings, "args", context)?;
            let command = program_command(&path, &list, context)?;
            if open {
                platform::write_string(platform::Root::CurrentUser, RUN, &name, &command);
            } else {
                platform::delete_value(platform::Root::CurrentUser, RUN, &name);
            }
        }
        #[cfg(not(windows))]
        let _ = (name, open);
        Ok(JsValue::undefined())
    }
}
