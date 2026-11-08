//! Inline `<svg>` and SVG pictures: the subtree written out with its computed styles, measured
//! and rasterised with usvg and resvg (the copy GPUI links, with the same features).
//!
//! The layout writes an `<svg>` element as text, sized to its content box, and puts the text in
//! the display list ([`crate::VectorSpec`]); the shell rasterises the text at the window's scale.
//! The same tree gives the boxes of the elements inside, which `getBBox` and
//! `getBoundingClientRect` answer.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use resvg::{tiny_skia, usvg};
use verve_dom::dom::Namespace;
use verve_dom::html::qualified_name;
use verve_dom::{NodeId, NodeKind, Rect, Snapshot};

use crate::display_list::ImageKey;
use crate::images::KIND_BITS;
use crate::layout::Laid;
use crate::place::Placed;
use crate::tree::BoxTree;

/// The properties of an SVG element that CSS can set and that usvg reads from a `style`
/// attribute. The computed value of each is written onto the element that has it.
const PROPERTIES: &[&str] = &[
    "stop-color",
    "stop-opacity",
    "opacity",
    "mix-blend-mode",
    "isolation",
    "dominant-baseline",
    "clip-path",
    "mask",
    "filter",
];

/// What the layout applies to the picture of the `<svg>` box, so that it is not applied again
/// inside: only the opacity, which it gives the picture as its alpha. A filter, a blend mode, a
/// clip path or a mask on the root is nothing the layout paints, and the drawing applies them
/// (Electron 39.8.10: `<svg style="filter:invert(1)">` over a red rect paints cyan).
const ROOT_SKIPS: &[&str] = &["opacity"];

/// The properties that are inherited by every element in CSS, so that they are written on the
/// root and wherever they differ from the parent's: not on every element, which would beat the
/// presentation attributes of the descendants (Electron 39.8.10: `<div style="fill:red">` over a
/// `<rect fill="blue">` paints blue).
const INHERITED: &[&str] = &[
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-opacity",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "clip-rule",
    "paint-order",
    "shape-rendering",
    "text-anchor",
    "marker-start",
    "marker-mid",
    "marker-end",
    "color",
    "visibility",
    "font-family",
    "font-size",
    "font-style",
    "font-weight",
    "font-stretch",
    "letter-spacing",
    "word-spacing",
    "text-rendering",
];

/// An `<svg>` written out.
pub(crate) struct Written {
    pub text: String,
    /// The element each `id` in `text` is.
    pub ids: HashMap<String, NodeId>,
    /// How many ids have been made up: the next one is `__v` and this. Ids are numbered by the
    /// order of the elements, not by their nodes, so that two copies of one icon are one text.
    made: usize,
    /// The `#id` targets of `href`s, in the order met.
    refs: Vec<String>,
    /// Elements are being copied in from outside the drawing: they get no ids of their own.
    in_defs: bool,
}

/// Whether `node` is the outermost `<svg>` of a drawing: the element that is a box of its own.
pub(crate) fn is_root(snapshot: &Snapshot, node: NodeId) -> bool {
    let dom = &snapshot.dom;
    let Some(found) = dom.get(node) else {
        return false;
    };
    found.tag() == Some("svg")
        && found.namespace == Namespace::Svg
        && !dom
            .parent(node)
            .and_then(|parent| dom.get(parent))
            .is_some_and(|parent| parent.namespace == Namespace::Svg)
}

/// The key of an inline SVG's text: the same text is the same picture.
pub(crate) fn key_of(text: &str) -> ImageKey {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    ImageKey(hasher.finish() & !KIND_BITS)
}

/// The `<svg>` at `root` as text, `width` by `height` CSS pixels.
pub(crate) fn write(snapshot: &Snapshot, root: NodeId, size: [f32; 2]) -> Written {
    let mut out = Written {
        text: String::new(),
        ids: HashMap::new(),
        made: 0,
        refs: Vec::new(),
        in_defs: false,
    };
    // A `<use>` that points outside the drawing (an icon sprite in a hidden `<svg>`) draws what
    // it points at: the targets are copied in after the content, in a `<defs>`, and so are the
    // targets of those.
    let mut done = std::collections::HashSet::new();
    let mut at = 0;
    let mut tail = String::new();
    element(snapshot, root, Some(size), &mut out);
    let close = out.text.rfind("</").unwrap_or(out.text.len());
    let mut body = out.text.split_off(close);
    out.in_defs = true;
    while at < out.refs.len() {
        let target = out.refs[at].clone();
        at += 1;
        if out.ids.contains_key(&target) || !done.insert(target.clone()) {
            continue;
        }
        if let Some(node) = find_id(snapshot, &target) {
            let before = std::mem::take(&mut out.text);
            element(snapshot, node, None, &mut out);
            tail.push_str(&std::mem::replace(&mut out.text, before));
        }
    }
    if !tail.is_empty() {
        out.text.push_str("<defs>");
        out.text.push_str(&tail);
        out.text.push_str("</defs>");
    }
    out.text.push_str(&std::mem::take(&mut body));
    out
}

/// The SVG element of the document whose `id` is `id`: the first in tree order, as
/// `getElementById` finds it.
fn find_id(snapshot: &Snapshot, id: &str) -> Option<NodeId> {
    let dom = &snapshot.dom;
    let mut stack = vec![dom.html];
    while let Some(node) = stack.pop() {
        let found = dom.get(node)?;
        if found.attr("id") == Some(id) {
            return (found.namespace == Namespace::Svg).then_some(node);
        }
        stack.extend(dom.children(node).iter().rev().copied());
    }
    None
}

/// The elements whose `href` copies what it points at into the drawing.
fn follows_href(tag: &str) -> bool {
    matches!(
        tag,
        "use" | "feimage" | "textpath" | "mpath" | "lineargradient" | "radialgradient" | "pattern"
    )
}

/// Whether `name` can be an attribute name in the XML usvg reads: a name without a colon, or an
/// `xlink:` or `xml:` one. A name with another prefix has no namespace declared and the whole
/// drawing would not parse; a name with other characters does not parse at all.
fn writable_name(name: &str) -> bool {
    let plain = |part: &str| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match name.split_once(':') {
        None => plain(name),
        Some(("xlink" | "xml", rest)) => plain(rest),
        Some(_) => false,
    }
}

fn element(snapshot: &Snapshot, id: NodeId, size: Option<[f32; 2]>, out: &mut Written) {
    let dom = &snapshot.dom;
    let Some(node) = dom.get(id) else { return };
    match &node.kind {
        NodeKind::Text(text) => escape(text, &mut out.text),
        NodeKind::Element(tag) => {
            if matches!(tag.as_str(), "style" | "script" | "foreignobject") {
                return;
            }
            let name = qualified_name(dom, id, false);
            // An element of another vocabulary (Inkscape's `sodipodi:namedview`, `rdf:RDF`) has no
            // namespace declared here: it and what is in it would blank the drawing.
            if !writable_name(&name) || name.contains(':') {
                return;
            }
            out.text.push('<');
            out.text.push_str(&name);
            if size.is_some() {
                out.text.push_str(
                    " xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\"",
                );
            }
            attributes(snapshot, id, size, out);
            out.text.push('>');
            for child in dom.flat_children(id) {
                element(snapshot, child, None, out);
            }
            out.text.push_str("</");
            out.text.push_str(&name);
            out.text.push('>');
        }
        _ => {}
    }
}

fn attributes(snapshot: &Snapshot, id: NodeId, size: Option<[f32; 2]>, out: &mut Written) {
    let mut has_id = false;
    for (key, value) in snapshot.dom.attributes(id) {
        let key_lower = key.to_ascii_lowercase();
        if key_lower == "style" || key_lower.starts_with("xmlns") || !writable_name(&key) {
            continue;
        }
        if matches!(key_lower.as_str(), "href" | "xlink:href")
            && snapshot
                .dom
                .get(id)
                .and_then(|n| n.tag())
                .is_some_and(follows_href)
            && let Some(target) = value.strip_prefix('#')
        {
            out.refs.push(target.to_owned());
        }
        if size.is_some() && matches!(key_lower.as_str(), "width" | "height") {
            continue;
        }
        if key_lower == "id" {
            has_id = true;
            out.ids.insert(value.clone(), id);
        }
        pair(&key, &value, &mut out.text);
    }
    if !has_id && !out.in_defs {
        let synthetic = format!("__v{}", out.made);
        out.made += 1;
        pair("id", &synthetic, &mut out.text);
        out.ids.insert(synthetic, id);
    }
    if let Some([width, height]) = size {
        pair("width", &width.to_string(), &mut out.text);
        pair("height", &height.to_string(), &mut out.text);
    }
    let style = computed(snapshot, id, size.is_some());
    if !style.is_empty() {
        pair("style", &style, &mut out.text);
    }
}

/// The computed style usvg needs: what CSS set on the element, and the inherited properties
/// where they differ from the parent's.
fn computed(snapshot: &Snapshot, id: NodeId, root: bool) -> String {
    let mut style = String::new();
    let parent = snapshot
        .dom
        .parent(id)
        .filter(|parent| !root && snapshot.dom.get(*parent).is_some());
    for property in PROPERTIES {
        // The layout fades, blends, clips and masks the picture of a box as a whole: the root
        // element does it once, not again inside the drawing.
        if root && ROOT_SKIPS.contains(property) {
            continue;
        }
        if let Some(value) = snapshot.style(id, property)
            && !matches!(value, "" | "normal" | "auto")
        {
            declare(&mut style, property, value);
        }
    }
    for property in INHERITED {
        let value = snapshot.style(id, property);
        let before = parent.and_then(|parent| snapshot.style(parent, property));
        if let Some(value) = value
            && (parent.is_none() || before != Some(value))
        {
            declare(&mut style, property, value);
        }
    }
    if snapshot.style(id, "display") == Some("none") {
        declare(&mut style, "display", "none");
    }
    style
}

fn declare(style: &mut String, property: &str, value: &str) {
    style.push_str(property);
    style.push(':');
    style.push_str(value);
    style.push(';');
}

fn pair(key: &str, value: &str, out: &mut String) {
    out.push(' ');
    out.push_str(key);
    out.push_str("=\"");
    escape(value, out);
    out.push('"');
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // Not in XML at all: one would fail the whole drawing.
            '\u{1}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' => {}
            c => out.push(c),
        }
    }
}

/// The fonts SVG text is set in: the system's, loaded when the first drawing with text needs
/// them.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    Arc::clone(FONTS.get_or_init(|| {
        let mut database = usvg::fontdb::Database::new();
        database.load_system_fonts();
        Arc::new(database)
    }))
}

/// The tree of `source`, if it is SVG.
pub fn parse(source: &[u8]) -> Option<usvg::Tree> {
    // A drawing reads nothing from the machine or the network: `<image>` and `feImage` take a
    // `data:` address and no other (Chromium: an http page, and an SVG in an `<img>`, load no
    // file and make no request). The default resolver would `fs::read` any path, UNC ones too.
    let mut options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    if source.windows(5).any(|w| w == b"<text") {
        options.fontdb = fonts();
    }
    usvg::Tree::from_data(source, &options).ok()
}

/// `source` drawn into `width` by `height` device pixels, as straight-alpha BGRA.
pub fn rasterize(source: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let tree = parse(source)?;
    let size = tree.size();
    if size.width() <= 0. || size.height() <= 0. {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let scale = tiny_skia::Transform::from_scale(
        width as f32 / size.width(),
        height as f32 / size.height(),
    );
    resvg::render(&tree, scale, &mut pixmap.as_mut());
    let mut bytes = pixmap.take();
    verve_dom::canvas::unpremultiply(&mut bytes);
    for pixel in bytes.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(bytes)
}

/// The box of each element of `text` that has an `id`, in the drawing's own pixels (its content
/// box's top left is the origin): the fill's bounding box, as `getBBox` answers it. The drawing
/// itself is under `""`.
fn id_boxes(text: &str) -> HashMap<String, Boxes> {
    let mut out = HashMap::new();
    let Some(tree) = parse(text.as_bytes()) else {
        return out;
    };
    let root = tree.root();
    // A drawing with a viewBox is a group that scales its content, without an id of its own: the
    // content is measured in the units inside it.
    let inside = match root.children() {
        [usvg::Node::Group(group)] if group.id().is_empty() => group,
        _ => root,
    };
    let both = Boxes {
        local: rect_of(inside.bounding_box()),
        window: rect_of(root.abs_bounding_box()),
    };
    out.insert(String::new(), both);
    collect(root, &mut out);
    out
}

fn collect(group: &usvg::Group, out: &mut HashMap<String, Boxes>) {
    for child in group.children() {
        if !child.id().is_empty() {
            let both = Boxes {
                local: rect_of(child.bounding_box()),
                window: rect_of(child.abs_bounding_box()),
            };
            out.insert(child.id().to_owned(), both);
        }
        if let usvg::Node::Group(inner) = child {
            collect(inner, out);
        }
    }
}

/// The box of an element of a drawing: in its own coordinates, in the user space of the element
/// (`getBBox`, which leaves out the stroke and the element's own transform), and in the
/// drawing's pixels (what `getBoundingClientRect` answers, moved to the page).
#[derive(Clone, Copy)]
pub(crate) struct Boxes {
    pub local: Rect,
    pub window: Rect,
}

/// The boxes inside drawings, worked out once for each text: its key and the box of each id.
pub(crate) type Cache = HashMap<ImageKey, Arc<HashMap<String, Boxes>>>;

/// One inline `<svg>` laid out.
pub(crate) struct Drawing {
    pub key: ImageKey,
    pub source: Arc<[u8]>,
    /// The boxes of the elements inside, relative to the top left of the content box.
    pub boxes: Vec<(NodeId, Rect)>,
    /// What `getBBox` answers for each of them, and for the drawing itself.
    pub bboxes: Vec<(NodeId, Rect)>,
}

/// The content box of box `index` when its border box is `rect`.
pub(crate) fn content_box(rect: Rect, laid: &Laid, index: usize) -> Rect {
    let [top, right, bottom, left] = laid.border[index];
    let [pad_top, pad_right, pad_bottom, pad_left] = laid.padding[index];
    Rect::new(
        rect.x + left + pad_left,
        rect.y + top + pad_top,
        rect.width - left - right - pad_left - pad_right,
        rect.height - top - bottom - pad_top - pad_bottom,
    )
}

/// The inline SVGs of the page, by box, written at the size they are laid out. `cache` holds
/// what the last layout worked out and keeps what this one uses.
pub(crate) fn drawings(
    snapshot: &Snapshot,
    tree: &BoxTree,
    laid: &Laid,
    placed: &Placed,
    cache: &mut Cache,
) -> HashMap<usize, Drawing> {
    let mut out = HashMap::new();
    let mut kept = Cache::new();
    for (index, found) in tree.boxes.iter().enumerate() {
        if found.anonymous || !is_root(snapshot, found.node) {
            continue;
        }
        let content = content_box(placed.rect[index], laid, index);
        if content.width <= 0. || content.height <= 0. {
            continue;
        }
        let written = write(snapshot, found.node, [content.width, content.height]);
        let key = key_of(&written.text);
        let known = cache
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Arc::new(id_boxes(&written.text)));
        kept.insert(key, Arc::clone(&known));
        let boxes = written
            .ids
            .iter()
            .filter_map(|(id, node)| Some((*node, known.get(id)?.window)))
            .collect();
        // The drawing itself is measured by `getBBox` alone: its box is the layout's.
        let bboxes = written
            .ids
            .iter()
            .filter_map(|(id, node)| Some((*node, known.get(id)?.local)))
            .chain(known.get("").map(|both| (found.node, both.local)))
            .collect();
        out.insert(
            index,
            Drawing {
                key,
                source: Arc::from(written.text.into_bytes()),
                boxes,
                bboxes,
            },
        );
    }
    *cache = kept;
    out
}

/// The boxes of the elements inside the drawings, in the window: each drawing's boxes moved to
/// where its content box is.
pub(crate) fn client_rects<'a>(
    laid: &'a Laid,
    placed: &'a Placed,
    drawings: &'a HashMap<usize, Drawing>,
) -> impl Iterator<Item = (NodeId, Rect)> + 'a {
    drawings.iter().flat_map(move |(index, drawing)| {
        let origin = content_box(placed.rect[*index], laid, *index);
        drawing.boxes.iter().map(move |(node, rect)| {
            (
                *node,
                Rect::new(
                    rect.x + origin.x,
                    rect.y + origin.y,
                    rect.width,
                    rect.height,
                ),
            )
        })
    })
}

fn rect_of(rect: usvg::Rect) -> Rect {
    Rect::new(rect.x(), rect.y(), rect.width(), rect.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="4"><rect id="r" width="8" height="4" fill="#ff0000"/></svg>"##;

    #[test]
    fn a_drawing_is_rasterised_to_straight_bgra_at_the_size_asked() {
        let bytes = rasterize(DOT.as_bytes(), 16, 8).expect("drawn");
        assert_eq!(bytes.len(), 16 * 8 * 4);
        // Opaque red, as BGRA, over the whole drawing.
        assert_eq!(&bytes[..4], &[0, 0, 255, 255]);
        assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 255, 255]);
    }

    #[test]
    fn text_that_is_not_an_svg_does_not_draw() {
        assert!(rasterize(b"not a drawing", 4, 4).is_none());
    }

    #[test]
    fn the_same_text_is_the_same_key_and_never_a_canvas_or_gradient() {
        assert_eq!(key_of(DOT), key_of(DOT));
        assert_ne!(key_of(DOT), key_of("<svg/>"));
        assert_eq!(key_of(DOT).0 & KIND_BITS, 0);
    }
}

#[cfg(test)]
mod reading {
    use super::*;

    // A 1x1 PNG of one opaque red pixel.
    const RED: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[test]
    fn an_image_that_names_a_file_draws_nothing() {
        // usvg's default resolver would read the file (a path, or a UNC one); a drawing may name
        // only `data:` pictures.
        let path = std::env::temp_dir().join(format!("verve-svg-image-{}.png", std::process::id()));
        std::fs::write(&path, RED).unwrap();
        let href = path
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="4" height="4"><image href="{href}" xlink:href="{href}" width="4" height="4"/></svg>"#
        );
        let bytes = rasterize(svg.as_bytes(), 4, 4).expect("the drawing parses");
        let _ = std::fs::remove_file(&path);
        assert!(
            bytes.chunks_exact(4).all(|pixel| pixel[3] == 0),
            "a file was drawn"
        );
    }
}
