//! `Menu`: Electron 39.8.10's `lib/browser/api/menu.ts` over a native menu that is the shell's.
//!
//! The menu is an `EventEmitter` holding `items`, a `commandsMap` and a `groupsMap` as Electron's
//! does. `Menu.buildFromTemplate` validates, orders ([`crate::menu::sort`]) and builds a menu
//! from a template; `setApplicationMenu` and a window's `setMenu` hand a menu to the shell
//! ([`menu_state`]), and `popup` asks the shell to open one. A radio item is the one place an
//! item is not plain: its `checked` is an accessor, so choosing it unchecks the rest of its group.

use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};
use verve::native::{self, ClassSpec, arg, js_text};

use super::browser_window::window_id_of;
use super::define;
use super::menu_item;
use super::menu_state::{self, MENU, MENU_ITEM, menu_of, text, this_object};
use super::shared;
use crate::menu::roles::Os;
use crate::menu::sort::{self, Entry, Slot};
use crate::msg::MenuOp;

/// The state a radio item keeps its check mark in.
const RADIO_CHECKED: &str = "__radioChecked";

/// The `Menu` class.
///
/// # Errors
///
/// The class could not be made.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    if let Some(class) = menu_state::state(context).menu_class() {
        return Ok(class);
    }
    let emitter = define::emitter(context)?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "Menu",
            length: 0,
            init,
            parent: Some(&emitter),
            methods: &[
                ("popup", 0, popup),
                ("closePopup", 0, close_popup),
                ("getMenuItemById", 1, get_item_by_id),
                ("append", 1, |this, args, cx| {
                    let menu = this_menu(this, cx)?;
                    let at = item_count(&menu, cx)?;
                    insert_item(cx, &menu, at as f64, &arg(args, 0))?;
                    Ok(JsValue::undefined())
                }),
                ("insert", 2, |this, args, cx| {
                    let menu = this_menu(this, cx)?;
                    let at = arg(args, 0).to_number(cx)?;
                    insert_item(cx, &menu, at, &arg(args, 1))?;
                    Ok(JsValue::undefined())
                }),
                ("getItemCount", 0, |this, _, cx| {
                    let menu = this_menu(this, cx)?;
                    Ok(item_count(&menu, cx)?.into())
                }),
            ],
            statics: &[
                ("buildFromTemplate", 1, |_, args, cx| {
                    build_from_template(cx, &arg(args, 0))
                }),
                ("setApplicationMenu", 1, |_, args, cx| {
                    set_application_menu(cx, &arg(args, 0))?;
                    Ok(JsValue::undefined())
                }),
                ("getApplicationMenu", 0, |_, _, cx| Ok(application_menu(cx))),
                ("sendActionToFirstResponder", 1, |_, _, _| {
                    Ok(JsValue::undefined())
                }),
            ],
            ..ClassSpec::default()
        },
    )?;
    menu_state::state(context).set_menu_class(class.clone());
    Ok(class)
}

fn init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    let id = menu_state::state(context).new_menu_id();
    define::bind(this, MENU, id, context);
    let commands = native::object(context);
    let groups = native::object(context);
    let items = native::array(context, Vec::new());
    for (name, value) in [
        ("commandsMap", commands.into()),
        ("groupsMap", groups.into()),
        ("items", items),
    ] {
        this.set(JsString::from(name), value, false, context)?;
    }
    Ok(())
}

fn this_menu(this: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    menu_of(this, context)
        .map(|(menu, _)| menu)
        .ok_or_else(|| native::type_error("Illegal invocation"))
}

pub(crate) fn items_of(menu: &JsObject, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let items = menu.get(js_string!("items"), context)?;
    native::list_of(&items, context)
}

fn item_count(menu: &JsObject, context: &mut Context) -> JsResult<usize> {
    Ok(items_of(menu, context)?.len())
}

/// `menu.insert(position, item)`: the item goes in at `position` of `items`.
fn insert_item(
    context: &mut Context,
    menu: &JsObject,
    position: f64,
    item: &JsValue,
) -> JsResult<()> {
    let Some(item_object) = define::id_of(item, MENU_ITEM, context).and(item.as_object()) else {
        return Err(native::type_error("Invalid item"));
    };
    let count = item_count(menu, context)?;
    if position < 0. {
        let shown = JsValue::new(position).display().to_string();
        return Err(native::range_error(format!(
            "Position {shown} cannot be less than 0"
        )));
    }
    if position > count as f64 {
        let shown = JsValue::new(position).display().to_string();
        return Err(native::range_error(format!(
            "Position {shown} cannot be greater than the total MenuItem count"
        )));
    }
    let at = if position.is_nan() {
        0
    } else {
        position as usize
    };
    if text(&item_object, "type", context)? == "radio" {
        join_radio_group(context, menu, &item_object, at)?;
    }
    if item_object
        .get(js_string!("menu"), context)?
        .is_null_or_undefined()
    {
        item_object.set(js_string!("menu"), menu.clone(), false, context)?;
    }
    item_object.define_property_or_throw(
        js_string!("menu"),
        PropertyDescriptor::builder()
            .writable(false)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    let items = menu.get(js_string!("items"), context)?;
    native::invoke(
        &items,
        "splice",
        &[at.into(), 0.into(), item_object.clone().into()],
        context,
    )?;
    let commands = menu.get(js_string!("commandsMap"), context)?;
    let command = item_object.get(js_string!("commandId"), context)?;
    if let (Some(commands), Ok(key)) = (commands.as_object(), command.to_property_key(context)) {
        commands.set(key, item_object, false, context)?;
    }
    Ok(())
}

/// The group a radio item inserted at `at` belongs to: that of the radio item before it, or after
/// it when it is first, up to a separator; a new one otherwise.
fn group_for(context: &mut Context, items: &[JsValue], at: usize) -> JsResult<u32> {
    let scan: Vec<usize> = if at > 0 {
        (0..at).rev().collect()
    } else {
        (0..items.len()).collect()
    };
    for index in scan {
        let Some(item) = items[index].as_object() else {
            continue;
        };
        match text(&item, "type", context)?.as_str() {
            "radio" => {
                if let Some(group) = item.get(js_string!("groupId"), context)?.as_number() {
                    return Ok(group as u32);
                }
            }
            "separator" => break,
            _ => {}
        }
    }
    Ok(menu_state::state(context).new_group_id())
}

fn join_radio_group(
    context: &mut Context,
    menu: &JsObject,
    item: &JsObject,
    at: usize,
) -> JsResult<()> {
    let items = items_of(menu, context)?;
    let group = group_for(context, &items, at)?;
    if item
        .get(js_string!("groupId"), context)?
        .is_null_or_undefined()
    {
        item.set(js_string!("groupId"), group, false, context)?;
    }
    item.define_property_or_throw(
        js_string!("groupId"),
        PropertyDescriptor::builder()
            .writable(false)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    let groups = menu.get(js_string!("groupsMap"), context)?;
    let key = JsString::from(group.to_string());
    let list = native::get(&groups, &group.to_string(), context)?;
    let list = if list.is_object() {
        list
    } else {
        let made = native::array(context, Vec::new());
        if let Some(groups) = groups.as_object() {
            groups.set(key, made.clone(), false, context)?;
        }
        made
    };
    native::invoke(&list, "push", &[item.clone().into()], context)?;
    let checked = item.get(js_string!("checked"), context)?.to_boolean();
    native::hidden(item, JsString::from(RADIO_CHECKED), checked, context)?;
    let get = native::function(context, "checked", 0, radio_checked);
    let set = native::function(context, "checked", 1, set_radio_checked);
    item.define_property_or_throw(
        js_string!("checked"),
        PropertyDescriptor::builder()
            .get(get)
            .set(set)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    Ok(())
}

fn radio_checked(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    native::get(this, RADIO_CHECKED, context)
}

/// Whatever a radio item's `checked` is set to, it becomes the checked one of its group.
fn set_radio_checked(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let item = this_object(this)?;
    let menu = native::get(this, "menu", context)?;
    let group = native::get(this, "groupId", context)?;
    let groups = native::get(&menu, "groupsMap", context)?;
    let key = native::text_of(&group, context)?;
    let members = native::get(&groups, &key, context)?;
    for other in native::list_of(&members, context)? {
        if let Some(other) = other.as_object().filter(|o| !JsObject::equals(o, &item)) {
            native::hidden(&other, JsString::from(RADIO_CHECKED), false, context)?;
        }
    }
    native::hidden(&item, JsString::from(RADIO_CHECKED), true, context)?;
    Ok(JsValue::undefined())
}

/// The menu is about to open: each radio group has a checked item, and `'menu-will-show'` is
/// emitted.
pub(crate) fn menu_will_show(context: &mut Context, menu: &JsObject) -> JsResult<()> {
    settle_radio_groups(context, menu)?;
    let event = define::event(context)?;
    define::emit(context, menu, "menu-will-show", &[event.into()])
}

fn settle_radio_groups(context: &mut Context, menu: &JsObject) -> JsResult<()> {
    let groups = menu.get(js_string!("groupsMap"), context)?;
    if let Some(groups) = groups.as_object() {
        let object = native::global(context, "Object")?;
        let keys = native::invoke(&object, "keys", &[groups.clone().into()], context)?;
        for key in native::list_of(&keys, context)? {
            let members = groups.get(key.to_property_key(context)?, context)?;
            let members = native::list_of(&members, context)?;
            settle_group(context, &members)?;
        }
    }
    for item in items_of(menu, context)? {
        let sub = native::get(&item, "submenu", context)?;
        if let Some((sub, _)) = menu_of(&sub, context) {
            settle_radio_groups(context, &sub)?;
        }
    }
    Ok(())
}

/// A radio group with no checked item has its first checked.
fn settle_group(context: &mut Context, members: &[JsValue]) -> JsResult<()> {
    for member in members {
        if native::get(member, "checked", context)?.to_boolean() {
            return Ok(());
        }
    }
    if let Some(first) = members.first().and_then(JsValue::as_object) {
        native::hidden(&first, JsString::from(RADIO_CHECKED), true, context)?;
    }
    Ok(())
}

fn get_item_by_id(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let menu = this_menu(this, context)?;
    let wanted = arg(args, 0);
    let items = items_of(&menu, context)?;
    for item in &items {
        if native::get(item, "id", context)?.strict_equals(&wanted) {
            return Ok(item.clone());
        }
    }
    for item in &items {
        let sub = native::get(item, "submenu", context)?;
        if sub.is_object() {
            let found = native::invoke(
                &sub,
                "getMenuItemById",
                std::slice::from_ref(&wanted),
                context,
            )?;
            if !found.is_null_or_undefined() {
                return Ok(found);
            }
        }
    }
    Ok(JsValue::null())
}

// ---------- templates ----------

/// `Menu.buildFromTemplate(template)`.
///
/// # Errors
///
/// `TypeError`s for a template that is no array or has an item with none of `label`, `role` and
/// `type: 'separator'`, and what building an item throws.
pub(crate) fn build_from_template(context: &mut Context, template: &JsValue) -> JsResult<JsValue> {
    let is_array = template.as_object().is_some_and(|o| o.is_array());
    if !is_array {
        return Err(native::type_error(
            "Invalid template for Menu: Menu template must be an array",
        ));
    }
    let items = native::list_of(template, context)?;
    for item in &items {
        if !is_template_item(item, context)? {
            return Err(native::type_error(
                "Invalid template for MenuItem: must have at least one of label, role or type",
            ));
        }
    }
    let sorted = sort_template(context, items)?;
    let sorted = without_extra_separators(context, sorted)?;
    let class = menu_state::state(context).menu_class();
    let class = class.ok_or_else(|| native::type_error("Menu is not a constructor"))?;
    let menu = class.construct(&[], None, context)?;
    for item in sorted {
        let item = if define::id_of(&item, MENU_ITEM, context).is_some() {
            item
        } else {
            let class = menu_state::state(context).item_class();
            let class = class.ok_or_else(|| native::type_error("MenuItem is not a constructor"))?;
            class.construct(&[item], None, context)?.into()
        };
        let at = item_count(&menu, context)?;
        insert_item(context, &menu, at as f64, &item)?;
    }
    Ok(menu.into())
}

fn is_template_item(item: &JsValue, context: &mut Context) -> JsResult<bool> {
    let Some(object) = item.as_object().filter(|o| !o.is_callable()) else {
        return Ok(false);
    };
    for key in ["label", "role"] {
        if object.has_own_property(JsString::from(key), context)? {
            return Ok(true);
        }
    }
    Ok(text(&object, "type", context)? == "separator")
}

fn is_separator(item: &JsValue, context: &mut Context) -> JsResult<bool> {
    let kind = native::get(item, "type", context)?;
    Ok(kind
        .as_string()
        .is_some_and(|k| k.to_std_string_escaped() == "separator"))
}

fn names_of(value: &JsValue, context: &mut Context) -> JsResult<Option<Vec<String>>> {
    if !value.to_boolean() {
        return Ok(None);
    }
    let list = native::list_of(value, context)?;
    Ok(Some(
        list.iter()
            .filter_map(|v| v.as_string().map(|s| s.to_std_string_escaped()))
            .collect(),
    ))
}

fn entry_of(item: &JsValue, context: &mut Context) -> JsResult<Entry> {
    let field = |name: &str, context: &mut Context| native::get(item, name, context);
    let id = field("id", context)?;
    Ok(Entry {
        id: id.as_string().map(|id| id.to_std_string_escaped()),
        separator: is_separator(item, context)?,
        before: names_of(&field("before", context)?, context)?,
        after: names_of(&field("after", context)?, context)?,
        before_group: names_of(&field("beforeGroupContaining", context)?, context)?,
        after_group: names_of(&field("afterGroupContaining", context)?, context)?,
    })
}

/// `sortTemplate`: the items put in order, and the template array of every item's `submenu` too.
fn sort_template(context: &mut Context, items: Vec<JsValue>) -> JsResult<Vec<JsValue>> {
    let entries = items
        .iter()
        .map(|item| entry_of(item, context))
        .collect::<JsResult<Vec<_>>>()?;
    let mut sorted = Vec::new();
    for slot in sort::sort_entries(&entries) {
        let item = match slot {
            Slot::Item(at) | Slot::Separator(Some(at)) => items[at].clone(),
            Slot::Separator(None) => {
                define::record(context, &[("type", js_text("separator"))])?.into()
            }
        };
        sorted.push(item);
    }
    for item in &sorted {
        let sub = native::get(item, "submenu", context)?;
        if sub.as_object().is_some_and(|o| o.is_array()) {
            let inner = native::list_of(&sub, context)?;
            let inner = sort_template(context, inner)?;
            let inner = native::array(context, inner);
            if let Some(object) = item.as_object() {
                object.set(js_string!("submenu"), inner, false, context)?;
            }
        }
    }
    Ok(sorted)
}

fn without_extra_separators(context: &mut Context, items: Vec<JsValue>) -> JsResult<Vec<JsValue>> {
    let mut shape = Vec::new();
    for item in &items {
        let separator = is_separator(item, context)?;
        let invisible = native::get(item, "visible", context)?.as_boolean() == Some(false);
        shape.push((separator, invisible));
    }
    let keep = sort::keep_after_extra_separators(&shape);
    Ok(items
        .into_iter()
        .zip(keep)
        .filter_map(|(item, keep)| keep.then_some(item))
        .collect())
}

// ---------- the application menu ----------

/// Gives the exports of the main process's `require('electron')` the classes of this package.
///
/// # Errors
///
/// A class could not be made.
pub(crate) fn add_exports(context: &mut Context, exports: &JsObject) -> JsResult<()> {
    let menu = make(context)?;
    exports.set(js_string!("Menu"), menu, false, context)?;
    let item = menu_item::make(context)?;
    exports.set(js_string!("MenuItem"), item, false, context)?;
    Ok(())
}

/// `Menu.getApplicationMenu()`.
fn application_menu(context: &mut Context) -> JsValue {
    menu_state::state(context)
        .app_menu()
        .map_or(JsValue::null(), JsValue::from)
}

/// `Menu.setApplicationMenu(menu)`: macOS shows it as the menu bar; elsewhere every window is given
/// it as its own, and one made later is too.
///
/// # Errors
///
/// `Invalid menu` for a value that is neither a `Menu` nor falsy.
pub(crate) fn set_application_menu(context: &mut Context, menu: &JsValue) -> JsResult<()> {
    if menu.to_boolean() && menu_of(menu, context).is_none() {
        return Err(native::type_error("Invalid menu"));
    }
    let state = menu_state::state(context);
    state.mark_app_menu_set();
    state.set_app_menu(menu.as_object());
    let id = match menu.to_boolean() {
        true => Some(menu_state::attach(menu, context)?),
        false => None,
    };
    menu_state::send(context, MenuOp::SetApplicationMenu(id));
    if Os::CURRENT != Os::Mac {
        let windows = shared(context).windows.newest_first();
        for record in windows.iter().filter(|w| !w.dead.get()) {
            super::menu_window::set_menu_of(context, record.id, menu)?;
        }
    }
    Ok(())
}

/// `app.applicationMenu`'s getter.
pub(crate) fn application_menu_getter(
    _: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    Ok(application_menu(context))
}

/// `app.applicationMenu`'s setter.
pub(crate) fn application_menu_setter(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    set_application_menu(context, &arg(args, 0))?;
    Ok(JsValue::undefined())
}

/// What Electron does at `will-finish-launching` for an app that set no menu (`default-menu.ts`):
/// File, Edit, View and Window by role, and Help, whose links are for an app still in development.
///
/// # Errors
///
/// The menu could not be built.
pub(crate) fn set_default_application_menu(context: &mut Context) -> JsResult<()> {
    if menu_state::state(context).app_menu_set() {
        return Ok(());
    }
    // The classes are made if no script has asked for them yet.
    make(context)?;
    menu_item::make(context)?;
    let help = help_menu(context)?;
    let mut roles: Vec<JsValue> = Vec::new();
    if Os::CURRENT == Os::Mac {
        roles.push(define::record(context, &[("role", js_text("appMenu"))])?.into());
    }
    for role in ["fileMenu", "editMenu", "viewMenu", "windowMenu"] {
        roles.push(define::record(context, &[("role", js_text(role))])?.into());
    }
    roles.push(help.into());
    let template = native::array(context, roles);
    let menu = build_from_template(context, &template)?;
    set_application_menu(context, &menu)
}

fn help_menu(context: &mut Context) -> JsResult<JsObject> {
    let links = if shared(context).app.is_packaged() {
        Vec::new()
    } else {
        let docs = format!(
            "https://github.com/electron/electron/tree/v{}/docs#readme",
            crate::ELECTRON_VERSION
        );
        let pairs = [
            ("Learn More", "https://electronjs.org".to_owned()),
            ("Documentation", docs),
            (
                "Community Discussions",
                "https://discord.gg/electronjs".to_owned(),
            ),
            (
                "Search Issues",
                "https://github.com/electron/electron/issues".to_owned(),
            ),
        ];
        let mut links = Vec::new();
        for (label, url) in pairs {
            let click = native::closure(context, "click", 0, open_link, js_text(&url));
            let item = define::record(
                context,
                &[("label", js_text(label)), ("click", click.into())],
            )?;
            links.push(item.into());
        }
        links
    };
    let submenu = native::array(context, links);
    define::record(context, &[("role", js_text("help")), ("submenu", submenu)])
}

/// A link of the Help menu: `shell.openExternal(url)`.
fn open_link(
    _: &JsValue,
    _: &[JsValue],
    url: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let shell = super::shell::make(context)?;
    native::invoke(&shell, "openExternal", std::slice::from_ref(url), context)
}

// ---------- popup ----------

/// `menu.popup(options)`: the shell opens the menu and `callback` is called when it closes.
fn popup(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let menu = this_menu(this, context)?;
    let options = match arg(args, 0) {
        value if value.is_undefined() => native::object(context),
        value => value
            .as_object()
            .ok_or_else(|| native::type_error("Options must be an object"))?,
    };
    let field = |name: &str, context: &mut Context| options.get(JsString::from(name), context);
    let number = |name: &str, context: &mut Context| -> JsResult<f64> {
        Ok(field(name, context)?.as_number().unwrap_or(-1.))
    };
    let (x, y) = (number("x", context)?, number("y", context)?);
    let position = number("positioningItem", context)?;
    let given = field("window", context)?;
    let window = popup_window(context, &given)?;
    let callback = match field("callback", context)?.as_callable() {
        Some(callback) => callback.clone(),
        None => native::function(context, "callback", 0, |_, _, _| Ok(JsValue::undefined())),
    };
    let id = menu_state::attach(&menu.clone().into(), context)?;
    let call = menu_state::state(context).start_popup(callback, menu.clone());
    let at = (x >= 0. && y >= 0.).then_some((x as f32, y as f32));
    let positioning_item = (position >= 0.).then_some(position as usize);
    let target = window_id_of(&window, context)
        .ok_or_else(|| define::error("Cannot open Menu without a BaseWindow present"))?;
    menu_state::send(
        context,
        MenuOp::Popup {
            menu: id,
            window: target,
            at,
            positioning_item,
            call,
        },
    );
    let shown = define::record(
        context,
        &[
            ("browserWindow", window),
            ("x", x.into()),
            ("y", y.into()),
            ("position", position.into()),
        ],
    )?;
    Ok(shown.into())
}

/// The window a popup is over: the one the options name if it is open, else the focused one,
/// else the first.
fn popup_window(context: &mut Context, given: &JsValue) -> JsResult<JsValue> {
    let windows = shared(context).windows.newest_first();
    let open: Vec<_> = windows.iter().filter(|w| !w.dead.get()).collect();
    if let Some(id) = window_id_of(given, context)
        && open.iter().any(|w| w.id == id)
    {
        return Ok(given.clone());
    }
    let chosen = open
        .iter()
        .find(|w| w.state.borrow().focused)
        .or(open.first());
    chosen
        .map(|w| w.object.clone().into())
        .ok_or_else(|| define::error("Cannot open Menu without a BaseWindow present"))
}

fn close_popup(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    this_menu(this, context)?;
    let window = window_id_of(&arg(args, 0), context);
    menu_state::send(context, MenuOp::ClosePopup { window });
    Ok(JsValue::undefined())
}
