//! Markup both ways: read into nodes of [`crate::dom`] (`innerHTML`, `DOMParser`,
//! `insertAdjacentHTML`), and nodes written out (`innerHTML`, `outerHTML`, `XMLSerializer`).
//!
//! Not the HTML standard's whole tree builder, but the parts pages lean on: tags, attributes,
//! comments, doctypes and character references; raw text inside `<script>`, `<style>`,
//! `<textarea>` and `<title>`; the end tags HTML implies (a `<p>` closed by the block that
//! follows it, an `<li>` by the next, table rows and cells, options); a `<tbody>` put round rows
//! written straight into a `<table>`; `<html>`, `<head>` and `<body>` of a whole document going
//! to the document's own; and SVG and MathML read as foreign content, in their namespaces, where
//! `<x/>` closes itself. No script runs and no style loads: a `<script>` is kept as an inert
//! element, as one set through `innerHTML` is in a browser.
//!
//! Lifted from su-editor's Obsidian host.

mod markup;
mod write;
use markup::{Tag, cdata, comment, declaration};
pub use write::{inner, inner_xml, outer, outer_xml};

use crate::dom::{Dom, Namespace, NodeId, NodeKind, ShadowMode, SlotAssignment};

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr", "param", "keygen", "frame",
];

/// Elements whose content is text up to their end tag, and whether references in it are read.
fn raw_text(tag: &str) -> Option<bool> {
    match tag {
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => Some(false),
        "textarea" | "title" => Some(true),
        _ => None,
    }
}

/// The elements whose start closes an open `<p>`.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
    "search",
    "listing",
];

/// What the parse puts the markup in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// A fragment of a page: `innerHTML`. `<html>`, `<head>` and `<body>` are ignored.
    Fragment,
    /// A whole document's `<html>`: what `DOMParser` and `document.write` read.
    Document {
        html: NodeId,
        head: NodeId,
        body: NodeId,
    },
}

/// Reads `markup` into `parent`, after what is already there, as `innerHTML` does.
pub fn parse_into(dom: &mut Dom, parent: NodeId, markup: &str) {
    parse(dom, parent, markup, Context::Fragment);
}

/// Same, for a fragment whose scripts run when it is put in a page (a range's contextual
/// fragment; `innerHTML`'s never do).
pub fn parse_runnable(dom: &mut Dom, parent: NodeId, markup: &str) {
    parse_into(dom, parent, markup);
    dom.let_scripts_run(parent);
}

/// The three a whole document's markup goes between, when `document` has them.
fn document_context(dom: &Dom, document: NodeId) -> Option<(Context, NodeId)> {
    let html = dom
        .children(document)
        .iter()
        .copied()
        .find(|c| dom.get(*c).and_then(|n| n.tag()) == Some("html"))?;
    let head = dom.first_tag(html, "head").unwrap_or(html);
    let body = dom.first_tag(html, "body").unwrap_or(html);
    Some((Context::Document { html, head, body }, body))
}

/// Reads a whole page into a document made with [`Dom::create_html_document`].
pub fn parse_document(dom: &mut Dom, document: NodeId, markup: &str) {
    let Some((context, body)) = document_context(dom, document) else {
        return parse_into(dom, document, markup);
    };
    parse(dom, body, markup, context);
}

/// What a [`StreamParser`] needs of its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// All the markup given has been read; more may come, or [`StreamParser::finish`].
    NeedMore,
    /// A `<script>` is complete and in the tree: the page runs it, then steps again.
    Script(NodeId),
    /// The markup has ended and been read to the end.
    Done,
}

/// A whole page read as it arrives, stopping at each `<script>` so the page can run it before the
/// markup after it is read — and so what it `document.write`s goes in at that point.
pub struct StreamParser {
    parser: Parser,
    /// The markup given and not yet read.
    text: String,
    /// How much of the front of `text` scripts wrote (`document.write`).
    inserted: usize,
    /// How many of those written bytes lie after the insertion point: the next write goes at
    /// `inserted - tail`. HTML's "insertion point", kept as a distance from the end of what was
    /// written, which neither the parser reading nor a write at the point changes.
    tail: usize,
    eof: bool,
}

impl StreamParser {
    /// A parse into `document`'s `<html>` (a document made with [`Dom::create_html_document`]).
    pub fn new(dom: &Dom, document: NodeId) -> Self {
        let (context, parent) =
            document_context(dom, document).unwrap_or((Context::Fragment, document));
        Self {
            parser: Parser::new(dom, parent, context),
            text: String::new(),
            inserted: 0,
            tail: 0,
            eof: false,
        }
    }

    /// More of the markup has arrived.
    pub fn feed(&mut self, chunk: &str) {
        self.text.push_str(chunk);
    }

    /// The markup has ended.
    pub fn finish(&mut self) {
        self.eof = true;
    }

    /// Reads on to the next `<script>`, or as far as the markup given goes.
    pub fn step(&mut self, dom: &mut Dom) -> Step {
        let feed = Feed {
            pause: true,
            eof: self.eof,
        };
        let (used, script) = self.parser.scan(dom, &self.text, feed);
        self.text.drain(..used);
        self.inserted = self.inserted.saturating_sub(used);
        self.tail = self.tail.min(self.inserted);
        match script {
            Some(script) => Step::Script(script),
            None if self.eof && self.text.is_empty() => Step::Done,
            None => Step::NeedMore,
        }
    }

    /// `document.write` while a script runs: the text goes in at the insertion point, after
    /// what earlier writes put there.
    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.inserted - self.tail, text);
        self.inserted += text.len();
    }

    /// A parser-inserted script is about to run: what it writes goes right after it. Answers
    /// what to give [`StreamParser::leave_script`] when it has run.
    pub fn enter_script(&mut self) -> usize {
        std::mem::replace(&mut self.tail, self.inserted)
    }

    /// The script is done: writes go where they went before it.
    pub fn leave_script(&mut self, saved: usize) {
        self.tail = saved;
    }

    /// Reads what was [`inserted`](StreamParser::insert) up to the insertion point and no more,
    /// as far as it goes: a script it writes runs before `document.write` returns. What lies
    /// after the point is an outer script's text, read when that script is done.
    pub fn step_inserted(&mut self, dom: &mut Dom) -> Step {
        let feed = Feed {
            pause: true,
            eof: false,
        };
        let point = self.inserted - self.tail;
        let (used, script) = self.parser.scan(dom, &self.text[..point], feed);
        self.text.drain(..used);
        self.inserted -= used;
        script.map_or(Step::NeedMore, Step::Script)
    }
}

struct Open {
    id: NodeId,
    tag: String,
    namespace: Namespace,
}

fn open_stack(dom: &Dom, parent: NodeId) -> Vec<Open> {
    let parent_namespace = dom.get(parent).map_or(Namespace::Html, |n| {
        if n.is_element() {
            n.namespace
        } else {
            Namespace::Html
        }
    });
    vec![Open {
        id: parent,
        tag: dom
            .get(parent)
            .and_then(|n| n.tag())
            .unwrap_or("")
            .to_owned(),
        namespace: parent_namespace,
    }]
}

/// How far a parse reads, and what it stops for.
#[derive(Clone, Copy)]
struct Feed {
    /// Stop after a `<script>` element is complete: the page runs it before reading on.
    pause: bool,
    /// No more markup will come: what is unfinished is finished as it stands.
    eof: bool,
}

/// A parse in progress: the open elements and where the markup is being put. It reads what it is
/// given as far as it can, and is picked up where it stopped.
struct Parser {
    stack: Vec<Open>,
    /// In a whole document, what comes before `<body>` that belongs in `<head>` goes there.
    in_body: bool,
    context: Context,
}

fn parse(dom: &mut Dom, parent: NodeId, markup: &str, context: Context) {
    let mut parser = Parser::new(dom, parent, context);
    parser.scan(
        dom,
        markup,
        Feed {
            pause: false,
            eof: true,
        },
    );
}

impl Parser {
    fn new(dom: &Dom, parent: NodeId, context: Context) -> Self {
        Self {
            stack: open_stack(dom, parent),
            in_body: !matches!(context, Context::Document { .. }),
            context,
        }
    }

    /// A whole document's own `<html>`, `<head>` and `<body>`: what they say lands on the
    /// elements the document has, and what is written before the body goes to the head.
    /// Answers whether the tag was one of them and is done with.
    fn route(
        stack: &mut Vec<Open>,
        in_body: &mut bool,
        context: Context,
        dom: &mut Dom,
        name: &str,
        attrs: &str,
    ) -> bool {
        if let Context::Document { html, head, body } = context {
            match name {
                "html" => {
                    set_attrs(dom, html, attrs, Namespace::Html, true);
                    return true;
                }
                "head" if !*in_body => {
                    set_attrs(dom, head, attrs, Namespace::Html, true);
                    stack.truncate(1);
                    stack[0] = Open {
                        id: head,
                        tag: "head".into(),
                        namespace: Namespace::Html,
                    };
                    return true;
                }
                "body" => {
                    set_attrs(dom, body, attrs, Namespace::Html, true);
                    *in_body = true;
                    stack.truncate(1);
                    stack[0] = Open {
                        id: body,
                        tag: "body".into(),
                        namespace: Namespace::Html,
                    };
                    return true;
                }
                "head" => return true,
                tag if !*in_body
                    && matches!(
                        tag,
                        "meta" | "title" | "link" | "style" | "script" | "base" | "noscript"
                    ) =>
                {
                    // What belongs in `<head>` before the body goes there, whether or not
                    // `<head>` was written; once there, it stays (this arm, not the next).
                    #[allow(clippy::collapsible_if, clippy::collapsible_match)]
                    if stack[0].id != head {
                        stack.truncate(1);
                        stack[0] = Open {
                            id: head,
                            tag: "head".into(),
                            namespace: Namespace::Html,
                        };
                    }
                }
                _ if !*in_body => {
                    *in_body = true;
                    stack.truncate(1);
                    stack[0] = Open {
                        id: body,
                        tag: "body".into(),
                        namespace: Namespace::Html,
                    };
                }
                _ => {}
            }
        } else if matches!(name, "html" | "head" | "body") {
            return true;
        }
        false
    }

    /// Reads `text` as far as it goes, into the open elements: answers how much of it was read
    /// and, when `feed` pauses at scripts, the `<script>` that was just completed.
    fn scan(&mut self, dom: &mut Dom, text: &str, feed: Feed) -> (usize, Option<NodeId>) {
        let context = self.context;
        let stack = &mut self.stack;
        let in_body = &mut self.in_body;
        let total = text.len();
        let mut rest = text;
        while !rest.is_empty() {
            let Some(lt) = rest.find('<') else {
                push_text(dom, stack, rest, true, context, in_body);
                rest = "";
                break;
            };
            if lt > 0 {
                push_text(dom, stack, &rest[..lt], true, context, in_body);
            }
            rest = &rest[lt..];
            if let Some(after) = rest.strip_prefix("<!--") {
                let Some(next) = comment(dom, stack, after, feed.eof) else {
                    break;
                };
                rest = next;
                continue;
            }
            if let Some(after) = rest.strip_prefix("<![CDATA[") {
                rest = cdata(dom, stack, after, context, in_body);
                continue;
            }
            if rest.starts_with("<!") || rest.starts_with("<?") {
                rest = declaration(dom, stack, context, rest);
                continue;
            }
            // `<` not followed by a tag name is text.
            let opens_tag = rest[1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '/');
            if !opens_tag {
                push_text(dom, stack, "<", false, context, in_body);
                rest = &rest[1..];
                continue;
            }
            match tag(dom, stack, in_body, context, rest, feed) {
                Tag::Read(next) => rest = next,
                Tag::Wait(at) => {
                    rest = at;
                    break;
                }
                Tag::Script(next, element) => return (total - next.len(), Some(element)),
            }
        }
        (total - rest.len(), None)
    }
}

/// The tag at the start of `rest`, an end tag or the start of an element, with what the tag
/// brings (a raw text element's text, a `<script>` the page runs).
fn tag<'a>(
    dom: &mut Dom,
    stack: &mut Vec<Open>,
    in_body: &mut bool,
    context: Context,
    mut rest: &'a str,
    feed: Feed,
) -> Tag<'a> {
    let Some(gt) = find_tag_end(rest) else {
        // An unclosed tag at the end is dropped, as a browser drops it; before the end it
        // waits for the rest of the markup.
        return Tag::Wait(if feed.eof { "" } else { rest });
    };
    let tag_start = rest;
    let inside = &rest[1..gt];
    rest = &rest[gt + 1..];
    if let Some(name) = inside.strip_prefix('/') {
        let name = name
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        close(dom, stack, &name, context, in_body);
        return Tag::Read(rest);
    }
    let self_closing = inside.trim_end().ends_with('/');
    let inside = inside.trim_end().trim_end_matches('/');
    let (raw_name, attrs) = split_name(inside);
    let name = raw_name.to_ascii_lowercase();
    if Parser::route(stack, in_body, context, dom, &name, attrs) {
        return Tag::Read(rest);
    }
    let start = Start {
        name: &name,
        attrs,
        self_closing,
        tag_start,
        rest,
    };
    open_element(dom, stack, context, &start, feed)
}

/// A start tag, read: its name and attributes, and the markup from its `<` and after it.
struct Start<'n, 'a> {
    name: &'n str,
    attrs: &'a str,
    self_closing: bool,
    tag_start: &'a str,
    rest: &'a str,
}

/// The namespace a start tag's element is in, from the element it is put in.
fn namespace_of(stack: &[Open], name: &str) -> Namespace {
    match (stack.last().map(|o| o.namespace), name) {
        (_, "svg") => Namespace::Svg,
        (_, "math") => Namespace::MathMl,
        // HTML inside SVG's `foreignObject` is HTML again.
        (Some(Namespace::Svg), _) if stack.last().is_some_and(|o| o.tag == "foreignobject") => {
            Namespace::Html
        }
        (Some(ns @ (Namespace::Svg | Namespace::MathMl)), _) => ns,
        _ => Namespace::Html,
    }
}

/// The element a start tag opens: it goes in the tree and, unless it is empty, on the stack; a
/// raw text element takes its text with it.
fn open_element<'a>(
    dom: &mut Dom,
    stack: &mut Vec<Open>,
    context: Context,
    start: &Start<'_, 'a>,
    feed: Feed,
) -> Tag<'a> {
    let (name, attrs, rest) = (start.name, start.attrs, start.rest);
    let namespace = namespace_of(stack, name);
    if namespace == Namespace::Html {
        imply_end_tags(dom, stack, name);
    }
    // Rows written straight into a table are in a body of it.
    if namespace == Namespace::Html
        && name == "tr"
        && stack.last().is_some_and(|o| o.tag == "table")
    {
        let tbody = dom.create_element("tbody");
        dom.append(current(stack), tbody);
        stack.push(Open {
            id: tbody,
            tag: "tbody".into(),
            namespace,
        });
    }
    if open_shadow_template(dom, stack, context, name, attrs) {
        return Tag::Read(rest);
    }
    if namespace == Namespace::Html
        && raw_text(name).is_some()
        && !feed.eof
        && find_end_tag(rest, name) == rest.len()
    {
        // The element's text has not all arrived.
        return Tag::Wait(start.tag_start);
    }
    let element = dom.create_element_ns(name, namespace);
    mark_made_by_parser(dom, stack, element, name, namespace, feed);
    set_attrs(dom, element, attrs, namespace, false);
    dom.append(current(stack), element);
    let closes = if namespace == Namespace::Html {
        VOID.contains(&name)
    } else {
        start.self_closing
    };
    if closes {
        return Tag::Read(rest);
    }
    if namespace == Namespace::Html
        && let Some(decode_refs) = raw_text(name)
    {
        let rest = fill_raw_text(dom, element, name, decode_refs, rest);
        if feed.pause && name == "script" && !stack.iter().any(|o| o.tag == "template") {
            return Tag::Script(rest, element);
        }
        return Tag::Read(rest);
    }
    stack.push(Open {
        id: element,
        tag: name.to_owned(),
        namespace,
    });
    Tag::Read(rest)
}

/// What the parser notes of the element it made. A script of the page's own parse has its
/// "already started" flag set, for the parser runs it itself, except in a template: there it
/// never runs, and runs when put in a page, as it does in a browser. A `<link>` the page's
/// parser made may hold back scripts.
fn mark_made_by_parser(
    dom: &mut Dom,
    stack: &[Open],
    element: NodeId,
    name: &str,
    namespace: Namespace,
    feed: Feed,
) {
    if namespace != Namespace::Html {
        return;
    }
    let in_template = stack.iter().any(|o| o.tag == "template");
    if name == "script" && !(feed.pause && in_template) {
        dom.mark_started(element);
    }
    if name == "link" && feed.pause {
        dom.mark_parser_made(element);
    }
}

/// Puts the text of a raw text element (up to its end tag, whatever it holds) in `element`, and
/// answers the markup after the end tag.
fn fill_raw_text<'a>(
    dom: &mut Dom,
    element: NodeId,
    name: &str,
    decode_refs: bool,
    rest: &'a str,
) -> &'a str {
    let end = find_end_tag(rest, name);
    let content = &rest[..end];
    let text = if decode_refs {
        decode(content)
    } else {
        content.to_owned()
    };
    // A newline right after `<textarea>` or `<pre>` is not part of its text.
    let text = if name == "textarea" {
        text.strip_prefix('\n').unwrap_or(&text).to_owned()
    } else {
        text
    };
    if !text.is_empty() {
        let node = dom.create_text(&text);
        dom.append(element, node);
    }
    rest[end..].find('>').map_or("", |gt| &rest[end + gt + 1..])
}

fn current(stack: &[Open]) -> NodeId {
    stack.last().expect("the parent is always open").id
}

/// Where `</name` begins in `text`, or its end.
fn find_end_tag(text: &str, name: &str) -> usize {
    let needle = format!("</{name}");
    let mut from = 0;
    while let Some(at) = text[from..].find('<') {
        let at = from + at;
        if text
            .get(at..at + needle.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(&needle))
        {
            let after = text[at + needle.len()..].chars().next();
            if after.is_none_or(|c| c == '>' || c.is_whitespace() || c == '/') {
                return at;
            }
        }
        from = at + 1;
    }
    text.len()
}

/// Closes what starting `tag` implies is over.
fn imply_end_tags(dom: &Dom, stack: &mut Vec<Open>, tag: &str) {
    let _ = dom;
    let open = |stack: &Vec<Open>, names: &[&str], fence: &[&str]| -> Option<usize> {
        for (i, o) in stack.iter().enumerate().rev() {
            if i == 0 {
                return None;
            }
            if o.namespace != Namespace::Html {
                return None;
            }
            if names.contains(&o.tag.as_str()) {
                return Some(i);
            }
            if fence.contains(&o.tag.as_str())
                || (names == ["p"].as_slice() && CLOSES_P.contains(&o.tag.as_str()))
            {
                return None;
            }
        }
        None
    };
    let scope_fence: &[&str] = &[
        "table", "td", "th", "caption", "template", "html", "applet", "marquee", "object", "button",
    ];
    if CLOSES_P.contains(&tag)
        && let Some(at) = open(stack, &["p"], scope_fence)
    {
        stack.truncate(at);
    }
    let close_nearest = |stack: &mut Vec<Open>, names: &[&str], fence: &[&str]| {
        if let Some(at) = open(stack, names, fence) {
            stack.truncate(at);
        }
    };
    match tag {
        "li" => close_nearest(stack, &["li"], &["ul", "ol", "menu", "table", "div"]),
        "dt" | "dd" => close_nearest(stack, &["dt", "dd"], &["dl", "table", "div"]),
        "option" => close_nearest(stack, &["option"], &["select", "datalist", "optgroup"]),
        "optgroup" => close_nearest(stack, &["option", "optgroup"], &["select"]),
        "tr" => close_nearest(
            stack,
            &["tr", "td", "th"],
            &["table", "thead", "tbody", "tfoot"],
        ),
        "td" | "th" => close_nearest(stack, &["td", "th"], &["tr", "table"]),
        "thead" | "tbody" | "tfoot" => close_nearest(
            stack,
            &[
                "thead", "tbody", "tfoot", "tr", "td", "th", "caption", "colgroup",
            ],
            &["table"],
        ),
        "caption" | "colgroup" => close_nearest(stack, &["caption", "colgroup"], &["table"]),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            if stack.last().is_some_and(|o| {
                matches!(o.tag.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
            }) =>
        {
            stack.pop();
        }
        _ => {}
    }
}

/// An end tag: closes the nearest open element of that name, and what is open inside it.
fn close(dom: &mut Dom, stack: &mut Vec<Open>, name: &str, context: Context, in_body: &mut bool) {
    if let Context::Document { body, .. } = context {
        match name {
            "head" => {
                *in_body = true;
                stack.truncate(1);
                stack[0] = Open {
                    id: body,
                    tag: "body".into(),
                    namespace: Namespace::Html,
                };
                return;
            }
            "body" | "html" => return,
            _ => {}
        }
    }
    let at = stack
        .iter()
        .rposition(|o| {
            o.tag == name || (o.namespace != Namespace::Html && o.tag.eq_ignore_ascii_case(name))
        })
        .filter(|at| *at > 0);
    match at {
        Some(at) => stack.truncate(at),
        // `</p>` with no `<p>` open is an empty paragraph; `</br>` a `<br>`.
        None if name == "p" || name == "br" => {
            let element = dom.create_element(name);
            dom.append(current(stack), element);
        }
        None => {}
    }
}

fn find_tag_end(text: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in text.char_indices().skip(1) {
        match (quote, c) {
            (None, '"' | '\'') => {
                // A quote only opens a value after `=`.
                let before = text[..i].trim_end();
                if before.ends_with('=') {
                    quote = Some(c);
                }
            }
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

fn split_name(inside: &str) -> (String, &str) {
    let end = inside
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inside.len());
    (inside[..end].to_owned(), &inside[end..])
}

/// Opens the shadow root a `<template shadowrootmode>` of a page stands for, as the shadow root
/// of the element it is in. Answers whether `name` was one.
fn open_shadow_template(
    dom: &mut Dom,
    stack: &mut Vec<Open>,
    context: Context,
    name: &str,
    attrs: &str,
) -> bool {
    if !matches!(context, Context::Document { .. }) || name != "template" {
        return false;
    }
    let Some(root) = declarative_shadow(dom, current(stack), attrs) else {
        return false;
    };
    let namespace = Namespace::Html;
    stack.push(Open {
        id: root,
        tag: name.to_owned(),
        namespace,
    });
    true
}

/// The shadow root `<template shadowrootmode=…>` attributes `host`, if it takes one.
fn declarative_shadow(dom: &mut Dom, host: NodeId, attrs: &str) -> Option<NodeId> {
    let attrs = parse_attrs(attrs);
    let attr = |name: &str| attrs.iter().find(|(key, _)| key.eq_ignore_ascii_case(name));
    let mode = ShadowMode::parse(&attr("shadowrootmode")?.1.to_ascii_lowercase())?;
    if !dom.can_host_shadow(host) {
        return None;
    }
    let delegates = attr("shadowrootdelegatesfocus").is_some();
    dom.attach_shadow(host, mode, delegates, SlotAssignment::Named)
}

fn set_attrs(dom: &mut Dom, element: NodeId, attrs: &str, namespace: Namespace, only_new: bool) {
    for (key, value) in parse_attrs(attrs) {
        let key = if namespace == Namespace::Html {
            key.to_ascii_lowercase()
        } else {
            svg_attribute_case(&key)
        };
        if only_new && dom.get_attr(element, &key).is_some() {
            continue;
        }
        // The first of two attributes of one name wins.
        if !only_new && dom.get(element).is_some_and(|n| n.attr(&key).is_some()) {
            continue;
        }
        dom.set_attr(element, &key, &value);
    }
}

fn svg_attribute_case(name: &str) -> String {
    SVG_ATTRIBUTES
        .iter()
        .find(|t| t.eq_ignore_ascii_case(name))
        .map_or_else(|| name.to_owned(), |t| (*t).to_owned())
}

fn parse_attrs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == '/') {
            i += 1;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '=' && chars[i] != '/' {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        if name.is_empty() {
            i += 1;
            continue;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < chars.len() && chars[i] == '=' {
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
                let quote = chars[i];
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
                value = chars[start..i].iter().collect();
                i += 1;
            } else {
                let start = i;
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                value = chars[start..i].iter().collect();
            }
        }
        out.push((name, decode(&value)));
    }
    out
}

fn push_text(
    dom: &mut Dom,
    stack: &mut Vec<Open>,
    raw: &str,
    references: bool,
    context: Context,
    in_body: &mut bool,
) {
    let text = if references {
        decode(raw)
    } else {
        raw.to_owned()
    };
    if text.is_empty() {
        return;
    }
    // Text before `<body>` that is not just space starts the body.
    if let Context::Document { body, .. } = context
        && !*in_body
    {
        if text.trim().is_empty() {
            return;
        }
        *in_body = true;
        stack.truncate(1);
        stack[0] = Open {
            id: body,
            tag: "body".into(),
            namespace: Namespace::Html,
        };
    }
    let parent = current(stack);
    // Next to a text node already there, the text joins it, as a parser's does.
    if let Some(last) = dom.children(parent).last().copied()
        && dom.get(last).is_some_and(|n| n.is_text())
    {
        dom.append_text(last, &text);
        return;
    }
    let node = dom.create_text(&text);
    dom.append(parent, node);
}

/// Character references read: `&amp;`, `&#39;`, `&#x1F600;`, the named ones.
pub fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let mut end = rest.len().min(40);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let window = &rest[1..end];
        let semi = window.find(';');
        let entity = semi.map(|s| &window[..s]);
        let decoded = entity.and_then(|entity| {
            if let Some(number) = entity.strip_prefix('#') {
                let code = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => number.parse().ok(),
                }?;
                let code = if code == 0 || code > 0x10ffff {
                    0xfffd
                } else {
                    code
                };
                return char::from_u32(code).map(String::from);
            }
            named(entity).map(str::to_owned)
        });
        match (decoded, semi) {
            (Some(text), Some(semi)) => {
                out.push_str(&text);
                rest = &rest[1 + semi + 1..];
            }
            _ => {
                // The old five are read without their `;` too.
                let legacy = ["amp", "lt", "gt", "quot", "nbsp"]
                    .iter()
                    .find(|name| window.starts_with(*name));
                match legacy {
                    Some(name) => {
                        out.push_str(named(name).expect("a known name"));
                        rest = &rest[1 + name.len()..];
                    }
                    None => {
                        out.push('&');
                        rest = &rest[1..];
                    }
                }
            }
        }
    }
    out.push_str(rest);
    out
}

fn named(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "hellip" => "…",
        "mdash" => "—",
        "ndash" => "–",
        "lsquo" => "‘",
        "rsquo" => "’",
        "sbquo" => "‚",
        "ldquo" => "“",
        "rdquo" => "”",
        "bdquo" => "„",
        "laquo" => "«",
        "raquo" => "»",
        "lsaquo" => "‹",
        "rsaquo" => "›",
        "bull" => "•",
        "middot" => "·",
        "deg" => "°",
        "plusmn" => "±",
        "times" => "×",
        "divide" => "÷",
        "minus" => "−",
        "frac12" => "½",
        "frac14" => "¼",
        "frac34" => "¾",
        "sup1" => "¹",
        "sup2" => "²",
        "sup3" => "³",
        "micro" => "µ",
        "para" => "¶",
        "sect" => "§",
        "cent" => "¢",
        "pound" => "£",
        "yen" => "¥",
        "euro" => "€",
        "curren" => "¤",
        "iexcl" => "¡",
        "iquest" => "¿",
        "shy" => "\u{ad}",
        "zwj" => "\u{200d}",
        "zwnj" => "\u{200c}",
        "lrm" => "\u{200e}",
        "rlm" => "\u{200f}",
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "thinsp" => "\u{2009}",
        "larr" => "←",
        "rarr" => "→",
        "uarr" => "↑",
        "darr" => "↓",
        "harr" => "↔",
        "lArr" => "⇐",
        "rArr" => "⇒",
        "hArr" => "⇔",
        "le" => "≤",
        "ge" => "≥",
        "ne" => "≠",
        "asymp" => "≈",
        "equiv" => "≡",
        "infin" => "∞",
        "sum" => "∑",
        "prod" => "∏",
        "radic" => "√",
        "part" => "∂",
        "nabla" => "∇",
        "isin" => "∈",
        "notin" => "∉",
        "cap" => "∩",
        "cup" => "∪",
        "and" => "∧",
        "or" => "∨",
        "forall" => "∀",
        "exist" => "∃",
        "empty" => "∅",
        "prime" => "′",
        "Prime" => "″",
        "dagger" => "†",
        "Dagger" => "‡",
        "permil" => "‰",
        "loz" => "◊",
        "spades" => "♠",
        "clubs" => "♣",
        "hearts" => "♥",
        "diams" => "♦",
        "check" => "✓",
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ε",
        "lambda" => "λ",
        "mu" => "μ",
        "pi" => "π",
        "sigma" => "σ",
        "tau" => "τ",
        "phi" => "φ",
        "omega" => "ω",
        "Delta" => "Δ",
        "Sigma" => "Σ",
        "Omega" => "Ω",
        "Pi" => "Π",
        "aacute" => "á",
        "eacute" => "é",
        "iacute" => "í",
        "oacute" => "ó",
        "uacute" => "ú",
        "agrave" => "à",
        "egrave" => "è",
        "ograve" => "ò",
        "auml" => "ä",
        "euml" => "ë",
        "ouml" => "ö",
        "uuml" => "ü",
        "Auml" => "Ä",
        "Ouml" => "Ö",
        "Uuml" => "Ü",
        "szlig" => "ß",
        "ntilde" => "ñ",
        "ccedil" => "ç",
        "acirc" => "â",
        "ecirc" => "ê",
        "ocirc" => "ô",
        "aring" => "å",
        "aelig" => "æ",
        "oslash" => "ø",
        "Eacute" => "É",
        "tab" | "Tab" => "\t",
        "NewLine" => "\n",
        "colon" => ":",
        "comma" => ",",
        "period" => ".",
        "excl" => "!",
        "quest" => "?",
        "num" => "#",
        "dollar" => "$",
        "percnt" => "%",
        "lpar" => "(",
        "rpar" => ")",
        "ast" => "*",
        "plus" => "+",
        "sol" => "/",
        "bsol" => "\\",
        "semi" => ";",
        "equals" => "=",
        "lsqb" | "lbrack" => "[",
        "rsqb" | "rbrack" => "]",
        "lcub" | "lbrace" => "{",
        "rcub" | "rbrace" => "}",
        "verbar" | "vert" => "|",
        "Hat" => "^",
        "grave" => "`",
        "lowbar" => "_",
        "commat" => "@",
        _ => return None,
    })
}

fn escape(text: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' if !attribute => out.push_str("&lt;"),
            '>' if !attribute => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
    out
}

/// SVG's names that are not all lower case — the document keeps names in lower case, as an HTML
/// document does, and gives SVG's back their case when written out (the HTML standard's
/// "adjust SVG attributes" and tag name tables).
pub(crate) const SVG_TAGS: &[&str] = &[
    "altGlyph",
    "altGlyphDef",
    "altGlyphItem",
    "animateColor",
    "animateMotion",
    "animateTransform",
    "clipPath",
    "feBlend",
    "feColorMatrix",
    "feComponentTransfer",
    "feComposite",
    "feConvolveMatrix",
    "feDiffuseLighting",
    "feDisplacementMap",
    "feDistantLight",
    "feDropShadow",
    "feFlood",
    "feFuncA",
    "feFuncB",
    "feFuncG",
    "feFuncR",
    "feGaussianBlur",
    "feImage",
    "feMerge",
    "feMergeNode",
    "feMorphology",
    "feOffset",
    "fePointLight",
    "feSpecularLighting",
    "feSpotLight",
    "feTile",
    "feTurbulence",
    "foreignObject",
    "glyphRef",
    "linearGradient",
    "radialGradient",
    "textPath",
];
const SVG_ATTRIBUTES: &[&str] = &[
    "attributeName",
    "attributeType",
    "baseFrequency",
    "baseProfile",
    "calcMode",
    "clipPathUnits",
    "diffuseConstant",
    "edgeMode",
    "filterUnits",
    "glyphRef",
    "gradientTransform",
    "gradientUnits",
    "kernelMatrix",
    "kernelUnitLength",
    "keyPoints",
    "keySplines",
    "keyTimes",
    "lengthAdjust",
    "limitingConeAngle",
    "markerHeight",
    "markerUnits",
    "markerWidth",
    "maskContentUnits",
    "maskUnits",
    "numOctaves",
    "pathLength",
    "patternContentUnits",
    "patternTransform",
    "patternUnits",
    "pointsAtX",
    "pointsAtY",
    "pointsAtZ",
    "preserveAlpha",
    "preserveAspectRatio",
    "primitiveUnits",
    "refX",
    "refY",
    "repeatCount",
    "repeatDur",
    "requiredExtensions",
    "requiredFeatures",
    "specularConstant",
    "specularExponent",
    "spreadMethod",
    "startOffset",
    "stdDeviation",
    "stitchTiles",
    "surfaceScale",
    "systemLanguage",
    "tableValues",
    "targetX",
    "targetY",
    "textLength",
    "viewBox",
    "viewTarget",
    "xChannelSelector",
    "yChannelSelector",
    "zoomAndPan",
];

/// An element's name as written out and as `tagName` answers it: an HTML element's in upper
/// case (when `upper`), a foreign one's in its own case.
pub fn qualified_name(dom: &Dom, id: NodeId, upper: bool) -> String {
    let Some(node) = dom.get(id) else {
        return String::new();
    };
    let Some(tag) = node.tag() else {
        return String::new();
    };
    match node.namespace {
        Namespace::Html if upper => tag.to_ascii_uppercase(),
        Namespace::Html => tag.to_owned(),
        Namespace::Svg => svg_case(SVG_TAGS, tag).into_owned(),
        _ => tag.to_owned(),
    }
}

fn svg_case<'a>(table: &[&'static str], name: &'a str) -> std::borrow::Cow<'a, str> {
    match table.iter().find(|t| t.eq_ignore_ascii_case(name)) {
        Some(proper) => std::borrow::Cow::Borrowed(proper),
        None => std::borrow::Cow::Borrowed(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_round_trips() {
        let mut dom = Dom::new();
        let div = dom.create_element("div");
        parse_into(
            &mut dom,
            div,
            "<p class=\"a b\" data-x='1'>Hi &amp; <b>bye</b><br>!</p><!-- c --><input value=\"v\">",
        );
        assert_eq!(
            inner(&dom, div),
            "<p class=\"a b\" data-x=\"1\">Hi &amp; <b>bye</b><br>!</p><!-- c --><input value=\"v\">"
        );
        let input = dom.query(div, "input").unwrap();
        assert_eq!(dom.value(input), "v");
    }

    #[test]
    fn what_html_implies_is_implied() {
        let mut dom = Dom::new();
        let div = dom.create_element("div");
        parse_into(
            &mut dom,
            div,
            "<p>one<p>two<ul><li>a<li>b</ul><table><tr><td>1<td>2</table>\
             <script>if (a < b && c) {}</script><textarea>\n&lt;x&gt;</textarea>\
             <svg viewBox=\"0 0 1 1\"><clipPath id=c><rect/></clipPath></svg>",
        );
        assert_eq!(
            inner(&dom, div),
            "<p>one</p><p>two</p><ul><li>a</li><li>b</li></ul>\
             <table><tbody><tr><td>1</td><td>2</td></tr></tbody></table>\
             <script>if (a < b && c) {}</script><textarea>&lt;x&gt;</textarea>\
             <svg viewBox=\"0 0 1 1\"><clipPath id=\"c\"><rect></rect></clipPath></svg>"
        );
        let textarea = dom.query(div, "textarea").unwrap();
        assert_eq!(dom.value(textarea), "<x>");
        let rect = dom.query(div, "rect").unwrap();
        assert_eq!(dom.get(rect).unwrap().namespace, Namespace::Svg);
    }

    #[test]
    fn a_whole_document_goes_to_its_own_three() {
        let mut dom = Dom::new();
        let (document, _, head, body) = dom.create_html_document();
        parse_document(
            &mut dom,
            document,
            "<!DOCTYPE html><html lang=en><head><title>T</title><meta charset=utf-8></head>\
             <body class=b><h1>Hi</h1></body></html>",
        );
        assert_eq!(
            inner(&dom, head),
            "<title>T</title><meta charset=\"utf-8\">"
        );
        assert_eq!(inner(&dom, body), "<h1>Hi</h1>");
        assert_eq!(dom.get_attr(body, "class").as_deref(), Some("b"));
        assert!(outer(&dom, document).starts_with("<!DOCTYPE html><html lang=\"en\">"));
    }

    fn streamed(chunks: &[&str]) -> (Dom, StreamParser, NodeId) {
        let mut dom = Dom::new();
        let (document, _, _, body) = dom.create_html_document();
        let mut parser = StreamParser::new(&dom, document);
        for chunk in chunks {
            parser.feed(chunk);
        }
        (dom, parser, body)
    }

    #[test]
    fn the_stream_stops_at_each_script_with_the_page_so_far() {
        let (mut dom, mut parser, body) =
            streamed(&["<p>a</p><script>one</script><p>b</p><script>two</script><p>c</p>"]);
        parser.finish();
        let Step::Script(first) = parser.step(&mut dom) else {
            panic!("a script")
        };
        assert_eq!(dom.text_content(first), "one");
        assert_eq!(inner(&dom, body), "<p>a</p><script>one</script>");
        assert!(dom.is_started(first));
        let Step::Script(_) = parser.step(&mut dom) else {
            panic!("another script")
        };
        assert_eq!(parser.step(&mut dom), Step::Done);
        assert_eq!(
            dom.query(body, "p:last-of-type")
                .map(|p| dom.text_content(p)),
            Some("c".into())
        );
    }

    #[test]
    fn what_a_script_writes_goes_in_where_the_parser_is() {
        let (mut dom, mut parser, body) = streamed(&["<p>x</p><script>w</script><i>after</i>"]);
        parser.finish();
        assert!(matches!(parser.step(&mut dom), Step::Script(_)));
        parser.insert("<b>written</b>");
        assert_eq!(parser.step(&mut dom), Step::Done);
        assert_eq!(
            inner(&dom, body),
            "<p>x</p><script>w</script><b>written</b><i>after</i>"
        );
    }

    #[test]
    fn a_tag_split_between_chunks_waits_for_its_end() {
        let (mut dom, mut parser, body) = streamed(&["<p>x</p><div cla"]);
        assert_eq!(parser.step(&mut dom), Step::NeedMore);
        assert_eq!(inner(&dom, body), "<p>x</p>");
        parser.feed("ss=a>y</div><script>un");
        assert_eq!(parser.step(&mut dom), Step::NeedMore);
        assert_eq!(inner(&dom, body), "<p>x</p><div class=\"a\">y</div>");
        parser.feed("finished</script>");
        parser.finish();
        assert!(matches!(parser.step(&mut dom), Step::Script(_)));
        assert_eq!(parser.step(&mut dom), Step::Done);
    }

    #[test]
    fn scripts_made_by_a_fragment_parse_never_run() {
        let mut dom = Dom::new();
        let div = dom.create_element("div");
        parse_into(&mut dom, div, "<script>x</script>");
        let script = dom.query(div, "script").unwrap();
        assert!(dom.is_started(script));
    }

    #[test]
    fn a_copy_of_a_script_that_has_run_has_run_and_a_template_script_has_not() {
        let (mut dom, mut parser, body) = streamed(&[
            "<script>one</script><template><script>two</script></template><link rel=x><p>c</p>",
        ]);
        parser.finish();
        let Step::Script(first) = parser.step(&mut dom) else {
            panic!("a script")
        };
        let copy = dom.clone_node(first, true).unwrap();
        assert!(dom.is_started(copy));
        assert_eq!(parser.step(&mut dom), Step::Done);
        let inert = dom.query(body, "template script").unwrap();
        assert!(!dom.is_started(inert));
        let again = dom.clone_node(inert, true).unwrap();
        assert!(!dom.is_started(again));
        let link = dom.query(body, "link").unwrap();
        assert!(dom.is_parser_made(link));
    }
}
