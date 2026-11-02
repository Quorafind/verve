//! What a host with a tree tells a dispatch: where each target sits in it ([`EventParent`]),
//! and what each scope of it — a shadow tree — hides from the listeners outside ([`EventScope`]).

use boa_engine::{Context, JsObject, JsResult, JsValue};

use super::{EventData, field, set_field};

/// What a host with a tree says an event target is in: the next target on an event's path
/// (a node's parent; the window, for its document), or `None` at the top. See
/// [`set_event_parent`].
pub type EventParent =
    fn(target: &JsValue, event: &JsObject, context: &mut Context) -> JsResult<Option<JsValue>>;

/// How a host whose targets nest scopes (a shadow tree) keeps what is inside one from a
/// listener outside it. See [`set_event_scope`].
#[derive(Clone, Copy)]
pub struct EventScope {
    /// What a listener on `node` sees the event's `target` as: the target itself, or the
    /// outermost thing of it that `node` can see.
    pub retarget: fn(node: &JsValue, target: &JsValue, context: &mut Context) -> JsValue,
    /// Whether `composedPath()` called from a listener on `node` lists `item` of the path.
    pub visible: fn(node: &JsValue, item: &JsValue, context: &mut Context) -> bool,
}

#[derive(Clone, Copy)]
struct ParentHook(EventParent);

#[derive(Clone, Copy)]
struct ScopeHook(EventScope);

/// Tells every dispatch from now on what each target is in: see [`EventParent`].
pub fn set_event_parent(context: &mut Context, parent: EventParent) {
    context.insert_data(ParentHook(parent));
}

/// Tells every dispatch from now on what each scope hides from the listeners outside it.
pub fn set_event_scope(context: &mut Context, scope: EventScope) {
    context.insert_data(ScopeHook(scope));
}

/// Whether `event` crosses the edge of a shadow tree (`composed`).
pub fn is_composed(event: &JsObject) -> bool {
    event
        .downcast_ref::<EventData>()
        .is_some_and(|data| data.0.borrow().composed)
}

/// The targets an event dispatched at `target` goes through, target first, as the host's
/// [`EventParent`] has them.
pub(super) fn path_of(
    target: &JsValue,
    event: &JsObject,
    context: &mut Context,
) -> JsResult<Vec<JsValue>> {
    let mut path = vec![target.clone()];
    if let Some(ParentHook(parent)) = context.get_data::<ParentHook>().copied() {
        let mut at = target.clone();
        while let Some(next) = parent(&at, event, context)? {
            // A path never loops, whatever the host answers.
            if path.contains(&next) || path.len() > 4096 {
                break;
            }
            path.push(next.clone());
            at = next;
        }
    }
    Ok(path)
}

/// The scope a dispatch of `event` runs under, with the `relatedTarget` it started with; `None`
/// when the host set none.
pub(super) fn scope_of(
    context: &Context,
    event: &JsObject,
) -> JsResult<Option<(EventScope, JsValue)>> {
    let Some(ScopeHook(scope)) = context.get_data::<ScopeHook>().copied() else {
        return Ok(None);
    };
    Ok(Some((
        scope,
        field(&event.clone().into(), "relatedTarget")?,
    )))
}

/// Sets what the event's `target` and `relatedTarget` are while it is heard by `node`.
pub(super) fn apply(
    context: &mut Context,
    scope: Option<&(EventScope, JsValue)>,
    node: &JsValue,
    target: &JsValue,
    event: &JsObject,
) {
    let Some((scope, related)) = scope else {
        return;
    };
    let seen = (scope.retarget)(node, target, context);
    let related = (!related.is_undefined()).then(|| (scope.retarget)(node, related, context));
    if let Some(data) = event.downcast_ref::<EventData>() {
        let mut state = data.0.borrow_mut();
        state.target = seen;
        if let Some(related) = related {
            set_field(&mut state, "relatedTarget", related);
        }
    }
}

/// The items of an event's path that `composedPath()`, called from a listener on `node`, lists.
pub(super) fn visible_to(
    context: &mut Context,
    node: &JsValue,
    mut items: Vec<JsValue>,
) -> Vec<JsValue> {
    if let Some(ScopeHook(scope)) = context.get_data::<ScopeHook>().copied() {
        items.retain(|item| (scope.visible)(node, item, context));
    }
    items
}
