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

use crate::dom::{Dom, Namespace, NodeId, NodeKind};

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

/// Reads a whole page into a document made with [`Dom::create_html_document`].
pub fn parse_document(dom: &mut Dom, document: NodeId, markup: &str) {
    let Some(html) = dom
        .children(document)
        .iter()
        .copied()
        .find(|c| dom.get(*c).and_then(|n| n.tag()) == Some("html"))
    else {
        return parse_into(dom, document, markup);
    };
    let head = dom.first_tag(html, "head").unwrap_or(html);
    let body = dom.first_tag(html, "body").unwrap_or(html);
    parse(dom, body, markup, Context::Document { html, head, body });
}

struct Open {
    id: NodeId,
    tag: String,
    namespace: Namespace,
}

fn parse(dom: &mut Dom, parent: NodeId, markup: &str, context: Context) {
    let parent_namespace = dom.get(parent).map_or(Namespace::Html, |n| {
        if n.is_element() {
            n.namespace
        } else {
            Namespace::Html
        }
    });
    let mut stack: Vec<Open> = vec![Open {
        id: parent,
        tag: dom
            .get(parent)
            .and_then(|n| n.tag())
            .unwrap_or("")
            .to_owned(),
        namespace: parent_namespace,
    }];
    // In a whole document, what comes before `<body>` that belongs in `<head>` goes there.
    let mut in_body = !matches!(context, Context::Document { .. });
    let mut rest = markup;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            push_text(dom, &mut stack, rest, true, context, &mut in_body);
            break;
        };
        if lt > 0 {
            push_text(dom, &mut stack, &rest[..lt], true, context, &mut in_body);
        }
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            let (data, next) = match after.find("-->") {
                Some(end) => (&after[..end], &after[end + 3..]),
                None => (after, ""),
            };
            let comment = dom.create_comment(data);
            dom.append(current(&stack), comment);
            rest = next;
            continue;
        }
        if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let (data, next) = match after.find("]]>") {
                Some(end) => (&after[..end], &after[end + 3..]),
                None => (after, ""),
            };
            if stack.last().is_some_and(|o| o.namespace != Namespace::Html) {
                push_text(dom, &mut stack, data, false, context, &mut in_body);
            } else {
                let comment = dom.create_comment(&format!("[CDATA[{data}]]"));
                dom.append(current(&stack), comment);
            }
            rest = next;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            let end = rest.find('>').unwrap_or(rest.len());
            let inside = &rest[2..end.max(2)];
            if let Some(name) = inside
                .get(..7)
                .filter(|d| d.eq_ignore_ascii_case("doctype"))
                .map(|_| inside[7..].trim())
            {
                if let Context::Document { .. } = context {
                    let document = dom.root_of(parent);
                    if dom.children(document).iter().all(|c| {
                        !matches!(dom.get(*c).map(|n| &n.kind), Some(NodeKind::Doctype(_)))
                    }) {
                        let doctype =
                            dom.create_doctype(name.split_whitespace().next().unwrap_or("html"));
                        let first = dom.children(document).first().copied();
                        dom.insert_before(document, doctype, first);
                    }
                }
            } else {
                let comment = dom.create_comment(inside.trim_start_matches('-'));
                dom.append(current(&stack), comment);
            }
            rest = rest.get(end + 1..).unwrap_or("");
            continue;
        }
        // `<` not followed by a tag name is text.
        let opens_tag = rest[1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/');
        if !opens_tag {
            push_text(dom, &mut stack, "<", false, context, &mut in_body);
            rest = &rest[1..];
            continue;
        }
        let Some(gt) = find_tag_end(rest) else {
            // An unclosed tag at the end is dropped, as a browser drops it.
            break;
        };
        let inside = &rest[1..gt];
        rest = &rest[gt + 1..];
        if let Some(name) = inside.strip_prefix('/') {
            let name = name
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            close(dom, &mut stack, &name, context, &mut in_body);
            continue;
        }
        let self_closing = inside.trim_end().ends_with('/');
        let inside = inside.trim_end().trim_end_matches('/');
        let (raw_name, attrs) = split_name(inside);
        let name = raw_name.to_ascii_lowercase();
        // A whole document's own three.
        if let Context::Document { html, head, body } = context {
            match name.as_str() {
                "html" => {
                    set_attrs(dom, html, attrs, Namespace::Html, true);
                    continue;
                }
                "head" if !in_body => {
                    set_attrs(dom, head, attrs, Namespace::Html, true);
                    stack.truncate(1);
                    stack[0] = Open {
                        id: head,
                        tag: "head".into(),
                        namespace: Namespace::Html,
                    };
                    continue;
                }
                "body" => {
                    set_attrs(dom, body, attrs, Namespace::Html, true);
                    in_body = true;
                    stack.truncate(1);
                    stack[0] = Open {
                        id: body,
                        tag: "body".into(),
                        namespace: Namespace::Html,
                    };
                    continue;
                }
                "head" => continue,
                tag if !in_body
                    && matches!(
                        tag,
                        "meta" | "title" | "link" | "style" | "script" | "base" | "noscript"
                    )
                    && stack[0].id != head =>
                {
                    stack.truncate(1);
                    stack[0] = Open {
                        id: head,
                        tag: "head".into(),
                        namespace: Namespace::Html,
                    };
                }
                _ if !in_body => {
                    in_body = true;
                    stack.truncate(1);
                    stack[0] = Open {
                        id: body,
                        tag: "body".into(),
                        namespace: Namespace::Html,
                    };
                }
                _ => {}
            }
        } else if matches!(name.as_str(), "html" | "head" | "body") {
            continue;
        }
        let namespace = match (stack.last().map(|o| o.namespace), name.as_str()) {
            (_, "svg") => Namespace::Svg,
            (_, "math") => Namespace::MathMl,
            // HTML inside SVG's `foreignObject` is HTML again.
            (Some(Namespace::Svg), _) if stack.last().is_some_and(|o| o.tag == "foreignobject") => {
                Namespace::Html
            }
            (Some(ns @ (Namespace::Svg | Namespace::MathMl)), _) => ns,
            _ => Namespace::Html,
        };
        if namespace == Namespace::Html {
            imply_end_tags(dom, &mut stack, &name);
        }
        // Rows written straight into a table are in a body of it.
        if namespace == Namespace::Html
            && name == "tr"
            && stack.last().is_some_and(|o| o.tag == "table")
        {
            let tbody = dom.create_element("tbody");
            dom.append(current(&stack), tbody);
            stack.push(Open {
                id: tbody,
                tag: "tbody".into(),
                namespace,
            });
        }
        let element = dom.create_element_ns(&name, namespace);
        set_attrs(dom, element, attrs, namespace, false);
        dom.append(current(&stack), element);
        let closes = if namespace == Namespace::Html {
            VOID.contains(&name.as_str())
        } else {
            self_closing
        };
        if closes {
            continue;
        }
        if namespace == Namespace::Html
            && let Some(decode_refs) = raw_text(&name)
        {
            // Text up to the matching end tag, whatever it holds.
            let end = find_end_tag(rest, &name);
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
            rest = rest[end..].find('>').map_or("", |gt| &rest[end + gt + 1..]);
            continue;
        }
        stack.push(Open {
            id: element,
            tag: name,
            namespace,
        });
    }
}

fn current(stack: &[Open]) -> NodeId {
    stack.last().expect("the parent is always open").id
}

/// Where `</name` begins in `text`, or its end.
fn find_end_tag(text: &str, name: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let needle = format!("</{name}");
    let mut from = 0;
    while let Some(at) = lower[from..].find(&needle) {
        let at = from + at;
        let after = lower[at + needle.len()..].chars().next();
        if after.is_none_or(|c| c == '>' || c.is_whitespace() || c == '/') {
            return at;
        }
        from = at + needle.len();
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
            if fence.contains(&o.tag.as_str()) {
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
        && let Some(NodeKind::Text(existing)) = dom.get(last).map(|n| &n.kind)
    {
        let joined = format!("{existing}{text}");
        dom.set_text(last, &joined);
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
        let window = &rest[1..rest.len().min(40)];
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

/// The node's children as markup.
pub fn inner(dom: &Dom, id: NodeId) -> String {
    let mut out = String::new();
    let raw = dom
        .get(id)
        .and_then(|n| n.tag())
        .is_some_and(|t| raw_text(t) == Some(false));
    for child in dom.children(id) {
        write(dom, *child, &mut out, raw);
    }
    out
}

/// The node itself as markup.
pub fn outer(dom: &Dom, id: NodeId) -> String {
    let mut out = String::new();
    write(dom, id, &mut out, false);
    out
}

fn write(dom: &Dom, id: NodeId, out: &mut String, raw: bool) {
    let Some(node) = dom.get(id) else { return };
    match &node.kind {
        NodeKind::Text(text) if raw => out.push_str(text),
        NodeKind::Text(text) => out.push_str(&escape(text, false)),
        NodeKind::Comment(data) => {
            out.push_str("<!--");
            out.push_str(data);
            out.push_str("-->");
        }
        NodeKind::Doctype(name) => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            out.push('>');
        }
        NodeKind::Element(tag) => {
            let name = qualified_name(dom, id, false);
            out.push('<');
            out.push_str(&name);
            for (key, value) in dom.attributes(id) {
                out.push(' ');
                out.push_str(&key);
                out.push_str("=\"");
                out.push_str(&escape(&value, true));
                out.push('"');
            }
            let html = node.namespace == Namespace::Html;
            if !html && node.children.is_empty() {
                out.push_str("/>");
                return;
            }
            out.push('>');
            if html && VOID.contains(&tag.as_str()) {
                return;
            }
            let raw = html && raw_text(tag) == Some(false);
            for child in &node.children {
                write(dom, *child, out, raw);
            }
            out.push_str("</");
            out.push_str(&name);
            out.push('>');
        }
        NodeKind::Document | NodeKind::Fragment => {
            for child in &node.children {
                write(dom, *child, out, raw);
            }
        }
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
             <svg viewBox=\"0 0 1 1\"><clipPath id=\"c\"><rect/></clipPath></svg>"
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
}
