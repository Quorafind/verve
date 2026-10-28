//! `MutationObserver`, `ResizeObserver` and `IntersectionObserver`.
//!
//! - A mutation observer hears what changed in what it watches as the DOM records it — child
//!   lists with the nodes added and removed and their siblings, attributes with their old
//!   values, text — in a microtask after the change, as in a browser.
//! - A resize observer hears a target whenever the box the drawer painted it in changes size
//!   (see [`crate::Document::set_layout`]), and once at first unless it has no size.
//! - An intersection observer hears a target once at first and whenever it comes into sight
//!   or leaves it: its painted box against the viewport, or against its `root`'s box.
//!
//! Both of the latter are delivered when the drawer reports where it painted things, and after
//! each animation frame.

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};

use super::node::{node_list, read_dom, wrap, wrap_opt};
use super::{
    ClassSpec, Kind, Method, arg, array, class, get, handle_of, instance, list, node_of,
    plain_object, set, string, text, type_error,
};
use crate::Rect;
use crate::dom::{Mutation, NodeId};
use crate::state::{MutationOptions, ObservedTarget, ObserverRecord, Watch, shared};

fn construct(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    watch: Watch,
) -> JsResult<JsValue> {
    let callback = arg(args, 0);
    if !callback.is_callable() {
        return Err(type_error(
            "Failed to construct an observer: The callback provided as parameter 1 is not a function.",
        ));
    }
    let id = shared(context).borrow_mut().id();
    let object = instance(new_target, Kind::Plain, id, context)?;
    if watch == Watch::Intersection {
        let options = arg(args, 1);
        let root = get(&options, "root", context)?;
        set(
            &object,
            "root",
            if root.is_undefined() {
                JsValue::null()
            } else {
                root
            },
            context,
        )?;
        let margin = get(&options, "rootMargin", context)?;
        set(
            &object,
            "rootMargin",
            if margin.is_undefined() {
                text("0px 0px 0px 0px")
            } else {
                margin
            },
            context,
        )?;
        let threshold = get(&options, "threshold", context)?;
        let thresholds = if threshold.is_undefined() {
            array(vec![0.into()], context)
        } else if threshold.is_object() {
            threshold
        } else {
            array(vec![threshold], context)
        };
        set(&object, "thresholds", thresholds, context)?;
    }
    // Kept on the observer too: the host lets go of an observer watching nothing, and takes it
    // back from here when it watches again.
    super::hidden(&object, js_string!("__callback"), callback.clone(), context)?;
    super::hidden(&object, js_string!("__watch"), watch as u32, context)?;
    Ok(object.into())
}

fn watch_of(code: u32) -> Watch {
    match code {
        0 => Watch::Resize,
        1 => Watch::Mutation,
        _ => Watch::Intersection,
    }
}

/// The observer's record, made again if the host let go of it while it watched nothing.
fn record_id(this: &JsValue, context: &mut Context) -> JsResult<u32> {
    let id = handle_of(this, Kind::Plain)
        .ok_or_else(|| type_error("Illegal invocation: not an observer"))?;
    let known = shared(context)
        .borrow()
        .observers
        .iter()
        .any(|o| o.id == id);
    if !known {
        let watch = watch_of(get(this, "__watch", context)?.to_u32(context)?);
        let object = this.as_object().expect("an observer");
        shared(context).borrow_mut().observers.push(ObserverRecord {
            id,
            watch,
            object: object.downgrade(),
            kept: Vec::new(),
            transient: Vec::new(),
            targets: Vec::new(),
            records: Vec::new(),
        });
    }
    Ok(id)
}

fn with_record<R>(
    id: u32,
    context: &Context,
    f: impl FnOnce(&mut ObserverRecord) -> R,
) -> Option<R> {
    let state = shared(context);
    let mut state = state.borrow_mut();
    state.observers.iter_mut().find(|o| o.id == id).map(f)
}

/// An observer watching nothing is not the host's to keep: it lives as long as a script holds
/// it, and what its callback holds with it.
fn let_go_if_idle(id: u32, context: &Context) {
    let state = shared(context);
    let mut state = state.borrow_mut();
    state
        .observers
        .retain(|o| o.id != id || !o.targets.is_empty() || !o.records.is_empty());
    let recording = state
        .observers
        .iter()
        .any(|o| o.watch == Watch::Mutation && !o.targets.is_empty());
    state.dom.record_mutations = recording;
}

fn release_target(context: &Context, node: NodeId, observer: &JsValue) {
    if let (Some(target), Some(observer)) = (
        super::node::existing_wrapper(context, node),
        observer.as_object(),
    ) {
        verve::dom_events::hold(&target, |held| held.retain(|object| object != &observer));
    }
}

fn flag(options: &JsValue, name: &str, context: &mut Context) -> JsResult<Option<bool>> {
    let value = get(options, name, context)?;
    Ok((!value.is_undefined()).then(|| value.to_boolean()))
}

fn mutation_options(options: &JsValue, context: &mut Context) -> JsResult<MutationOptions> {
    let filter = get(options, "attributeFilter", context)?;
    let filter = if filter.is_undefined() {
        None
    } else {
        Some(
            list(&filter, context)?
                .iter()
                .map(|v| string(v, context))
                .collect::<JsResult<Vec<_>>>()?,
        )
    };
    let old_attributes = flag(options, "attributeOldValue", context)?;
    let old_data = flag(options, "characterDataOldValue", context)?;
    let attributes = flag(options, "attributes", context)?
        .unwrap_or(filter.is_some() || old_attributes == Some(true));
    let character_data = flag(options, "characterData", context)?.unwrap_or(old_data == Some(true));
    let child_list = flag(options, "childList", context)?.unwrap_or(false);
    if !child_list && !attributes && !character_data {
        return Err(type_error(
            "Failed to execute 'observe' on 'MutationObserver': The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.",
        ));
    }
    Ok(MutationOptions {
        child_list,
        attributes,
        character_data,
        subtree: flag(options, "subtree", context)?.unwrap_or(false),
        attribute_old_value: old_attributes.unwrap_or(false),
        character_data_old_value: old_data.unwrap_or(false),
        attribute_filter: filter,
    })
}

const OBSERVER_METHODS: &[Method] = &[
    ("observe", 2, |this, args, cx| {
        distribute(cx);
        let node = super::node_arg(args, 0, "observe", "Observer")?;
        let id = record_id(this, cx)?;
        let watch = watch_of(get(this, "__watch", cx)?.to_u32(cx)?);
        let options = if watch == Watch::Mutation {
            mutation_options(&arg(args, 1), cx)?
        } else {
            MutationOptions::default()
        };
        with_record(id, cx, |record| {
            record.targets.retain(|t| t.node != node);
            record.targets.push(ObservedTarget {
                node,
                // A box with no size is not heard about at first.
                last: Some([0.; 4]),
                intersecting: None,
                options,
            });
        });
        if let (Some(target), Some(observer)) = (wrap(cx, node).as_object(), this.as_object()) {
            verve::dom_events::hold(&target, |held| {
                if !held.contains(&observer) {
                    held.push(observer);
                }
            });
        }
        if watch == Watch::Mutation {
            shared(cx).borrow_mut().dom.record_mutations = true;
        } else {
            // Heard at the next frame.
            shared(cx).borrow_mut().requests.push(crate::Request::Frame);
            schedule_layout_delivery(cx);
        }
        Ok(JsValue::undefined())
    }),
    ("unobserve", 1, |this, args, cx| {
        distribute(cx);
        let node = node_of(&arg(args, 0))?;
        let id = record_id(this, cx)?;
        release_target(cx, node, this);
        with_record(id, cx, |record| record.targets.retain(|t| t.node != node));
        let_go_if_idle(id, cx);
        Ok(JsValue::undefined())
    }),
    ("disconnect", 0, |this, _, cx| {
        distribute(cx);
        let id = record_id(this, cx)?;
        let targets = with_record(id, cx, |record| {
            record.targets.iter().map(|t| t.node).collect::<Vec<_>>()
        })
        .unwrap_or_default();
        for node in targets {
            release_target(cx, node, this);
        }
        with_record(id, cx, |record| {
            record.targets.clear();
            record.records.clear();
            record.transient.clear();
            record.kept.clear();
        });
        let_go_if_idle(id, cx);
        Ok(JsValue::undefined())
    }),
    ("takeRecords", 0, |this, _, cx| {
        let id = record_id(this, cx)?;
        // What the document recorded and has not yet handed out is this observer's too.
        distribute(cx);
        let (records, _kept) = with_record(id, cx, |record| {
            (
                std::mem::take(&mut record.records),
                std::mem::take(&mut record.kept),
            )
        })
        .unwrap_or_default();
        let mut out = Vec::new();
        for mutation in records {
            out.push(mutation_record(cx, &mutation)?);
        }
        Ok(array(out, cx))
    }),
];

// ─── Mutations ──────────────────────────────────────────────────────────────────────────────

/// Queues the delivery of the mutations recorded, a microtask from now.
pub fn queue_delivery(context: &mut Context) {
    shared(context).borrow_mut().mutation_queued = true;
    let deliver = super::function(context, "deliver", 0, |_, _, cx| {
        deliver_mutations(cx)?;
        Ok(JsValue::undefined())
    });
    let queue = context
        .global_object()
        .get(js_string!("queueMicrotask"), context);
    if let Ok(queue) = queue {
        let _ = verve::call(context, &queue, &JsValue::undefined(), &[deliver.into()]);
    }
}

/// Whether `observer` (with `options` on `target`) hears `mutation`.
fn hears(path: &[NodeId], target: NodeId, options: &MutationOptions, mutation: &Mutation) -> bool {
    let at = mutation.target();
    let reaches = at == target || (options.subtree && path.contains(&target));
    if !reaches {
        return false;
    }
    match mutation {
        Mutation::ChildList { .. } => options.child_list,
        Mutation::Attributes { name, .. } => {
            options.attributes
                && options
                    .attribute_filter
                    .as_ref()
                    .is_none_or(|f| f.iter().any(|n| n == name))
        }
        Mutation::CharacterData { .. } => options.character_data,
    }
}

/// Hands the document's recorded mutations to the observers that hear them.
pub(crate) fn distribute(context: &mut Context) {
    let mut keep = Vec::new();
    {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let mutations = state.dom.take_mutation_paths();
        for (mutation, path) in &mutations {
            for observer in state
                .observers
                .iter_mut()
                .filter(|o| o.watch == Watch::Mutation)
            {
                let options: Vec<MutationOptions> = observer
                    .targets
                    .iter()
                    .map(|t| (t.node, &t.options))
                    .chain(
                        observer
                            .transient
                            .iter()
                            .map(|(node, options)| (*node, options)),
                    )
                    .filter(|(target, options)| hears(path, *target, options, mutation))
                    .map(|(_, options)| options.clone())
                    .collect();
                // 移除后的子树在本次交付前仍受原 subtree 注册观察，不要求观察 childList。
                if let Mutation::ChildList { removed, .. } = mutation {
                    let inherited: Vec<MutationOptions> = observer
                        .targets
                        .iter()
                        .map(|t| (t.node, &t.options))
                        .chain(
                            observer
                                .transient
                                .iter()
                                .map(|(node, options)| (*node, options)),
                        )
                        .filter(|(target, options)| options.subtree && path.contains(target))
                        .map(|(_, options)| options.clone())
                        .collect();
                    for removed in removed {
                        for options in &inherited {
                            if !observer
                                .transient
                                .iter()
                                .any(|(node, known)| node == removed && known == options)
                            {
                                observer.transient.push((*removed, options.clone()));
                            }
                        }
                    }
                }
                if options.is_empty() {
                    continue;
                }
                let mut kept = mutation.clone();
                match &mut kept {
                    Mutation::Attributes { old, .. }
                        if !options.iter().any(|o| o.attribute_old_value) =>
                    {
                        *old = None
                    }
                    Mutation::CharacterData { old, .. }
                        if !options.iter().any(|o| o.character_data_old_value) =>
                    {
                        old.clear()
                    }
                    _ => {}
                }
                let mut nodes = vec![kept.target()];
                if let Mutation::ChildList {
                    added,
                    removed,
                    previous,
                    next,
                    ..
                } = &kept
                {
                    nodes.extend(added.iter().chain(removed.iter()).copied());
                    nodes.extend(previous.iter().chain(next.iter()).copied());
                }
                observer.records.push(kept);
                keep.push((observer.id, observer.object.upgrade(), nodes));
            }
        }
    }
    // 只有尚未交付的记录由宿主强持有；普通注册的保活边在目标 JS 对象上，可被 GC 一起回收。
    for (id, observer, nodes) in keep {
        let mut objects: Vec<JsObject> = observer.into_iter().collect();
        objects.extend(
            nodes
                .into_iter()
                .filter_map(|node| wrap(context, node).as_object()),
        );
        with_record(id, context, |record| record.kept.extend(objects));
    }
}

fn mutation_record(context: &mut Context, mutation: &Mutation) -> JsResult<JsValue> {
    let record = plain_object(context);
    let prototype = shared(context).borrow().prototype("MutationRecord");
    record.set_prototype(Some(prototype));
    let target = wrap(context, mutation.target());
    set(&record, "target", target, context)?;
    let empty = node_list(Vec::new(), context);
    match mutation {
        Mutation::ChildList {
            added,
            removed,
            previous,
            next,
            ..
        } => {
            set(&record, "type", text("childList"), context)?;
            let added = node_list(added.clone(), context);
            set(&record, "addedNodes", added, context)?;
            let removed = node_list(removed.clone(), context);
            set(&record, "removedNodes", removed, context)?;
            let previous = wrap_opt(context, *previous);
            set(&record, "previousSibling", previous, context)?;
            let next = wrap_opt(context, *next);
            set(&record, "nextSibling", next, context)?;
            set(&record, "attributeName", JsValue::null(), context)?;
            set(&record, "oldValue", JsValue::null(), context)?;
        }
        Mutation::Attributes { name, old, .. } => {
            set(&record, "type", text("attributes"), context)?;
            set(&record, "addedNodes", empty.clone(), context)?;
            set(&record, "removedNodes", empty, context)?;
            set(&record, "previousSibling", JsValue::null(), context)?;
            set(&record, "nextSibling", JsValue::null(), context)?;
            set(&record, "attributeName", text(name), context)?;
            set(
                &record,
                "oldValue",
                old.as_deref().map_or(JsValue::null(), text),
                context,
            )?;
        }
        Mutation::CharacterData { old, .. } => {
            set(&record, "type", text("characterData"), context)?;
            set(&record, "addedNodes", empty.clone(), context)?;
            set(&record, "removedNodes", empty, context)?;
            set(&record, "previousSibling", JsValue::null(), context)?;
            set(&record, "nextSibling", JsValue::null(), context)?;
            set(&record, "attributeName", JsValue::null(), context)?;
            set(
                &record,
                "oldValue",
                if old.is_empty() {
                    JsValue::null()
                } else {
                    text(old)
                },
                context,
            )?;
        }
    }
    set(&record, "attributeNamespace", JsValue::null(), context)?;
    Ok(record.into())
}

/// The microtask: every mutation observer with records hears them, in the order they were
/// made.
pub fn deliver_mutations(context: &mut Context) -> JsResult<()> {
    shared(context).borrow_mut().mutation_queued = false;
    distribute(context);
    let due: Vec<u32> = shared(context)
        .borrow()
        .observers
        .iter()
        .filter(|o| o.watch == Watch::Mutation)
        .map(|o| o.id)
        .collect();
    for id in due {
        let record = with_record(id, context, |o| {
            o.transient.clear();
            o.object.upgrade().map(|object| {
                (
                    object,
                    std::mem::take(&mut o.records),
                    std::mem::take(&mut o.kept),
                )
            })
        })
        .flatten();
        let Some((observer, mutations, _kept)) = record else {
            continue;
        };
        let callback = observer.get(js_string!("__callback"), context)?;
        let mut records = Vec::new();
        for mutation in &mutations {
            // A record about a node freed since says nothing.
            if read_dom(context, |dom| dom.exists(mutation.target())) {
                records.push(mutation_record(context, mutation)?);
            }
        }
        if records.is_empty() {
            continue;
        }
        let list = array(records, context);
        if let Err(error) = verve::call(
            context,
            &callback,
            &observer.clone().into(),
            &[list, observer.into()],
        ) {
            verve::report(context, &error);
        }
    }
    Ok(())
}

// ─── Resize and intersection ────────────────────────────────────────────────────────────────

/// On a realm nothing draws, the next frame's delivery is a timer's.
fn schedule_layout_delivery(context: &mut Context) {
    if shared(context).borrow().frame_clock != crate::state::FrameClock::Timer {
        return;
    }
    let deliver = super::function(context, "observe", 0, |_, _, cx| {
        deliver_layout(cx)?;
        Ok(JsValue::undefined())
    });
    verve::set_timeout(context, deliver.into(), 16., false, Vec::new());
}

fn rect_object(context: &mut Context, rect: Rect) -> JsResult<JsValue> {
    super::platform::make_rect(
        context,
        [
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.width),
            f64::from(rect.height),
        ],
    )
}

fn size_object(context: &mut Context, width: f32, height: f32) -> JsResult<JsValue> {
    let size = plain_object(context);
    set(&size, "inlineSize", f64::from(width), context)?;
    set(&size, "blockSize", f64::from(height), context)?;
    Ok(size.into())
}

/// Every resize and intersection observer with something to say hears it. Answers whether any
/// did.
pub fn deliver_layout(context: &mut Context) -> JsResult<bool> {
    let observers: Vec<(u32, Watch)> = shared(context)
        .borrow()
        .observers
        .iter()
        .filter(|o| o.watch != Watch::Mutation && !o.targets.is_empty())
        .map(|o| (o.id, o.watch))
        .collect();
    let mut told = false;
    for (id, watch) in observers {
        let entries = match watch {
            Watch::Resize => resize_entries(context, id)?,
            _ => intersection_entries(context, id)?,
        };
        if entries.is_empty() {
            continue;
        }
        let found = with_record(id, context, |o| o.object.upgrade()).flatten();
        if let Some(object) = found {
            let callback = object.get(js_string!("__callback"), context)?;
            told = true;
            let list = array(entries, context);
            if let Err(error) = verve::call(
                context,
                &callback,
                &object.clone().into(),
                &[list, object.into()],
            ) {
                verve::report(context, &error);
            }
        }
    }
    Ok(told)
}

fn resize_entries(context: &mut Context, id: u32) -> JsResult<Vec<JsValue>> {
    let targets: Vec<(NodeId, Option<[f32; 4]>)> = with_record(id, context, |o| {
        o.targets.iter().map(|t| (t.node, t.last)).collect()
    })
    .unwrap_or_default();
    let mut out = Vec::new();
    for (node, last) in targets {
        if !read_dom(context, |dom| dom.exists(node)) {
            continue;
        }
        let now = super::element::painted(context, node);
        if last.is_some_and(|l| l[2] == now.width && l[3] == now.height) {
            continue;
        }
        with_record(id, context, |o| {
            if let Some(t) = o.targets.iter_mut().find(|t| t.node == node) {
                t.last = Some([now.x, now.y, now.width, now.height]);
            }
        });
        let entry = plain_object(context);
        let prototype = shared(context).borrow().prototype("ResizeObserverEntry");
        entry.set_prototype(Some(prototype));
        let target = wrap(context, node);
        set(&entry, "target", target, context)?;
        let rect = rect_object(
            context,
            Rect {
                x: 0.,
                y: 0.,
                width: now.width,
                height: now.height,
            },
        )?;
        set(&entry, "contentRect", rect, context)?;
        for key in [
            "borderBoxSize",
            "contentBoxSize",
            "devicePixelContentBoxSize",
        ] {
            let size = size_object(context, now.width, now.height)?;
            let sizes = array(vec![size], context);
            set(&entry, key, sizes, context)?;
        }
        out.push(entry.into());
    }
    Ok(out)
}

fn intersection_entries(context: &mut Context, id: u32) -> JsResult<Vec<JsValue>> {
    let (targets, object): (Vec<(NodeId, Option<bool>)>, Option<JsObject>) =
        with_record(id, context, |o| {
            (
                o.targets.iter().map(|t| (t.node, t.intersecting)).collect(),
                o.object.upgrade(),
            )
        })
        .unwrap_or_default();
    let root = match &object {
        Some(object) => super::node_id(&object.get(js_string!("root"), context)?),
        None => None,
    };
    let bounds = match root {
        Some(root) => super::element::painted(context, root),
        None => {
            let env = shared(context).borrow().env.clone();
            Rect {
                x: 0.,
                y: 0.,
                width: env.width,
                height: env.height,
            }
        }
    };
    let mut out = Vec::new();
    for (node, last) in targets {
        if !read_dom(context, |dom| dom.exists(node)) {
            continue;
        }
        let rect = super::element::painted(context, node);
        let connected = read_dom(context, |dom| dom.is_connected(node));
        let overlap = rect.intersection(&bounds);
        let intersecting = connected && overlap.is_some();
        if last == Some(intersecting) {
            continue;
        }
        with_record(id, context, |o| {
            if let Some(t) = o.targets.iter_mut().find(|t| t.node == node) {
                t.intersecting = Some(intersecting);
            }
        });
        let area = rect.width * rect.height;
        let shown = overlap.unwrap_or_default();
        let ratio = if !intersecting {
            0.
        } else if area > 0. {
            (shown.width * shown.height / area).min(1.)
        } else {
            1.
        };
        let entry = plain_object(context);
        let prototype = shared(context)
            .borrow()
            .prototype("IntersectionObserverEntry");
        entry.set_prototype(Some(prototype));
        let target = wrap(context, node);
        set(&entry, "target", target, context)?;
        set(&entry, "isIntersecting", intersecting, context)?;
        set(&entry, "isVisible", intersecting, context)?;
        set(&entry, "intersectionRatio", f64::from(ratio), context)?;
        let bounding = rect_object(context, rect)?;
        set(&entry, "boundingClientRect", bounding, context)?;
        let intersection = rect_object(context, shown)?;
        set(&entry, "intersectionRect", intersection, context)?;
        let root_bounds = rect_object(context, bounds)?;
        set(&entry, "rootBounds", root_bounds, context)?;
        set(&entry, "time", verve::now(context), context)?;
        out.push(entry.into());
    }
    Ok(out)
}

/// Whether any observer of the layout is watching anything.
pub fn watching_layout(context: &Context) -> bool {
    shared(context)
        .borrow()
        .observers
        .iter()
        .any(|o| o.watch != Watch::Mutation && !o.targets.is_empty())
}

pub fn install(context: &mut Context) -> JsResult<()> {
    for (name, construct_fn) in [
        (
            "ResizeObserver",
            (|t, a, c| construct(t, a, c, Watch::Resize))
                as boa_engine::native_function::NativeFunctionPointer,
        ),
        ("MutationObserver", |t, a, c| {
            construct(t, a, c, Watch::Mutation)
        }),
        ("IntersectionObserver", |t, a, c| {
            construct(t, a, c, Watch::Intersection)
        }),
    ] {
        class(
            context,
            ClassSpec {
                name,
                length: 1,
                construct: construct_fn,
                parent: None,
                methods: OBSERVER_METHODS,
                accessors: &[],
                statics: &[],
                global: true,
            },
        )?;
    }
    context.register_global_property(
        js_string!("WebKitMutationObserver"),
        shared(context)
            .borrow()
            .class("MutationObserver")
            .expect("made")
            .0,
        boa_engine::property::Attribute::WRITABLE | boa_engine::property::Attribute::CONFIGURABLE,
    )?;
    for name in [
        "MutationRecord",
        "ResizeObserverEntry",
        "IntersectionObserverEntry",
        "ResizeObserverSize",
    ] {
        class(
            context,
            ClassSpec {
                name,
                length: 0,
                construct: super::illegal_constructor,
                parent: None,
                methods: &[],
                accessors: &[],
                statics: &[],
                global: true,
            },
        )?;
    }
    let _ = JsString::from("");
    Ok(())
}
