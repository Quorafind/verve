//! Canvas paths: the one a context builds with `moveTo` and the rest, `Path2D`'s own, and the
//! SVG path strings `new Path2D(d)` reads.
//!
//! The commands are the same natives on both: each reads its arguments into a [`Command`], and
//! [`edit`] applies it to the path the receiver means, so the geometry is written once.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use boa_engine::{Context, JsError, JsResult, JsValue};
use tiny_skia::{FillRule, Mask, Path, PathBuilder, Point, Transform};

use crate::bind::{Kind, Method, dom_error, handle_of};
use crate::canvas::finite_numbers;
use crate::canvas::matrix::transform_argument;
use crate::canvas::store::{CanvasStore, illegal_invocation, this_handle};
use crate::state::shared;
use verve::native::{arg, list_of, type_error};

/// The commands `CanvasRenderingContext2D` and `Path2D` share.
pub const PATH_METHODS: &[Method] = &[
    ("moveTo", 2, |this, args, cx| {
        let command = finite_numbers(args, cx)?.map(|[x, y]| Command::MoveTo(x, y));
        edit(this, cx, command)
    }),
    ("lineTo", 2, |this, args, cx| {
        let command = finite_numbers(args, cx)?.map(|[x, y]| Command::LineTo(x, y));
        edit(this, cx, command)
    }),
    ("bezierCurveTo", 6, |this, args, cx| {
        let command = finite_numbers(args, cx)?.map(Command::CubicTo);
        edit(this, cx, command)
    }),
    ("quadraticCurveTo", 4, |this, args, cx| {
        let command = finite_numbers(args, cx)?.map(Command::QuadTo);
        edit(this, cx, command)
    }),
    ("arc", 6, |this, args, cx| {
        let command = arc_command(args, cx, interface_of(this))?;
        edit(this, cx, command)
    }),
    ("arcTo", 5, |this, args, cx| {
        let command = arc_to_command(args, cx, interface_of(this))?;
        edit(this, cx, command)
    }),
    ("ellipse", 8, |this, args, cx| {
        let command = ellipse_command(args, cx, interface_of(this))?;
        edit(this, cx, command)
    }),
    ("rect", 4, |this, args, cx| {
        let command = finite_numbers(args, cx)?.map(Command::Rect);
        edit(this, cx, command)
    }),
    ("roundRect", 2, |this, args, cx| {
        let command = round_rect_command(args, cx)?;
        edit(this, cx, command)
    }),
    ("closePath", 0, |this, _, cx| {
        edit(this, cx, Some(Command::Close))
    }),
];

/// The commands only a `Path2D` has.
pub const PATH2D_METHODS: &[Method] = &[("addPath", 2, add_path)];

/// Which way an arc turns from its start to its end.
#[derive(Clone, Copy, PartialEq)]
pub enum Turn {
    Clockwise,
    Counterclockwise,
}

/// An elliptical arc as `ellipse()` takes it (and `arc()`, with equal radii and no rotation).
#[derive(Clone, Copy)]
struct Ellipse {
    center: [f32; 2],
    radii: [f32; 2],
    rotation: f32,
    start: f32,
    end: f32,
    turn: Turn,
}

/// One path-building command, its arguments read and checked.
enum Command {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    CubicTo([f32; 6]),
    QuadTo([f32; 4]),
    Arc(Ellipse),
    ArcTo([f32; 5]),
    Rect([f32; 4]),
    RoundRect { rect: [f32; 4], radii: [f32; 4] },
    Close,
}

impl Command {
    fn apply(self, path: &mut PathBuilder) {
        match self {
            Command::MoveTo(x, y) => path.move_to(x, y),
            Command::LineTo(x, y) => path.line_to(x, y),
            Command::CubicTo([a, b, c, d, e, f]) => path.cubic_to(a, b, c, d, e, f),
            Command::QuadTo([a, b, c, d]) => path.quad_to(a, b, c, d),
            Command::Arc(ellipse) => push_ellipse(path, ellipse),
            Command::ArcTo(values) => push_arc_to(path, values),
            Command::Rect([x, y, width, height]) => push_rect(path, x, y, width, height),
            Command::RoundRect { rect, radii } => push_round_rect(path, rect, radii),
            Command::Close => path.close(),
        }
    }
}

/// The interface a path command was called on, for its error messages.
fn interface_of(this: &JsValue) -> &'static str {
    match handle_of(this, Kind::Path) {
        Some(_) => "Path2D",
        None => "CanvasRenderingContext2D",
    }
}

fn negative_radius(context: &mut Context, method: &str, interface: &str) -> JsError {
    dom_error(
        context,
        &format!("Failed to execute '{method}' on '{interface}': The radius provided is negative."),
        "IndexSizeError",
    )
}

fn arc_command(
    args: &[JsValue],
    context: &mut Context,
    interface: &str,
) -> JsResult<Option<Command>> {
    let Some([x, y, radius, start, end]) = finite_numbers(args, context)? else {
        return Ok(None);
    };
    if radius < 0. {
        return Err(negative_radius(context, "arc", interface));
    }
    Ok(Some(Command::Arc(Ellipse {
        center: [x, y],
        radii: [radius, radius],
        rotation: 0.,
        start,
        end,
        turn: turn_of(&arg(args, 5)),
    })))
}

fn ellipse_command(
    args: &[JsValue],
    context: &mut Context,
    interface: &str,
) -> JsResult<Option<Command>> {
    let Some([x, y, rx, ry, rotation, start, end]) = finite_numbers(args, context)? else {
        return Ok(None);
    };
    if rx < 0. || ry < 0. {
        return Err(negative_radius(context, "ellipse", interface));
    }
    Ok(Some(Command::Arc(Ellipse {
        center: [x, y],
        radii: [rx, ry],
        rotation,
        start,
        end,
        turn: turn_of(&arg(args, 7)),
    })))
}

fn turn_of(counterclockwise: &JsValue) -> Turn {
    match counterclockwise.to_boolean() {
        true => Turn::Counterclockwise,
        false => Turn::Clockwise,
    }
}

fn arc_to_command(
    args: &[JsValue],
    context: &mut Context,
    interface: &str,
) -> JsResult<Option<Command>> {
    let Some(values) = finite_numbers::<5>(args, context)? else {
        return Ok(None);
    };
    if values[4] < 0. {
        return Err(negative_radius(context, "arcTo", interface));
    }
    Ok(Some(Command::ArcTo(values)))
}

fn round_rect_command(args: &[JsValue], context: &mut Context) -> JsResult<Option<Command>> {
    let Some(rect) = finite_numbers::<4>(args, context)? else {
        return Ok(None);
    };
    let radii = corner_radii(&arg(args, 4), context)?;
    Ok(Some(Command::RoundRect { rect, radii }))
}

/// The four corner radii of `roundRect`, in CSS order, from a number or a list of one to four.
fn corner_radii(value: &JsValue, context: &mut Context) -> JsResult<[f32; 4]> {
    let items = match value.is_object() {
        true => list_of(value, context)?,
        false => vec![value.clone()],
    };
    let mut values = Vec::with_capacity(4);
    for item in items.iter().take(4) {
        values.push(item.to_number(context)? as f32);
    }
    Ok(match values[..] {
        [all] => [all; 4],
        [diagonal, other] => [diagonal, other, diagonal, other],
        [top_left, other, bottom_right] => [top_left, other, bottom_right, other],
        [a, b, c, d] => [a, b, c, d],
        _ => [0.; 4],
    })
}

/// Applies `command` to the path `this` means: a context's current path, or a `Path2D`'s.
fn edit(this: &JsValue, context: &mut Context, command: Option<Command>) -> JsResult<JsValue> {
    let state = shared(context);
    let mut state = state.borrow_mut();
    let path = path_of_receiver(&mut state.canvas, this)?;
    if let Some(command) = command {
        command.apply(path);
    }
    Ok(JsValue::undefined())
}

fn path_of_receiver<'a>(
    store: &'a mut CanvasStore,
    this: &JsValue,
) -> JsResult<&'a mut PathBuilder> {
    if let Some(id) = handle_of(this, Kind::Context2d) {
        let canvas = store.canvases.get_mut(&id);
        return canvas
            .map(|canvas| &mut canvas.path)
            .ok_or_else(|| illegal_invocation("CanvasRenderingContext2D"));
    }
    let id = this_handle(this, Kind::Path, "Path2D")?;
    Ok(store.paths.entry(id).or_default())
}

/// `Path2D.addPath(path, transform)`.
fn add_path(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = this_handle(this, Kind::Path, "Path2D")?;
    let source = handle_of(&arg(args, 0), Kind::Path).ok_or_else(|| {
        type_error("Failed to execute 'addPath' on 'Path2D': parameter 1 is not of type 'Path2D'.")
    })?;
    let transform = transform_argument(&arg(args, 1), context)?;
    let state = shared(context);
    let mut state = state.borrow_mut();
    let moved = state
        .canvas
        .paths
        .get(&source)
        .and_then(|path| path.clone().finish())
        .and_then(|path| path.transform(transform));
    if let (Some(moved), Some(target)) = (moved, state.canvas.paths.get_mut(&target)) {
        target.push_path(&moved);
    }
    Ok(JsValue::undefined())
}

/// A path of one rectangle `[x, y, width, height]`.
pub(crate) fn rectangle(rect: [f32; 4]) -> PathBuilder {
    let mut path = PathBuilder::new();
    let [x, y, width, height] = rect;
    push_rect(&mut path, x, y, width, height);
    path
}

/// A rectangle in the direction it was given: a negative width draws backwards.
pub(crate) fn push_rect(path: &mut PathBuilder, x: f32, y: f32, width: f32, height: f32) {
    let (right, bottom) = (x + width, y + height);
    path.move_to(x, y);
    path.line_to(right, y);
    path.line_to(right, bottom);
    path.line_to(x, bottom);
    path.close();
}

/// A rectangle with rounded corners, `[top-left, top-right, bottom-right, bottom-left]` radii
/// each cut to half the shorter side.
fn push_round_rect(path: &mut PathBuilder, [x, y, width, height]: [f32; 4], radii: [f32; 4]) {
    if width == 0. || height == 0. {
        push_rect(path, x, y, width, height);
        return;
    }
    let (left, right) = (x.min(x + width), x.max(x + width));
    let (top, bottom) = (y.min(y + height), y.max(y + height));
    let half = ((right - left) / 2.).min((bottom - top) / 2.);
    let [top_left, top_right, bottom_right, bottom_left] = radii.map(|r| r.clamp(0., half));
    // Each corner: its arc's center, radius and the angle the arc starts at.
    let corners = [
        (right - top_right, top + top_right, top_right, -FRAC_PI_2),
        (
            right - bottom_right,
            bottom - bottom_right,
            bottom_right,
            0.,
        ),
        (
            left + bottom_left,
            bottom - bottom_left,
            bottom_left,
            FRAC_PI_2,
        ),
        (left + top_left, top + top_left, top_left, PI),
    ];
    path.move_to(left + top_left, top);
    for (center_x, center_y, radius, start) in corners {
        path.line_to(
            center_x + radius * start.cos(),
            center_y + radius * start.sin(),
        );
        let arc = Ellipse {
            center: [center_x, center_y],
            radii: [radius, radius],
            rotation: 0.,
            start,
            end: start + FRAC_PI_2,
            turn: Turn::Clockwise,
        };
        push_ellipse(path, arc);
    }
    path.close();
}

/// `arcTo`'s circle between the current point and the two it is given, by the specification's
/// algorithm: a line when the corner is degenerate.
fn push_arc_to(path: &mut PathBuilder, [x1, y1, x2, y2, radius]: [f32; 5]) {
    let Some(start) = path.last_point() else {
        path.move_to(x1, y1);
        return;
    };
    let (ax, ay) = (start.x - x1, start.y - y1);
    let (bx, by) = (x2 - x1, y2 - y1);
    let (a_length, b_length) = (ax.hypot(ay), bx.hypot(by));
    if radius == 0. || a_length == 0. || b_length == 0. {
        path.line_to(x1, y1);
        return;
    }
    let (ux, uy) = (ax / a_length, ay / a_length);
    let (vx, vy) = (bx / b_length, by / b_length);
    let cross = ux * vy - uy * vx;
    let dot = (ux * vx + uy * vy).clamp(-1., 1.);
    let half = dot.acos() / 2.;
    let distance = radius / half.tan();
    if cross.abs() < COLLINEAR || !distance.is_finite() {
        path.line_to(x1, y1);
        return;
    }
    let (tangent_x, tangent_y) = (x1 + ux * distance, y1 + uy * distance);
    let (end_x, end_y) = (x1 + vx * distance, y1 + vy * distance);
    let (bisector_x, bisector_y) = ((ux + vx) / 2., (uy + vy) / 2.);
    let reach = radius / half.sin() / bisector_x.hypot(bisector_y);
    let (center_x, center_y) = (x1 + bisector_x * reach, y1 + bisector_y * reach);
    let start = (tangent_y - center_y).atan2(tangent_x - center_x);
    let end = (end_y - center_y).atan2(end_x - center_x);
    path.line_to(tangent_x, tangent_y);
    let turn = match cross < 0. {
        true => Turn::Counterclockwise,
        false => Turn::Clockwise,
    };
    let arc = Ellipse {
        center: [center_x, center_y],
        radii: [radius, radius],
        rotation: 0.,
        start,
        end,
        turn,
    };
    push_ellipse(path, arc);
}

/// Below this `sin` of the angle between `arcTo`'s two lines, they are one line.
const COLLINEAR: f32 = 1e-6;

/// An arc, joined to the current point by a line as the specification says.
fn push_ellipse(path: &mut PathBuilder, ellipse: Ellipse) {
    let [x, y] = ellipse.center;
    let [rx, ry] = ellipse.radii;
    let sweep = sweep_of(ellipse.start, ellipse.end, ellipse.turn);
    let (start_x, start_y) = point_on(x, y, rx, ry, ellipse.rotation, ellipse.start);
    match path.last_point() {
        Some(last)
            if (last.x - start_x).abs() < JOIN_TOLERANCE
                && (last.y - start_y).abs() < JOIN_TOLERANCE => {}
        Some(_) => path.line_to(start_x, start_y),
        None => path.move_to(start_x, start_y),
    }
    push_arc(
        path,
        ellipse.center,
        ellipse.radii,
        ellipse.rotation,
        ellipse.start,
        sweep,
    );
}

/// How close an arc's start must be to the current point to need no joining line.
const JOIN_TOLERANCE: f32 = 1e-4;

/// How far an arc turns, in radians, in the direction asked for.
fn sweep_of(start: f32, end: f32, turn: Turn) -> f32 {
    // A whole turn or more is exactly one circle, not the remainder.
    match (turn, end - start) {
        (Turn::Clockwise, whole) if whole >= TAU => TAU,
        (Turn::Counterclockwise, whole) if -whole >= TAU => -TAU,
        (Turn::Clockwise, part) => part.rem_euclid(TAU),
        (Turn::Counterclockwise, part) => -(-part).rem_euclid(TAU),
    }
}

fn point_on(x: f32, y: f32, rx: f32, ry: f32, rotation: f32, angle: f32) -> (f32, f32) {
    let (sin, cos) = rotation.sin_cos();
    let (ax, ay) = (rx * angle.cos(), ry * angle.sin());
    (x + ax * cos - ay * sin, y + ax * sin + ay * cos)
}

fn derivative(rx: f32, ry: f32, rotation: f32, angle: f32) -> (f32, f32) {
    let (sin, cos) = rotation.sin_cos();
    let (ax, ay) = (-rx * angle.sin(), ry * angle.cos());
    (ax * cos - ay * sin, ax * sin + ay * cos)
}

/// An elliptical arc from `start` turning `sweep` radians, as cubic curves no wider than a
/// quarter turn each.
pub(crate) fn push_arc(
    path: &mut PathBuilder,
    [x, y]: [f32; 2],
    [rx, ry]: [f32; 2],
    rotation: f32,
    start: f32,
    sweep: f32,
) {
    if rx == 0. || ry == 0. || sweep == 0. {
        return;
    }
    let count = (sweep.abs() / FRAC_PI_2).ceil() as usize;
    let step = sweep / count as f32;
    // The control points of a cubic that follows a circular arc of `step` radians.
    let k = 4. / 3. * (step / 4.).tan();
    for at in 0..count {
        let from = start + step * at as f32;
        let to = from + step;
        let (x0, y0) = point_on(x, y, rx, ry, rotation, from);
        let (x3, y3) = point_on(x, y, rx, ry, rotation, to);
        let (dx0, dy0) = derivative(rx, ry, rotation, from);
        let (dx1, dy1) = derivative(rx, ry, rotation, to);
        path.cubic_to(
            x0 + k * dx0,
            y0 + k * dy0,
            x3 - k * dx1,
            y3 - k * dy1,
            x3,
            y3,
        );
    }
}

// ─── SVG path data ──────────────────────────────────────────────────────────────────────────

/// An SVG path's data as a path: `new Path2D('M0 0 L10 10 Z')`. `None` where the data is
/// malformed (the specification keeps what came before; Chrome draws nothing).
pub fn from_svg(data: &str) -> Option<PathBuilder> {
    let mut parser = Svg {
        data: data.as_bytes(),
        at: 0,
        path: PathBuilder::new(),
        current: Point::zero(),
        start: Point::zero(),
        last_control: None,
        last_command: ' ',
    };
    parser.run()?;
    Some(parser.path)
}

/// Whether a curve command reads its first control point or reflects the last.
#[derive(Clone, Copy)]
enum Control {
    Given,
    Reflected,
}

/// Which curve family a smooth command (`S`, `T`) continues.
#[derive(Clone, Copy)]
enum Curve {
    Cubic,
    Quadratic,
}

struct Svg<'a> {
    data: &'a [u8],
    at: usize,
    path: PathBuilder,
    current: Point,
    start: Point,
    last_control: Option<Point>,
    last_command: char,
}

impl Svg<'_> {
    fn run(&mut self) -> Option<()> {
        let mut command = ' ';
        loop {
            self.skip_separators();
            let Some(&byte) = self.data.get(self.at) else {
                return Some(());
            };
            if byte.is_ascii_alphabetic() {
                command = byte as char;
                self.at += 1;
            } else if command == ' ' {
                return None;
            }
            let relative = command.is_ascii_lowercase();
            let offset = if relative {
                self.current
            } else {
                Point::zero()
            };
            self.segment(command.to_ascii_uppercase(), offset)?;
            if command.eq_ignore_ascii_case(&'m') {
                // Further pairs after a move are lines.
                command = if relative { 'l' } else { 'L' };
            }
            self.last_command = command;
        }
    }

    /// One command's arguments and effect, its coordinates relative to `offset`.
    fn segment(&mut self, letter: char, offset: Point) -> Option<()> {
        match letter {
            'M' => self.move_to(offset),
            'L' => self.line_to(offset),
            'H' => self.horizontal(offset),
            'V' => self.vertical(offset),
            'C' => self.cubic(offset, Control::Given),
            'S' => self.cubic(offset, Control::Reflected),
            'Q' => self.quadratic(offset, Control::Given),
            'T' => self.quadratic(offset, Control::Reflected),
            'A' => self.arc(offset),
            'Z' => {
                self.path.close();
                self.current = self.start;
                Some(())
            }
            _ => None,
        }
    }

    fn move_to(&mut self, offset: Point) -> Option<()> {
        let to = self.point(offset)?;
        self.current = to;
        self.start = to;
        self.path.move_to(to.x, to.y);
        Some(())
    }

    fn line_to(&mut self, offset: Point) -> Option<()> {
        let to = self.point(offset)?;
        self.path.line_to(to.x, to.y);
        self.current = to;
        Some(())
    }

    fn horizontal(&mut self, offset: Point) -> Option<()> {
        let x = self.number()? + offset.x;
        self.path.line_to(x, self.current.y);
        self.current.x = x;
        Some(())
    }

    fn vertical(&mut self, offset: Point) -> Option<()> {
        let y = self.number()? + offset.y;
        self.path.line_to(self.current.x, y);
        self.current.y = y;
        Some(())
    }

    /// `C`, and `S` whose first control point is the last one reflected.
    fn cubic(&mut self, offset: Point, control: Control) -> Option<()> {
        let first = match control {
            Control::Reflected => self.reflected(Curve::Cubic),
            Control::Given => self.point(offset)?,
        };
        let second = self.point(offset)?;
        let to = self.point(offset)?;
        self.path
            .cubic_to(first.x, first.y, second.x, second.y, to.x, to.y);
        self.current = to;
        self.last_control = Some(second);
        Some(())
    }

    /// `Q`, and `T` whose control point is the last one reflected.
    fn quadratic(&mut self, offset: Point, control: Control) -> Option<()> {
        let control = match control {
            Control::Reflected => self.reflected(Curve::Quadratic),
            Control::Given => self.point(offset)?,
        };
        let to = self.point(offset)?;
        self.path.quad_to(control.x, control.y, to.x, to.y);
        self.current = to;
        self.last_control = Some(control);
        Some(())
    }

    fn arc(&mut self, offset: Point) -> Option<()> {
        let (rx, ry, rotation) = (self.number()?, self.number()?, self.number()?);
        let large = self.number()? != 0.;
        let sweep = self.number()? != 0.;
        let end = self.point(offset)?;
        self.arc_to(rx, ry, rotation, (large, sweep), end);
        Some(())
    }

    /// The control point a smooth curve reflects: the last one, or the current point.
    fn reflected(&self, curve: Curve) -> Point {
        let continues = match curve {
            Curve::Cubic => matches!(self.last_command.to_ascii_uppercase(), 'C' | 'S'),
            Curve::Quadratic => matches!(self.last_command.to_ascii_uppercase(), 'Q' | 'T'),
        };
        match self.last_control {
            Some(control) if continues => Point::from_xy(
                2. * self.current.x - control.x,
                2. * self.current.y - control.y,
            ),
            _ => self.current,
        }
    }

    /// An SVG elliptical arc from the current point to `end`, as the standard describes it:
    /// radii are scaled up when the endpoints cannot be joined, the flags pick which of the
    /// four arcs is drawn.
    fn arc_to(
        &mut self,
        rx: f32,
        ry: f32,
        rotation: f32,
        (large, sweep): (bool, bool),
        end: Point,
    ) {
        let from = self.current;
        let (mut rx, mut ry) = (rx.abs(), ry.abs());
        self.current = end;
        if rx == 0. || ry == 0. || from == end {
            self.path.line_to(end.x, end.y);
            return;
        }
        let radians = rotation.to_radians();
        let (sin, cos) = radians.sin_cos();
        let (dx, dy) = ((from.x - end.x) / 2., (from.y - end.y) / 2.);
        let (px, py) = (cos * dx + sin * dy, -sin * dx + cos * dy);
        let scale = (px * px) / (rx * rx) + (py * py) / (ry * ry);
        if scale > 1. {
            rx *= scale.sqrt();
            ry *= scale.sqrt();
        }
        let numerator = ((rx * rx * ry * ry) - (rx * rx * py * py) - (ry * ry * px * px)).max(0.);
        let denominator = (rx * rx * py * py) + (ry * ry * px * px);
        let mut factor = if denominator == 0. {
            0.
        } else {
            (numerator / denominator).sqrt()
        };
        if large == sweep {
            factor = -factor;
        }
        let (center_px, center_py) = (factor * rx * py / ry, -factor * ry * px / rx);
        let center_x = cos * center_px - sin * center_py + (from.x + end.x) / 2.;
        let center_y = sin * center_px + cos * center_py + (from.y + end.y) / 2.;
        let start = ((py - center_py) / ry).atan2((px - center_px) / rx);
        let stop = ((-py - center_py) / ry).atan2((-px - center_px) / rx);
        let mut turn = stop - start;
        if !sweep && turn > 0. {
            turn -= TAU;
        } else if sweep && turn < 0. {
            turn += TAU;
        }
        let (start_x, start_y) = point_on(center_x, center_y, rx, ry, radians, start);
        self.path.line_to(start_x, start_y);
        push_arc(
            &mut self.path,
            [center_x, center_y],
            [rx, ry],
            radians,
            start,
            turn,
        );
    }

    /// Two numbers, as a point moved by `offset`.
    fn point(&mut self, offset: Point) -> Option<Point> {
        Some(Point::from_xy(
            self.number()? + offset.x,
            self.number()? + offset.y,
        ))
    }

    fn skip_separators(&mut self) {
        while matches!(
            self.data.get(self.at),
            Some(b' ' | b',' | b'\t' | b'\n' | b'\r')
        ) {
            self.at += 1;
        }
    }

    fn digits(&mut self) -> bool {
        let before = self.at;
        while self.data.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        self.at > before
    }

    fn sign(&mut self) {
        if matches!(self.data.get(self.at), Some(b'-' | b'+')) {
            self.at += 1;
        }
    }

    /// A number as SVG writes one: sign, digits, a fraction, an exponent.
    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let start = self.at;
        self.sign();
        let mut digits = self.digits();
        if self.data.get(self.at) == Some(&b'.') {
            self.at += 1;
            digits |= self.digits();
        }
        if !digits {
            return None;
        }
        if matches!(self.data.get(self.at), Some(b'e' | b'E')) {
            let mark = self.at;
            self.at += 1;
            self.sign();
            if !self.digits() {
                self.at = mark;
            }
        }
        std::str::from_utf8(&self.data[start..self.at])
            .ok()?
            .parse()
            .ok()
    }
}

/// `isPointInPath`'s answer for a path in device space: whether the point covers any of it.
pub(crate) fn covers(path: &Path, rule: FillRule, x: f32, y: f32) -> bool {
    let Some(mut mask) = Mask::new(1, 1) else {
        return false;
    };
    mask.fill_path(
        path,
        rule,
        true,
        Transform::from_translate(0.5 - x, 0.5 - y),
    );
    mask.data()[0] > HALF_COVERED
}

/// The coverage (of 255) above which a point counts as inside a path.
const HALF_COVERED: u8 = 0x40;
