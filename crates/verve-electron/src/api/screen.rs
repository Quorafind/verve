//! `screen`: the displays, from what the shell last reported (the Hub keeps it, so `screen` is
//! right from the app's first line), and the cursor, from the system. An `EventEmitter` object, as
//! Electron's is, that emits `display-added`, `display-removed` and `display-metrics-changed` when
//! a report differs from the one before.

use std::cell::RefCell;

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native;

use super::define::{self, Args, Fields};
use super::shared;
use crate::msg::{Display, Rect};

/// What `screen` knows between calls: the displays it last told the script of.
#[derive(Default)]
struct Known(RefCell<Vec<Display>>);

/// The `screen` module.
///
/// # Errors
///
/// A property that could not be defined.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let emitter = define::emitter(context)?;
    let screen = emitter.construct(&[], None, context)?;
    native::exports(
        context,
        &screen,
        &[
            ("getPrimaryDisplay", 0, primary),
            ("getAllDisplays", 0, all),
            ("getCursorScreenPoint", 0, cursor),
            ("getDisplayNearestPoint", 1, nearest),
            ("getDisplayMatching", 1, matching),
            ("screenToDipPoint", 1, |_, a, cx| {
                convert_point(a, cx, false)
            }),
            ("dipToScreenPoint", 1, |_, a, cx| convert_point(a, cx, true)),
            ("screenToDipRect", 2, |_, a, cx| convert_rect(a, cx, false)),
            ("dipToScreenRect", 2, |_, a, cx| convert_rect(a, cx, true)),
        ],
    )?;
    let known = Known(RefCell::new(shared(context).hub.displays()));
    context.insert_data(known);
    context.insert_data(Screen(screen.clone()));
    Ok(screen.into())
}

/// The `screen` object, kept to emit on.
struct Screen(JsObject);

/// The shell reported `displays`: whatever differs from the last report is emitted.
pub(crate) fn displays_changed(context: &mut Context, displays: &[Display]) {
    let Some(Screen(screen)) = context.get_data::<Screen>().map(|s| Screen(s.0.clone())) else {
        return;
    };
    let before = context
        .get_data::<Known>()
        .map(|known| known.0.replace(displays.to_vec()))
        .unwrap_or_default();
    if before.is_empty() {
        return;
    }
    if let Err(error) = emit_differences(context, &screen, &before, displays) {
        verve::report(context, &error);
    }
}

fn emit_differences(
    context: &mut Context,
    screen: &JsObject,
    before: &[Display],
    after: &[Display],
) -> JsResult<()> {
    for display in after
        .iter()
        .filter(|d| !before.iter().any(|b| b.id == d.id))
    {
        let (event, value) = (define::event(context)?, display_value(context, display)?);
        define::emit(context, screen, "display-added", &[event.into(), value])?;
    }
    for display in before
        .iter()
        .filter(|d| !after.iter().any(|a| a.id == d.id))
    {
        let (event, value) = (define::event(context)?, display_value(context, display)?);
        define::emit(context, screen, "display-removed", &[event.into(), value])?;
    }
    for display in after {
        let Some(old) = before.iter().find(|b| b.id == display.id) else {
            continue;
        };
        let changed = changed_metrics(old, display);
        if changed.is_empty() {
            continue;
        }
        let names = changed.into_iter().map(native::js_text).collect();
        let (event, value) = (define::event(context)?, display_value(context, display)?);
        let list = native::array(context, names);
        define::emit(
            context,
            screen,
            "display-metrics-changed",
            &[event.into(), value, list],
        )?;
    }
    Ok(())
}

/// What differs between a display's two reports, by Electron's names.
fn changed_metrics(old: &Display, new: &Display) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if old.bounds != new.bounds {
        changed.push("bounds");
    }
    if old.work_area != new.work_area {
        changed.push("workArea");
    }
    if old.scale_factor != new.scale_factor {
        changed.push("scaleFactor");
    }
    changed
}

fn rect_value(context: &mut Context, rect: Rect) -> JsResult<JsValue> {
    let fields = [
        ("x", rect.x.into()),
        ("y", rect.y.into()),
        ("width", rect.width.into()),
        ("height", rect.height.into()),
    ];
    Ok(define::record(context, &fields)?.into())
}

fn size_value(context: &mut Context, rect: Rect) -> JsResult<JsValue> {
    let fields = [("width", rect.width.into()), ("height", rect.height.into())];
    Ok(define::record(context, &fields)?.into())
}

fn point_value(context: &mut Context, x: f64, y: f64) -> JsResult<JsValue> {
    Ok(define::record(context, &[("x", x.into()), ("y", y.into())])?.into())
}

/// A `Display`, with the fields Electron 39 reports on Windows.
fn display_value(context: &mut Context, display: &Display) -> JsResult<JsValue> {
    let bounds = rect_value(context, display.bounds)?;
    let size = size_value(context, display.bounds)?;
    let work_area = rect_value(context, display.work_area)?;
    let work_size = size_value(context, display.work_area)?;
    let origin = point_value(context, display.bounds.x.into(), display.bounds.y.into())?;
    let cursor = define::record(context, &[("width", 0.into()), ("height", 0.into())])?;
    let color_space = native::js_text("{primaries:BT709, transfer:SRGB, matrix:RGB, range:FULL}");
    let fields: [(&str, JsValue); 19] = [
        ("accelerometerSupport", native::js_text("unknown")),
        ("bounds", bounds),
        ("colorDepth", 24.into()),
        ("colorSpace", color_space),
        ("depthPerComponent", 8.into()),
        ("detected", true.into()),
        ("displayFrequency", 60.into()),
        ("id", f64::from(display.id).into()),
        ("internal", false.into()),
        ("label", native::js_text("")),
        ("maximumCursorSize", cursor.into()),
        ("monochrome", false.into()),
        ("nativeOrigin", origin),
        ("rotation", 0.into()),
        ("scaleFactor", display.scale_factor.into()),
        ("size", size),
        ("workArea", work_area),
        ("workAreaSize", work_size),
        ("touchSupport", native::js_text("unknown")),
    ];
    Ok(define::record(context, &fields)?.into())
}

fn displays(context: &mut Context) -> Vec<Display> {
    shared(context).hub.displays()
}

fn primary_of(displays: &[Display]) -> Option<&Display> {
    displays.iter().find(|d| d.primary).or(displays.first())
}

fn primary(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let all = displays(context);
    match primary_of(&all) {
        Some(display) => display_value(context, display),
        None => Err(define::error("No displays")),
    }
}

fn all(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut values = Vec::new();
    for display in &displays(context) {
        values.push(display_value(context, display)?);
    }
    Ok(native::array(context, values))
}

fn cursor(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (x, y) = crate::os::cursor_position();
    let (x, y) = to_dip(&displays(context), f64::from(x), f64::from(y));
    point_value(context, x, y)
}

/// The scale factor of the display whose DIP area holds the point, else the primary's.
fn scale_at(displays: &[Display], x: f64, y: f64) -> f64 {
    let holds = |d: &&Display| contains(d.bounds, x, y);
    let found = displays.iter().find(holds).or(primary_of(displays));
    found.map_or(1.0, |d| f64::from(d.scale_factor))
}

fn contains(rect: Rect, x: f64, y: f64) -> bool {
    x >= f64::from(rect.x)
        && y >= f64::from(rect.y)
        && x < f64::from(rect.x + rect.width)
        && y < f64::from(rect.y + rect.height)
}

/// A point in screen pixels as DIPs: the scale of the primary display, as the cursor's display is
/// not known until it is placed.
fn to_dip(displays: &[Display], x: f64, y: f64) -> (f64, f64) {
    let scale = primary_of(displays).map_or(1.0, |d| f64::from(d.scale_factor));
    (x / scale, y / scale)
}

/// A `{ x, y }` argument, in gin's terms.
fn point_arg(args: &[JsValue], context: &mut Context) -> JsResult<(f64, f64)> {
    let object = define::Args::new(args).object_opt(0, "point", context)?;
    let fields = Fields::new(object.as_ref());
    Ok((
        fields.f64("x", 0.0, context)?,
        fields.f64("y", 0.0, context)?,
    ))
}

fn rect_arg(args: &[JsValue], at: usize, context: &mut Context) -> JsResult<Rect> {
    let object = define::Args::new(args).object_opt(at, "rect", context)?;
    let fields = Fields::new(object.as_ref());
    Ok(Rect {
        x: fields.f32("x", 0.0, context)?,
        y: fields.f32("y", 0.0, context)?,
        width: fields.f32("width", 0.0, context)?,
        height: fields.f32("height", 0.0, context)?,
    })
}

fn nearest(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Args::new(args).require(1)?;
    let (x, y) = point_arg(args, context)?;
    let all = displays(context);
    let distance = |d: &&Display| {
        let r = d.bounds;
        let dx = (f64::from(r.x) - x)
            .max(0.0)
            .max(x - f64::from(r.x + r.width));
        let dy = (f64::from(r.y) - y)
            .max(0.0)
            .max(y - f64::from(r.y + r.height));
        dx * dx + dy * dy
    };
    let nearest = all
        .iter()
        .min_by(|a, b| distance(a).total_cmp(&distance(b)));
    match nearest.or(primary_of(&all)) {
        Some(display) => display_value(context, display),
        None => Err(define::error("No displays")),
    }
}

fn matching(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Args::new(args).require(1)?;
    let rect = rect_arg(args, 0, context)?;
    let all = displays(context);
    let overlap = |d: &&Display| {
        let (a, b) = (d.bounds, rect);
        let w = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
        let h = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
        f64::from(w.max(0.0) * h.max(0.0))
    };
    let best = all.iter().max_by(|a, b| overlap(a).total_cmp(&overlap(b)));
    let found = best.filter(|d| overlap(d) > 0.0).or(primary_of(&all));
    match found {
        Some(display) => display_value(context, display),
        None => Err(define::error("No displays")),
    }
}

/// `dipToScreenPoint` and `screenToDipPoint`: by the scale factor of the display the point is on.
fn convert_point(args: &[JsValue], context: &mut Context, to_screen: bool) -> JsResult<JsValue> {
    Args::new(args).require(1)?;
    let (x, y) = point_arg(args, context)?;
    let scale = scale_at(&displays(context), x, y);
    let factor = if to_screen { scale } else { 1.0 / scale };
    point_value(context, (x * factor).round(), (y * factor).round())
}

/// `dipToScreenRect(window, rect)` and `screenToDipRect(window, rect)`.
fn convert_rect(args: &[JsValue], context: &mut Context, to_screen: bool) -> JsResult<JsValue> {
    Args::new(args).require(2)?;
    let rect = rect_arg(args, 1, context)?;
    let scale = scale_at(&displays(context), f64::from(rect.x), f64::from(rect.y));
    let factor = if to_screen { scale } else { 1.0 / scale } as f32;
    let scaled = Rect {
        x: (rect.x * factor).round(),
        y: (rect.y * factor).round(),
        width: (rect.width * factor).round(),
        height: (rect.height * factor).round(),
    };
    rect_value(context, scaled)
}
