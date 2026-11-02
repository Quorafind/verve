//! `getComputedStyle`: an element's style after the cascade, read as a `CSSStyleDeclaration`,
//! and `innerText`, which reads the same styles to know what is rendered.
//!
//! `getComputedStyle(element, '::before')` is about a part of the element that is no node, and
//! `getComputedStyle(element, '::nonsense')` about nothing, which has no values: what the
//! declaration is about is kept on its [`Handle`], where a script cannot reach it.

use boa_engine::{Context, JsObject, JsResult, JsValue};

use super::{
    Handle, Kind, Method, arg, arg_string, function, get, native_instance, text, type_error,
};
use crate::css::{PseudoElement, Style, style_of, style_of_part};
use crate::dom::NodeId;
use crate::state::shared;

/// What a computed style declaration is about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Target {
    #[default]
    Element,
    /// A pseudo-element of the element.
    Part(PseudoElement),
    /// A pseudo-element that is not one: the declaration has no values.
    Nothing,
}

impl Target {
    /// What `getComputedStyle`'s second argument names: the element itself when there is none.
    pub fn named(pseudo: Option<&str>) -> Self {
        match pseudo.map(str::trim).filter(|name| !name.is_empty()) {
            None => Self::Element,
            Some(name) => PseudoElement::from_argument(name).map_or(Self::Nothing, Self::Part),
        }
    }
}

/// The element and the part a computed style declaration stands for.
fn computed_target(value: &JsValue) -> Option<(NodeId, Target)> {
    let object = value.as_object()?;
    let handle = object.downcast_ref::<Handle>()?;
    (handle.kind == Kind::Computed).then_some((handle.id as NodeId, handle.target))
}

/// The style of `id`, or of the part of it `target` names, after the cascade.
fn style_of_target(context: &Context, id: NodeId, target: Target) -> Style {
    let state = shared(context);
    let mut state = state.borrow_mut();
    let styler = state.styler();
    match target {
        Target::Element => style_of(&state.dom, &styler, id),
        Target::Part(part) => style_of_part(&state.dom, &styler, id, part),
        Target::Nothing => Style::new(),
    }
}

/// A node's — or one of its parts' — computed value of `property`: the cascade over every
/// sheet, `var()` resolved, and the property's initial value when nothing set it.
pub fn computed_value(context: &Context, id: NodeId, target: Target, property: &str) -> String {
    if target == Target::Nothing {
        return String::new();
    }
    let style = style_of_target(context, id, target);
    // A part has no tag of its own to take initial values from: `::before` is inline.
    let tag = match target {
        Target::Element => shared(context)
            .borrow()
            .dom
            .get(id)
            .and_then(|node| node.tag().map(str::to_owned)),
        Target::Part(_) | Target::Nothing => None,
    };
    crate::css::value_or_initial(&style, property, tag.as_deref()).to_owned()
}

/// `innerText` of `id`: see [`crate::inner_text`].
pub fn rendered_text(context: &Context, id: NodeId) -> String {
    let state = shared(context);
    let mut state = state.borrow_mut();
    let styler = state.styler();
    let dom = &state.dom;
    crate::inner_text::inner_text(dom, id, &|node| style_of(dom, &styler, node))
}

const COMPUTED_METHODS: &[Method] = &[
    ("getPropertyValue", 1, |this, args, cx| {
        let (id, target) = computed_target(this).ok_or_else(|| type_error("Illegal invocation"))?;
        let property = arg_string(args, 0, cx)?;
        Ok(text(computed_value(cx, id, target, property.trim()).trim()))
    }),
    ("getPropertyPriority", 1, |_, _, _| Ok(text(""))),
    ("item", 1, |this, args, cx| {
        let (id, target) = computed_target(this).ok_or_else(|| type_error("Illegal invocation"))?;
        let at = arg(args, 0).to_number(cx)? as usize;
        let style = style_of_target(cx, id, target);
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

/// `getComputedStyle(element[, pseudo])`: the element's — or one of its parts' — style after
/// the cascade, read as properties — `style.color`, `style.getPropertyValue('--text-muted')`.
pub fn computed_style(context: &mut Context, id: NodeId, target: Target) -> JsResult<JsValue> {
    let declaration = native_instance(context, "CSSStyleDeclaration", Kind::Computed, id);
    if let Some(mut handle) = declaration.downcast_mut::<Handle>() {
        handle.target = target;
    }
    let proxy = boa_engine::object::builtins::JsProxyBuilder::new(declaration)
        .get(|_, args, cx| {
            let declaration = arg(args, 0);
            let key = arg(args, 1);
            let Some(name) = key.as_string().map(|n| n.to_std_string_escaped()) else {
                return Ok(JsValue::undefined());
            };
            if let Some((_, length, f)) = COMPUTED_METHODS.iter().find(|(m, ..)| *m == name) {
                let method = function(cx, &name, *length, *f);
                return super::call_method(&method.into(), "bind", &[declaration], cx);
            }
            let (id, target) = computed_target(&declaration).unwrap_or_default();
            match name.as_str() {
                "constructor" => return get(&declaration, "constructor", cx),
                "length" => return Ok(style_of_target(cx, id, target).len().into()),
                "cssText" => return Ok(text("")),
                "parentRule" => return Ok(JsValue::null()),
                _ => {}
            }
            let property = if name.contains('-') {
                name
            } else {
                crate::dom::css_name(&name)
            };
            Ok(text(computed_value(cx, id, target, &property).trim()))
        })
        .set(|_, _, _| Ok(true.into()))
        .build(context)?;
    Ok(JsObject::from(proxy).into())
}
