//! What a markup read finds besides elements: comments, CDATA sections, doctypes and bogus
//! comments, and what reading a tag came to.

use super::{Context, Dom, Namespace, NodeId, NodeKind, Open, current, push_text};

/// What reading a start or end tag came to.
pub(super) enum Tag<'a> {
    /// Read; the markup goes on from here.
    Read(&'a str),
    /// The tag, or the text of the element it opens, has not all arrived: read on from here when
    /// it has.
    Wait(&'a str),
    /// A `<script>` is complete and in the tree; the markup goes on from here.
    Script(&'a str, NodeId),
}

/// A comment whose text follows `<!--` in `after`: the markup after it, or `None` to wait for
/// the end of the comment.
pub(super) fn comment<'a>(
    dom: &mut Dom,
    stack: &[Open],
    after: &'a str,
    eof: bool,
) -> Option<&'a str> {
    let (data, next) = match after.find("-->") {
        Some(end) => (&after[..end], &after[end + 3..]),
        None if !eof => return None,
        None => (after, ""),
    };
    let comment = dom.create_comment(data);
    dom.append(current(stack), comment);
    Some(next)
}

/// A CDATA section whose text follows `<![CDATA[` in `after`: text in foreign content, a
/// comment in HTML. Answers the markup after it.
pub(super) fn cdata<'a>(
    dom: &mut Dom,
    stack: &mut Vec<Open>,
    after: &'a str,
    context: Context,
    in_body: &mut bool,
) -> &'a str {
    let (data, next) = match after.find("]]>") {
        Some(end) => (&after[..end], &after[end + 3..]),
        None => (after, ""),
    };
    if stack.last().is_some_and(|o| o.namespace != Namespace::Html) {
        push_text(dom, stack, data, false, context, in_body);
    } else {
        let comment = dom.create_comment(&format!("[CDATA[{data}]]"));
        dom.append(current(stack), comment);
    }
    next
}

/// `<!doctype …>`, or a bogus comment (`<!…>`, `<?…>`) at the start of `rest`: the markup after.
pub(super) fn declaration<'a>(
    dom: &mut Dom,
    stack: &[Open],
    context: Context,
    rest: &'a str,
) -> &'a str {
    let end = rest.find('>').unwrap_or(rest.len());
    let inside = &rest[2..end.max(2)];
    let doctype = inside
        .get(..7)
        .filter(|d| d.eq_ignore_ascii_case("doctype"))
        .map(|_| inside[7..].trim());
    match doctype {
        Some(name) => {
            if let Context::Document { .. } = context {
                let document = dom.root_of(stack[0].id);
                let has = dom
                    .children(document)
                    .iter()
                    .any(|c| matches!(dom.get(*c).map(|n| &n.kind), Some(NodeKind::Doctype(_))));
                if !has {
                    let doctype =
                        dom.create_doctype(name.split_whitespace().next().unwrap_or("html"));
                    let first = dom.children(document).first().copied();
                    dom.insert_before(document, doctype, first);
                }
            }
        }
        None => {
            let comment = dom.create_comment(inside.trim_start_matches('-'));
            dom.append(current(stack), comment);
        }
    }
    rest.get(end + 1..).unwrap_or("")
}
