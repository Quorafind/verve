//! The shapes of form controls (P32): what [`verve_layout::PaintItem::Control`] comes to in the
//! shell, in the flat look of Chromium's default on Windows 11. The text of a field, a select
//! or a button is not here: layout shapes it as the page's own text.
//!
//! Everything lowers to rectangles with rounded corners, so the window and the software painter
//! (`screenshot`) draw a control the same way. A check or an arrow is a run of small squares.

use std::time::{Duration, Instant};

use verve_dom::Rect;
use verve_layout::{ControlKind, ControlLook, PaintItem, Placed, Rgba};

use crate::plan::{Command, Paint, Shape};

const WHITE: Rgba = [1., 1., 1., 1.];
/// The border of a box that takes input: Chromium's `#767676`.
const EDGE: Rgba = [0.463, 0.463, 0.463, 1.];
const TRACK: Rgba = [0.78, 0.78, 0.78, 1.];
const GROOVE: Rgba = [0.9, 0.9, 0.9, 1.];
const MARK_THICKNESS: f32 = 1.;
/// How long a caret is shown, and how long it is hidden (Chromium's 530 ms).
pub(crate) const BLINK: Duration = Duration::from_millis(530);

/// When the caret last moved, so that it shows solid for a period after a key.
#[derive(Default)]
pub(crate) struct Blink {
    moved: Option<(Rect, Instant)>,
    /// A repaint is already waiting to flip the caret.
    pub waiting: bool,
}

impl Blink {
    /// Whether the caret at `bounds` is shown at `now`.
    pub(crate) fn visible(&mut self, bounds: Rect, now: Instant) -> bool {
        let since = match self.moved {
            Some((rect, since)) if rect == bounds => since,
            _ => {
                self.moved = Some((bounds, now));
                now
            }
        };
        (now.duration_since(since).as_millis() / BLINK.as_millis()).is_multiple_of(2)
    }

    /// How long from `now` until the caret flips: the rest of the period it is in, so that the
    /// blink keeps its rhythm however late a repaint was.
    pub(crate) fn until_flip(&self, now: Instant) -> Duration {
        let elapsed = self
            .moved
            .map_or(Duration::ZERO, |(_, since)| now.duration_since(since));
        BLINK - Duration::from_nanos((elapsed.as_nanos() % BLINK.as_nanos()) as u64)
    }
}

/// The shapes of the control item `placed`, appended to `out`.
pub(crate) fn lower(placed: &Placed, out: &mut Vec<Command>) {
    let PaintItem::Control {
        bounds, kind, look, ..
    } = placed.item
    else {
        return;
    };
    let mut shapes = Vec::new();
    let at = placed.in_window(*bounds);
    match kind {
        ControlKind::TextField { marked, caret, .. } => {
            text_field(placed, (marked, *caret), look, &mut shapes);
        }
        ControlKind::Checkbox { checked, .. } => checkbox(at, *checked, look, &mut shapes),
        ControlKind::Radio { checked } => radio(at, *checked, look, &mut shapes),
        ControlKind::Select { .. } => arrow(at, look, &mut shapes),
        ControlKind::Range { min, max, value } => {
            range(at, (*min, *max, *value), look, &mut shapes)
        }
        ControlKind::Progress { value } => progress(at, *value, look, &mut shapes),
        ControlKind::Meter {
            value,
            min,
            max,
            low,
            high,
            optimum,
        } => meter(at, [*value, *min, *max, *low, *high, *optimum], &mut shapes),
        ControlKind::Color { value } => {
            let swatch = Rect::new(
                at.x + 4.,
                at.y + 4.,
                (at.width - 8.).max(0.),
                (at.height - 8.).max(0.),
            );
            shapes.push(fill(swatch, 0., *value));
        }
        ControlKind::Date { .. } | ControlKind::File { .. } => {}
    }
    out.extend(shapes.into_iter().map(|shape| Command {
        clip: placed.clip,
        shape: fade(shape, placed.opacity),
    }));
}

/// The composition's underline and the caret. The selection is a box layout paints under the
/// text, in the highlight colour of the page's own (a constant: `::selection` is not read).
fn text_field(
    placed: &Placed,
    (marked, caret): (&[Rect], Option<Rect>),
    look: &ControlLook,
    out: &mut Vec<Shape>,
) {
    for rect in marked {
        let rect = placed.in_window(*rect);
        let y = rect.y + rect.height - MARK_THICKNESS;
        out.push(fill(
            Rect::new(rect.x, y, rect.width, MARK_THICKNESS),
            0.,
            look.color,
        ));
    }
    if let Some(caret) = caret {
        let caret = placed.in_window(caret);
        out.push(Shape::Caret {
            bounds: Rect::new(caret.x, caret.y, 1., caret.height),
            color: look.color,
        });
    }
}

fn fade(shape: Shape, opacity: f32) -> Shape {
    let scale = |[r, g, b, a]: Rgba| [r, g, b, a * opacity];
    match shape {
        Shape::Fill {
            bounds,
            radii,
            paint: Paint::Solid(color),
        } => Shape::Fill {
            bounds,
            radii,
            paint: Paint::Solid(scale(color)),
        },
        Shape::Border {
            bounds,
            radii,
            widths,
            color,
            dashed,
        } => Shape::Border {
            bounds,
            radii,
            widths,
            color: scale(color),
            dashed,
        },
        Shape::Caret { bounds, color } => Shape::Caret {
            bounds,
            color: scale(color),
        },
        other => other,
    }
}

fn fill(bounds: Rect, radius: f32, color: Rgba) -> Shape {
    Shape::Fill {
        bounds,
        radii: [radius; 4],
        paint: Paint::Solid(color),
    }
}

fn ring(bounds: Rect, radius: f32, width: f32, color: Rgba) -> Shape {
    Shape::Border {
        bounds,
        radii: [radius; 4],
        widths: [width; 4],
        color,
        dashed: false,
    }
}

/// `rect` with `by` taken off every side.
fn inset(rect: Rect, by: f32) -> Rect {
    Rect::new(
        rect.x + by,
        rect.y + by,
        (rect.width - 2. * by).max(0.),
        (rect.height - 2. * by).max(0.),
    )
}

/// A disabled control is drawn in washed-out colours.
fn dimmed(color: Rgba, look: &ControlLook) -> Rgba {
    if look.disabled {
        [color[0], color[1], color[2], color[3] * 0.4]
    } else {
        color
    }
}

fn checkbox(at: Rect, checked: bool, look: &ControlLook, out: &mut Vec<Shape>) {
    let radius = 2.;
    if checked {
        out.push(fill(at, radius, dimmed(look.accent, look)));
        // The check: a short stroke down and a long one up, drawn for a box 13px wide.
        let scale = at.width / 13.;
        let stroke = [[3., 6.5], [5.5, 9.], [10., 4.]].map(|[x, y]| [x * scale, y * scale]);
        out.extend(squares(&stroke, 1.7 * scale, at, WHITE));
    } else {
        out.push(fill(at, radius, dimmed(WHITE, look)));
        out.push(ring(at, radius, 1., dimmed(EDGE, look)));
    }
}

fn radio(at: Rect, checked: bool, look: &ControlLook, out: &mut Vec<Shape>) {
    let round = at.width / 2.;
    if checked {
        let accent = dimmed(look.accent, look);
        out.push(fill(at, round, accent));
        out.push(fill(inset(at, 2.), round, WHITE));
        out.push(fill(inset(at, 3.5), round, accent));
    } else {
        out.push(fill(at, round, dimmed(WHITE, look)));
        out.push(ring(at, round, 1., dimmed(EDGE, look)));
    }
}

/// The chevron of a drop-down, 8px wide, 18px from the right edge.
fn arrow(at: Rect, look: &ControlLook, out: &mut Vec<Shape>) {
    let origin = Rect::new(at.x + at.width - 18., at.y + at.height / 2., 0., 0.);
    let stroke = [[0., -1.5], [4., 2.], [8., -1.5]];
    out.extend(squares(&stroke, 1.6, origin, dimmed(look.color, look)));
}

/// Little squares along the polyline `points` (relative to the origin of `at`), `size` wide.
fn squares(points: &[[f32; 2]], size: f32, at: Rect, color: Rgba) -> Vec<Shape> {
    let step = size * 0.5;
    let mut out = Vec::new();
    for pair in points.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let length = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2)).sqrt();
        let count = (length / step).ceil().max(1.) as usize;
        for index in 0..=count {
            let t = index as f32 / count as f32;
            let x = at.x + from[0] + (to[0] - from[0]) * t - size / 2.;
            let y = at.y + from[1] + (to[1] - from[1]) * t - size / 2.;
            out.push(fill(Rect::new(x, y, size, size), size / 2., color));
        }
    }
    out
}

fn range(at: Rect, (min, max, value): (f64, f64, f64), look: &ControlLook, out: &mut Vec<Shape>) {
    let track = Rect::new(at.x, at.y + at.height / 2. - 2., at.width, 4.);
    let across = if max > min {
        ((value - min) / (max - min)) as f32
    } else {
        0.
    };
    let diameter = 12_f32.min(at.height);
    let thumb_x = at.x + diameter / 2. + (at.width - diameter) * across;
    out.push(fill(track, 2., dimmed(TRACK, look)));
    let done = Rect::new(track.x, track.y, (thumb_x - track.x).max(0.), track.height);
    out.push(fill(done, 2., dimmed(look.accent, look)));
    let thumb = Rect::new(
        thumb_x - diameter / 2.,
        at.y + (at.height - diameter) / 2.,
        diameter,
        diameter,
    );
    out.push(fill(thumb, diameter / 2., dimmed(look.accent, look)));
}

fn progress(at: Rect, value: Option<f64>, look: &ControlLook, out: &mut Vec<Shape>) {
    let bar = Rect::new(at.x, at.y + at.height / 2. - 4., at.width, 8.);
    out.push(fill(bar, 4., GROOVE));
    // A bar whose progress is not known stands at a third.
    let share = value.map_or(1. / 3., |value| value as f32);
    let done = Rect::new(bar.x, bar.y, bar.width * share, bar.height);
    out.push(fill(done, 4., dimmed(look.accent, look)));
}

fn meter(at: Rect, [value, min, max, low, high, optimum]: [f64; 6], out: &mut Vec<Shape>) {
    let bar = Rect::new(at.x, at.y + at.height / 2. - 4., at.width, 8.);
    out.push(fill(bar, 4., GROOVE));
    // Which region the value is in against the optimum's: green in it, yellow next to it, red
    // two away (the HTML standard's rules for a meter's colour).
    let region = |at: f64| u8::from(at >= low) + u8::from(at >= high);
    let color = match region(value).abs_diff(region(optimum)) {
        0 => [0.063, 0.627, 0.188, 1.],
        1 => [0.929, 0.69, 0.0, 1.],
        _ => [0.8, 0.114, 0.114, 1.],
    };
    let share = if max > min {
        ((value - min) / (max - min)) as f32
    } else {
        0.
    };
    out.push(fill(
        Rect::new(bar.x, bar.y, bar.width * share, bar.height),
        4.,
        color,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use verve_layout::DisplayList;

    fn shapes(kind: ControlKind) -> Vec<Shape> {
        let look = ControlLook {
            accent: [0., 0.4, 1., 1.],
            color: [0., 0., 0., 1.],
            disabled: false,
        };
        let list = DisplayList {
            items: vec![PaintItem::Control {
                node: 3,
                bounds: Rect::new(10., 20., 13., 13.),
                kind,
                look,
            }],
            ..DisplayList::default()
        };
        let plan = crate::plan::plan(&list, &|_| None);
        plan.commands.into_iter().map(|c| c.shape).collect()
    }

    #[test]
    fn a_checked_box_is_the_accent_with_a_check_on_it() {
        let drawn = shapes(ControlKind::Checkbox {
            checked: true,
            indeterminate: false,
        });
        assert!(matches!(
            &drawn[0],
            Shape::Fill { paint: Paint::Solid(c), .. } if *c == [0., 0.4, 1., 1.]
        ));
        assert!(drawn.len() > 5, "the check is drawn: {}", drawn.len());
    }

    #[test]
    fn an_unchecked_box_has_an_edge_and_nothing_inside() {
        let drawn = shapes(ControlKind::Checkbox {
            checked: false,
            indeterminate: false,
        });
        assert_eq!(drawn.len(), 2);
        assert!(matches!(drawn[1], Shape::Border { .. }));
    }

    #[test]
    fn a_caret_is_a_shape_the_shell_can_blink() {
        let drawn = shapes(ControlKind::TextField {
            caret: Some(Rect::new(14., 21., 0., 15.)),
            marked: Vec::new(),
            multiline: false,
            focused: true,
        });
        assert_eq!(
            drawn,
            [Shape::Caret {
                bounds: Rect::new(14., 21., 1., 15.),
                color: [0., 0., 0., 1.]
            }]
        );
    }

    #[test]
    fn a_meter_is_green_at_its_optimum_and_red_two_regions_away() {
        let color_at = |value: f64| {
            let mut out = Vec::new();
            meter(
                Rect::new(0., 0., 80., 16.),
                [value, 0., 10., 3., 7., 9.],
                &mut out,
            );
            match &out[1] {
                Shape::Fill {
                    paint: Paint::Solid(color),
                    ..
                } => *color,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(color_at(8.)[1], 0.627);
        assert_eq!(color_at(1.)[0], 0.8);
    }
}
