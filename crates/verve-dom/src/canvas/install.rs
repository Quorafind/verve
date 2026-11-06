//! The canvas classes — the 2D context, gradients, patterns, `Path2D`, `ImageData`,
//! `ImageBitmap`, `OffscreenCanvas` — as one table, and the small ones' methods.

use boa_engine::property::Attribute;
use boa_engine::{Context, JsResult, JsValue, js_string, native_function::NativeFunctionPointer};

use crate::bind::{
    self, Accessor, Kind, Method, WebIdlClass, dom_error, extend_prototype, handle_instance,
    webidl_class,
};
use crate::canvas::bitmap::{
    BITMAP_ACCESSORS, BITMAP_METHODS, BITMAP_RENDERER_METHODS, OFFSCREEN_ACCESSORS,
    OFFSCREEN_METHODS, construct_offscreen, create_image_bitmap,
};
use crate::canvas::commands::CONTEXT_METHODS;
use crate::canvas::matrix::transform_argument;
use crate::canvas::paint::parse_color;
use crate::canvas::path::{PATH_METHODS, PATH2D_METHODS, from_svg};
use crate::canvas::pixels::construct_image_data;
use crate::canvas::properties::install_properties;
use crate::canvas::store::{owner_of_context, this_handle};
use crate::state::shared;
use verve::native::{arg, arg_text, function, illegal_constructor};

/// A class: its constructor, its own methods and accessors, and methods it shares with another.
struct Entry {
    name: &'static str,
    length: usize,
    construct: NativeFunctionPointer,
    methods: &'static [Method],
    accessors: &'static [Accessor],
    shared: &'static [Method],
}

const fn entry(name: &'static str, methods: &'static [Method]) -> Entry {
    Entry {
        name,
        length: 0,
        construct: illegal_constructor,
        methods,
        accessors: &[],
        shared: &[],
    }
}

/// `context.canvas`, read-only.
const OWNER: &[Accessor] = &[(
    "canvas",
    Some(|this, _, cx| owner_of_context(this, cx)),
    None,
)];

const CLASSES: &[Entry] = &[
    Entry {
        shared: PATH_METHODS,
        accessors: OWNER,
        ..entry("CanvasRenderingContext2D", CONTEXT_METHODS)
    },
    entry("CanvasGradient", &[("addColorStop", 2, add_color_stop)]),
    entry(
        "CanvasPattern",
        &[("setTransform", 1, set_pattern_transform)],
    ),
    entry("TextMetrics", &[]),
    Entry {
        length: 1,
        construct: construct_image_data,
        ..entry("ImageData", &[])
    },
    Entry {
        construct: construct_path,
        shared: PATH_METHODS,
        ..entry("Path2D", PATH2D_METHODS)
    },
    Entry {
        accessors: BITMAP_ACCESSORS,
        ..entry("ImageBitmap", BITMAP_METHODS)
    },
    Entry {
        length: 2,
        construct: construct_offscreen,
        accessors: OFFSCREEN_ACCESSORS,
        ..entry("OffscreenCanvas", OFFSCREEN_METHODS)
    },
    Entry {
        accessors: OWNER,
        ..entry("ImageBitmapRenderingContext", BITMAP_RENDERER_METHODS)
    },
];

/// The canvas classes, their methods and their properties.
pub fn install(context: &mut Context) -> JsResult<()> {
    for entry in CLASSES {
        let spec = WebIdlClass {
            name: entry.name,
            length: entry.length,
            construct: entry.construct,
            parent: None,
            methods: entry.methods,
            accessors: entry.accessors,
            statics: &[],
            global: true,
        };
        let (_, prototype) = webidl_class(context, spec)?;
        extend_prototype(context, &prototype, entry.shared, &[])?;
        if entry.name == "CanvasRenderingContext2D" {
            install_properties(context, &prototype)?;
        }
    }
    let create = function(context, "createImageBitmap", 1, create_image_bitmap);
    let writable = Attribute::WRITABLE | Attribute::CONFIGURABLE;
    context.register_global_property(js_string!("createImageBitmap"), create, writable)
}

/// `new Path2D()`, `new Path2D(path)` and `new Path2D(svgPathData)`.
fn construct_path(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let first = arg(args, 0);
    let builder = if first.is_string() {
        from_svg(&arg_text(args, 0, context)?).unwrap_or_default()
    } else {
        let source = bind::handle_of(&first, Kind::Path);
        let state = shared(context);
        let state = state.borrow();
        source
            .and_then(|id| state.canvas.paths.get(&id).cloned())
            .unwrap_or_default()
    };
    let id = shared(context).borrow_mut().canvas.add_path(builder);
    Ok(handle_instance(new_target, Kind::Path, id, context)?.into())
}

fn add_color_stop(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::Gradient, "CanvasGradient")?;
    let offset = arg(args, 0).to_number(context)? as f32;
    if !(0. ..=1.).contains(&offset) {
        let message = "Failed to execute 'addColorStop' on 'CanvasGradient': The provided value is outside the range (0.0, 1.0).";
        return Err(dom_error(context, message, "IndexSizeError"));
    }
    let written = arg_text(args, 1, context)?;
    let Some((color, _)) = parse_color(&written) else {
        let message = "Failed to execute 'addColorStop' on 'CanvasGradient': The value provided is not a valid color.";
        return Err(dom_error(context, message, "SyntaxError"));
    };
    if let Some(gradient) = shared(context).borrow_mut().canvas.gradients.get_mut(&id) {
        gradient.add_stop(offset, color);
    }
    Ok(JsValue::undefined())
}

fn set_pattern_transform(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::Pattern, "CanvasPattern")?;
    let transform = transform_argument(&arg(args, 0), context)?;
    if let Some(pattern) = shared(context).borrow_mut().canvas.patterns.get_mut(&id) {
        pattern.transform = transform;
    }
    Ok(JsValue::undefined())
}
