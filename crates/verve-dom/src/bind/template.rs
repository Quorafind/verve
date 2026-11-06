//! A `<template>`'s `content`: a fragment of its own that holds what was put in the template.

use boa_engine::Context;

use super::node::{read_dom, with_dom, wrap, wrap_opt};
use super::{Accessor, node_of};
use crate::dom::NodeId;
use crate::state::shared;

/// A `<template>`'s content fragment, made the first time it is asked for, holding what was
/// put in the template since; `None` for anything else.
pub fn template_target(context: &mut Context, id: NodeId) -> Option<NodeId> {
    if read_dom(context, |dom| {
        dom.get(id).and_then(|n| n.tag()) != Some("template")
    }) {
        return None;
    }
    let (fragment, fresh) = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let existing = state
            .template_contents
            .get(&id)
            .copied()
            .filter(|f| state.dom.exists(*f));
        match existing {
            Some(fragment) => (fragment, false),
            None => {
                let fragment = state.dom.create_fragment();
                state.template_contents.insert(id, fragment);
                (fragment, true)
            }
        }
    };
    if fresh {
        // The template keeps its content alive.
        let template = wrap(context, id).as_object().expect("an element");
        let content = wrap(context, fragment).as_object().expect("a fragment");
        verve::dom_events::hold(&template, |held| held.push(content.clone()));
    }
    with_dom(context, |dom| {
        let children = dom.children(id).to_vec();
        for child in children {
            dom.append(fragment, child);
        }
    });
    Some(fragment)
}

pub const TEMPLATE_ACCESSORS: &[Accessor] = &[(
    "content",
    Some(|this, _, cx| {
        let template = node_of(this)?;
        let fragment = template_target(cx, template);
        Ok(wrap_opt(cx, fragment))
    }),
    None,
)];
