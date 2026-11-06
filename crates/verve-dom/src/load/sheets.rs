//! `<link>` elements: stylesheets that join the cascade once fetched, and the icons a page
//! names. A sheet the parser meets in `<head>` (and whose `media` is the screen's) holds back
//! scripts and the first paint; one the page adds, or one in `<body>`, only holds back `load`.

use boa_engine::Context;
use url::Url;

use super::{Destination, Document, LoadResult, Wait, net};
use crate::css::{MediaQuery, Sheet};
use crate::dom::NodeId;
use crate::state::{SheetOwner, SheetRecord, State, shared};

/// A `<link>` joined the document: a stylesheet is asked for, an icon is noted.
pub(super) fn discover(context: &mut Context, node: NodeId) {
    let wanted = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let rel = state
            .dom
            .get_attr(node, "rel")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let tokens: Vec<&str> = rel.split_whitespace().collect();
        let href = state.dom.get_attr(node, "href").unwrap_or_default();
        let url = state.base_url().join(href.trim()).ok();
        let url = url.filter(|_| !href.trim().is_empty());
        let media = state.dom.get_attr(node, "media").unwrap_or_default();
        let sheet = tokens.contains(&"stylesheet")
            && !tokens.contains(&"alternate")
            && state.dom.get_attr(node, "disabled").is_none()
            && MediaQuery::new(&media).matches(&state.env);
        if tokens.iter().any(|t| t.ends_with("icon"))
            && let Some(url) = &url
        {
            state.load.icons.push(url.to_string());
        }
        let blocking = blocks(&state, node);
        match url {
            Some(url) if sheet && state.load.linked.insert(node) => {
                state.load.blocking_sheets += usize::from(blocking);
                Some((url, blocking))
            }
            _ => None,
        }
    };
    if let Some((url, blocking)) = wanted {
        net::start(
            context,
            url,
            Destination::Style,
            Wait::Sheet(node, blocking),
        );
    }
}

/// Whether the sheet of `node` holds back scripts and the first paint: the page's parser made
/// the link, in `<head>`.
fn blocks(state: &State, node: NodeId) -> bool {
    state.dom.is_parser_made(node) && state.dom.parent(node) == Some(state.dom.head)
}

fn release(state: &mut State, blocking: bool) {
    if blocking {
        state.load.blocking_sheets = state.load.blocking_sheets.saturating_sub(1);
    }
}

/// A stylesheet has come, or has not.
pub(super) fn fetched(doc: &Document, node: NodeId, blocking: bool, result: LoadResult) {
    let ok = result.body.is_ok() && result.status < 400;
    {
        let mut state = doc.state.borrow_mut();
        release(&mut state, blocking);
        if let Ok(bytes) = &result.body
            && ok
            && state.dom.is_connected(node)
        {
            let text = net::decoded_text(bytes);
            let tree = state.dom.root_of(node);
            let sheet = Sheet::parse(&text);
            let owner = SheetOwner::Link(node);
            state.sheets.push(SheetRecord { owner, sheet, tree });
            // Where its element is in the page, not when it came.
            state.order_sheets();
            state.sheets_changed();
            request_imports(&mut state, node, &[], &text, &result.url, blocking);
        }
    }
    let kind = if ok { "load" } else { "error" };
    // The event is heard once what the sheet imports is in.
    let mut state = doc.state.borrow_mut();
    if state.load.imports_coming.get(&node).is_some_and(|n| *n > 0) {
        state.load.link_events.insert(node, kind);
    } else {
        state.load.notices.push((node, kind));
    }
}

/// A sheet `@import`ed by a `<link>`'s has come: its rules go in just before the sheet that
/// imported it, after the imports before it.
pub(super) fn imported(
    doc: &Document,
    (node, rank, blocking): (NodeId, Vec<usize>, bool),
    result: LoadResult,
) {
    let mut state = doc.state.borrow_mut();
    release(&mut state, blocking);
    take_import(&mut state, (node, rank), &result, blocking);
    // This one is in, or never will be: the link's event waits for the rest.
    let coming = state.load.imports_coming.entry(node).or_default();
    *coming = coming.saturating_sub(1);
    if *coming == 0
        && let Some(kind) = state.load.link_events.remove(&node)
    {
        state.load.notices.push((node, kind));
    }
}

/// Puts the sheet an import came with among the page's, and asks for what it imports.
fn take_import(
    state: &mut State,
    (node, rank): (NodeId, Vec<usize>),
    result: &LoadResult,
    blocking: bool,
) {
    let Ok(bytes) = &result.body else { return };
    let linked = state
        .sheets
        .iter()
        .any(|r| r.owner == SheetOwner::Link(node));
    if result.status >= 400 || !linked {
        return;
    }
    let text = net::decoded_text(bytes);
    let tree = state.dom.root_of(node);
    let sheet = Sheet::parse(&text);
    let owner = SheetOwner::Import(node, rank.clone());
    state.sheets.push(SheetRecord { owner, sheet, tree });
    state.order_sheets();
    state.sheets_changed();
    request_imports(state, node, &rank, &text, &result.url, blocking);
}

/// Asks for the sheets `text` `@import`s, relative to `from`, for the `<link>` `node`; `parent`
/// is the path of the sheet that names them.
fn request_imports(
    state: &mut State,
    node: NodeId,
    parent: &[usize],
    text: &str,
    from: &Url,
    blocking: bool,
) {
    for rule in crate::css::split_rules(text) {
        let Some(url) = import_of(&rule).and_then(|href| from.join(&href).ok()) else {
            continue;
        };
        if !state.load.requested.insert(format!("import:{node}:{url}")) {
            continue;
        }
        let count = state
            .load
            .import_ranks
            .entry((node, parent.to_vec()))
            .or_default();
        *count += 1;
        let mut path = parent.to_vec();
        path.push(*count);
        *state.load.imports_coming.entry(node).or_default() += 1;
        state.load.blocking_sheets += usize::from(blocking);
        state
            .load
            .wanted
            .push((url, Wait::Import(node, path, blocking)));
    }
}

/// The address an `@import` rule names: `@import url("a.css") screen;`, `@import 'a.css';`.
fn import_of(rule: &str) -> Option<String> {
    let rest = rule.strip_prefix("@import")?.trim_start();
    let rest = match rest.strip_prefix("url(") {
        Some(inner) => inner.trim_start(),
        None => rest,
    };
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''));
    let end = match quote {
        Some(q) => rest[1..].find(q).map(|end| (1, end + 1)),
        None => rest.find(')').map(|end| (0, end)),
    }?;
    Some(rest[end.0..end.1].trim().to_owned()).filter(|href| !href.is_empty())
}
