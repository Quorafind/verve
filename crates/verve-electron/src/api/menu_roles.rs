//! What a menu item with a `role` does when it is chosen: Electron 39.8.10's `execute` in
//! `menu-item-roles.ts`, whose table gives each role an app method, a window method or a
//! `webContents` method. They are called as a script would call them, on the focused window and
//! page, so a role does what the same call by hand does.

use boa_engine::{Context, JsResult, JsValue};
use verve::native;

use super::{app, contents_ipc, shared};
use crate::ids::WindowId;

/// What a role acts on.
enum Target {
    /// `app`.
    App,
    /// The window, when there is one.
    Window,
    /// The page, when there is one.
    Contents,
}

/// What `role` does: the object it is done to, the method and the arguments. `None` for a role
/// that does nothing from JavaScript (a macOS role, or a menu).
fn action(role: &str) -> Option<(Target, &'static str)> {
    let edit = |method| Some((Target::Contents, method));
    match role {
        "about" if !cfg!(target_os = "macos") => Some((Target::App, "showAboutPanel")),
        "quit" => Some((Target::App, "quit")),
        "close" => Some((Target::Window, "close")),
        "copy" => edit("copy"),
        "cut" => edit("cut"),
        "paste" => edit("paste"),
        "pasteandmatchstyle" => edit("pasteAndMatchStyle"),
        "delete" => edit("delete"),
        "selectall" => edit("selectAll"),
        "undo" => edit("undo"),
        "redo" => edit("redo"),
        "toggledevtools" => edit("toggleDevTools"),
        "togglespellchecker" => edit("spellCheckerEnabled"),
        "resetzoom" | "zoomin" | "zoomout" => edit("zoomLevel"),
        "forcereload" => Some((Target::Window, "reloadIgnoringCache")),
        "reload" => Some((Target::Window, "reload")),
        "minimize" => Some((Target::Window, "minimize")),
        "togglefullscreen" => Some((Target::Window, "setFullScreen")),
        _ => None,
    }
}

/// Whether macOS leaves `role` to the system instead of calling a method (`nonNativeMacOSRole`
/// marks the ones it does not).
fn left_to_the_system(role: &str) -> bool {
    let ours = [
        "forcereload",
        "reload",
        "resetzoom",
        "toggledevtools",
        "zoomin",
        "zoomout",
        "togglespellchecker",
    ];
    cfg!(target_os = "macos") && !ours.contains(&role)
}

/// Does what `role` stands for, to `window` and `contents` (`null` when there is none); whether it
/// did anything.
///
/// # Errors
///
/// What the method called threw.
pub(crate) fn execute(
    context: &mut Context,
    role: &str,
    window: &JsValue,
    contents: &JsValue,
) -> JsResult<bool> {
    if left_to_the_system(role) {
        return Ok(false);
    }
    let Some((target, method)) = action(role) else {
        return Ok(false);
    };
    match target {
        Target::App => {
            let app = app::make(context)?;
            native::invoke(&app.into(), method, &[], context)?;
            Ok(true)
        }
        Target::Window if !window.is_null_or_undefined() => {
            window_method(context, role, window)?;
            Ok(true)
        }
        Target::Contents if !contents.is_null_or_undefined() => {
            contents_method(context, role, method, contents)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn window_method(context: &mut Context, role: &str, window: &JsValue) -> JsResult<()> {
    match role {
        "close" => native::invoke(window, "close", &[], context)?,
        "forcereload" => {
            let contents = native::get(window, "webContents", context)?;
            native::invoke(&contents, "reloadIgnoringCache", &[], context)?
        }
        "reload" => native::invoke(window, "reload", &[], context)?,
        "minimize" => {
            if native::get(window, "minimizable", context)?.to_boolean() {
                native::invoke(window, "minimize", &[], context)?
            } else {
                JsValue::undefined()
            }
        }
        _ => {
            let full = native::invoke(window, "isFullScreen", &[], context)?.to_boolean();
            native::invoke(window, "setFullScreen", &[(!full).into()], context)?
        }
    };
    Ok(())
}

fn contents_method(
    context: &mut Context,
    role: &str,
    method: &str,
    contents: &JsValue,
) -> JsResult<()> {
    let Some(object) = contents.as_object() else {
        return Ok(());
    };
    if method == "spellCheckerEnabled" {
        // The page's session setting is flipped when it has one; the role is the page's either way.
        let session = native::get(contents, "session", context)?;
        if let Some(session) = session.as_object()
            && session.has_property(boa_engine::JsString::from(method), context)?
        {
            let on = native::get(&session.clone().into(), method, context)?.to_boolean();
            session.set(boa_engine::JsString::from(method), !on, true, context)?;
        }
        return Ok(());
    }
    if method != "zoomLevel" {
        native::invoke(contents, method, &[], context)?;
        return Ok(());
    }
    let level = match role {
        "resetzoom" => 0.,
        _ => {
            let now = native::get(contents, "zoomLevel", context)?
                .as_number()
                .unwrap_or(0.);
            now + if role == "zoomin" { 0.5 } else { -0.5 }
        }
    };
    object.set(
        boa_engine::JsString::from("zoomLevel"),
        level,
        true,
        context,
    )?;
    Ok(())
}

/// The window and the page a click is for: `window` if it is open, else the focused one; `null` for
/// each that is none.
///
/// # Errors
///
/// The page's object could not be made.
pub(crate) fn targets(
    context: &mut Context,
    window: Option<WindowId>,
) -> JsResult<(JsValue, JsValue)> {
    let windows = shared(context).windows.newest_first();
    let record = window
        .and_then(|id| windows.iter().find(|w| w.id == id && !w.dead.get()))
        .or_else(|| windows.iter().find(|w| w.state.borrow().focused));
    let Some(record) = record else {
        return Ok((JsValue::null(), JsValue::null()));
    };
    let object = record.object.clone();
    let contents = contents_ipc::contents_object(context, record.page.contents)?;
    Ok((object.into(), contents.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_that_is_a_label_only_does_nothing() {
        for role in [
            "help", "window", "services", "front", "zoom", "appmenu", "bogus",
        ] {
            assert!(action(role).is_none(), "{role}");
        }
        assert!(action("quit").is_some());
    }
}
