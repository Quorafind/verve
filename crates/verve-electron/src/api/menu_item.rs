//! `MenuItem`: Electron 39.8.10's `lib/browser/api/menu-item.ts`, step for step.
//!
//! An item is a plain object. The constructor copies the options onto it, then fills in what
//! they left out (the role's label and accelerator, `enabled: true`…) in the order Electron does,
//! so `Object.keys(item)` lists the same names in the same order; `type`, `role`, `accelerator`,
//! `icon`, `submenu` and `commandId` are then read-only. The shell is told of an item by reading
//! it again, not by hooking its properties ([`menu_state`](super::menu_state)).

use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};
use verve::native::{self, ClassSpec, arg, js_text};

use super::define;
use super::menu;
use super::menu_roles;
use super::menu_state::{self, MENU_ITEM, menu_of, text, this_object};
use super::shared;
use crate::menu::roles::{self, Os, Template};

/// The types an item can have, in `MenuItem.types`.
const TYPES: [&str; 7] = [
    "normal",
    "separator",
    "submenu",
    "checkbox",
    "radio",
    "header",
    "palette",
];

/// The `MenuItem` class.
///
/// # Errors
///
/// The class could not be made.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    if let Some(class) = menu_state::state(context).item_class() {
        return Ok(class);
    }
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "MenuItem",
            length: 1,
            init,
            methods: &[
                ("getDefaultRoleAccelerator", 0, default_accelerator),
                ("getCheckStatus", 0, check_status),
                ("overrideProperty", 2, |this, args, cx| {
                    override_method(this, args, cx, false)
                }),
                ("overrideReadOnlyProperty", 2, |this, args, cx| {
                    override_method(this, args, cx, true)
                }),
            ],
            ..ClassSpec::default()
        },
    )?;
    let types: Vec<JsValue> = TYPES.iter().map(|t| js_text(t)).collect();
    let types = native::array(context, types);
    class.set(js_string!("types"), types, false, context)?;
    menu_state::state(context).set_item_class(class.clone());
    Ok(class)
}

/// `this[name] = default` unless it already has a value.
#[inline(never)]
fn override_property(
    object: &JsObject,
    name: &str,
    default: JsValue,
    context: &mut Context,
) -> JsResult<()> {
    if native::get(&object.clone().into(), name, context)?.is_null_or_undefined() {
        object.set(JsString::from(name), default, false, context)?;
    }
    Ok(())
}

/// [`override_property`], then the property is made read-only.
#[inline(never)]
fn override_read_only(
    object: &JsObject,
    name: &str,
    default: JsValue,
    context: &mut Context,
) -> JsResult<()> {
    override_property(object, name, default, context)?;
    let value = native::get(&object.clone().into(), name, context)?;
    object.define_property_or_throw(
        JsString::from(name),
        PropertyDescriptor::builder()
            .value(value)
            .writable(false)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    Ok(())
}

/// `overrideProperty(name, default)` and `overrideReadOnlyProperty`: `default` is `null` when the
/// call gave none.
fn override_method(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    read_only: bool,
) -> JsResult<JsValue> {
    let name = native::arg_text(args, 0, context)?;
    let default = arg(args, 1);
    let default = if default.is_undefined() {
        JsValue::null()
    } else {
        default
    };
    let object = this_object(this)?;
    if read_only {
        override_read_only(&object, &name, default, context)?;
    } else {
        override_property(&object, &name, default, context)?;
    }
    Ok(JsValue::undefined())
}

fn default_accelerator(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let role = text(&this_object(this)?, "role", context)?;
    let found = roles::defaults(&role, Os::CURRENT).and_then(|d| d.accelerator);
    Ok(found.map_or(JsValue::undefined(), js_text))
}

fn check_status(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let item = this_object(this)?;
    let role = text(&item, "role", context)?;
    if overrides_check(&role) {
        return Ok(false.into());
    }
    native::get(&item.clone().into(), "checked", context)
}

/// Whether the role decides the check mark itself: only the spell checker's.
fn overrides_check(role: &str) -> bool {
    roles::defaults(role, Os::CURRENT).is_some_and(|d| d.checkbox)
}

/// `for (const key in options) key in this || (this[key] = options[key])`: the enumerable string
/// keys of `options` and of its prototypes.
fn copy_options(item: &JsObject, options: &JsValue, context: &mut Context) -> JsResult<()> {
    let object = native::global(context, "Object")?;
    let mut level = options.clone();
    while let Some(source) = level.as_object() {
        let keys = native::invoke(&object, "keys", &[level.clone()], context)?;
        for key in native::list_of(&keys, context)? {
            let key = key.to_property_key(context)?;
            if !item.has_property(key.clone(), context)? {
                let value = source.get(key.clone(), context)?;
                item.set(key, value, false, context)?;
            }
        }
        level = native::invoke(&object, "getPrototypeOf", &[level], context)?;
    }
    Ok(())
}

/// The templates of a role's submenu, as the objects `Menu.buildFromTemplate` takes.
pub(crate) fn template_value(templates: &[Template], context: &mut Context) -> JsResult<JsValue> {
    let mut items = Vec::new();
    for template in templates {
        let item = match template {
            Template::Role(role) => define::record(context, &[("role", js_text(role))])?,
            Template::Separator => define::record(context, &[("type", js_text("separator"))])?,
            Template::Menu(label, inner) => {
                let submenu = template_value(inner, context)?;
                define::record(context, &[("label", js_text(label)), ("submenu", submenu)])?
            }
        };
        items.push(item.into());
    }
    Ok(native::array(context, items))
}

fn init(this: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    let options = arg(args, 0);
    if options.is_null_or_undefined() {
        let what = if options.is_null() {
            "null"
        } else {
            "undefined"
        };
        return Err(native::type_error(format!(
            "Cannot read properties of {what} (reading 'click')"
        )));
    }
    let state = menu_state::state(context);
    let id = state.new_item_id();
    define::bind(this, MENU_ITEM, id, context);
    copy_options(this, &options, context)?;
    let me: JsValue = this.clone().into();
    if let Some(role) = native::get(&me, "role", context)?.as_string() {
        let lower = role.to_std_string_escaped().to_lowercase();
        this.set(js_string!("role"), js_text(&lower), false, context)?;
    }
    let role = text(this, "role", context)?;
    let submenu = submenu_of(this, &role, context)?;
    this.set(js_string!("submenu"), submenu, false, context)?;
    infer_type(this, context)?;
    let default_type = if overrides_check(&role) {
        "checkbox"
    } else {
        "normal"
    };
    let defaults = roles::defaults(&role, Os::CURRENT);
    override_read_only(this, "type", js_text(default_type), context)?;
    override_read_only(this, "role", JsValue::null(), context)?;
    let accelerator = defaults
        .and_then(|d| d.accelerator)
        .map_or(JsValue::null(), js_text);
    override_read_only(this, "accelerator", accelerator, context)?;
    override_read_only(this, "icon", JsValue::null(), context)?;
    override_read_only(this, "submenu", JsValue::null(), context)?;
    let app_name = shared(context).app.name();
    let label = roles::default_label(&role, Os::CURRENT, &app_name);
    override_property(this, "label", js_text(&label), context)?;
    override_property(this, "sublabel", js_text(""), context)?;
    override_property(this, "toolTip", js_text(""), context)?;
    for (name, value) in [("enabled", true), ("visible", true), ("checked", false)] {
        override_property(this, name, value.into(), context)?;
    }
    override_property(this, "acceleratorWorksWhenHidden", true.into(), context)?;
    let register = defaults.is_none_or(|d| d.register_accelerator);
    override_property(this, "registerAccelerator", register.into(), context)?;
    let kind = native::text_of(&native::get(&me, "type", context)?, context)?;
    if !TYPES.contains(&kind.as_str()) {
        return Err(define::error(&format!("Unknown menu item type: {kind}")));
    }
    override_read_only(this, "commandId", JsValue::from(id), context)?;
    let user_accelerator =
        native::function(context, "userAccelerator", 0, |_, _, _| Ok(JsValue::null()));
    this.define_property_or_throw(
        js_string!("userAccelerator"),
        PropertyDescriptor::builder()
            .get(user_accelerator)
            .enumerable(true)
            .configurable(false),
        context,
    )?;
    let user_click = native::get(&me, "click", context)?;
    let click = native::closure(context, "click", 3, click_item, (this.clone(), user_click));
    this.set(js_string!("click"), click, false, context)?;
    state.remember_item(id, this);
    Ok(())
}

/// `this.submenu || defaultSubmenu(role)`, built into a `Menu` unless it is one.
fn submenu_of(item: &JsObject, role: &str, context: &mut Context) -> JsResult<JsValue> {
    let given = native::get(&item.clone().into(), "submenu", context)?;
    let template = if given.to_boolean() {
        given
    } else {
        match roles::default_submenu(role, Os::CURRENT) {
            Some(templates) => template_value(&templates, context)?,
            None => JsValue::undefined(),
        }
    };
    if template.is_null_or_undefined() || menu_of(&template, context).is_some() {
        return Ok(template);
    }
    menu::build_from_template(context, &template)
}

/// A `type` an item with a submenu was not given is `'submenu'`; one that says so without a
/// `Menu` is an error.
fn infer_type(item: &JsObject, context: &mut Context) -> JsResult<()> {
    let me: JsValue = item.clone().into();
    let submenu = native::get(&me, "submenu", context)?;
    if native::get(&me, "type", context)?.is_null_or_undefined() && !submenu.is_null_or_undefined()
    {
        item.set(js_string!("type"), js_text("submenu"), false, context)?;
    }
    if text(item, "type", context)? == "submenu" && menu_of(&submenu, context).is_none() {
        return Err(define::error("Invalid submenu"));
    }
    Ok(())
}

/// The item's `click(event, focusedWindow, focusedWebContents)`: it flips a checkbox or radio, then
/// does the role, and only for a role that did nothing calls the script's own `click`.
fn click_item(
    _: &JsValue,
    args: &[JsValue],
    (item, user_click): &(JsObject, JsValue),
    context: &mut Context,
) -> JsResult<JsValue> {
    let role = text(item, "role", context)?;
    let flips = matches!(text(item, "type", context)?.as_str(), "checkbox" | "radio");
    if flips && !overrides_check(&role) {
        let checked = native::get(&item.clone().into(), "checked", context)?.to_boolean();
        item.set(js_string!("checked"), !checked, false, context)?;
    }
    let (event, window, contents) = (arg(args, 0), arg(args, 1), arg(args, 2));
    if menu_roles::execute(context, &role, &window, &contents)? {
        return Ok(JsValue::undefined());
    }
    if let Some(callback) = user_click.as_callable() {
        callback.call(
            &JsValue::undefined(),
            &[item.clone().into(), window, event],
            context,
        )?;
    }
    Ok(JsValue::undefined())
}
