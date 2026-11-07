//! Box layout for Verve's pages, on the renderer's own thread.
//!
//! A [`LayoutEngine`] takes the document's `Snapshot`, builds the boxes taffy lays out (block flow
//! with floats, flexbox, grid; absolute, fixed and sticky positioning; `display: contents`), and
//! answers what a script and a drawer ask: where every element is, how far a box scrolls, what is
//! under a point, and the [`DisplayList`] to paint. Nothing here knows GPUI; text is measured
//! through [`TextMeasurer`].

mod atoms;
mod background;
mod display_list;
mod engine;
mod flow;
pub mod fonts;
mod gradient;
mod grid;
mod images;
mod inline;
mod layout;
mod measure;
mod paint;
mod paint_style;
mod place;
mod props;
mod replaced;
mod shadow;
mod transform;
mod tree;
mod value;

pub use display_list::{
    AppRegionKind, Border, BorderLine, ControlKind, Cursor, DecorationKind, DisplayList, Fill,
    ImageKey, ObjectFit, PaintItem, Placed, RasterSpec, ResolvedHitbox, ResolvedScroller, Rgba,
    Shadow,
};
pub use engine::LayoutEngine;
pub use fonts::{FontRegistry, Platform};
pub use gradient::{
    ColorStop, EllipseSize, Extent, Gradient, GradientPosition, LinearDirection, RadialShape,
    gradient_line, is_gradient,
};
pub use images::{ImageFrames, ImageTable, key_for_url};
pub use inline::{
    DecorationStyle, Fragment, InlineLayout, LineBox, SpanBox, SpanPaint, Stop, TextDecoration,
    TextFragment, TextPaint, TextShadow,
};
pub use measure::{FontKey, FontMetrics, HeadlessMeasurer, PlacedGlyph, ShapedRun, TextMeasurer};
