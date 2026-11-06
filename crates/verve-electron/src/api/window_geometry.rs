//! The size and place of a `BrowserWindow`: what its getters answer from the cached state and what
//! its setters send the shell.
//!
//! Electron's native methods read their numbers as gin does (`TypeError: Error processing
//! argument at index 1, conversion failure from b`, the last bad argument named, and
//! `Insufficient number of arguments.` for a missing one). A rectangle is a lenient dictionary: a
//! side left out keeps its value and anything that is not an object changes nothing.

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsResult, JsValue};
use verve::native::{self, arg};

use super::define::{self, Args, Fields};
use super::window_methods::{act, record_of};
use crate::msg::{Rect, WindowOp};
use crate::window_state::Sides;

pub(crate) const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("getBounds", 0, |this, _, cx| {
        rect(record_of(this, cx)?.state.borrow().bounds, cx)
    }),
    ("getContentBounds", 0, |this, _, cx| {
        rect(record_of(this, cx)?.state.borrow().content_bounds(), cx)
    }),
    ("getNormalBounds", 0, |this, _, cx| {
        rect(record_of(this, cx)?.state.borrow().normal, cx)
    }),
    ("getSize", 0, |this, _, cx| {
        let bounds = record_of(this, cx)?.state.borrow().bounds;
        pair(bounds.width, bounds.height, cx)
    }),
    ("getContentSize", 0, |this, _, cx| {
        let content = record_of(this, cx)?.state.borrow().content_bounds();
        pair(content.width, content.height, cx)
    }),
    ("getPosition", 0, |this, _, cx| {
        let bounds = record_of(this, cx)?.state.borrow().bounds;
        pair(bounds.x, bounds.y, cx)
    }),
    ("getMinimumSize", 0, |this, _, cx| {
        let (width, height) = record_of(this, cx)?.state.borrow().min_size;
        pair(width, height, cx)
    }),
    ("getMaximumSize", 0, |this, _, cx| {
        let (width, height) = record_of(this, cx)?.state.borrow().max_size;
        pair(width, height, cx)
    }),
    ("setBounds", 1, set_bounds),
    ("setContentBounds", 1, set_content_bounds),
    ("setSize", 2, |this, args, cx| {
        let [width, height] = Args::new(args).numbers(cx)?;
        let sides = Sides {
            width: Some(width as f32),
            height: Some(height as f32),
            ..Sides::default()
        };
        move_to(this, args, cx, sides)
    }),
    ("setContentSize", 2, |this, args, cx| {
        let [width, height] = Args::new(args).numbers(cx)?;
        let record = record_of(this, cx)?;
        let state = record.state.borrow();
        let wanted = Rect {
            width: width as f32,
            height: height as f32,
            ..state.content_bounds()
        };
        let outside = state.bounds_for_content(wanted);
        drop(state);
        let sides = Sides {
            width: Some(outside.width),
            height: Some(outside.height),
            ..Sides::default()
        };
        move_to(this, args, cx, sides)
    }),
    ("setPosition", 2, |this, args, cx| {
        let [x, y] = Args::new(args).numbers(cx)?;
        let sides = Sides {
            x: Some(x as f32),
            y: Some(y as f32),
            ..Sides::default()
        };
        move_to(this, args, cx, sides)
    }),
    ("setMinimumSize", 2, |this, args, cx| {
        let [width, height] = Args::new(args).numbers(cx)?;
        let record = record_of(this, cx)?;
        act(
            cx,
            &record,
            WindowOp::SetMinSize(width as f32, height as f32),
        );
        Ok(JsValue::undefined())
    }),
    ("setMaximumSize", 2, |this, args, cx| {
        let [width, height] = Args::new(args).numbers(cx)?;
        let record = record_of(this, cx)?;
        act(
            cx,
            &record,
            WindowOp::SetMaxSize(width as f32, height as f32),
        );
        Ok(JsValue::undefined())
    }),
];

/// `[a, b]`, whole numbers.
fn pair(a: f32, b: f32, context: &mut Context) -> JsResult<JsValue> {
    let numbers = vec![JsValue::new(a.round()), JsValue::new(b.round())];
    Ok(native::array(context, numbers))
}

/// `{ x, y, width, height }`, whole numbers.
fn rect(rect: Rect, context: &mut Context) -> JsResult<JsValue> {
    let fields = [
        ("x", rect.x),
        ("y", rect.y),
        ("width", rect.width),
        ("height", rect.height),
    ];
    let fields: Vec<_> = fields
        .into_iter()
        .map(|(name, value)| (name, JsValue::new(value.round())))
        .collect();
    Ok(define::record(context, &fields)?.into())
}

/// The sides of the rectangle `args[0]` is, each left out when it is no number.
fn sides(value: &JsValue, context: &mut Context) -> JsResult<Option<Sides>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let fields = Fields::new(Some(&object));
    let mut side = |name: &str| fields.f64_opt(name, context).map(|n| n.map(|n| n as f32));
    Ok(Some(Sides {
        x: side("x")?,
        y: side("y")?,
        width: side("width")?,
        height: side("height")?,
    }))
}

/// `setBounds(bounds[, animate])`.
fn set_bounds(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    if let Some(sides) = sides(&arg(args, 0), context)? {
        let animate = arg(args, 1).to_boolean();
        send_bounds(context, &record, sides, animate);
    }
    Ok(JsValue::undefined())
}

/// `setContentBounds(bounds[, animate])`: the window whose content is `bounds`.
fn set_content_bounds(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    if let Some(wanted) = sides(&arg(args, 0), context)? {
        let state = record.state.borrow();
        let content = state.content_bounds();
        let content = Rect {
            x: wanted.x.unwrap_or(content.x),
            y: wanted.y.unwrap_or(content.y),
            width: wanted.width.unwrap_or(content.width),
            height: wanted.height.unwrap_or(content.height),
        };
        let outside = state.bounds_for_content(content);
        drop(state);
        let all = Sides {
            x: Some(outside.x),
            y: Some(outside.y),
            width: Some(outside.width),
            height: Some(outside.height),
        };
        send_bounds(context, &record, all, arg(args, 1).to_boolean());
    }
    Ok(JsValue::undefined())
}

fn move_to(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    sides: Sides,
) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    send_bounds(context, &record, sides, arg(args, 2).to_boolean());
    Ok(JsValue::undefined())
}

fn send_bounds(
    context: &mut Context,
    record: &super::windows::WindowRecord,
    sides: Sides,
    animate: bool,
) {
    let wanted = WindowOp::SetBounds {
        x: sides.x.map(f32::round),
        y: sides.y.map(f32::round),
        width: sides.width.map(f32::round),
        height: sides.height.map(f32::round),
        animate,
    };
    act(context, record, wanted);
}
