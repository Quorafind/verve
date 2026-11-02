//! The methods of `CanvasRenderingContext2D`. Each reads its arguments, calls one typed
//! function on the canvas, and answers what the specification says.

use boa_engine::{Context, JsResult, JsValue};
use tiny_skia::{FillRule, PathBuilder, Point, Transform};

use crate::bind::{
    self, Kind, Method, arg, arg_string, array, dom_error, get, handle_of, list, native_instance,
    set, text, type_error,
};
use crate::canvas::context2d::{Canvas2d, Op, Place, Scene, hit_test};
use crate::canvas::finite_numbers;
use crate::canvas::matrix::{matrix_object, transform_argument};
use crate::canvas::paint::{GradientKind, GradientSpec, PatternSpec, Repetition};
use crate::canvas::path::rectangle;
use crate::canvas::pixels::{Region, blank_bytes, image_data_pixels, make_image_data};
use crate::canvas::properties::value_of;
use crate::canvas::source::source_pixmap;
use crate::canvas::store::{CanvasId, canvas_object, read_canvas, this_handle};
use crate::canvas::text::{Align, HANGING_BASELINE, Placement};
use crate::state::shared;

pub const CONTEXT_METHODS: &[Method] = &[
    ("save", 0, |this, _, cx| update(this, cx, Canvas2d::save)),
    ("restore", 0, |this, _, cx| {
        update(this, cx, Canvas2d::restore)
    }),
    ("reset", 0, |this, _, cx| update(this, cx, Canvas2d::reset)),
    ("scale", 2, scale),
    ("rotate", 1, rotate),
    ("translate", 2, translate),
    ("transform", 6, transform),
    ("setTransform", 1, set_transform),
    ("resetTransform", 0, |this, _, cx| {
        update(this, cx, |canvas| {
            canvas.draw.transform = Transform::identity()
        })
    }),
    ("getTransform", 0, get_transform),
    ("fillRect", 4, fill_rect),
    ("strokeRect", 4, stroke_rect),
    ("clearRect", 4, clear_rect),
    ("beginPath", 0, |this, _, cx| {
        update(this, cx, |canvas| canvas.path = PathBuilder::new())
    }),
    ("fill", 1, fill),
    ("stroke", 1, stroke),
    ("clip", 1, clip),
    ("isPointInPath", 2, is_point_in_path),
    ("isPointInStroke", 2, is_point_in_stroke),
    ("createLinearGradient", 4, create_linear_gradient),
    ("createRadialGradient", 6, create_radial_gradient),
    ("createConicGradient", 3, create_conic_gradient),
    ("createPattern", 2, create_pattern),
    ("setLineDash", 1, set_line_dash),
    ("getLineDash", 0, get_line_dash),
    ("fillText", 3, |this, args, cx| {
        draw_text(this, args, cx, Op::Fill(FillRule::Winding))
    }),
    ("strokeText", 3, |this, args, cx| {
        draw_text(this, args, cx, Op::Stroke)
    }),
    ("measureText", 1, measure_text),
    ("drawImage", 3, draw_image),
    ("getImageData", 4, get_image_data),
    ("putImageData", 3, put_image_data),
    ("createImageData", 2, create_image_data),
    ("getContextAttributes", 0, get_context_attributes),
    ("drawFocusIfNeeded", 1, |_, _, _| Ok(JsValue::undefined())),
    ("scrollPathIntoView", 0, |_, _, _| Ok(JsValue::undefined())),
];

fn canvas_id(this: &JsValue) -> JsResult<CanvasId> {
    this_handle(this, Kind::Context2d, "CanvasRenderingContext2D")
}

/// Runs `change` on the canvas `this` draws into.
fn update(
    this: &JsValue,
    context: &mut Context,
    change: impl FnOnce(&mut Canvas2d),
) -> JsResult<JsValue> {
    let id = canvas_id(this)?;
    if let Some(canvas) = shared(context).borrow_mut().canvas.canvases.get_mut(&id) {
        change(canvas);
    }
    Ok(JsValue::undefined())
}

/// Looks at the canvas `this` draws into.
fn read<R>(
    this: &JsValue,
    context: &Context,
    look: impl FnOnce(&Canvas2d) -> R,
) -> JsResult<Option<R>> {
    read_canvas(
        this,
        context,
        (Kind::Context2d, "CanvasRenderingContext2D"),
        look,
    )
}

/// Runs `draw` on the canvas `this` draws into, with the stores a paint reads.
fn with_scene(
    this: &JsValue,
    context: &mut Context,
    draw: impl FnOnce(&mut Scene),
) -> JsResult<JsValue> {
    let id = canvas_id(this)?;
    if let Some(mut scene) = shared(context).borrow_mut().canvas.scene(id) {
        draw(&mut scene);
    }
    Ok(JsValue::undefined())
}

/// A non-finite argument is a `TypeError`, as in browsers.
fn non_finite(method: &str) -> boa_engine::JsError {
    type_error(format!(
        "Failed to execute '{method}' on 'CanvasRenderingContext2D': The provided float value is non-finite."
    ))
}

// ─── Transforms ─────────────────────────────────────────────────────────────────────────────

fn scale(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some([x, y]) = finite_numbers(args, context)? else {
        return Ok(JsValue::undefined());
    };
    update(this, context, |canvas| {
        canvas.concat(Transform::from_scale(x, y))
    })
}

fn rotate(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some([angle]) = finite_numbers(args, context)? else {
        return Ok(JsValue::undefined());
    };
    update(this, context, |canvas| {
        canvas.concat(Transform::from_rotate(angle.to_degrees()))
    })
}

fn translate(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some([x, y]) = finite_numbers(args, context)? else {
        return Ok(JsValue::undefined());
    };
    update(this, context, |canvas| {
        canvas.concat(Transform::from_translate(x, y))
    })
}

fn transform(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some([a, b, c, d, e, f]) = finite_numbers(args, context)? else {
        return Ok(JsValue::undefined());
    };
    update(this, context, |canvas| {
        canvas.concat(Transform::from_row(a, b, c, d, e, f))
    })
}

/// `setTransform(a, b, c, d, e, f)` and `setTransform(matrix)`.
fn set_transform(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let transform = match arg(args, 0).is_object() {
        true => transform_argument(&arg(args, 0), context)?,
        false => match finite_numbers(args, context)? {
            Some([a, b, c, d, e, f]) => Transform::from_row(a, b, c, d, e, f),
            None => return Ok(JsValue::undefined()),
        },
    };
    update(this, context, |canvas| canvas.draw.transform = transform)
}

fn get_transform(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let transform = read(this, context, |canvas| canvas.draw.transform)?;
    matrix_object(transform.unwrap_or_else(Transform::identity), context)
}

// ─── Rectangles and paths ───────────────────────────────────────────────────────────────────

/// Paints `shape` (user space) with the paint `op` uses.
fn paint_shape(
    this: &JsValue,
    context: &mut Context,
    shape: &PathBuilder,
    op: Op,
) -> JsResult<JsValue> {
    with_scene(this, context, |scene| {
        let source = scene.canvas.draw.paint_for(op);
        scene.paint(shape, op, &source);
    })
}

fn fill_rect(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(rect) = finite_numbers(args, context)? else {
        return Ok(JsValue::undefined());
    };
    paint_shape(this, context, &rectangle(rect), Op::Fill(FillRule::Winding))
}

fn stroke_rect(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(rect @ [_, _, width, height]) = finite_numbers(args, context)? else {
        return Ok(JsValue::undefined());
    };
    if width == 0. && height == 0. {
        return Ok(JsValue::undefined());
    }
    paint_shape(this, context, &rectangle(rect), Op::Stroke)
}

fn clear_rect(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(rect) = finite_numbers(args, context)? else {
        return Ok(JsValue::undefined());
    };
    update(this, context, |canvas| canvas.clear_rect(rect))
}

/// `fill`, `stroke` and `clip` take an optional `Path2D` and an optional fill rule.
fn path_and_rule(
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<(Option<CanvasId>, FillRule)> {
    let path = handle_of(&arg(args, 0), Kind::Path);
    let rule = fill_rule(&arg(args, usize::from(path.is_some())), context)?;
    Ok((path, rule))
}

fn fill_rule(value: &JsValue, context: &mut Context) -> JsResult<FillRule> {
    if !value.is_string() {
        return Ok(FillRule::Winding);
    }
    Ok(match bind::string(value, context)?.as_str() {
        "evenodd" => FillRule::EvenOdd,
        _ => FillRule::Winding,
    })
}

/// The path a command means — the `Path2D` given, or the current one — copied out of the store.
fn path_for(
    this: &JsValue,
    path: Option<CanvasId>,
    context: &mut Context,
) -> JsResult<Option<PathBuilder>> {
    let id = canvas_id(this)?;
    Ok(shared(context).borrow().canvas.path_of(path, id))
}

fn fill(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (path, rule) = path_and_rule(args, context)?;
    match path_for(this, path, context)? {
        Some(shape) => paint_shape(this, context, &shape, Op::Fill(rule)),
        None => Ok(JsValue::undefined()),
    }
}

fn stroke(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (path, _) = path_and_rule(args, context)?;
    match path_for(this, path, context)? {
        Some(shape) => paint_shape(this, context, &shape, Op::Stroke),
        None => Ok(JsValue::undefined()),
    }
}

fn clip(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (path, rule) = path_and_rule(args, context)?;
    let Some(shape) = path_for(this, path, context)? else {
        return Ok(JsValue::undefined());
    };
    update(this, context, |canvas| canvas.clip_path(&shape, rule))
}

fn is_point_in_path(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let has_path = handle_of(&arg(args, 0), Kind::Path).is_some();
    let rule = fill_rule(&arg(args, usize::from(has_path) + 2), context)?;
    hit(this, args, context, Op::Fill(rule))
}

fn is_point_in_stroke(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    hit(this, args, context, Op::Stroke)
}

/// `isPointInPath([path,] x, y[, rule])` and `isPointInStroke([path,] x, y)`.
fn hit(this: &JsValue, args: &[JsValue], context: &mut Context, op: Op) -> JsResult<JsValue> {
    let path = handle_of(&arg(args, 0), Kind::Path);
    let numbers = args.get(usize::from(path.is_some())..).unwrap_or_default();
    let Some([x, y]) = finite_numbers(numbers, context)? else {
        return Ok(false.into());
    };
    let id = canvas_id(this)?;
    let shape = shared(context).borrow().canvas.path_of(path, id);
    let hit = shape.and_then(|shape| {
        read(this, context, |canvas| {
            hit_test(&shape, &canvas.draw, op, (x, y))
        })
        .ok()?
    });
    Ok(hit.unwrap_or(false).into())
}

// ─── Gradients and patterns ─────────────────────────────────────────────────────────────────

fn gradient_object(kind: GradientKind, context: &mut Context) -> JsResult<JsValue> {
    let spec = GradientSpec {
        kind,
        stops: Vec::new(),
    };
    let id = shared(context).borrow_mut().canvas.add_gradient(spec);
    Ok(canvas_object(context, id, (Kind::Gradient, "CanvasGradient")).into())
}

fn create_linear_gradient(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some([x0, y0, x1, y1]) = finite_numbers(args, context)? else {
        return Err(non_finite("createLinearGradient"));
    };
    let kind = GradientKind::Linear {
        from: Point::from_xy(x0, y0),
        to: Point::from_xy(x1, y1),
    };
    gradient_object(kind, context)
}

fn create_radial_gradient(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some([x0, y0, r0, x1, y1, r1]) = finite_numbers(args, context)? else {
        return Err(non_finite("createRadialGradient"));
    };
    if r0 < 0. || r1 < 0. {
        let message = "Failed to execute 'createRadialGradient' on 'CanvasRenderingContext2D': The radius provided is negative.";
        return Err(dom_error(context, message, "IndexSizeError"));
    }
    let kind = GradientKind::Radial {
        from: Point::from_xy(x0, y0),
        to: Point::from_xy(x1, y1),
        to_radius: r1,
    };
    gradient_object(kind, context)
}

fn create_conic_gradient(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some([angle, x, y]) = finite_numbers(args, context)? else {
        return Err(non_finite("createConicGradient"));
    };
    let center = Point::from_xy(x, y);
    gradient_object(GradientKind::Conic { center, angle }, context)
}

fn create_pattern(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(source) = source_pixmap(&arg(args, 0), context)? else {
        return Ok(JsValue::null());
    };
    let repetition = match arg(args, 1) {
        value if value.is_null_or_undefined() => Repetition::Repeat,
        value => {
            let name = bind::string(&value, context)?;
            let known = value_of(Repetition::NAMES, &name);
            let known = known.or_else(|| name.is_empty().then_some(Repetition::Repeat));
            known.ok_or_else(|| {
                let message = format!(
                    "Failed to execute 'createPattern' on 'CanvasRenderingContext2D': The provided type ('{name}') is not a valid repetition."
                );
                dom_error(context, &message, "SyntaxError")
            })?
        }
    };
    let spec = PatternSpec {
        pixmap: source.pixmap,
        repetition,
        transform: Transform::identity(),
    };
    let id = shared(context).borrow_mut().canvas.add_pattern(spec);
    Ok(canvas_object(context, id, (Kind::Pattern, "CanvasPattern")).into())
}

// ─── Dashes ─────────────────────────────────────────────────────────────────────────────────

/// `setLineDash`: a list with a negative or non-finite member is ignored, and an odd one is
/// repeated so it alternates evenly.
fn set_line_dash(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut dashes = Vec::new();
    for value in list(&arg(args, 0), context)? {
        let dash = value.to_number(context)?;
        if !dash.is_finite() || dash < 0. {
            return Ok(JsValue::undefined());
        }
        dashes.push(dash as f32);
    }
    if dashes.len() % 2 == 1 {
        dashes.extend_from_within(..);
    }
    update(this, context, |canvas| canvas.draw.line.dash = dashes)
}

fn get_line_dash(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let dashes = read(this, context, |canvas| canvas.draw.line.dash.clone())?;
    let dashes = dashes.unwrap_or_default();
    Ok(array(
        dashes.into_iter().map(JsValue::from).collect(),
        context,
    ))
}

// ─── Text ───────────────────────────────────────────────────────────────────────────────────

/// `fillText` and `strokeText`: `content`, a point, and an optional width to condense to.
fn draw_text(this: &JsValue, args: &[JsValue], context: &mut Context, op: Op) -> JsResult<JsValue> {
    let content = arg_string(args, 0, context)?;
    let Some([x, y]) = finite_numbers(args.get(1..).unwrap_or_default(), context)? else {
        return Ok(JsValue::undefined());
    };
    let max_width = match arg(args, 3).is_undefined() {
        true => None,
        // A `maxWidth` that is not finite draws nothing.
        false => match finite_numbers::<1>(args.get(3..).unwrap_or_default(), context)? {
            Some([width]) => Some(width),
            None => return Ok(JsValue::undefined()),
        },
    };
    with_scene(this, context, |scene| {
        let draw = &scene.canvas.draw;
        let placement = Placement {
            x,
            y,
            align: physical_align(draw.align, draw.rtl),
            baseline: draw.baseline,
            max_width,
        };
        scene.draw_text(&content, placement, op);
    })
}

/// `start` and `end` are the left and right edge, or the right and left in right-to-left text.
fn physical_align(align: Align, rtl: bool) -> Align {
    match (align, rtl) {
        (Align::Start, false) | (Align::End, true) => Align::Left,
        (Align::Start, true) | (Align::End, false) => Align::Right,
        (align, _) => align,
    }
}

fn measure_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let content = arg_string(args, 0, context)?;
    let id = canvas_id(this)?;
    let metrics = shared(context)
        .borrow_mut()
        .canvas
        .scene(id)
        .map(|mut scene| scene.measure(&content));
    let metrics = metrics.ok_or_else(|| type_error("The canvas has no 2d context."))?;
    let object = native_instance(context, "TextMetrics", Kind::Plain, 0);
    let members = [
        ("width", metrics.width),
        ("actualBoundingBoxLeft", metrics.actual_left),
        ("actualBoundingBoxRight", metrics.actual_right),
        ("actualBoundingBoxAscent", metrics.actual_ascent),
        ("actualBoundingBoxDescent", metrics.actual_descent),
        ("fontBoundingBoxAscent", metrics.font_ascent),
        ("fontBoundingBoxDescent", metrics.font_descent),
        ("emHeightAscent", metrics.font_ascent),
        ("emHeightDescent", metrics.font_descent),
        ("alphabeticBaseline", 0.),
        ("hangingBaseline", metrics.font_ascent * HANGING_BASELINE),
        ("ideographicBaseline", -metrics.font_descent),
    ];
    for (name, value) in members {
        set(&object, name, value, context)?;
    }
    Ok(object.into())
}

// ─── Images ─────────────────────────────────────────────────────────────────────────────────

/// `drawImage(image, dx, dy)`, `(image, dx, dy, dw, dh)` and `(image, sx, sy, sw, sh, dx, dy, dw, dh)`.
fn draw_image(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(source) = source_pixmap(&arg(args, 0), context)? else {
        return Ok(JsValue::undefined());
    };
    let mut numbers = Vec::new();
    for value in args.iter().skip(1).take(8) {
        numbers.push(value.to_number(context)? as f32);
    }
    let size = (source.pixmap.width(), source.pixmap.height());
    let place = Place::from_numbers(&numbers, size.0, size.1);
    let Some(place) = place.filter(|_| numbers.iter().all(|n| n.is_finite())) else {
        return Ok(JsValue::undefined());
    };
    with_scene(this, context, |scene| {
        scene.draw_image(&source.pixmap, source.tainted, place);
    })
}

fn get_image_data(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(rect @ [_, _, width, height]) = finite_numbers(args, context)? else {
        return Err(non_finite("getImageData"));
    };
    if width == 0. || height == 0. {
        let message = "Failed to execute 'getImageData' on 'CanvasRenderingContext2D': The source width is zero or not a number.";
        return Err(dom_error(context, message, "IndexSizeError"));
    }
    let region = Region::from_rect(rect);
    let pixels = read(this, context, |canvas| {
        (canvas.tainted, canvas.read_region(region))
    })?;
    match pixels {
        Some((true, _)) => {
            let message = "Failed to execute 'getImageData' on 'CanvasRenderingContext2D': The canvas has been tainted by cross-origin data.";
            Err(dom_error(context, message, "SecurityError"))
        }
        Some((false, pixels)) => make_image_data(context, region.width, region.height, pixels),
        None => Ok(JsValue::undefined()),
    }
}

fn put_image_data(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some((width, height, pixels)) = image_data_pixels(&arg(args, 0), context)? else {
        return Err(type_error(
            "Failed to execute 'putImageData' on 'CanvasRenderingContext2D': parameter 1 is not of type 'ImageData'.",
        ));
    };
    let Some([x, y]) = finite_numbers(args.get(1..).unwrap_or_default(), context)? else {
        return Err(non_finite("putImageData"));
    };
    let region = Region {
        x: x as i32,
        y: y as i32,
        width,
        height,
    };
    update(this, context, |canvas| canvas.write_region(region, &pixels))
}

/// `createImageData(width, height)` and `createImageData(imageData)`: a transparent one.
fn create_image_data(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let first = arg(args, 0);
    let (width, height) = match first.is_object() {
        true => (
            get(&first, "width", context)?,
            get(&first, "height", context)?,
        ),
        false => (first, arg(args, 1)),
    };
    let (width, height) = (width.to_number(context)?, height.to_number(context)?);
    if width == 0. || height == 0. || !width.is_finite() || !height.is_finite() {
        let message = "Failed to execute 'createImageData' on 'CanvasRenderingContext2D': The source width is zero or not a number.";
        return Err(dom_error(context, message, "IndexSizeError"));
    }
    let (width, height) = (width.abs() as u32, height.abs() as u32);
    let pixels = blank_bytes(width, height);
    make_image_data(context, width, height, pixels)
}

fn get_context_attributes(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let attributes = read(this, context, |canvas| {
        (canvas.alpha, canvas.will_read_frequently)
    })?;
    let (alpha, frequently) = attributes.unwrap_or((true, false));
    let members = [
        ("alpha", JsValue::from(alpha)),
        ("desynchronized", false.into()),
        ("colorSpace", text("srgb")),
        ("willReadFrequently", frequently.into()),
    ];
    Ok(bind::dictionary(context, &members)?.into())
}
