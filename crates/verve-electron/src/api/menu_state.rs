//! What the main process keeps of menus: the `Menu` and `MenuItem` objects a script holds are the
//! truth, and the shell is sent a [`MenuSpec`] read out of them.
//!
//! A menu is *attached* once the shell has been sent it: as the application menu, a window's menu,
//! a tray's menu or a popup. Each tick of the main process's loop (after the timers and the
//! microtasks they queued have run) ends with [`sync`], which reads
//! every attached menu again and sends the ones that differ from what the shell has, so any
//! number of changes a script makes in one turn (`item.enabled = false`, `menu.append(...)`) come
//! to the shell as one [`MenuOp::Upsert`]. Reading the objects, rather than hooking every
//! property, keeps `MenuItem` the plain object Electron's is: its properties are ordinary data.
//!
//! An attached menu is held weakly, so a menu nothing refers to is collected and the shell told
//! to [`MenuOp::Drop`] it; the application menu and the window menus are held strongly, as the
//! shell's copies of them stay in use.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::object::WeakJsObject;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native;

use super::define::{self, Kind};
use super::native_image::image_of;
use super::shared;
use crate::accelerator::Accelerator;
use crate::ids::{ContentsId, WindowId};
use crate::keyed::Keyed;
use crate::menu::MenuBook;
use crate::msg::{MenuId, MenuItemId, MenuItemKind, MenuItemSpec, MenuOp, MenuSpec, ToShell};
use crate::pixels::NativeImage;

/// The kind of object a `Menu` is, in [`define`]'s table.
pub(crate) const MENU: Kind = Kind("Menu");

/// The kind of object a `MenuItem` is.
pub(crate) const MENU_ITEM: Kind = Kind("MenuItem");

/// A submenu nested deeper than this is a cycle a script made.
const MAX_DEPTH: usize = 32;

struct Attached {
    object: WeakJsObject,
    /// What the shell was last sent of it.
    sent: MenuSpec,
}

/// A popup that was asked for and has not closed.
struct Popup {
    callback: JsObject,
    /// The menu, held while it is open as Electron holds it.
    _menu: JsObject,
}

#[derive(Default)]
struct Inner {
    next_menu: MenuId,
    next_item: MenuItemId,
    next_group: u32,
    next_call: u64,
    items: BTreeMap<MenuItemId, WeakJsObject>,
    attached: Keyed<Attached>,
    app_menu: Option<JsObject>,
    /// Whether `Menu.setApplicationMenu` was called: the default menu is for apps that did not.
    app_menu_set: bool,
    window_menus: Keyed<JsObject>,
    popups: BTreeMap<u64, Popup>,
    pictures: BTreeMap<String, Arc<NativeImage>>,
    menu_class: Option<JsObject>,
    item_class: Option<JsObject>,
    /// The windows whose script hid the menu bar.
    hidden_bars: std::collections::BTreeSet<WindowId>,
    /// The menus as the shell has them, which the main process's own key lookup reads.
    book: MenuBook,
    /// The `code`s of keys a menu took on each page and that are still down: their key-up is the
    /// menu's too.
    swallowed: Vec<(ContentsId, String)>,
}

/// The menus of the realm.
#[derive(Default)]
pub(crate) struct MenuState(RefCell<Inner>);

/// The realm's menu state.
pub(crate) fn state(context: &mut Context) -> Rc<MenuState> {
    if let Some(state) = context.get_data::<Rc<MenuState>>() {
        return Rc::clone(state);
    }
    let made = Rc::<MenuState>::default();
    context.insert_data(Rc::clone(&made));
    made
}

impl MenuState {
    fn inner(&self) -> std::cell::RefMut<'_, Inner> {
        self.0.borrow_mut()
    }

    pub(crate) fn set_menu_class(&self, class: JsObject) {
        self.inner().menu_class = Some(class);
    }

    pub(crate) fn menu_class(&self) -> Option<JsObject> {
        self.inner().menu_class.clone()
    }

    pub(crate) fn set_item_class(&self, class: JsObject) {
        self.inner().item_class = Some(class);
    }

    pub(crate) fn item_class(&self) -> Option<JsObject> {
        self.inner().item_class.clone()
    }

    pub(crate) fn set_bar_hidden(&self, window: WindowId, hidden: bool) {
        let mut inner = self.inner();
        if hidden {
            inner.hidden_bars.insert(window);
        } else {
            inner.hidden_bars.remove(&window);
        }
    }

    pub(crate) fn bar_hidden(&self, window: WindowId) -> bool {
        self.inner().hidden_bars.contains(&window)
    }

    /// A number for a new `Menu`.
    pub(crate) fn new_menu_id(&self) -> MenuId {
        let mut inner = self.inner();
        inner.next_menu += 1;
        inner.next_menu
    }

    /// A number for a new `MenuItem`: its `commandId`.
    pub(crate) fn new_item_id(&self) -> MenuItemId {
        let mut inner = self.inner();
        inner.next_item += 1;
        inner.next_item
    }

    /// A number for a new radio group.
    pub(crate) fn new_group_id(&self) -> u32 {
        let mut inner = self.inner();
        inner.next_group += 1;
        inner.next_group
    }

    /// Remembers the object of item `id`, for a click to find.
    pub(crate) fn remember_item(&self, id: MenuItemId, object: &JsObject) {
        self.inner().items.insert(id, object.downgrade());
    }

    fn item(&self, id: MenuItemId) -> Option<JsObject> {
        self.inner().items.get(&id).and_then(WeakJsObject::upgrade)
    }

    /// Whether the script set an application menu itself.
    pub(crate) fn app_menu_set(&self) -> bool {
        self.inner().app_menu_set
    }

    pub(crate) fn mark_app_menu_set(&self) {
        self.inner().app_menu_set = true;
    }

    pub(crate) fn app_menu(&self) -> Option<JsObject> {
        self.inner().app_menu.clone()
    }

    pub(crate) fn set_app_menu(&self, menu: Option<JsObject>) {
        self.inner().app_menu = menu;
    }

    pub(crate) fn set_window_menu(&self, window: WindowId, menu: Option<JsObject>) {
        let mut inner = self.inner();
        match menu {
            Some(menu) => inner.window_menus.insert(window, menu),
            None => inner.window_menus.remove(&window),
        };
    }

    /// The main process's copy of the shell's menus, for the lookup of a key's menu item.
    pub(crate) fn item_for(&self, window: WindowId, key: &verve_dom::Key) -> Option<MenuItemId> {
        self.inner().book.item_for(window, key)
    }

    /// A menu took the key `code` pressed on `contents`. A key held down repeats its press, and
    /// the one release that ends it must find a single entry, so a second press adds none.
    pub(crate) fn swallow(&self, contents: ContentsId, code: &str) {
        let mut inner = self.inner();
        if !inner
            .swallowed
            .iter()
            .any(|(page, held)| *page == contents && held == code)
        {
            inner.swallowed.push((contents, code.to_owned()));
        }
    }

    /// No menu took the key `code` pressed on `contents`: a release that an earlier press left
    /// waiting (its menu item opened a dialog, say) is not this key's, and it goes to the page.
    pub(crate) fn pass(&self, contents: ContentsId, code: &str) {
        (self.inner().swallowed).retain(|(page, held)| *page != contents || held != code);
    }

    /// Whether the key `code` coming up on `contents` is one a menu took going down.
    pub(crate) fn release(&self, contents: ContentsId, code: &str) -> bool {
        let mut inner = self.inner();
        let held = inner
            .swallowed
            .iter()
            .position(|(page, held)| *page == contents && held == code);
        held.map(|at| inner.swallowed.remove(at)).is_some()
    }

    /// Takes a callback for a popup and numbers it.
    pub(crate) fn start_popup(&self, callback: JsObject, menu: JsObject) -> u64 {
        let mut inner = self.inner();
        inner.next_call += 1;
        let call = inner.next_call;
        inner.popups.insert(
            call,
            Popup {
                callback,
                _menu: menu,
            },
        );
        call
    }

    /// The picture at `path`, read the first time it is asked for: a menu is read again every
    /// turn, and the file is not.
    fn picture(&self, path: &str) -> Arc<NativeImage> {
        let mut inner = self.inner();
        let found = inner.pictures.entry(path.to_owned());
        Arc::clone(
            found.or_insert_with(|| Arc::new(NativeImage::from_path(std::path::Path::new(path)))),
        )
    }
}

/// A window is gone: the main process's copy of the shell's menus lets go of it.
pub(crate) fn forget_window(context: &mut Context, window: WindowId) {
    state(context).inner().book.forget_window(window);
}

/// Sends `op` to the shell, and to the main process's own copy of what the shell has.
pub(crate) fn send(context: &mut Context, op: MenuOp) {
    state(context).inner().book.apply(&op);
    shared(context).hub.to_shell(ToShell::Menu(op));
}

/// `object[key]` when it is a string, else an empty one. Kept out of line: a menu item is read
/// with it a dozen times, and a copy at each call is code the binary carries.
#[inline(never)]
pub(crate) fn text(object: &JsObject, key: &str, context: &mut Context) -> JsResult<String> {
    let value = native::get(&object.clone().into(), key, context)?;
    Ok(value
        .as_string()
        .map(|text| text.to_std_string_escaped())
        .unwrap_or_default())
}

/// `this` as an object, for a method of a class the script can call on anything.
pub(crate) fn this_object(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .ok_or_else(|| native::type_error("Illegal invocation"))
}

#[inline(never)]
fn flag(object: &JsObject, key: &str, context: &mut Context) -> JsResult<bool> {
    Ok(native::get(&object.clone().into(), key, context)?.to_boolean())
}

/// The `Menu` object `value` is, and its id.
pub(crate) fn menu_of(value: &JsValue, context: &mut Context) -> Option<(JsObject, MenuId)> {
    let id = define::id_of(value, MENU, context)?;
    Some((value.as_object()?, id))
}

/// Reads `menu` as the shell is to build it.
///
/// # Errors
///
/// A getter a script defined threw.
pub(crate) fn read_spec(menu: &JsObject, id: MenuId, context: &mut Context) -> JsResult<MenuSpec> {
    read_nested(menu, id, 0, context)
}

fn read_nested(
    menu: &JsObject,
    id: MenuId,
    depth: usize,
    context: &mut Context,
) -> JsResult<MenuSpec> {
    let items = native::get(&menu.clone().into(), "items", context)?;
    let mut specs = Vec::new();
    for item in native::list_of(&items, context)? {
        let Some(item) = item.as_object() else {
            continue;
        };
        if let Some(spec) = read_item(&item, depth, context)? {
            specs.push(spec);
        }
    }
    Ok(MenuSpec { id, items: specs })
}

fn read_item(
    item: &JsObject,
    depth: usize,
    context: &mut Context,
) -> JsResult<Option<MenuItemSpec>> {
    let Some(id) = define::id_of(&item.clone().into(), MENU_ITEM, context) else {
        return Ok(None);
    };
    let kind = match text(item, "type", context)?.as_str() {
        "separator" => MenuItemKind::Separator,
        "submenu" => submenu_kind(item, depth, context)?,
        "checkbox" => MenuItemKind::Checkbox,
        "radio" => MenuItemKind::Radio,
        "header" => MenuItemKind::Header,
        "palette" => MenuItemKind::Palette,
        _ => MenuItemKind::Normal,
    };
    let role = native::get(&item.clone().into(), "role", context)?
        .as_string()
        .map(|role| role.to_std_string_escaped());
    let icon = icon_of(item, context)?;
    let accelerator = native::get(&item.clone().into(), "accelerator", context)?
        .as_string()
        .and_then(|text| Accelerator::parse(&text.to_std_string_escaped()).ok());
    // The spell checker's checkbox is the session's setting, which Verve has no spell checker for.
    let checked = role.as_deref() != Some("togglespellchecker") && flag(item, "checked", context)?;
    Ok(Some(MenuItemSpec {
        id,
        kind,
        role,
        label: text(item, "label", context)?,
        sublabel: text(item, "sublabel", context)?,
        tool_tip: text(item, "toolTip", context)?,
        accelerator,
        icon,
        enabled: flag(item, "enabled", context)?,
        visible: flag(item, "visible", context)?,
        checked,
        register_accelerator: flag(item, "registerAccelerator", context)?,
        accelerator_works_when_hidden: flag(item, "acceleratorWorksWhenHidden", context)?,
    }))
}

fn submenu_kind(item: &JsObject, depth: usize, context: &mut Context) -> JsResult<MenuItemKind> {
    let sub = native::get(&item.clone().into(), "submenu", context)?;
    match menu_of(&sub, context) {
        Some((object, id)) if depth < MAX_DEPTH => Ok(MenuItemKind::Submenu(read_nested(
            &object,
            id,
            depth + 1,
            context,
        )?)),
        _ => Ok(MenuItemKind::Normal),
    }
}

fn icon_of(item: &JsObject, context: &mut Context) -> JsResult<Option<Arc<NativeImage>>> {
    let icon = native::get(&item.clone().into(), "icon", context)?;
    if let Some(path) = icon.as_string() {
        return Ok(Some(state(context).picture(&path.to_std_string_escaped())));
    }
    Ok(image_of(&icon).map(Arc::new))
}

/// Sends `menu` to the shell if it has not been, and answers its id. Called when the menu is
/// made the application menu, a window's menu, a tray's, or is popped up.
///
/// # Errors
///
/// `menu` is no `Menu`, or a getter threw.
pub(crate) fn attach(menu: &JsValue, context: &mut Context) -> JsResult<MenuId> {
    let (object, id) = menu_of(menu, context).ok_or_else(|| native::type_error("Invalid menu"))?;
    let spec = read_spec(&object, id, context)?;
    let state = state(context);
    let known = state.inner().attached.contains_key(&id);
    if !known
        || state
            .inner()
            .attached
            .get(&id)
            .is_some_and(|a| a.sent != spec)
    {
        state.inner().attached.insert(
            id,
            Attached {
                object: object.downgrade(),
                sent: spec.clone(),
            },
        );
        send(context, MenuOp::Upsert(spec));
    }
    Ok(id)
}

/// Ends a turn of the main process: every attached menu that changed since the shell was sent it is
/// sent again, whole, and one nothing refers to any more is dropped.
pub(crate) fn sync(context: &mut Context) {
    let state = state(context);
    let ids: Vec<MenuId> = state.inner().attached.keys().copied().collect();
    for id in ids {
        let object = state
            .inner()
            .attached
            .get(&id)
            .and_then(|a| a.object.upgrade());
        let Some(object) = object else {
            state.inner().attached.remove(&id);
            send(context, MenuOp::Drop(id));
            continue;
        };
        let read = read_spec(&object, id, context);
        let Some(spec) = define::report(context, read) else {
            continue;
        };
        let changed = state
            .inner()
            .attached
            .get(&id)
            .is_some_and(|a| a.sent != spec);
        if changed {
            if let Some(a) = state.inner().attached.get_mut(&id) {
                a.sent = spec.clone();
            }
            send(context, MenuOp::Upsert(spec));
        }
    }
    state
        .inner()
        .items
        .retain(|_, weak| weak.upgrade().is_some());
    super::tray::sync(context);
}

/// A menu the shell opens: `'menu-will-show'`, after the radio groups are settled.
pub(crate) fn will_show(context: &mut Context, menu: MenuId) {
    let Some(object) = attached_object(context, menu) else {
        return;
    };
    let shown = super::menu::menu_will_show(context, &object);
    define::report(context, shown);
}

/// A menu the shell closed: `'menu-will-close'`, and the callback of the popup it was.
pub(crate) fn will_close(context: &mut Context, menu: MenuId, call: Option<u64>) {
    if let Some(object) = attached_object(context, menu) {
        let event = define::event(context);
        let emitted = event
            .and_then(|event| define::emit(context, &object, "menu-will-close", &[event.into()]));
        define::report(context, emitted);
    }
    let popup = call.and_then(|call| state(context).inner().popups.remove(&call));
    if let Some(popup) = popup {
        let called = popup.callback.call(&JsValue::undefined(), &[], context);
        define::report(context, called);
    }
}

fn attached_object(context: &mut Context, menu: MenuId) -> Option<JsObject> {
    state(context)
        .inner()
        .attached
        .get(&menu)
        .and_then(|a| a.object.upgrade())
}

/// The `event` a click hands its `click` function: the keys held, and how it was chosen.
fn click_event(
    context: &mut Context,
    modifiers: verve_dom::Modifiers,
    accelerator: bool,
) -> JsResult<JsObject> {
    define::record(
        context,
        &[
            ("shiftKey", modifiers.shift.into()),
            ("ctrlKey", modifiers.ctrl.into()),
            ("altKey", modifiers.alt.into()),
            ("metaKey", modifiers.meta.into()),
            ("triggeredByAccelerator", accelerator.into()),
        ],
    )
}

/// A person chose item `item`: its `click(event, focusedWindow, focusedWebContents)` runs, as
/// Electron's `Menu._executeCommand` runs it.
pub(crate) fn clicked(
    context: &mut Context,
    item: MenuItemId,
    window: Option<WindowId>,
    accelerator: bool,
    modifiers: verve_dom::Modifiers,
) {
    let Some(object) = state(context).item(item) else {
        return;
    };
    let run = (|| {
        let event = click_event(context, modifiers, accelerator)?;
        let (window, contents) = super::menu_roles::targets(context, window)?;
        native::invoke(
            &object.into(),
            "click",
            &[event.into(), window, contents],
            context,
        )
    })();
    define::report(context, run);
}
