//! The menu of a `BrowserWindow`: `setMenu`, `removeMenu` and the menu bar's visibility.
//!
//! Electron gives every window the application menu when it is made (`BaseWindow._init` in
//! `base-window.ts`), except on macOS where the menu belongs to the app, not the window. A menu
//! set with `setMenu` replaces it for that window; `removeMenu` leaves the window without one.

use boa_engine::{Context, JsResult, JsValue};
use verve::native::{self, arg};

use super::menu_state::{self, menu_of};
use super::shared;
use super::window_methods::record_of;
use super::windows::WindowRecord;
use crate::ids::WindowId;
use crate::menu::roles::Os;
use crate::msg::MenuOp;
use crate::window_state::Flag;

/// `win.setMenu(menu | null)`.
///
/// # Errors
///
/// `Invalid Menu` for a value that is neither a `Menu` nor `null`.
pub(crate) fn set_menu(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let menu = arg(args, 0);
    if !menu.is_null() && menu_of(&menu, context).is_none() {
        return Err(native::type_error("Invalid Menu"));
    }
    if Os::CURRENT != Os::Mac {
        set_menu_of(context, record.id, &menu)?;
    }
    Ok(JsValue::undefined())
}

/// `win.removeMenu()`.
///
/// # Errors
///
/// The window is gone.
pub(crate) fn remove_menu(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    if Os::CURRENT != Os::Mac {
        set_menu_of(context, record.id, &JsValue::null())?;
    }
    Ok(JsValue::undefined())
}

/// Gives `window` `menu`, or no menu when it is not a `Menu`. A menu without items is no bar either:
/// Electron's `BaseWindow::SetMenu` removes the bar for one.
pub(crate) fn set_menu_of(context: &mut Context, window: WindowId, menu: &JsValue) -> JsResult<()> {
    let state = menu_state::state(context);
    let mut id = None;
    if let Some((object, _)) = menu_of(menu, context) {
        state.set_window_menu(window, Some(object.clone()));
        if !super::menu::items_of(&object, context)?.is_empty() {
            id = Some(menu_state::attach(menu, context)?);
        }
    } else {
        state.set_window_menu(window, None);
    }
    menu_state::send(context, MenuOp::SetWindowMenu { window, menu: id });
    Ok(())
}

/// A window was made: it shows the application menu, if there is one, and the shell is told when
/// its `autoHideMenuBar` option hides the bar.
pub(crate) fn window_created(context: &mut Context, window: WindowId) -> JsResult<()> {
    if let Some(record) = shared(context).windows.get(window)
        && record.state.borrow().flag(Flag::MenuBarAutoHide)
    {
        menu_bar_changed(context, &record, (Flag::MenuBarAutoHide, true));
    }
    if Os::CURRENT == Os::Mac {
        return Ok(());
    }
    match menu_state::state(context).app_menu() {
        Some(menu) => set_menu_of(context, window, &menu.into()),
        None => Ok(()),
    }
}

/// A window is gone: its menu is let go of.
pub(crate) fn window_gone(context: &mut Context, window: WindowId) {
    menu_state::state(context).set_window_menu(window, None);
    menu_state::state(context).set_bar_hidden(window, false);
    menu_state::forget_window(context, window);
}

/// `setMenuBarVisibility` or `setAutoHideMenuBar` was called with `on` for `flag`: the shell is told
/// whether the bar is shown and whether it hides itself. The bar is hidden only by a script that
/// said `setMenuBarVisibility(false)`; the shell has no Alt key to bring an auto-hidden bar back.
pub(crate) fn menu_bar_changed(
    context: &mut Context,
    record: &WindowRecord,
    (flag, on): (Flag, bool),
) {
    let state = menu_state::state(context);
    if flag == Flag::MenuBarVisible {
        state.set_bar_hidden(record.id, !on);
    }
    let auto_hide = record.state.borrow().flag(Flag::MenuBarAutoHide);
    let op = MenuOp::SetMenuBarVisibility {
        window: record.id,
        visible: !state.bar_hidden(record.id),
        auto_hide,
    };
    menu_state::send(context, op);
}
