//! `shell`: what the desktop does with a file or an address — open it, show it, bin it. The
//! promises settle as Electron's do (observed in `tests/electron/system-a/expected/system.json`):
//! `openPath` resolves an error string, `''` for none, and `trashItem` and `openExternal` reject.

use std::path::Path;

use boa_engine::{Context, JsResult, JsValue};
use verve::native;

use super::define::{self, Args};

/// The `shell` module.
///
/// # Errors
///
/// A property that could not be defined.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("openExternal", 1, open_external),
            ("openPath", 1, open_path),
            ("showItemInFolder", 1, show_item_in_folder),
            ("trashItem", 1, trash_item),
            ("beep", 0, |_, _, _| {
                crate::os::beep();
                Ok(JsValue::undefined())
            }),
            ("writeShortcutLink", 2, unsupported_link),
            ("readShortcutLink", 1, unsupported_link),
        ],
    )?;
    Ok(module.into())
}

/// Shortcut links are Windows' `IShellLink`, which Verve does not bind yet.
fn unsupported_link(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    define::not_supported(context, "shell shortcut links")
}

/// `openExternal(url[, options])`: rejects `Failed to open: <url>` when the desktop would not.
fn open_external(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let url = Args::new(args).string(0, "url", context)?;
    let opened = url::Url::parse(&url).is_ok() && crate::os::open_with_shell(&url);
    let outcome = if opened {
        Ok(JsValue::undefined())
    } else {
        Err(define::error(&format!("Failed to open: {url}")))
    };
    native::settled(outcome, context)
}

/// `openPath(path)`: the answer is the error, `''` when there was none.
fn open_path(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = Args::new(args).string(0, "path", context)?;
    let opened = Path::new(&path).exists() && crate::os::open_with_shell(&path);
    let answer = if opened { "" } else { "Failed to open path" };
    native::settled(Ok(native::js_text(answer)), context)
}

fn show_item_in_folder(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = Args::new(args).string(0, "fullPath", context)?;
    crate::os::show_in_folder(&path);
    Ok(JsValue::undefined())
}

/// The fully qualified path the recycle bin may be given for `path`, or `None`. It only honours
/// `FOF_ALLOWUNDO` for a path that is absolute, with backslashes and no `.` or `..` in it; given
/// any other it deletes for good. So a relative path is refused, `\\?\` and `\\.\` paths are
/// refused (they are passed on as they are), and the rest is made canonical in form.
fn trash_target(path: &str) -> Option<std::path::PathBuf> {
    use std::path::{Component, Prefix};
    let given = Path::new(path);
    if !given.is_absolute() {
        return None;
    }
    if let Some(Component::Prefix(prefix)) = given.components().next()
        && !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(..))
    {
        return None;
    }
    std::path::absolute(given).ok()
}

/// `trashItem(path)`: a path that is not absolute, or not there, is `Failed to parse path`. The
/// recycle bin only takes fully qualified paths; given another it deletes for good.
fn trash_item(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = Args::new(args).string(0, "path", context)?;
    let target = trash_target(&path).filter(|target| target.exists());
    let outcome = if let Some(target) = target {
        if crate::os::move_to_trash(&target.to_string_lossy()) {
            Ok(JsValue::undefined())
        } else {
            Err(define::error(&format!(
                "Failed to move {path} to the trash"
            )))
        }
    } else {
        Err(define::error("Failed to parse path"))
    };
    native::settled(outcome, context)
}

#[cfg(all(test, windows))]
mod tests {
    use super::trash_target;

    #[test]
    fn a_path_for_the_recycle_bin_is_absolute_and_free_of_dots() {
        let target = trash_target(r"C:\a\..\b").unwrap();
        assert_eq!(target.to_str(), Some(r"C:\b"));
        assert_eq!(
            trash_target("C:/a/b.txt").unwrap().to_str(),
            Some(r"C:\a\b.txt")
        );
        assert!(trash_target("notes.txt").is_none());
        assert!(trash_target(r"\\?\C:\a").is_none());
        assert!(trash_target(r"\\.\C:\a").is_none());
        assert!(trash_target(r"\\?\UNC\server\share\a").is_none());
        assert!(trash_target(r"\\server\share\a").is_some());
    }
}
