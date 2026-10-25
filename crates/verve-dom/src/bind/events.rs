//! Events in the tree: the classes a page's events are made of, all on Verve's own `Event`
//! (`MouseEvent` ← `UIEvent` ← `Event`); the path a dispatch takes, which Verve asks of
//! [`parent`]; and what the document itself does with events — focus moving, a click ticking a
//! box, choosing a radio button, pressing a label's control, submitting a form.
//!
//! An event the host dispatches for what a person did is trusted (`isTrusted`), and for the
//! length of its dispatch it is `window.event`, as React reads it to tell how urgent an update
//! is.

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};
use verve::dom_events::FieldDefault as F;

use super::node::{read_dom, with_dom, wrap};
use super::{Method, arg, node_id, plain_object, text};
use crate::dom::{NodeId, NodeKind};
use crate::state::shared;

/// The next target on an event's path from `target`: a node's parent, the window from its
/// document, a shadow root's host.
pub fn parent(target: &JsValue, _event: &JsObject, context: &mut Context) -> JsResult<Option<JsValue>> {
    let Some(id) = node_id(target) else {
        return Ok(None);
    };
    let next = {
        let state = shared(context);
        let state = state.borrow();
        if id == state.dom.document {
            return Ok(Some(context.global_object().into()));
        }
        state
            .dom
            .parent(id)
            .or_else(|| state.shadow_hosts.get(&id).copied())
    };
    Ok(next.map(|n| wrap(context, n)))
}

const UI: &[(&str, F)] = &[("view", F::Null), ("detail", F::Number(0.)), ("which", F::Number(0.))];

const MODIFIERS: &[(&str, F)] = &[
    ("ctrlKey", F::Bool(false)),
    ("shiftKey", F::Bool(false)),
    ("altKey", F::Bool(false)),
    ("metaKey", F::Bool(false)),
];

const MOUSE: &[(&str, F)] = &[
    ("view", F::Null),
    ("detail", F::Number(0.)),
    ("which", F::Number(0.)),
    ("ctrlKey", F::Bool(false)),
    ("shiftKey", F::Bool(false)),
    ("altKey", F::Bool(false)),
    ("metaKey", F::Bool(false)),
    ("screenX", F::Number(0.)),
    ("screenY", F::Number(0.)),
    ("clientX", F::Number(0.)),
    ("clientY", F::Number(0.)),
    ("pageX", F::Number(0.)),
    ("pageY", F::Number(0.)),
    ("offsetX", F::Number(0.)),
    ("offsetY", F::Number(0.)),
    ("layerX", F::Number(0.)),
    ("layerY", F::Number(0.)),
    ("movementX", F::Number(0.)),
    ("movementY", F::Number(0.)),
    ("button", F::Number(0.)),
    ("buttons", F::Number(0.)),
    ("relatedTarget", F::Null),
];

const POINTER: &[(&str, F)] = &[
    ("view", F::Null),
    ("detail", F::Number(0.)),
    ("which", F::Number(0.)),
    ("ctrlKey", F::Bool(false)),
    ("shiftKey", F::Bool(false)),
    ("altKey", F::Bool(false)),
    ("metaKey", F::Bool(false)),
    ("screenX", F::Number(0.)),
    ("screenY", F::Number(0.)),
    ("clientX", F::Number(0.)),
    ("clientY", F::Number(0.)),
    ("pageX", F::Number(0.)),
    ("pageY", F::Number(0.)),
    ("offsetX", F::Number(0.)),
    ("offsetY", F::Number(0.)),
    ("layerX", F::Number(0.)),
    ("layerY", F::Number(0.)),
    ("movementX", F::Number(0.)),
    ("movementY", F::Number(0.)),
    ("button", F::Number(0.)),
    ("buttons", F::Number(0.)),
    ("relatedTarget", F::Null),
    ("pointerId", F::Number(1.)),
    ("width", F::Number(1.)),
    ("height", F::Number(1.)),
    ("pressure", F::Number(0.)),
    ("tangentialPressure", F::Number(0.)),
    ("tiltX", F::Number(0.)),
    ("tiltY", F::Number(0.)),
    ("twist", F::Number(0.)),
    ("altitudeAngle", F::Number(std::f64::consts::FRAC_PI_2)),
    ("azimuthAngle", F::Number(0.)),
    ("pointerType", F::Text("mouse")),
    ("isPrimary", F::Bool(true)),
];

const WHEEL: &[(&str, F)] = &[
    ("view", F::Null),
    ("detail", F::Number(0.)),
    ("which", F::Number(0.)),
    ("ctrlKey", F::Bool(false)),
    ("shiftKey", F::Bool(false)),
    ("altKey", F::Bool(false)),
    ("metaKey", F::Bool(false)),
    ("screenX", F::Number(0.)),
    ("screenY", F::Number(0.)),
    ("clientX", F::Number(0.)),
    ("clientY", F::Number(0.)),
    ("pageX", F::Number(0.)),
    ("pageY", F::Number(0.)),
    ("offsetX", F::Number(0.)),
    ("offsetY", F::Number(0.)),
    ("movementX", F::Number(0.)),
    ("movementY", F::Number(0.)),
    ("button", F::Number(0.)),
    ("buttons", F::Number(0.)),
    ("relatedTarget", F::Null),
    ("deltaX", F::Number(0.)),
    ("deltaY", F::Number(0.)),
    ("deltaZ", F::Number(0.)),
    ("deltaMode", F::Number(0.)),
];

const DRAG: &[(&str, F)] = &[
    ("view", F::Null),
    ("detail", F::Number(0.)),
    ("ctrlKey", F::Bool(false)),
    ("shiftKey", F::Bool(false)),
    ("altKey", F::Bool(false)),
    ("metaKey", F::Bool(false)),
    ("screenX", F::Number(0.)),
    ("screenY", F::Number(0.)),
    ("clientX", F::Number(0.)),
    ("clientY", F::Number(0.)),
    ("pageX", F::Number(0.)),
    ("pageY", F::Number(0.)),
    ("button", F::Number(0.)),
    ("buttons", F::Number(0.)),
    ("relatedTarget", F::Null),
    ("dataTransfer", F::Null),
];

const KEYBOARD: &[(&str, F)] = &[
    ("view", F::Null),
    ("detail", F::Number(0.)),
    ("which", F::Number(0.)),
    ("ctrlKey", F::Bool(false)),
    ("shiftKey", F::Bool(false)),
    ("altKey", F::Bool(false)),
    ("metaKey", F::Bool(false)),
    ("key", F::Text("")),
    ("code", F::Text("")),
    ("location", F::Number(0.)),
    ("repeat", F::Bool(false)),
    ("isComposing", F::Bool(false)),
    ("charCode", F::Number(0.)),
    ("keyCode", F::Number(0.)),
];

const FOCUS: &[(&str, F)] = &[("view", F::Null), ("detail", F::Number(0.)), ("relatedTarget", F::Null)];

const INPUT: &[(&str, F)] = &[
    ("view", F::Null),
    ("detail", F::Number(0.)),
    ("data", F::Null),
    ("inputType", F::Text("")),
    ("isComposing", F::Bool(false)),
    ("dataTransfer", F::Null),
];

const COMPOSITION: &[(&str, F)] = &[("view", F::Null), ("detail", F::Number(0.)), ("data", F::Text(""))];

const TOUCH: &[(&str, F)] = &[
    ("view", F::Null),
    ("detail", F::Number(0.)),
    ("ctrlKey", F::Bool(false)),
    ("shiftKey", F::Bool(false)),
    ("altKey", F::Bool(false)),
    ("metaKey", F::Bool(false)),
    ("touches", F::Undefined),
    ("targetTouches", F::Undefined),
    ("changedTouches", F::Undefined),
];

/// The event classes: name, parent, fields (the parent's included), whether a global.
const CLASSES: &[(&str, &str, &[(&str, F)])] = &[
    ("UIEvent", "Event", UI),
    ("MouseEvent", "UIEvent", MOUSE),
    ("PointerEvent", "MouseEvent", POINTER),
    ("WheelEvent", "MouseEvent", WHEEL),
    ("DragEvent", "MouseEvent", DRAG),
    ("KeyboardEvent", "UIEvent", KEYBOARD),
    ("FocusEvent", "UIEvent", FOCUS),
    ("InputEvent", "UIEvent", INPUT),
    ("CompositionEvent", "UIEvent", COMPOSITION),
    ("TouchEvent", "UIEvent", TOUCH),
    ("AnimationEvent", "Event", &[("animationName", F::Text("")), ("elapsedTime", F::Number(0.)), ("pseudoElement", F::Text(""))]),
    ("TransitionEvent", "Event", &[("propertyName", F::Text("")), ("elapsedTime", F::Number(0.)), ("pseudoElement", F::Text(""))]),
    ("ClipboardEvent", "Event", &[("clipboardData", F::Null)]),
    ("SubmitEvent", "Event", &[("submitter", F::Null)]),
    ("FormDataEvent", "Event", &[("formData", F::Null)]),
    ("HashChangeEvent", "Event", &[("oldURL", F::Text("")), ("newURL", F::Text(""))]),
    ("PopStateEvent", "Event", &[("state", F::Null), ("hasUAVisualTransition", F::Bool(false))]),
    ("PageTransitionEvent", "Event", &[("persisted", F::Bool(false))]),
    ("BeforeUnloadEvent", "Event", &[]),
    ("StorageEvent", "Event", &[("key", F::Null), ("oldValue", F::Null), ("newValue", F::Null), ("url", F::Text("")), ("storageArea", F::Null)]),
    ("ToggleEvent", "Event", &[("oldState", F::Text("")), ("newState", F::Text(""))]),
    ("MediaQueryListEvent", "Event", &[("media", F::Text("")), ("matches", F::Bool(false))]),
    ("SecurityPolicyViolationEvent", "Event", &[("violatedDirective", F::Text("")), ("blockedURI", F::Text(""))]),
];

const MODIFIER_METHODS: &[Method] = &[("getModifierState", 1, |this, args, cx| {
    let key = super::arg_string(args, 0, cx)?;
    let field = match key.as_str() {
        "Control" => "ctrlKey",
        "Shift" => "shiftKey",
        "Alt" => "altKey",
        "Meta" | "OS" => "metaKey",
        _ => return Ok(false.into()),
    };
    Ok(verve::dom_events::field_value(this, field)?.to_boolean().into())
})];

pub fn install(context: &mut Context) -> JsResult<()> {
    let classes = verve::dom_events::classes(context)?;
    {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.classes.insert("EventTarget", classes.event_target.clone());
        state.classes.insert("Event", classes.event.clone());
        state.classes.insert("CustomEvent", classes.custom_event.clone());
    }
    // The runtime's own events beside those, for the document to fire too.
    for name in ["ProgressEvent", "MessageEvent", "ErrorEvent", "CloseEvent", "PromiseRejectionEvent"] {
        let constructor = context.global_object().get(JsString::from(name), context)?;
        if let Some(constructor) = constructor.as_object() {
            let prototype = constructor.get(js_string!("prototype"), context)?.as_object();
            if let Some(prototype) = prototype {
                shared(context).borrow_mut().classes.insert(name, (constructor, prototype));
            }
        }
    }
    for (name, parent, fields) in CLASSES {
        let parent = shared(context).borrow().class(parent).expect("made first");
        let methods: &[Method] = if fields.iter().any(|(f, _)| *f == "ctrlKey") {
            MODIFIER_METHODS
        } else {
            &[]
        };
        let made = verve::dom_events::define_event_class(context, name, (&parent.0, &parent.1), fields, methods)?;
        shared(context).borrow_mut().classes.insert(name, made.clone());
        context.register_global_property(
            JsString::from(*name),
            made.0,
            boa_engine::property::Attribute::WRITABLE | boa_engine::property::Attribute::CONFIGURABLE,
        )?;
    }
    // `event.x` and `event.y` are `clientX` and `clientY`.
    let mouse = shared(context).borrow().prototype("MouseEvent");
    super::extend_prototype(
        context,
        &mouse,
        &[(
            "initMouseEvent",
            15,
            |this, args, cx| {
                super::call_method(this, "initEvent", &args[..args.len().min(3)], cx)?;
                for (index, field) in [(5, "screenX"), (6, "screenY"), (7, "clientX"), (8, "clientY"), (9, "ctrlKey"), (10, "altKey"), (11, "shiftKey"), (12, "metaKey"), (13, "button"), (14, "relatedTarget")] {
                    verve::dom_events::set_field_value(this, field, arg(args, index))?;
                }
                Ok(JsValue::undefined())
            },
        )],
        &[
            ("x", Some(|this, _, _| verve::dom_events::field_value(this, "clientX")), None),
            ("y", Some(|this, _, _| verve::dom_events::field_value(this, "clientY")), None),
        ],
    )?;
    let keyboard = shared(context).borrow().prototype("KeyboardEvent");
    for (key, value) in [("DOM_KEY_LOCATION_STANDARD", 0), ("DOM_KEY_LOCATION_LEFT", 1), ("DOM_KEY_LOCATION_RIGHT", 2), ("DOM_KEY_LOCATION_NUMPAD", 3)] {
        keyboard.set(JsString::from(key), value, false, context)?;
    }
    let ui = shared(context).borrow().prototype("UIEvent");
    super::extend_prototype(
        context,
        &ui,
        &[("initUIEvent", 5, |this, args, cx| {
            super::call_method(this, "initEvent", &args[..args.len().min(3)], cx)?;
            verve::dom_events::set_field_value(this, "view", arg(args, 3))?;
            verve::dom_events::set_field_value(this, "detail", arg(args, 4))?;
            Ok(JsValue::undefined())
        })],
        &[],
    )?;
    verve::dom_events::set_event_parent(context, parent);
    Ok(())
}

/// Whether `kind` bubbles and can be cancelled when the host dispatches it.
fn defaults(kind: &str) -> (bool, bool) {
    match kind {
        "mouseenter" | "mouseleave" | "pointerenter" | "pointerleave" | "focus" | "blur"
        | "load" | "unload" | "scroll" | "scrollend" | "resize" | "toggle" | "close"
        | "play" | "pause" | "invalid" => (false, false),
        "input" | "change" | "focusin" | "focusout" | "select" | "selectionchange"
        | "compositionend" | "pointercancel" | "gotpointercapture" | "lostpointercapture" => {
            (true, false)
        }
        _ => (true, true),
    }
}

/// Dispatches a fresh event of `class` and `kind` at `target`, with `init`'s fields over the
/// defaults for its kind: what the document does itself. Answers whether nothing prevented its
/// default.
pub fn fire(
    context: &mut Context,
    target: &JsValue,
    class: &str,
    kind: &str,
    init: &[(&str, JsValue)],
) -> JsResult<bool> {
    fire_event(context, target, class, kind, init, false).map(|(allowed, _)| allowed)
}

/// [`fire`], trusted when `trusted`: for what a person did. Answers the event too.
pub fn fire_event(
    context: &mut Context,
    target: &JsValue,
    class: &str,
    kind: &str,
    init: &[(&str, JsValue)],
    trusted: bool,
) -> JsResult<(bool, JsObject)> {
    let (bubbles, cancelable) = defaults(kind);
    let dict = plain_object(context);
    dict.set(js_string!("bubbles"), bubbles, false, context)?;
    dict.set(js_string!("cancelable"), cancelable, false, context)?;
    dict.set(js_string!("composed"), true, false, context)?;
    if class != "Event" && class != "CustomEvent" {
        let view = context.global_object();
        dict.set(js_string!("view"), view, false, context)?;
    }
    for (key, value) in init {
        dict.set(JsString::from(*key), value.clone(), false, context)?;
    }
    let constructor = shared(context)
        .borrow()
        .class(class)
        .map(|(c, _)| c)
        .ok_or_else(|| super::type_error(format!("no event class {class}")))?;
    let event = constructor.construct(&[text(kind), dict.into()], None, context)?;
    verve::dom_events::set_trusted(&event.clone().into(), trusted)?;
    let global = context.global_object();
    let previous = global.get(js_string!("event"), context)?;
    global.set(js_string!("event"), event.clone(), false, context)?;
    let outcome = verve::dom_events::dispatch_value(target, &event, context);
    global.set(js_string!("event"), previous, false, context)?;
    Ok((outcome?, event))
}

/// [`fire`] at a node.
pub fn fire_at(context: &mut Context, node: NodeId, class: &str, kind: &str, init: &[(&str, JsValue)], trusted: bool) -> JsResult<bool> {
    let target = wrap(context, node);
    fire_event(context, &target, class, kind, init, trusted).map(|(allowed, _)| allowed)
}

/// Whether `id` can take the focus: a field, a link, something with a `tabindex`, an editable
/// region — enabled, and in the document.
pub fn focusable(context: &Context, id: NodeId) -> bool {
    read_dom(context, |dom| {
        let Some(node) = dom.get(id) else { return false };
        if !dom.is_connected(id) || !node.is_element() {
            return false;
        }
        if node.attr("tabindex").is_some() || node.attr("contenteditable").is_some_and(|v| v != "false") {
            return true;
        }
        match node.tag() {
            Some("input") => node.attr("type").is_none_or(|t| !t.eq_ignore_ascii_case("hidden")) && node.attr("disabled").is_none(),
            Some("select" | "textarea" | "button") => node.attr("disabled").is_none(),
            Some("a" | "area") => node.attr("href").is_some(),
            Some("iframe" | "summary") => true,
            Some("audio" | "video") => node.attr("controls").is_some(),
            _ => false,
        }
    })
}

/// Focus moves to `id` (or away, with `None`), with the events that says.
pub fn set_focus(context: &mut Context, id: Option<NodeId>) -> JsResult<()> {
    set_focus_from(context, id, false)
}

pub fn set_focus_from(context: &mut Context, id: Option<NodeId>, trusted: bool) -> JsResult<()> {
    let before = read_dom(context, |dom| dom.focused);
    if before == id {
        return Ok(());
    }
    with_dom(context, |dom| {
        dom.focused = id;
        for node in [before, id].into_iter().flatten() {
            if dom.exists(node) {
                dom.mark(node);
            }
        }
    });
    let related = |other: Option<NodeId>, cx: &mut Context| other.map_or(JsValue::null(), |o| wrap(cx, o));
    if let Some(before) = before.filter(|b| read_dom(context, |dom| dom.exists(*b))) {
        let other = related(id, context);
        fire_at(context, before, "FocusEvent", "blur", &[("relatedTarget", other.clone())], trusted)?;
        fire_at(context, before, "FocusEvent", "focusout", &[("relatedTarget", other)], trusted)?;
    }
    if let Some(id) = id {
        let other = related(before, context);
        fire_at(context, id, "FocusEvent", "focus", &[("relatedTarget", other.clone())], trusted)?;
        fire_at(context, id, "FocusEvent", "focusin", &[("relatedTarget", other)], trusted)?;
    }
    Ok(())
}

/// Sets a checkbox's or radio button's `checked`, the others of a radio's group unchecked.
pub fn set_checked(context: &mut Context, id: NodeId, on: bool) {
    with_dom(context, |dom| {
        let radio = dom.get(id).is_some_and(|n| n.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("radio")));
        if let Some(node) = dom.get_mut(id) {
            node.checked = on;
            node.dirty_checked = true;
        }
        if radio && on {
            for other in radio_group(dom, id) {
                if other != id
                    && let Some(node) = dom.get_mut(other)
                {
                    node.checked = false;
                    node.dirty_checked = true;
                }
            }
        }
    });
}

/// The radio buttons of `id`'s group: the same `name`, in the same form (or none).
fn radio_group(dom: &crate::dom::Dom, id: NodeId) -> Vec<NodeId> {
    let Some(name) = dom.get_attr(id, "name").filter(|n| !n.is_empty()) else {
        return vec![id];
    };
    let form = |n: NodeId| {
        let mut at = dom.parent(n);
        while let Some(p) = at {
            if dom.get(p).and_then(|x| x.tag()) == Some("form") {
                return Some(p);
            }
            at = dom.parent(p);
        }
        None
    };
    let own_form = form(id);
    let root = own_form.unwrap_or_else(|| dom.root_of(id));
    dom.descendants(root)
        .into_iter()
        .filter(|n| {
            dom.get(*n).is_some_and(|x| {
                x.tag() == Some("input")
                    && x.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("radio"))
                    && x.attr("name") == Some(name.as_str())
            }) && form(*n) == own_form
        })
        .collect()
}

/// A click on `id`: the `click` the DOM dispatches, with its activation behaviour — a box
/// ticked first (and untouched again if the click was prevented), then its `input` and
/// `change`; a label pressing its control; a submit button submitting its form; a `<summary>`
/// opening its `<details>`.
pub fn click(context: &mut Context, id: NodeId, props: &[(&str, JsValue)], trusted: bool) -> JsResult<bool> {
    let disabled = read_dom(context, |dom| {
        dom.get(id).is_some_and(|n| {
            matches!(n.tag(), Some("input" | "button" | "select" | "textarea")) && n.attr("disabled").is_some()
        })
    });
    if disabled {
        return Ok(false);
    }
    // The element whose activation behaviour runs: the target, or what it is inside.
    let (activator, kind) = read_dom(context, |dom| {
        let mut at = Some(id);
        while let Some(node) = at {
            let n = dom.get(node);
            let kind = match n.and_then(|n| n.tag()) {
                Some("input") => match crate::bind::element::input_type(dom, node).as_str() {
                    "checkbox" => Some("checkbox"),
                    "radio" => Some("radio"),
                    "submit" | "image" => Some("submit"),
                    "reset" => Some("reset"),
                    _ => None,
                },
                Some("button") => match dom.get_attr(node, "type").map(|t| t.to_ascii_lowercase()).as_deref() {
                    Some("button") => None,
                    Some("reset") => Some("reset"),
                    _ => Some("submit"),
                },
                Some("label") => Some("label"),
                Some("summary") => Some("summary"),
                Some("a") if n.is_some_and(|n| n.attr("href").is_some()) => Some("link"),
                _ => None,
            };
            if kind.is_some() {
                return (Some(node), kind);
            }
            at = dom.parent(node);
        }
        (None, None)
    });
    // A checkbox's or radio's tick, before the click is heard.
    let before = match kind {
        Some("checkbox" | "radio") => {
            let node = activator.expect("found");
            let was = read_dom(context, |dom| {
                let group: Vec<(NodeId, bool)> = if kind == Some("radio") {
                    radio_group(dom, node).into_iter().map(|n| (n, dom.get(n).is_some_and(|x| x.checked))).collect()
                } else {
                    vec![(node, dom.get(node).is_some_and(|x| x.checked))]
                };
                group
            });
            let on = if kind == Some("radio") { true } else { !was.iter().find(|(n, _)| *n == node).is_some_and(|(_, c)| *c) };
            set_checked(context, node, on);
            Some(was)
        }
        _ => None,
    };
    let mut init: Vec<(&str, JsValue)> = vec![("detail", 1.into())];
    init.extend(props.iter().cloned());
    let allowed = fire_at(context, id, "PointerEvent", "click", &init, trusted)?;
    let Some(activator) = activator else {
        return Ok(allowed);
    };
    match kind {
        Some("checkbox" | "radio") => {
            let was = before.unwrap_or_default();
            if !allowed {
                // Prevented: as it was.
                with_dom(context, |dom| {
                    for (node, checked) in &was {
                        if let Some(n) = dom.get_mut(*node) {
                            n.checked = *checked;
                        }
                    }
                });
            } else {
                let changed = read_dom(context, |dom| dom.get(activator).is_some_and(|n| was.iter().find(|(x, _)| *x == activator).is_none_or(|(_, c)| *c != n.checked)));
                if changed {
                    fire_at(context, activator, "Event", "input", &[], trusted)?;
                    fire_at(context, activator, "Event", "change", &[], trusted)?;
                }
            }
        }
        _ if !allowed => {}
        Some("submit") => {
            if read_dom(context, |dom| dom.is_connected(activator)) {
                let form = form_of(context, activator);
                if let Some(form) = form {
                    submit(context, form, Some(activator))?;
                }
            }
        }
        Some("reset") => {
            if let Some(form) = form_of(context, activator) {
                let form = wrap(context, form);
                super::call_method(&form, "reset", &[], context)?;
            }
        }
        Some("label") => {
            let control = read_dom(context, |dom| crate::bind::element::label_control(dom, activator));
            // A click already on the control (or inside it) is the control's own.
            if let Some(control) = control.filter(|c| !read_dom(context, |dom| dom.contains(*c, id))) {
                if focusable(context, control) {
                    set_focus_from(context, Some(control), trusted)?;
                }
                click(context, control, props, trusted)?;
            }
        }
        Some("summary") => {
            let details = read_dom(context, |dom| dom.parent(activator).filter(|p| dom.get(*p).and_then(|n| n.tag()) == Some("details")));
            if let Some(details) = details {
                with_dom(context, |dom| {
                    if dom.get_attr(details, "open").is_some() {
                        dom.remove_attr(details, "open")
                    } else {
                        dom.set_attr(details, "open", "")
                    }
                });
                fire_at(context, details, "Event", "toggle", &[], trusted)?;
            }
        }
        Some("link") => {
            let href = read_dom(context, |dom| dom.get_attr(activator, "href")).unwrap_or_default();
            let resolved = shared(context).borrow().url.join(&href).map(|u| u.to_string()).unwrap_or(href);
            super::window::navigate(context, &resolved, false)?;
        }
        _ => {}
    }
    Ok(allowed)
}

fn form_of(context: &Context, id: NodeId) -> Option<NodeId> {
    read_dom(context, |dom| {
        if let Some(named) = dom.get_attr(id, "form") {
            return dom.descendants(dom.document).into_iter().find(|n| dom.get_attr(*n, "id").as_deref() == Some(named.as_str()));
        }
        let mut at = Some(id);
        while let Some(node) = at {
            if dom.get(node).and_then(|n| n.tag()) == Some("form") {
                return Some(node);
            }
            at = dom.parent(node);
        }
        None
    })
}

/// The form submitted: its `submit` event, and — when nothing prevented it — the submission,
/// which the drawer hears as a [`crate::Request::Submit`].
pub fn submit(context: &mut Context, form: NodeId, submitter: Option<NodeId>) -> JsResult<()> {
    let submitter = submitter.map_or(JsValue::null(), |s| wrap(context, s));
    let allowed = fire_at(context, form, "SubmitEvent", "submit", &[("submitter", submitter)], false)?;
    if allowed {
        shared(context).borrow_mut().requests.push(crate::Request::Submit(form));
    }
    Ok(())
}

/// The events with an `on…` property on elements, the document and the window.
pub const HANDLER_EVENTS: &[&str] = &[
    "abort", "animationcancel", "animationend", "animationiteration", "animationstart",
    "auxclick", "beforeinput", "beforetoggle", "blur", "cancel", "canplay", "canplaythrough",
    "change", "click", "close", "contextmenu", "copy", "cuechange", "cut", "dblclick", "drag",
    "dragend", "dragenter", "dragleave", "dragover", "dragstart", "drop", "durationchange",
    "emptied", "ended", "error", "focus", "focusin", "focusout", "formdata",
    "fullscreenchange", "gotpointercapture", "input", "invalid", "keydown", "keypress", "keyup",
    "load", "loadeddata", "loadedmetadata", "loadstart", "lostpointercapture", "mousedown",
    "mouseenter", "mouseleave", "mousemove", "mouseout", "mouseover", "mouseup", "paste",
    "pause", "play", "playing", "pointercancel", "pointerdown", "pointerenter", "pointerleave",
    "pointermove", "pointerout", "pointerover", "pointerup", "progress", "ratechange", "reset",
    "resize", "scroll", "scrollend", "securitypolicyviolation", "seeked", "seeking", "select",
    "selectionchange", "selectstart", "slotchange", "stalled", "submit", "suspend",
    "timeupdate", "toggle", "touchcancel", "touchend", "touchmove", "touchstart",
    "transitioncancel", "transitionend", "transitionrun", "transitionstart", "volumechange",
    "waiting", "wheel", "compositionstart", "compositionupdate", "compositionend",
];

/// What the window has beyond the elements' handlers.
pub const WINDOW_EVENTS: &[&str] = &[
    "afterprint", "beforeprint", "beforeunload", "hashchange", "languagechange", "message",
    "messageerror", "offline", "online", "pagehide", "pageshow", "popstate",
    "rejectionhandled", "storage", "unhandledrejection", "unload",
];

/// Puts the handler properties on the element and document prototypes and the window.
pub fn install_handlers(context: &mut Context) -> JsResult<()> {
    for owner in ["HTMLElement", "SVGElement", "MathMLElement", "Document"] {
        let prototype = shared(context).borrow().prototype(owner);
        verve::dom_events::handler_accessors(context, &prototype, HANDLER_EVENTS)?;
    }
    let window = shared(context).borrow().prototype("Window");
    verve::dom_events::handler_accessors(context, &window, HANDLER_EVENTS)?;
    verve::dom_events::handler_accessors(context, &window, WINDOW_EVENTS)?;
    let _ = MODIFIERS;
    let _ = NodeKind::Fragment;
    Ok(())
}
