//! What text takes from its element's computed style: the font, the spacing, the way lines break
//! and align, the colour and the decorations.
//!
//! The cascade leaves a value as text, and it passes an inherited length down as written (`1.5em`
//! on a parent reaches the child as `1.5em`, though CSS says the child gets what it came to on the
//! parent). [`Reader::inherited_length`] therefore finds the element the value was written on and
//! reads it against that element's font size.

use std::sync::Arc;

use verve_dom::css::{Style, color_with};
use verve_dom::{NodeId, Snapshot};

use super::breaks::{OverflowWrap, WordBreak};
use super::space::{Transform, WhiteSpace};
use crate::display_list::{Border, DecorationKind, Rgba};
use crate::fonts::{self, FontRegistry, Platform};
use crate::measure::{FontKey, FontMetrics, TextMeasurer};
use crate::paint_style::{Paint, border_side_width};
use crate::shadow;
use crate::value::{Dim, Units, length_percent, number, pixels, words};

/// `line-height`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum LineHeight {
    /// The font's own: ascent, descent and line gap.
    Normal,
    /// A multiple of the font size.
    Number(f32),
    Length(f32),
}

/// `text-align`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum TextAlign {
    Left,
    Right,
    Center,
    Justify,
    Start,
    End,
}

/// `vertical-align`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum VerticalAlign {
    Baseline,
    Sub,
    Super,
    TextTop,
    TextBottom,
    Middle,
    Top,
    Bottom,
    /// Raised by this many pixels.
    Length(f32),
    /// Raised by this share of the box's `line-height`.
    Percent(f32),
}

/// The line a `text-decoration` draws.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DecorationStyle {
    Solid,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

/// One decoration of a piece of text: `text-decoration: underline wavy red`.
#[derive(Clone, Debug, PartialEq)]
pub struct TextDecoration {
    pub kind: DecorationKind,
    pub color: Rgba,
    pub style: DecorationStyle,
    /// `text-decoration-thickness` as a length; `None` is `auto` (the font's).
    pub thickness: Option<f32>,
    /// `text-underline-offset` as a length; `None` is `auto`.
    pub offset: Option<f32>,
}

/// What an inline element paints of its own box: see [`SpanBox::paint`](super::SpanBox).
#[derive(Clone, Debug, PartialEq)]
pub struct SpanPaint {
    pub background: Rgba,
    pub border: Border,
    /// The radii that are lengths, top left, top right, bottom right, bottom left.
    pub radii: [f32; 4],
}

/// One `text-shadow`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TextShadow {
    pub offset: [f32; 2],
    pub blur: f32,
    pub color: Rgba,
}

/// How a run of text is painted: its colour, the decorations drawn through it (those of its
/// element and of every ancestor that propagates them) and its shadow.
#[derive(Clone, Debug, PartialEq)]
pub struct TextPaint {
    pub color: Rgba,
    pub decorations: Vec<TextDecoration>,
    pub shadow: Option<TextShadow>,
}

/// Everything layout reads of an element to lay its text out.
#[derive(Clone, Debug)]
pub(crate) struct TextStyle {
    pub size: f32,
    /// The font the families, weight and slant come to, and what it measures at `size`.
    pub font: FontKey,
    pub metrics: FontMetrics,
    pub line_height: LineHeight,
    pub white_space: WhiteSpace,
    pub word_break: WordBreak,
    pub overflow_wrap: OverflowWrap,
    pub align: TextAlign,
    /// `text-indent`: pixels or a share of the containing block.
    pub indent: Dim,
    pub transform: Transform,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub vertical_align: VerticalAlign,
    pub color: Rgba,
    pub rtl: bool,
    pub tab_size: usize,
    /// `text-overflow: ellipsis`.
    pub ellipsis: bool,
    /// `-webkit-line-clamp`.
    pub line_clamp: Option<u32>,
    /// What the element's own `text-decoration` adds; the ancestors' are added when its text is
    /// collected.
    pub decorations: Vec<TextDecoration>,
    pub shadow: Option<TextShadow>,
    /// `user-select: none`: the text takes no part in hit testing for selections.
    pub select_none: bool,
}

impl TextStyle {
    /// How the text is painted, given the decorations that come down from the ancestors.
    pub(crate) fn paint(&self, inherited: &[TextDecoration]) -> TextPaint {
        let mut decorations = inherited.to_vec();
        decorations.extend(self.decorations.iter().cloned());
        TextPaint {
            color: self.color,
            decorations,
            shadow: self.shadow,
        }
    }
}

/// What reading a style needs besides the node.
pub(crate) struct Env<'a> {
    pub measurer: &'a dyn TextMeasurer,
    pub snapshot: &'a Snapshot,
    pub root_font_size: f32,
    pub platform: Platform,
    pub registry: &'a FontRegistry,
}

/// The values of one style, by property name: an element's in the snapshot, or the style of a
/// generated box.
enum Values<'a> {
    Node(NodeId),
    Map(&'a Style),
}

struct Reader<'a> {
    env: &'a Env<'a>,
    /// The element the values are of (a generated box reads its owner's chain).
    node: NodeId,
    values: Values<'a>,
    units: Units,
}

impl<'a> Reader<'a> {
    fn text(&self, property: &str) -> Option<&str> {
        match &self.values {
            Values::Node(node) => self.env.snapshot.style(*node, property),
            Values::Map(map) => map.get(property).map(String::as_str),
        }
    }

    fn text_or<'s>(&'s self, property: &str, default: &'s str) -> &'s str {
        self.text(property).unwrap_or(default)
    }

    /// A length written on this element or inherited from an ancestor, in pixels. An inherited
    /// `em` is the font size of the ancestor it was written on.
    fn inherited_length(&self, property: &str) -> Option<Dim> {
        self.inherited_with_size(property).map(|(dim, _)| dim)
    }

    /// The same, and the font size of the element that wrote it: a percentage of it is of that
    /// element's font size, not of the one that inherited it.
    fn inherited_with_size(&self, property: &str) -> Option<(Dim, f32)> {
        let raw = self.text(property)?;
        let mut at = self.node;
        let dom = &self.env.snapshot.dom;
        while let Some(parent) = dom.get(at).and_then(|n| n.parent) {
            if self.env.snapshot.style(parent, property) == Some(raw) {
                at = parent;
            } else {
                break;
            }
        }
        let size = if at == self.node {
            self.units.font_size
        } else {
            font_size_of(self.env, at)
        };
        let units = Units::new(size, self.env.root_font_size, self.env.snapshot.viewport);
        Some((length_percent(raw, &units)?, size))
    }
}

/// An element's computed font size in pixels.
fn font_size_of(env: &Env, node: NodeId) -> f32 {
    env.snapshot
        .style(node, "font-size")
        .and_then(|text| {
            pixels(
                text,
                &Units::new(16., env.root_font_size, env.snapshot.viewport),
            )
        })
        .unwrap_or(16.)
}

/// The numeric weight a `font-weight` value comes to on `node`: `bolder` and `lighter` step from
/// the parent's.
fn weight_of(env: &Env, node: NodeId, depth: u32) -> f32 {
    let text = env.snapshot.style(node, "font-weight").unwrap_or("normal");
    match text.trim() {
        "normal" => 400.,
        "bold" => 700.,
        "bolder" | "lighter" if depth < 64 => {
            // The value is inherited as it was written, so the step is taken once, from the
            // parent of the element that wrote it.
            let dom = &env.snapshot.dom;
            let mut writer = node;
            while let Some(parent) = dom.get(writer).and_then(|n| n.parent) {
                if env.snapshot.style(parent, "font-weight") == Some(text) {
                    writer = parent;
                } else {
                    break;
                }
            }
            let parent = dom
                .get(writer)
                .and_then(|n| n.parent)
                .map_or(400., |parent| weight_of(env, parent, depth + 1));
            step_weight(parent, text.trim() == "bolder")
        }
        other => number(other).map_or(400., |w| w.clamp(1., 1000.)),
    }
}

/// CSS Fonts 4's relative weight table.
fn step_weight(parent: f32, bolder: bool) -> f32 {
    match (bolder, parent) {
        (true, w) if w < 350. => 400.,
        (true, w) if w < 550. => 700.,
        (true, w) => w.max(900.),
        (false, w) if w < 100. => w,
        (false, w) if w < 550. => 100.,
        (false, w) if w < 750. => 400.,
        (false, _) => 700.,
    }
}

/// The text style of an element, read from the snapshot.
pub(crate) fn read(env: &Env, node: NodeId) -> TextStyle {
    let size = font_size_of(env, node);
    let reader = Reader {
        env,
        node,
        values: Values::Node(node),
        units: Units::new(size, env.root_font_size, env.snapshot.viewport),
    };
    from_reader(&reader, size, weight_of(env, node, 0))
}

/// The text style of a generated box (`::before`, `::after`) of `owner`: the box's own computed
/// style.
pub(crate) fn read_generated(env: &Env, owner: NodeId, style: &Style) -> TextStyle {
    let size = style
        .get("font-size")
        .and_then(|text| {
            pixels(
                text,
                &Units::new(16., env.root_font_size, env.snapshot.viewport),
            )
        })
        .unwrap_or_else(|| font_size_of(env, owner));
    let reader = Reader {
        env,
        node: owner,
        values: Values::Map(style),
        units: Units::new(size, env.root_font_size, env.snapshot.viewport),
    };
    let weight = match style.get("font-weight").map(|t| t.trim()) {
        Some("bold") => 700.,
        Some("normal") | None => weight_of(env, owner, 0),
        Some(other) => number(other).unwrap_or(400.),
    };
    from_reader(&reader, size, weight)
}

fn from_reader(reader: &Reader, size: f32, weight: f32) -> TextStyle {
    let color = reader
        .text("color")
        .and_then(|text| color_with(text, [0., 0., 0., 1.]))
        .unwrap_or([0., 0., 0., 1.]);
    let declared = fonts::split_families(reader.text_or("font-family", ""));
    let lang = lang_of(reader.env.snapshot, reader.node);
    let families = fonts::stack(
        reader.env.platform,
        &declared,
        lang.as_deref(),
        reader.env.registry,
    );
    let italic = matches!(
        reader
            .text_or("font-style", "normal")
            .split_whitespace()
            .next(),
        Some("italic" | "oblique")
    );
    let wrap = reader
        .text("overflow-wrap")
        .filter(|text| *text != "normal")
        .or_else(|| reader.text("word-wrap"))
        .unwrap_or("normal");
    let mut word_break = reader.text_or("word-break", "normal").trim();
    let mut overflow_wrap = OverflowWrap::parse(wrap);
    // The old `word-break: break-word` is `overflow-wrap: anywhere` with the normal word-break.
    if word_break == "break-word" {
        overflow_wrap = OverflowWrap::Anywhere;
        word_break = "normal";
    }
    let font = reader.env.measurer.resolve(&families, weight, italic);
    TextStyle {
        size,
        font,
        metrics: reader.env.measurer.metrics(font, size),
        line_height: line_height(reader),
        white_space: WhiteSpace::parse(reader.text_or("white-space", "normal")),
        word_break: WordBreak::parse(word_break),
        overflow_wrap,
        align: text_align(reader.text_or("text-align", "start")),
        indent: reader
            .inherited_length("text-indent")
            .unwrap_or(Dim::Px(0.)),
        transform: Transform::parse(reader.text_or("text-transform", "none")),
        letter_spacing: spacing(reader, "letter-spacing"),
        word_spacing: spacing(reader, "word-spacing"),
        vertical_align: vertical_align(reader),
        color,
        rtl: is_rtl(reader),
        tab_size: tab_size(reader),
        ellipsis: reader.text_or("text-overflow", "clip").trim() == "ellipsis",
        line_clamp: reader
            .text("-webkit-line-clamp")
            .and_then(|text| text.trim().parse::<u32>().ok())
            .filter(|lines| *lines > 0),
        decorations: decorations(reader, color),
        shadow: reader
            .text("text-shadow")
            .and_then(|text| shadow::parse(text, color, &reader.units).into_iter().next())
            .map(|shadow| TextShadow {
                offset: shadow.offset,
                blur: shadow.blur,
                color: shadow.color,
            }),
        select_none: matches!(
            reader
                .text("user-select")
                .or_else(|| reader.text("-webkit-user-select")),
            Some("none")
        ),
    }
}

fn line_height(reader: &Reader) -> LineHeight {
    let Some(text) = reader.text("line-height").map(str::trim) else {
        return LineHeight::Normal;
    };
    if text == "normal" || text.is_empty() {
        return LineHeight::Normal;
    }
    if let Some(factor) = number(text) {
        return if factor >= 0. {
            LineHeight::Number(factor)
        } else {
            LineHeight::Normal
        };
    }
    match reader.inherited_with_size("line-height") {
        Some((Dim::Px(px), _)) if px >= 0. => LineHeight::Length(px),
        Some((Dim::Percent(share), writer)) if share >= 0. => LineHeight::Length(share * writer),
        _ => LineHeight::Normal,
    }
}

fn spacing(reader: &Reader, property: &str) -> f32 {
    match reader.inherited_length(property) {
        Some(Dim::Px(px)) => px,
        _ => 0.,
    }
}

fn text_align(text: &str) -> TextAlign {
    match text.split_whitespace().next() {
        Some("left" | "-webkit-left" | "-moz-left") => TextAlign::Left,
        Some("right" | "-webkit-right" | "-moz-right") => TextAlign::Right,
        Some("center" | "-webkit-center" | "-moz-center") => TextAlign::Center,
        Some("justify") => TextAlign::Justify,
        Some("end") => TextAlign::End,
        _ => TextAlign::Start,
    }
}

fn vertical_align(reader: &Reader) -> VerticalAlign {
    let Some(text) = reader.text("vertical-align").map(str::trim) else {
        return VerticalAlign::Baseline;
    };
    match text {
        "sub" => VerticalAlign::Sub,
        "super" => VerticalAlign::Super,
        "text-top" => VerticalAlign::TextTop,
        "text-bottom" => VerticalAlign::TextBottom,
        "middle" => VerticalAlign::Middle,
        "top" => VerticalAlign::Top,
        "bottom" => VerticalAlign::Bottom,
        other => match length_percent(other, &reader.units) {
            Some(Dim::Px(px)) => VerticalAlign::Length(px),
            Some(Dim::Percent(share)) => VerticalAlign::Percent(share),
            _ => VerticalAlign::Baseline,
        },
    }
}

fn tab_size(reader: &Reader) -> usize {
    reader
        .text("tab-size")
        .and_then(|text| text.trim().parse::<usize>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(8)
}

/// Whether text runs right to left: `direction: rtl`, or the nearest `dir` attribute that says so.
fn is_rtl(reader: &Reader) -> bool {
    match reader.text("direction").map(str::trim) {
        Some("rtl") => return true,
        Some("ltr") if !matches!(reader.values, Values::Node(_)) => return false,
        _ => {}
    }
    let dom = &reader.env.snapshot.dom;
    let mut at = Some(reader.node);
    while let Some(node) = at {
        match dom.get(node).and_then(|n| n.attr("dir")) {
            Some("rtl") => return true,
            Some("ltr") => return false,
            _ => {}
        }
        at = dom.get(node).and_then(|n| n.parent);
    }
    false
}

/// The language of the nearest ancestor (or the element itself) with a `lang` attribute.
pub(crate) fn lang_of(snapshot: &Snapshot, node: NodeId) -> Option<String> {
    let mut at = Some(node);
    while let Some(id) = at {
        let found = snapshot.dom.get(id)?;
        if let Some(lang) = found.attr("lang").filter(|lang| !lang.is_empty()) {
            return Some(lang.to_owned());
        }
        at = found.parent;
    }
    None
}

/// The `text-decoration` of one element: lines, style, colour and thickness, from the shorthand
/// and the longhands (a longhand wins).
fn decorations(reader: &Reader, current: Rgba) -> Vec<TextDecoration> {
    let mut lines: Vec<DecorationKind> = Vec::new();
    let mut style = DecorationStyle::Solid;
    let mut color: Option<Rgba> = None;
    let mut thickness: Option<f32> = None;
    let mut parts: Vec<&str> = Vec::new();
    if let Some(short) = reader.text("text-decoration") {
        parts.extend(words(short));
    }
    if let Some(line) = reader.text("text-decoration-line") {
        // A longhand replaces the lines the shorthand named.
        parts.retain(|word| !is_line_word(word));
        parts.extend(words(line));
    }
    if let Some(value) = reader.text("text-decoration-style") {
        parts.extend(words(value));
    }
    if let Some(value) = reader.text("text-decoration-color") {
        parts.push(value.trim());
    }
    if let Some(value) = reader.text("text-decoration-thickness") {
        parts.push(value.trim());
    }
    for word in parts {
        match word {
            "underline" => lines.push(DecorationKind::Underline),
            "overline" => lines.push(DecorationKind::Overline),
            "line-through" => lines.push(DecorationKind::LineThrough),
            "none" | "blink" | "spelling-error" | "grammar-error" => {}
            "solid" => style = DecorationStyle::Solid,
            "double" => style = DecorationStyle::Double,
            "dotted" => style = DecorationStyle::Dotted,
            "dashed" => style = DecorationStyle::Dashed,
            "wavy" => style = DecorationStyle::Wavy,
            "auto" | "from-font" => thickness = None,
            other => {
                if let Some(px) = pixels(other, &reader.units) {
                    thickness = Some(px);
                } else if let Some(found) = color_with(other, current) {
                    color = Some(found);
                }
            }
        }
    }
    let offset = reader
        .text("text-underline-offset")
        .and_then(|text| pixels(text, &reader.units));
    lines.dedup();
    lines
        .into_iter()
        .map(|kind| TextDecoration {
            kind,
            color: color.unwrap_or(current),
            style,
            thickness,
            offset,
        })
        .collect()
}

fn is_line_word(word: &str) -> bool {
    matches!(
        word,
        "underline" | "overline" | "line-through" | "none" | "blink"
    )
}

/// Whether an inline element has a margin, a border or padding on any side, so that it takes
/// room on a line even with nothing in it.
pub(crate) fn has_edges(snapshot: &Snapshot, node: NodeId) -> bool {
    let units = Units::new(16., 16., snapshot.viewport);
    SIDES.iter().any(|side| {
        let sized = |property: String| {
            snapshot
                .style(node, &property)
                .and_then(|text| length_percent(text, &units))
                .is_some_and(|dim| dim != Dim::Px(0.) && dim != Dim::Percent(0.))
        };
        let border = border_side_width(
            snapshot.style(node, &format!("border-{side}-width")),
            snapshot.style(node, &format!("border-{side}-style")),
            &units,
        );
        border > 0. || sized(format!("margin-{side}")) || sized(format!("padding-{side}"))
    })
}

/// The margins, borders and paddings of an inline box, top, right, bottom and left.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Edges {
    pub margin: [Dim; 4],
    pub padding: [Dim; 4],
    pub border: [f32; 4],
}

impl Edges {
    /// Takes away the margin, border and padding of one side (0 top, 1 right, 2 bottom, 3 left).
    pub(crate) fn cut(&mut self, side: usize) {
        self.margin[side] = Dim::Px(0.);
        self.padding[side] = Dim::Px(0.);
        self.border[side] = 0.;
    }
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// The margins of an element (an `auto` margin is none on an inline box).
pub(crate) fn margins_of(env: &Env, node: NodeId) -> [Dim; 4] {
    let reader = unit_reader(env, node);
    SIDES.map(|side| {
        reader
            .text(&format!("margin-{side}"))
            .and_then(|text| length_percent(text, &reader.units))
            .unwrap_or(Dim::Px(0.))
    })
}

/// The margins, paddings and border widths of an inline element.
pub(crate) fn edges_of(env: &Env, node: NodeId) -> Edges {
    let reader = unit_reader(env, node);
    let padding = SIDES.map(|side| {
        reader
            .text(&format!("padding-{side}"))
            .and_then(|text| length_percent(text, &reader.units))
            .unwrap_or(Dim::Px(0.))
    });
    let border = SIDES.map(|side| {
        border_side_width(
            reader.text(&format!("border-{side}-width")),
            reader.text(&format!("border-{side}-style")),
            &reader.units,
        )
    });
    Edges {
        margin: margins_of(env, node),
        padding,
        border,
    }
}

/// What an inline element paints of its own box: a plain background colour, a border and rounded
/// corners; `None` when it has none of them.
pub(crate) fn span_paint(env: &Env, node: NodeId) -> Option<Arc<SpanPaint>> {
    let reader = unit_reader(env, node);
    let color = reader
        .text("color")
        .and_then(|text| color_with(text, [0., 0., 0., 1.]))
        .unwrap_or([0., 0., 0., 1.]);
    let paint = Paint::read(env.snapshot, node, &reader.units, color);
    (paint.background_color[3] > 0. || paint.border.is_visible()).then(|| {
        Arc::new(SpanPaint {
            background: paint.background_color,
            border: paint.border.clone(),
            radii: paint.px_radii(),
        })
    })
}

/// The decorations an element's own `text-decoration` draws.
pub(crate) fn decorations_of(env: &Env, node: NodeId) -> Vec<TextDecoration> {
    let reader = unit_reader(env, node);
    let color = reader
        .text("color")
        .and_then(|text| color_with(text, [0., 0., 0., 1.]))
        .unwrap_or([0., 0., 0., 1.]);
    decorations(&reader, color)
}

/// A reader of `node`'s style for the properties that take only units.
fn unit_reader<'a>(env: &'a Env<'a>, node: NodeId) -> Reader<'a> {
    let size = font_size_of(env, node);
    Reader {
        env,
        node,
        values: Values::Node(node),
        units: Units::new(size, env.root_font_size, env.snapshot.viewport),
    }
}
