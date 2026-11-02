//! `getComputedStyle`: an element's style after the cascade, read as a `CSSStyleDeclaration`,
//! and `innerText`, which reads the same styles to know what is rendered.

use boa_engine::{Context, JsObject, JsResult, JsValue};

use super::{
    Kind, Method, arg, arg_string, function, get, handle_of, native_instance, text, type_error,
};
use crate::dom::NodeId;
use crate::state::shared;

/// A node's computed value of `property`: the cascade over every sheet, `var()` resolved, and
/// the property's initial value when nothing set it.
pub fn computed_value(context: &Context, id: NodeId, property: &str) -> String {
    let state = shared(context);
    let mut state = state.borrow_mut();
    let styler = state.styler();
    let style = crate::css::style_of(&state.dom, &styler, id);
    let tag = state.dom.get(id).and_then(|n| n.tag());
    crate::css::value_or_initial(&style, property, tag).to_owned()
}

/// `innerText` of `id`: see [`crate::inner_text`].
pub fn rendered_text(context: &Context, id: NodeId) -> String {
    let state = shared(context);
    let mut state = state.borrow_mut();
    let styler = state.styler();
    let dom = &state.dom;
    crate::inner_text::inner_text(dom, id, &|node| crate::css::style_of(dom, &styler, node))
}

const COMPUTED_METHODS: &[Method] = &[
    ("getPropertyValue", 1, |this, args, cx| {
        let id = handle_of(this, Kind::Computed).ok_or_else(|| type_error("Illegal invocation"))?;
        let property = arg_string(args, 0, cx)?;
        Ok(text(computed_value(cx, id, property.trim()).trim()))
    }),
    ("getPropertyPriority", 1, |_, _, _| Ok(text(""))),
    ("item", 1, |this, args, cx| {
        let id = handle_of(this, Kind::Computed).ok_or_else(|| type_error("Illegal invocation"))?;
        let at = arg(args, 0).to_number(cx)? as usize;
        let state = shared(cx);
        let mut state = state.borrow_mut();
        let styler = state.styler();
        let style = crate::css::style_of(&state.dom, &styler, id);
        let mut names: Vec<&String> = style.keys().collect();
        names.sort();
        Ok(names.get(at).map_or(text(""), |n| text(n)))
    }),
    ("setProperty", 2, |_, _, cx| {
        Err(super::dom_error(
            cx,
            "Failed to execute 'setProperty' on 'CSSStyleDeclaration': These styles are computed, and therefore read-only.",
            "NoModificationAllowedError",
        ))
    }),
    ("removeProperty", 1, |_, _, cx| {
        Err(super::dom_error(
            cx,
            "Failed to execute 'removeProperty' on 'CSSStyleDeclaration': These styles are computed, and therefore read-only.",
            "NoModificationAllowedError",
        ))
    }),
];

/// `getComputedStyle(element)`: the element's style after the cascade, read as properties —
/// `style.color`, `style.getPropertyValue('--text-muted')`.
pub fn computed_style(context: &mut Context, id: NodeId) -> JsResult<JsValue> {
    let target = native_instance(context, "CSSStyleDeclaration", Kind::Computed, id);
    let proxy = boa_engine::object::builtins::JsProxyBuilder::new(target)
        .get(|_, args, cx| {
            let target = arg(args, 0);
            let key = arg(args, 1);
            let Some(name) = key.as_string().map(|n| n.to_std_string_escaped()) else {
                return Ok(JsValue::undefined());
            };
            if let Some((_, length, f)) = COMPUTED_METHODS.iter().find(|(m, ..)| *m == name) {
                let method = function(cx, &name, *length, *f);
                return super::call_method(&method.into(), "bind", &[target], cx);
            }
            let id = handle_of(&target, Kind::Computed).unwrap_or_default();
            match name.as_str() {
                "constructor" => return get(&target, "constructor", cx),
                "length" => {
                    let state = shared(cx);
                    let mut state = state.borrow_mut();
                    let styler = state.styler();
                    return Ok(crate::css::style_of(&state.dom, &styler, id).len().into());
                }
                "cssText" => return Ok(text("")),
                "parentRule" => return Ok(JsValue::null()),
                _ => {}
            }
            let property = if name.contains('-') {
                name
            } else {
                crate::dom::css_name(&name)
            };
            Ok(text(computed_value(cx, id, &property).trim()))
        })
        .set(|_, _, _| Ok(true.into()))
        .build(context)?;
    Ok(JsObject::from(proxy).into())
}
