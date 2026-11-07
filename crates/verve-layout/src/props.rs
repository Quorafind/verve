//! A node's computed style, read into what layout and painting need: the taffy style, and the
//! rest of CSS that taffy has no word for (stacking, clipping, sticky offsets, paint).
//!
//! The cascade leaves every property as text and only writes what a rule declared, so each read
//! starts from CSS's initial value. Lengths that cannot be read (a `calc()` mixing pixels with a
//! percentage, a keyword like `max-content` on a size) fall back to `auto` and are counted in
//! [`BoxProps::unsupported`], so a page that leans on them can be told.

use taffy::style_helpers::{TaffyAuto, auto, length, percent, zero};
use taffy::{
    AlignContent, AlignItems, BoxSizing, Clear, Dimension, FlexDirection, FlexWrap, Float,
    LengthPercentage, LengthPercentageAuto, Overflow, Point, Rect, Size,
};
use verve_dom::css::{color_with, initial_value};
use verve_dom::{NodeId, Snapshot};

use crate::display_list::{AppRegionKind, Cursor, Rgba};
use crate::grid::{self, Raw};
use crate::paint_style::{Paint, border_side_width};
use crate::transform::{Translation, parse_transform, parse_translate};
use crate::value::{Dim, Units, length_percent, number, pixels};

/// CSS's `display`, reduced to what layout tells apart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Display {
    None,
    /// The element has no box and its children take its place.
    Contents,
    /// An inline box: until P31's line layout it has no box, and its children are lifted as for
    /// `contents`.
    Inline,
    Block,
    FlowRoot,
    ListItem,
    Flex,
    Grid,
    InlineBlock,
    InlineFlex,
    InlineGrid,
    Table,
    TableRowGroup,
    TableRow,
    TableCell,
    TableCaption,
    /// `table-column` and `table-column-group`: no box of their own.
    TableColumn,
}

impl Display {
    pub(crate) fn parse(text: &str) -> Self {
        let words: Vec<&str> = text.split_whitespace().collect();
        match words.as_slice() {
            ["none"] => Display::None,
            ["contents"] => Display::Contents,
            ["inline"] | ["inline", "flow"] => Display::Inline,
            ["block"] | ["block", "flow"] | ["-webkit-box"] => Display::Block,
            ["flow-root"] | ["block", "flow-root"] => Display::FlowRoot,
            ["list-item"] | ["block", "flow", "list-item"] => Display::ListItem,
            ["flex"] | ["block", "flex"] => Display::Flex,
            ["grid"] | ["block", "grid"] => Display::Grid,
            ["inline-block"] | ["inline", "flow-root"] => Display::InlineBlock,
            ["inline-flex"] | ["inline", "flex"] | ["-webkit-inline-box"] => Display::InlineFlex,
            ["inline-grid"] | ["inline", "grid"] => Display::InlineGrid,
            ["table"] | ["inline-table"] => Display::Table,
            ["table-row-group" | "table-header-group" | "table-footer-group"] => {
                Display::TableRowGroup
            }
            ["table-row"] => Display::TableRow,
            ["table-cell"] => Display::TableCell,
            ["table-caption"] => Display::TableCaption,
            ["table-column" | "table-column-group"] => Display::TableColumn,
            _ => Display::Inline,
        }
    }

    /// Whether the element's children take the element's place in the tree.
    pub(crate) fn is_transparent(self) -> bool {
        matches!(
            self,
            Display::Contents | Display::Inline | Display::TableRowGroup | Display::TableRow
        )
    }

    /// Whether the element is laid out as a box of its own.
    pub(crate) fn has_box(self) -> bool {
        !self.is_transparent() && !matches!(self, Display::None | Display::TableColumn)
    }

    /// Whether the element lays its children out as flex or grid items.
    pub(crate) fn is_flex_or_grid(self) -> bool {
        matches!(
            self,
            Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid
        )
    }

    /// What the display is for a box that is an item of a flex or grid container, or floats, or is
    /// out of flow: an inline-level display turns into its block-level twin (CSS Display 3, 2.7).
    pub(crate) fn blockified(self) -> Self {
        match self {
            Display::Inline => Display::Block,
            Display::InlineBlock => Display::FlowRoot,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            other => other,
        }
    }
}

/// CSS's `position`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

impl Position {
    fn parse(text: &str) -> Self {
        match text.trim() {
            "relative" => Position::Relative,
            "absolute" => Position::Absolute,
            "fixed" => Position::Fixed,
            "sticky" | "-webkit-sticky" => Position::Sticky,
            _ => Position::Static,
        }
    }

    /// Whether the box sits outside the flow, placed by its insets.
    pub(crate) fn is_out_of_flow(self) -> bool {
        matches!(self, Position::Absolute | Position::Fixed)
    }

    /// Whether `top`/`left` and the others place the box.
    pub(crate) fn is_positioned(self) -> bool {
        self != Position::Static
    }
}

/// What `overflow` does along an axis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Clip {
    Visible,
    /// Cut to the padding box and not scrollable by a person.
    Hidden,
    Scroll,
    Auto,
}

impl Clip {
    fn parse(text: &str) -> Self {
        match text.trim() {
            "hidden" | "clip" => Clip::Hidden,
            "scroll" => Clip::Scroll,
            "auto" | "overlay" => Clip::Auto,
            _ => Clip::Visible,
        }
    }

    pub(crate) fn clips(self) -> bool {
        self != Clip::Visible
    }
}

/// What a box does with the pointer and the scrollbars.
pub(crate) struct Input {
    /// `visibility` is `visible`.
    pub visible: bool,
    pub pointer_events: bool,
    pub cursor: Cursor,
    pub app_region: Option<AppRegionKind>,
    /// `scrollbar-width` is not `none`.
    pub scrollbar: bool,
}

/// Everything layout and painting read of one node.
pub(crate) struct BoxProps {
    pub display: Display,
    pub position: Position,
    pub float: bool,
    pub z_index: Option<i32>,
    pub order: i32,
    pub opacity: f32,
    pub overflow: [Clip; 2],
    pub translation: Option<Translation>,
    /// `top`, `right`, `bottom`, `left` of a sticky box, in pixels.
    pub sticky: [Option<f32>; 4],
    pub style: taffy::Style,
    pub paint: Paint,
    pub input: Input,
    /// A `filter`, `mix-blend-mode`, `isolation: isolate`, `contain: paint` or a `will-change` for
    /// one of them: the box starts a stacking context whatever else it is.
    pub isolated: bool,
    /// A box that absolute and fixed descendants are placed in, besides the positioned ones: it
    /// has a transform, a filter, a perspective or contains layout or paint.
    pub contains_fixed: bool,
    /// The share (`0.05` for `5%`) of a percentage `padding-top` and `padding-bottom`, which taffy
    /// resolves against the wrong side and layout puts right afterwards.
    pub padding_percent: [Option<f32>; 2],
    /// How many properties fell back to `auto` or to their initial value.
    pub unsupported: u32,
    pub grid_areas: grid::Areas,
}

impl BoxProps {
    /// The properties of an anonymous block box: in flow, drawing nothing, taking no pointer.
    pub(crate) fn anonymous() -> Self {
        Self {
            display: Display::Block,
            position: Position::Static,
            float: false,
            z_index: None,
            order: 0,
            opacity: 1.,
            overflow: [Clip::Visible; 2],
            translation: None,
            sticky: [None; 4],
            style: taffy::Style::default(),
            paint: Paint::none(),
            input: Input {
                visible: true,
                pointer_events: false,
                cursor: Cursor::Auto,
                app_region: None,
                scrollbar: false,
            },
            isolated: false,
            contains_fixed: false,
            padding_percent: [None; 2],
            unsupported: 0,
            grid_areas: grid::Areas::default(),
        }
    }

    /// Whether the box starts a stacking context of its own.
    pub(crate) fn is_stacking_context(&self, in_flex_or_grid: bool) -> bool {
        self.position == Position::Fixed
            || self.position == Position::Sticky
            || self.opacity < 1.
            || self.translation.is_some()
            || self.isolated
            || (self.z_index.is_some() && (self.position.is_positioned() || in_flex_or_grid))
    }

    /// Whether the box is a scroll container on either axis.
    pub(crate) fn clips(&self) -> bool {
        self.overflow[0].clips() || self.overflow[1].clips()
    }
}

/// What reading a node needs besides the node.
pub(crate) struct ReadContext<'a> {
    pub snapshot: &'a Snapshot,
    pub root_font_size: f32,
    /// The grid areas of the box the node is an item of, if that box is a grid.
    pub parent_areas: Option<&'a grid::Areas>,
    /// The node is an item of a flex or grid container: its display is blockified.
    pub blockify: bool,
}

struct Reader<'a> {
    snapshot: &'a Snapshot,
    node: NodeId,
    units: Units,
    color: Rgba,
    unsupported: u32,
    padding_percent: [Option<f32>; 2],
}

impl<'a> Reader<'a> {
    fn text(&self, property: &str) -> Option<&'a str> {
        self.snapshot.style(self.node, property)
    }

    fn text_or(&self, property: &str, default: &'a str) -> &'a str {
        self.text(property).unwrap_or(default)
    }

    fn dim(&mut self, property: &str) -> Dim {
        let Some(text) = self.text(property) else {
            return Dim::Auto;
        };
        if matches!(text, "auto" | "none" | "normal") {
            return Dim::Auto;
        }
        length_percent(text, &self.units).unwrap_or_else(|| {
            self.unsupported += 1;
            Dim::Auto
        })
    }

    fn length_or_zero(&mut self, property: &str) -> LengthPercentage {
        match self.dim(property) {
            Dim::Px(px) if px >= 0. => length(px),
            Dim::Percent(f) if f >= 0. => percent(f),
            _ => zero(),
        }
    }

    /// A border side's width: nothing when its style is `none`, as CSS says.
    fn border_side(&self, side: &str) -> f32 {
        border_side_width(
            self.text(&format!("border-{side}-width")),
            self.text(&format!("border-{side}-style")),
            &self.units,
        )
    }
}

fn margin_dim(dim: Dim) -> LengthPercentageAuto {
    match dim {
        Dim::Auto => auto(),
        Dim::Px(px) => length(px),
        Dim::Percent(f) => percent(f),
    }
}

fn dimension_of(dim: Dim) -> Dimension {
    match dim {
        Dim::Auto => auto(),
        Dim::Px(px) => length(px),
        Dim::Percent(f) => percent(f),
    }
}

/// Reads one node.
pub(crate) fn read(context: &ReadContext, node: NodeId) -> BoxProps {
    let snapshot = context.snapshot;
    let tag = snapshot.dom.get(node).and_then(|n| n.tag());
    let mut reader = Reader {
        snapshot,
        node,
        units: Units::new(16., context.root_font_size, snapshot.viewport),
        color: [0., 0., 0., 1.],
        unsupported: 0,
        padding_percent: [None; 2],
    };
    if let Some(size) = reader
        .text("font-size")
        .and_then(|t| pixels(t, &reader.units))
    {
        reader.units.font_size = size;
    }
    reader.color = reader
        .text("color")
        .and_then(|text| color_with(text, [0., 0., 0., 1.]))
        .unwrap_or([0., 0., 0., 1.]);
    let position = Position::parse(reader.text_or("position", "static"));
    // A box out of flow does not float, whatever `float` says.
    let float =
        !position.is_out_of_flow() && !matches!(reader.text_or("float", "none"), "none" | "");
    let mut display = Display::parse(
        reader
            .text("display")
            .or_else(|| initial_value("display", tag))
            .unwrap_or("inline"),
    );
    if float || position.is_out_of_flow() || context.blockify {
        display = display.blockified();
    }
    let mut style = taffy::Style::default();
    box_model(&mut reader, &mut style, display, position, float);
    item_alignment(&reader, &mut style);
    flex(&mut reader, &mut style);
    grid_container(&reader, &mut style);
    intrinsic(&reader, tag, &mut style);
    grid_item(&reader, context.parent_areas, &mut style);
    let translation = translation_of(&reader);
    reader.unsupported += u32::from(translation.as_ref().is_some_and(|t| t.lossy));
    let (isolated, contains_fixed) = containment(&reader, translation.is_some());
    BoxProps {
        display,
        position,
        float,
        z_index: reader.text("z-index").and_then(|t| t.trim().parse().ok()),
        order: reader
            .text("order")
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0),
        opacity: reader
            .text("opacity")
            .and_then(number)
            .map_or(1., |o| o.clamp(0., 1.)),
        overflow: overflow_pair(&reader),
        translation,
        sticky: sticky_insets(&reader, position),
        paint: Paint::read(snapshot, node, &reader.units, reader.color),
        input: input_of(&reader),
        isolated,
        contains_fixed,
        padding_percent: reader.padding_percent,
        unsupported: reader.unsupported,
        style,
        grid_areas: grid::areas(reader.text_or("grid-template-areas", "none")),
    }
}

fn input_of(reader: &Reader) -> Input {
    Input {
        visible: !matches!(
            reader.text_or("visibility", "visible"),
            "hidden" | "collapse"
        ),
        pointer_events: reader.text_or("pointer-events", "auto") != "none",
        cursor: reader.text("cursor").map_or(Cursor::Auto, Cursor::parse),
        app_region: app_region(reader),
        scrollbar: reader.text_or("scrollbar-width", "auto") != "none",
    }
}

/// The insets of a sticky box that are lengths; none for any other box.
fn sticky_insets(reader: &Reader, position: Position) -> [Option<f32>; 4] {
    if position != Position::Sticky {
        return [None; 4];
    }
    ["top", "right", "bottom", "left"]
        .map(|side| reader.text(side).and_then(|t| pixels(t, &reader.units)))
}

/// `overflow` on each axis. When one axis clips, the other cannot be `visible`: it computes to
/// `auto`.
fn overflow_pair(reader: &Reader) -> [Clip; 2] {
    let x = Clip::parse(reader.text_or("overflow-x", "visible"));
    let y = Clip::parse(reader.text_or("overflow-y", "visible"));
    match (x.clips(), y.clips()) {
        (true, false) => [x, Clip::Auto],
        (false, true) => [Clip::Auto, y],
        _ => [x, y],
    }
}

/// Whether the box is isolated into a stacking context by something besides position, opacity
/// and transform, and whether it holds absolute and fixed descendants.
fn containment(reader: &Reader, transformed: bool) -> (bool, bool) {
    let set = |property: &str, none: &str| {
        reader
            .text(property)
            .is_some_and(|value| !value.trim().is_empty() && value.trim() != none)
    };
    let filtered = set("filter", "none") || set("backdrop-filter", "none");
    let turned = set("rotate", "none") || set("scale", "none") || set("perspective", "none");
    let will_change = reader.text_or("will-change", "auto");
    let will = |names: &[&str]| will_change.split(',').any(|w| names.contains(&w.trim()));
    let contained = reader
        .text_or("contain", "none")
        .split_whitespace()
        .any(|word| matches!(word, "paint" | "layout" | "strict" | "content"));
    let isolated = filtered
        || turned
        || contained
        || set("mix-blend-mode", "normal")
        || reader.text_or("isolation", "auto") == "isolate"
        || will(&[
            "opacity",
            "transform",
            "filter",
            "perspective",
            "translate",
            "rotate",
            "scale",
            "mix-blend-mode",
            "isolation",
        ]);
    let holds = transformed
        || filtered
        || turned
        || contained
        || will(&["transform", "filter", "perspective"]);
    (isolated, holds)
}

fn app_region(reader: &Reader) -> Option<AppRegionKind> {
    let text = reader
        .text("-webkit-app-region")
        .or_else(|| reader.text("app-region"))?;
    match text.trim() {
        "drag" => Some(AppRegionKind::Drag),
        "no-drag" => Some(AppRegionKind::NoDrag),
        _ => None,
    }
}

fn translation_of(reader: &Reader) -> Option<Translation> {
    let transform = reader
        .text("transform")
        .and_then(|text| parse_transform(text, &reader.units));
    let translate = reader
        .text("translate")
        .and_then(|text| parse_translate(text, &reader.units));
    match (translate, transform) {
        (Some(first), Some(second)) => Some(first.then(second)),
        (first, second) => first.or(second),
    }
}

/// Display, position, sizes, spacing and borders: the box itself.
fn box_model(
    reader: &mut Reader,
    style: &mut taffy::Style,
    display: Display,
    position: Position,
    float: bool,
) {
    style.display = match display {
        Display::Flex | Display::InlineFlex => taffy::Display::Flex,
        Display::Grid | Display::InlineGrid | Display::Table => taffy::Display::Grid,
        Display::FlowRoot | Display::InlineBlock => taffy::Display::FlowRoot,
        Display::None | Display::TableColumn => taffy::Display::None,
        _ => taffy::Display::Block,
    };
    style.box_sizing = if reader.text_or("box-sizing", "content-box") == "border-box" {
        BoxSizing::BorderBox
    } else {
        BoxSizing::ContentBox
    };
    style.position = if position.is_out_of_flow() {
        taffy::Position::Absolute
    } else {
        taffy::Position::Relative
    };
    if float {
        style.float = if reader.text_or("float", "none") == "right" {
            Float::Right
        } else {
            Float::Left
        };
    }
    style.clear = match reader.text_or("clear", "none") {
        "left" => Clear::Left,
        "right" => Clear::Right,
        "both" => Clear::Both,
        _ => Clear::None,
    };
    let [overflow_x, overflow_y] = overflow_pair(reader);
    style.overflow = Point {
        x: taffy_overflow(overflow_x),
        y: taffy_overflow(overflow_y),
    };
    let inset = |reader: &mut Reader, side: &str| margin_dim(reader.dim(side));
    if matches!(
        position,
        Position::Relative | Position::Absolute | Position::Fixed
    ) {
        style.inset = Rect {
            left: inset(reader, "left"),
            right: inset(reader, "right"),
            top: inset(reader, "top"),
            bottom: inset(reader, "bottom"),
        };
    }
    sizes(reader, style);
    spacing(reader, style);
}

fn taffy_overflow(clip: Clip) -> Overflow {
    match clip {
        Clip::Visible => Overflow::Visible,
        Clip::Hidden => Overflow::Hidden,
        Clip::Scroll | Clip::Auto => Overflow::Scroll,
    }
}

fn sizes(reader: &mut Reader, style: &mut taffy::Style) {
    style.size = Size {
        width: dimension_of(reader.dim("width")),
        height: dimension_of(reader.dim("height")),
    };
    style.min_size = Size {
        width: dimension_of(reader.dim("min-width")),
        height: dimension_of(reader.dim("min-height")),
    };
    style.max_size = Size {
        width: dimension_of(reader.dim("max-width")),
        height: dimension_of(reader.dim("max-height")),
    };
    style.aspect_ratio = reader.text("aspect-ratio").and_then(aspect_ratio);
}

/// `16 / 9`, `1.5`, `auto 4 / 3`: the ratio, or `None` for `auto`.
fn aspect_ratio(text: &str) -> Option<f32> {
    let text = text.trim().trim_start_matches("auto").trim();
    let (width, height) = text.split_once('/').unwrap_or((text, "1"));
    let (width, height) = (number(width)?, number(height)?);
    (width > 0. && height > 0.).then(|| width / height)
}

fn spacing(reader: &mut Reader, style: &mut taffy::Style) {
    let sides = ["top", "right", "bottom", "left"];
    let mut margin = [auto::<LengthPercentageAuto>(); 4];
    let mut padding = [zero::<LengthPercentage>(); 4];
    let mut border = [zero::<LengthPercentage>(); 4];
    for (i, side) in sides.iter().enumerate() {
        margin[i] = match reader.text(&format!("margin-{side}")) {
            None => zero(),
            Some(_) => margin_dim(reader.dim(&format!("margin-{side}"))),
        };
        padding[i] = reader.length_or_zero(&format!("padding-{side}"));
        if i % 2 == 0 {
            reader.padding_percent[i / 2] = reader
                .text(&format!("padding-{side}"))
                .and_then(|text| length_percent(text, &reader.units))
                .and_then(|dim| match dim {
                    Dim::Percent(share) => Some(share),
                    _ => None,
                });
        }
        border[i] = length(reader.border_side(side));
    }
    style.margin = Rect {
        top: margin[0],
        right: margin[1],
        bottom: margin[2],
        left: margin[3],
    };
    style.padding = Rect {
        top: padding[0],
        right: padding[1],
        bottom: padding[2],
        left: padding[3],
    };
    style.border = Rect {
        top: border[0],
        right: border[1],
        bottom: border[2],
        left: border[3],
    };
}

/// `align-*` and `justify-*`.
fn item_alignment(reader: &Reader, style: &mut taffy::Style) {
    style.align_items = reader.text("align-items").and_then(item_align);
    style.align_self = reader.text("align-self").and_then(item_align);
    style.justify_items = reader.text("justify-items").and_then(item_align);
    style.justify_self = reader.text("justify-self").and_then(item_align);
    style.align_content = reader.text("align-content").and_then(content_align);
    style.justify_content = reader.text("justify-content").and_then(content_align);
}

fn item_align(text: &str) -> Option<AlignItems> {
    let word = text.split_whitespace().last()?;
    Some(match word {
        "start" | "left" => AlignItems::START,
        "end" | "right" => AlignItems::END,
        "flex-start" => AlignItems::FLEX_START,
        "flex-end" => AlignItems::FLEX_END,
        "self-start" => AlignItems::SELF_START,
        "self-end" => AlignItems::SELF_END,
        "center" => AlignItems::CENTER,
        "baseline" | "first" | "last" => AlignItems::BASELINE,
        "stretch" => AlignItems::STRETCH,
        _ => return None,
    })
}

fn content_align(text: &str) -> Option<AlignContent> {
    let word = text.split_whitespace().last()?;
    Some(match word {
        "start" | "left" => AlignContent::START,
        "end" | "right" => AlignContent::END,
        "flex-start" => AlignContent::FLEX_START,
        "flex-end" => AlignContent::FLEX_END,
        "center" => AlignContent::CENTER,
        "stretch" => AlignContent::STRETCH,
        "space-between" => AlignContent::SPACE_BETWEEN,
        "space-around" => AlignContent::SPACE_AROUND,
        "space-evenly" => AlignContent::SPACE_EVENLY,
        _ => return None,
    })
}

fn flex(reader: &mut Reader, style: &mut taffy::Style) {
    style.flex_direction = match reader.text_or("flex-direction", "row") {
        "column" => FlexDirection::Column,
        "row-reverse" => FlexDirection::RowReverse,
        "column-reverse" => FlexDirection::ColumnReverse,
        _ => FlexDirection::Row,
    };
    style.flex_wrap = match reader.text_or("flex-wrap", "nowrap") {
        "wrap" => FlexWrap::Wrap,
        "wrap-reverse" => FlexWrap::WrapReverse,
        _ => FlexWrap::NoWrap,
    };
    style.flex_grow = reader
        .text("flex-grow")
        .and_then(number)
        .unwrap_or(0.)
        .max(0.);
    style.flex_shrink = reader
        .text("flex-shrink")
        .and_then(number)
        .unwrap_or(1.)
        .max(0.);
    style.flex_basis = dimension_of(reader.dim("flex-basis"));
    style.gap = Size {
        width: reader.length_or_zero("column-gap"),
        height: reader.length_or_zero("row-gap"),
    };
}

fn grid_container(reader: &Reader, style: &mut taffy::Style) {
    let columns = grid::template(
        reader.text_or("grid-template-columns", "none"),
        &reader.units,
    );
    let rows = grid::template(reader.text_or("grid-template-rows", "none"), &reader.units);
    let named = !columns.names.iter().all(Vec::is_empty);
    if named {
        style.grid_template_column_names = columns.names.clone();
    }
    if !rows.names.iter().all(Vec::is_empty) {
        style.grid_template_row_names = rows.names.clone();
    }
    style.grid_template_columns = columns.tracks;
    style.grid_template_rows = rows.tracks;
    style.grid_auto_columns =
        grid::auto_tracks(reader.text_or("grid-auto-columns", "auto"), &reader.units);
    style.grid_auto_rows =
        grid::auto_tracks(reader.text_or("grid-auto-rows", "auto"), &reader.units);
    let flow = reader.text_or("grid-auto-flow", "row");
    let dense = flow.contains("dense");
    style.grid_auto_flow = match (flow.contains("column"), dense) {
        (true, true) => taffy::GridAutoFlow::ColumnDense,
        (true, false) => taffy::GridAutoFlow::Column,
        (false, true) => taffy::GridAutoFlow::RowDense,
        (false, false) => taffy::GridAutoFlow::Row,
    };
}

/// A grid item's placement.
fn grid_item(reader: &Reader, areas: Option<&grid::Areas>, style: &mut taffy::Style) {
    let raw = Raw {
        area: reader.text("grid-area"),
        row: reader.text("grid-row"),
        column: reader.text("grid-column"),
        row_start: reader.text("grid-row-start"),
        row_end: reader.text("grid-row-end"),
        column_start: reader.text("grid-column-start"),
        column_end: reader.text("grid-column-end"),
    };
    let none = grid::Areas::default();
    let lines = grid::lines(&raw, areas.unwrap_or(&none));
    style.grid_row = lines.row;
    style.grid_column = lines.column;
}

/// The size of a replaced element that its attributes and the web platform give it: `<canvas>`,
/// `<iframe>`, `<video>`, `<embed>`, `<object>` and `<svg>` are 300 by 150, and a picture is
/// what its `width` and `height` say. A size a style sets wins.
fn intrinsic(reader: &Reader, tag: Option<&str>, style: &mut taffy::Style) {
    let default = match tag {
        Some("canvas" | "iframe" | "video" | "embed" | "object" | "svg") => Some(300.),
        Some("img") => None,
        _ => return,
    };
    style.item_is_replaced = true;
    let attribute = |name: &str| {
        reader
            .snapshot
            .dom
            .get(reader.node)
            .and_then(|node| node.attr(name))
            .and_then(|value| value.trim().trim_end_matches("px").parse::<f32>().ok())
            .filter(|size| *size >= 0.)
    };
    if style.size.width == Dimension::AUTO {
        let width = attribute("width").or(default);
        style.size.width = width.map_or(Dimension::AUTO, length);
    }
    if style.size.height == Dimension::AUTO {
        let height = attribute("height").or(default.map(|width| width / 2.));
        style.size.height = height.map_or(Dimension::AUTO, length);
    }
}
