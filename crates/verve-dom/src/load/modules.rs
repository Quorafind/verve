//! Module scripts: the graph a `<script type="module">` imports is fetched whole before the
//! script runs, into a cache the kernel's module source hook answers from (a page's modules
//! need the network, and the kernel loads synchronously). `file:` modules need no fetching: the
//! kernel reads them itself, through the same virtual filesystem.

use std::collections::HashSet;
use std::path::Path;

use boa_engine::Context;
use boa_engine::object::builtins::JsPromise;
use url::Url;
use verve::ImportMap;

use super::{Destination, Document, LoadResult, Script, Status, Wait, net, script};
use crate::dom::NodeId;
use crate::state::shared;

/// Whether the kernel reads `url` itself.
fn is_local(url: &Url) -> bool {
    matches!(url.scheme(), "file" | "data" | "blob")
}

/// Where a module of a graph is.
pub(crate) enum Mod {
    /// Asked for; the scripts waiting for it are in `Load::waiters`.
    Fetching,
    /// Here, and the addresses it imports.
    Done(Vec<Edge>),
    Failed,
}

/// An import of a module: the address, and whether it is a literal `import()`.
#[derive(Clone)]
pub(crate) struct Edge {
    url: Url,
    dynamic: bool,
}

/// A module script starts: its graph is asked for, or it is ready at once.
pub(super) fn begin(context: &mut Context, node: NodeId) {
    let (url, text, map) = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.load.module_scripts_started += 1;
        let script = state.load.scripts.iter().find(|s| s.node == node);
        let found = script.map(|s| (s.url.clone(), s.text.clone()));
        let (url, text) = found.unwrap_or_default();
        (url, text, state.load.import_map.clone())
    };
    // An inline module's imports are relative to the document's base address.
    let own = shared(context).borrow().base_url();
    let wanted = match url {
        Some(url) => vec![Edge {
            url,
            dynamic: false,
        }],
        None => edges_of(&text, &own, &map),
    };
    shared(context)
        .borrow_mut()
        .load
        .pending
        .entry(node)
        .or_default();
    let mut seen = HashSet::new();
    for edge in wanted {
        need(context, node, &edge, true, &mut seen);
    }
    ready_if_done(context, node);
}

/// The addresses a module's source imports, from the module at `own`.
fn edges_of(source: &str, own: &Url, map: &Option<ImportMap>) -> Vec<Edge> {
    imports_of(source)
        .into_iter()
        .filter_map(|import| {
            resolve(&import.specifier, own, map).map(|url| Edge {
                url,
                dynamic: import.dynamic,
            })
        })
        .collect()
}

/// `node`'s graph needs the module at `edge` (and what it imports): it is asked for if nobody
/// has, and waited for if somebody has. `fatal` is whether the graph fails without it.
fn need(
    context: &mut Context,
    node: NodeId,
    edge: &Edge,
    fatal: bool,
    seen: &mut HashSet<(String, bool)>,
) {
    let url = &edge.url;
    let fatal = fatal && !edge.dynamic;
    if is_local(url) || !seen.insert((url.to_string(), fatal)) {
        return;
    }
    let key = url.to_string();
    let known = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        match state.load.mods.get(&key) {
            None => {
                state.load.mods.insert(key.clone(), Mod::Fetching);
                state
                    .load
                    .waiters
                    .entry(key.clone())
                    .or_default()
                    .insert(node, fatal);
                state
                    .load
                    .pending
                    .entry(node)
                    .or_default()
                    .insert(key.clone());
                None
            }
            Some(Mod::Fetching) => {
                let waiting = state.load.waiters.entry(key.clone()).or_default();
                *waiting.entry(node).or_insert(false) |= fatal;
                state
                    .load
                    .pending
                    .entry(node)
                    .or_default()
                    .insert(key.clone());
                Some(Vec::new())
            }
            Some(Mod::Done(edges)) => Some(edges.clone()),
            Some(Mod::Failed) => {
                if fatal {
                    state.load.failed_graphs.insert(node);
                }
                Some(Vec::new())
            }
        }
    };
    match known {
        None => {
            let wait = Wait::Module {
                script: node,
                url: key,
            };
            net::start(context, url.clone(), Destination::Module, wait);
        }
        Some(edges) => {
            for edge in edges {
                need(context, node, &edge, fatal, seen);
            }
        }
    }
}

fn set_status(context: &mut Context, node: NodeId, status: Status) {
    let state = shared(context);
    let mut state = state.borrow_mut();
    if let Some(script) = state.load.scripts.iter_mut().find(|s| s.node == node) {
        script.status = status;
    }
}

/// The script's graph is in, or has failed, when nothing is left for it to wait for.
fn ready_if_done(context: &mut Context, node: NodeId) {
    let (waiting, failed) = {
        let state = shared(context);
        let state = state.borrow();
        let waiting = state.load.pending.get(&node).is_some_and(|p| !p.is_empty());
        (waiting, state.load.failed_graphs.contains(&node))
    };
    if !waiting {
        set_status(
            context,
            node,
            if failed {
                Status::Failed
            } else {
                Status::Ready
            },
        );
    }
}

/// A module of a graph has come: every script waiting for it goes on to what it imports, and is
/// ready when nothing is left.
pub(super) fn fetched(
    doc: &Document,
    runtime: &mut verve::Runtime,
    _: NodeId,
    url: &str,
    result: LoadResult,
) {
    let map = doc.state.borrow().load.import_map.clone();
    let mut edges = Vec::new();
    let mut ok = false;
    if let (Ok(bytes), Ok(asked)) = (result.body, Url::parse(url))
        && result.status < 400
    {
        // What it imports is relative to where it ended up, not to where it was asked for.
        let source: std::rc::Rc<str> = net::text(&bytes).into();
        edges = edges_of(&source, &result.url, &map);
        let state = doc.state.borrow();
        let mut cache = state.load.modules.borrow_mut();
        cache.insert(url.to_owned(), source.clone());
        if result.url != asked {
            cache.insert(result.url.to_string(), source);
            drop(cache);
            drop(state);
            redirected(doc, runtime, url, result.url);
        }
        ok = true;
    }
    let context = runtime.context();
    let waiters: Vec<(NodeId, bool)> = {
        let mut state = doc.state.borrow_mut();
        let found = if ok {
            Mod::Done(edges.clone())
        } else {
            Mod::Failed
        };
        state.load.mods.insert(url.to_owned(), found);
        let waiters = state.load.waiters.remove(url).unwrap_or_default();
        for (node, fatal) in &waiters {
            if let Some(pending) = state.load.pending.get_mut(node) {
                pending.remove(url);
            }
            if !ok && *fatal {
                state.load.failed_graphs.insert(*node);
            }
        }
        waiters.into_iter().collect()
    };
    for (node, fatal) in &waiters {
        let mut seen = HashSet::from([(url.to_owned(), *fatal)]);
        for edge in &edges {
            need(context, *node, edge, *fatal, &mut seen);
        }
    }
    for (node, _) in waiters {
        ready_if_done(context, node);
    }
}

/// The module asked for at `asked` was redirected to `found`: whoever imports it gets the one at
/// `found`, so its own imports resolve against that.
fn redirected(doc: &Document, runtime: &mut verve::Runtime, asked: &str, found: Url) {
    let map = {
        let mut state = doc.state.borrow_mut();
        state.load.redirects.insert(asked.to_owned(), found);
        let mut map = state.load.import_map.clone().unwrap_or_default();
        for (from, to) in &state.load.redirects {
            if let Ok(from) = Url::parse(from) {
                map.redirect(&from, to);
            }
        }
        map
    };
    runtime.set_import_map(Some(map));
}

/// Runs a module script whose graph is in.
pub(super) fn run(doc: &Document, runtime: &mut verve::Runtime, script: &Script) {
    let imported = match &script.url {
        Some(url) => {
            let found = doc.state.borrow().load.redirects.get(url.as_str()).cloned();
            runtime.import_module(found.as_ref().unwrap_or(url).as_str(), None)
        }
        None => {
            let base = doc.state.borrow().base_url();
            let mut name = base.clone();
            name.set_fragment(Some(&format!("module-{}", script.node)));
            runtime.load_esm(Path::new(name.as_str()), &script.text)
        }
    };
    runtime.run_microtasks();
    let context = runtime.context();
    let rejected = match &imported {
        Ok(promise) => rejection(promise),
        Err(message) => Some(verve::native::type_error(message.clone())),
    };
    match rejected {
        Some(error) => {
            let name = script.url.as_ref().map_or_else(String::new, Url::to_string);
            // A graph that could not be made is the element's `error`; one that threw while it
            // ran was fetched, and is its `load`.
            let broken = graph_failed(&error, context);
            script::uncaught(context, &error, &name);
            if script.url.is_some() {
                script::fire_on(context, script.node, if broken { "error" } else { "load" });
            }
        }
        None if script.url.is_some() => script::fire_on(context, script.node, "load"),
        None => {}
    }
}

/// Whether `error` says the graph could not be made (a module not found, read or parsed)
/// rather than that a module threw when it ran: the first is the element's `error`.
fn graph_failed(error: &boa_engine::JsError, context: &mut Context) -> bool {
    if verve::is_parse_failure(error, context) {
        return true;
    }
    let Ok(value) = error.clone().into_opaque(context) else {
        return false;
    };
    verve::native::get(&value, "code", context)
        .ok()
        .and_then(|v| v.as_string().map(|s| s.to_std_string_escaped()))
        .as_deref()
        == Some("ERR_MODULE_NOT_FOUND")
}

/// Why a module's promise was rejected, if it was.
fn rejection(promise: &JsPromise) -> Option<boa_engine::JsError> {
    match promise.state() {
        boa_engine::builtins::promise::PromiseState::Rejected(value) => {
            Some(boa_engine::JsError::from_opaque(value))
        }
        _ => None,
    }
}

/// The address an import of `specifier` from the module at `from` is, when it is one the page
/// can fetch: through the import map, or relative to the importer.
fn resolve(specifier: &str, from: &Url, map: &Option<ImportMap>) -> Option<Url> {
    if let Some(map) = map {
        match map.resolve(specifier, from) {
            Ok(Some(url)) => return Some(url),
            Err(_) => return None,
            Ok(None) => {}
        }
    }
    verve::url_like(specifier, from)
}

/// What a module's source imports with a literal address.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Import {
    pub specifier: String,
    /// A literal `import()`, which fails when it runs rather than keep the module from running.
    pub dynamic: bool,
}

/// What came last in the source, to tell a `/` that divides from one that opens a regular
/// expression.
#[derive(Clone, Copy, PartialEq)]
enum Last {
    /// Nothing, or a punctuator but a closing `)` or `]`: a `/` after it opens a regular
    /// expression.
    Operator,
    /// A name, a number, a string, `)` or `]`: a `/` after it divides.
    Operand,
}

/// Words after which an expression, and so a regular expression, may start.
const BEFORE_EXPRESSION: &[&str] = &[
    "return",
    "typeof",
    "instanceof",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "yield",
    "await",
];

/// The specifiers a module's static `import` and `export … from` statements name, and its
/// `import()` of a literal, found by reading its tokens (comments, strings and regular
/// expressions skipped), not by parsing it.
pub(super) fn imports_of(source: &str) -> Vec<Import> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    let mut last = Last::Operator;
    while at < bytes.len() {
        if let Some(end) = comment_end(source, at) {
            at = end;
            continue;
        }
        match bytes[at] {
            b'/' => {
                let regex = (last == Last::Operator)
                    .then(|| regex_end(bytes, at))
                    .flatten();
                at = regex.unwrap_or(at + 1);
                last = if regex.is_some() {
                    Last::Operand
                } else {
                    Last::Operator
                };
            }
            quote @ (b'"' | b'\'' | b'`') => {
                at = skip_string(bytes, at, quote);
                last = Last::Operand;
            }
            b if is_word_start(b) => {
                let end = word_end(bytes, at);
                let word = &source[at..end];
                let before = source[..at].trim_end().chars().last();
                if before != Some('.')
                    && matches!(word, "import" | "export")
                    && let Some((specifier, dynamic, next)) =
                        statement(source, end, word == "import")
                {
                    found.push(Import { specifier, dynamic });
                    at = next;
                    last = Last::Operand;
                    continue;
                }
                last = if before == Some('.') || !BEFORE_EXPRESSION.contains(&word) {
                    Last::Operand
                } else {
                    Last::Operator
                };
                at = end;
            }
            b if b.is_ascii_digit() => {
                at = word_end(bytes, at);
                last = Last::Operand;
            }
            b')' | b']' => {
                at += 1;
                last = Last::Operand;
            }
            b if b.is_ascii_whitespace() => at += 1,
            _ => {
                at += 1;
                last = Last::Operator;
            }
        }
    }
    found
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

/// The index after the name (or number) that starts at `at`.
fn word_end(bytes: &[u8], at: usize) -> usize {
    at + bytes[at..]
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'$')
        .count()
}

/// The index after the comment that starts at `at`, if one does.
fn comment_end(source: &str, at: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(at) != Some(&b'/') {
        return None;
    }
    match bytes.get(at + 1) {
        Some(b'/') => Some(
            at + bytes[at..]
                .iter()
                .position(|b| *b == b'\n')
                .unwrap_or(bytes.len() - at),
        ),
        Some(b'*') => Some(
            source[at + 2..]
                .find("*/")
                .map_or(bytes.len(), |end| at + end + 4),
        ),
        _ => None,
    }
}

/// The index after the regular expression literal that starts at `at`, if the `/` opens one: a
/// regular expression ends on its line.
fn regex_end(bytes: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    let mut class = false;
    while let Some(&byte) = bytes.get(i) {
        match byte {
            b'\n' | b'\r' => return None,
            b'\\' => i += 1,
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => return Some(i + 1),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The index after the string literal that starts at `at`.
fn skip_string(bytes: &[u8], at: usize, quote: u8) -> usize {
    let mut i = at + 1;
    while i < bytes.len() && bytes[i] != quote {
        i += if bytes[i] == b'\\' { 2 } else { 1 };
    }
    (i + 1).min(bytes.len())
}

/// Past whitespace and comments.
fn trivia(source: &str, mut at: usize) -> usize {
    let bytes = source.as_bytes();
    loop {
        match bytes.get(at) {
            Some(b) if b.is_ascii_whitespace() => at += 1,
            _ => match comment_end(source, at) {
                Some(end) => at = end,
                None => return at,
            },
        }
    }
}

fn is_quote(byte: Option<&u8>) -> bool {
    matches!(byte, Some(b'"' | b'\''))
}

/// The specifier of the statement whose keyword ends at `from`, whether it is a dynamic
/// `import('…')`, and where it ends. The clause before `from` holds only names, `*`, `,`,
/// braces and (as export names) strings: the first thing else ends the search, so a `from '…'`
/// further on is no part of it.
fn statement(source: &str, from: usize, is_import: bool) -> Option<(String, bool, usize)> {
    let bytes = source.as_bytes();
    let mut at = trivia(source, from);
    if is_import && bytes.get(at) == Some(&b'(') {
        let start = trivia(source, at + 1);
        let (specifier, end) = is_quote(bytes.get(start)).then(|| literal(source, start))??;
        let after = trivia(source, end);
        return matches!(bytes.get(after), Some(b')' | b',')).then_some((specifier, true, end));
    }
    if is_import && is_quote(bytes.get(at)) {
        return literal(source, at).map(|(specifier, end)| (specifier, false, end));
    }
    let mut braces = 0usize;
    let mut closed = false;
    while at < bytes.len() {
        match bytes[at] {
            b'{' => braces += 1,
            b'}' => {
                braces = braces.checked_sub(1)?;
                closed = braces == 0;
            }
            b',' | b'*' => {}
            b'"' | b'\'' if braces > 0 => at = skip_string(bytes, at, bytes[at]) - 1,
            b if is_word_start(b) => {
                let end = word_end(bytes, at);
                if braces == 0 && &source[at..end] == "from" {
                    let quote = trivia(source, end);
                    return is_quote(bytes.get(quote))
                        .then(|| literal(source, quote))?
                        .map(|(specifier, end)| (specifier, false, end));
                }
                if closed {
                    return None;
                }
                at = end - 1;
            }
            _ => return None,
        }
        at = trivia(source, at + 1);
    }
    None
}

/// The string literal at `at`, and the index after it.
fn literal(source: &str, at: usize) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    let end = skip_string(bytes, at, bytes[at]);
    Some((source.get(at + 1..end - 1)?.to_owned(), end))
}

#[cfg(test)]
mod tests {
    use super::imports_of;

    fn specifiers(source: &str) -> Vec<String> {
        imports_of(source)
            .into_iter()
            .map(|import| import.specifier)
            .collect()
    }

    #[test]
    fn the_static_imports_of_a_module_are_found_among_the_rest_of_it() {
        let source = r#"
            import React from "react";
            import { a, b as c } from './a.js';
            import * as ns from '../ns.js'; import './side.js';
            export { x } from "./x.js";
            export * from './all.js';
            // import nope from './comment.js';
            const s = "import nope from './string.js'";
            const lazy = () => import('./lazy.js');
            const computed = () => import(name);
            export { late };
            const later = "from './string.js'";
            export {
                /* from './inside.js' */ kept
            } from './after-comment.js';
            const url = import.meta.url; obj.import = 1;
            export const keep = 1;
        "#;
        assert_eq!(
            specifiers(source),
            [
                "react",
                "./a.js",
                "../ns.js",
                "./side.js",
                "./x.js",
                "./all.js",
                "./lazy.js",
                "./after-comment.js"
            ]
        );
        let kinds: Vec<bool> = imports_of(source).iter().map(|i| i.dynamic).collect();
        assert_eq!(
            kinds,
            [false, false, false, false, false, false, true, false]
        );
    }

    #[test]
    fn a_regular_expression_is_no_comment_and_a_division_no_regular_expression() {
        let source = r#"
            const web = /^https?:\/\//.test(x); const lazy = () => import("./chunk.js");
            const half = a / 2; const other = b / 3; import("./two.js");
            const hidden = /[/]import\("\.\/not\.js"\)/; if (y) return /\//.test(z) && import('./last.js');
        "#;
        assert_eq!(specifiers(source), ["./chunk.js", "./two.js", "./last.js"]);
    }
}
