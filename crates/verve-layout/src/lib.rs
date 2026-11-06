//! Box layout for Verve's pages, on the renderer's own thread.
//!
//! A [`LayoutEngine`] takes the document's `Snapshot`, builds the boxes taffy lays out (block flow
//! with floats, flexbox, grid; absolute, fixed and sticky positioning; `display: contents`), and
//! answers what a script and a drawer ask: where every element is, how far a box scrolls, what is
//! under a point, and the [`DisplayList`] to paint. Nothing here knows GPUI; text is measured
//! through [`TextMeasurer`].

mod display_list;
mod gradient;
mod measure;
mod shadow;
mod transform;
mod value;

pub use display_list::{
    AppRegionKind, Border, BorderLine, ControlKind, Cursor, DecorationKind, DisplayList, Fill,
    ImageKey, ObjectFit, PaintItem, RasterSpec, ResolvedHitbox, Rgba, Shadow,
};
pub use gradient::{
    ColorStop, EllipseSize, Extent, Gradient, GradientPosition, LinearDirection, RadialShape,
    is_gradient,
};
pub use measure::{FontKey, FontMetrics, HeadlessMeasurer, PlacedGlyph, ShapedRun, TextMeasurer};
