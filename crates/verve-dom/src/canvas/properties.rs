//! The properties of `CanvasRenderingContext2D` that are drawing state — `fillStyle`,
//! `lineWidth`, `font` and the rest — as one table: what each reads back, and what writing it
//! does. A value a property does not accept leaves it as it was, as the specification says.

use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue};
use tiny_skia::{BlendMode, Color, FilterQuality, LineCap, LineJoin};

use crate::bind::{Kind, arg, closure, handle_of, string, text};
use crate::canvas::context2d::DrawState;
use crate::canvas::paint::{PaintSource, css_of, parse_color};
use crate::canvas::store::{canvas_object, read_canvas, this_handle};
use crate::canvas::text::{Align, Baseline, TextStyle};
use crate::css::ROOT_FONT_SIZE;
use crate::state::shared;

/// What a property reads back.
enum Reading {
    Text(String),
    Number(f32),
    Flag(bool),
    Paint(PaintSource),
}

/// What kind of value a property accepts, so one conversion serves them all.
#[derive(Clone, Copy)]
enum Input {
    Paint,
    Color,
    Number,
    Flag,
    Text,
}

/// A value a script wrote, converted as its property asks.
enum Written {
    Paint(PaintSource),
    Color(Color),
    Number(f32),
    Flag(bool),
    Text(String),
}

impl Written {
    fn paint(self) -> Option<PaintSource> {
        match self {
            Written::Paint(paint) => Some(paint),
            _ => None,
        }
    }

    fn color(self) -> Option<Color> {
        match self {
            Written::Color(color) => Some(color),
            _ => None,
        }
    }

    fn number(self) -> Option<f32> {
        match self {
            Written::Number(number) => Some(number),
            _ => None,
        }
    }

    fn flag(self) -> Option<bool> {
        match self {
            Written::Flag(flag) => Some(flag),
            _ => None,
        }
    }

    fn text(self) -> Option<String> {
        match self {
            Written::Text(text) => Some(text),
            _ => None,
        }
    }
}

struct Property {
    name: &'static str,
    input: Input,
    read: fn(&DrawState) -> Reading,
    write: fn(&mut DrawState, Written),
}

/// Puts `value` in `slot` when there is one.
fn assign<T>(slot: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *slot = value;
    }
}

/// The name a table gives `value`; the first entry's when it has none (the default).
pub fn name_of<T: Copy + PartialEq>(table: &[(&'static str, T)], value: T) -> &'static str {
    let found = table.iter().find(|(_, known)| *known == value);
    found.unwrap_or(&table[0]).0
}

/// The value a table gives `name`.
pub fn value_of<T: Copy>(table: &[(&'static str, T)], name: &str) -> Option<T> {
    table
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, value)| *value)
}

const ALIGNS: &[(&str, Align)] = &[
    ("start", Align::Start),
    ("end", Align::End),
    ("left", Align::Left),
    ("center", Align::Center),
    ("right", Align::Right),
];

const BASELINES: &[(&str, Baseline)] = &[
    ("top", Baseline::Top),
    ("hanging", Baseline::Hanging),
    ("middle", Baseline::Middle),
    ("alphabetic", Baseline::Alphabetic),
    ("ideographic", Baseline::Ideographic),
    ("bottom", Baseline::Bottom),
];

/// `direction`: whether text runs right to left (`inherit` is the document's, left to right).
const DIRECTIONS: &[(&str, bool)] = &[("ltr", false), ("inherit", false), ("rtl", true)];

const CAPS: &[(&str, LineCap)] = &[
    ("butt", LineCap::Butt),
    ("round", LineCap::Round),
    ("square", LineCap::Square),
];

const JOINS: &[(&str, LineJoin)] = &[
    ("miter", LineJoin::Miter),
    ("round", LineJoin::Round),
    ("bevel", LineJoin::Bevel),
];

/// `imageSmoothingQuality`: tiny-skia has two filters above nearest, so `medium` is `low`.
const SMOOTHING: &[(&str, FilterQuality)] = &[
    ("low", FilterQuality::Bilinear),
    ("medium", FilterQuality::Bilinear),
    ("high", FilterQuality::Bicubic),
];

/// `fontKerning`: `normal` is `auto`.
const KERNING: &[(&str, bool)] = &[("auto", true), ("normal", true), ("none", false)];

/// The composite operation names and what tiny-skia calls each: the 26 the specification has.
pub const COMPOSITES: &[(&str, BlendMode)] = &[
    ("source-over", BlendMode::SourceOver),
    ("source-in", BlendMode::SourceIn),
    ("source-out", BlendMode::SourceOut),
    ("source-atop", BlendMode::SourceAtop),
    ("destination-over", BlendMode::DestinationOver),
    ("destination-in", BlendMode::DestinationIn),
    ("destination-out", BlendMode::DestinationOut),
    ("destination-atop", BlendMode::DestinationAtop),
    ("lighter", BlendMode::Plus),
    ("copy", BlendMode::Source),
    ("xor", BlendMode::Xor),
    ("multiply", BlendMode::Multiply),
    ("screen", BlendMode::Screen),
    ("overlay", BlendMode::Overlay),
    ("darken", BlendMode::Darken),
    ("lighten", BlendMode::Lighten),
    ("color-dodge", BlendMode::ColorDodge),
    ("color-burn", BlendMode::ColorBurn),
    ("hard-light", BlendMode::HardLight),
    ("soft-light", BlendMode::SoftLight),
    ("difference", BlendMode::Difference),
    ("exclusion", BlendMode::Exclusion),
    ("hue", BlendMode::Hue),
    ("saturation", BlendMode::Saturation),
    ("color", BlendMode::Color),
    ("luminosity", BlendMode::Luminosity),
];

fn pixels(value: f32) -> Reading {
    Reading::Text(format!("{value}px"))
}

fn length(written: Written) -> Option<f32> {
    crate::css::length(&written.text()?, ROOT_FONT_SIZE)
}

/// Writes `font`, keeping the spacing and kerning properties it does not set.
fn write_font(draw: &mut DrawState, written: Written) {
    let Some(parsed) = written.text().and_then(|font| TextStyle::parse(&font)) else {
        return;
    };
    let TextStyle {
        letter_spacing,
        word_spacing,
        kerning,
        ..
    } = draw.text;
    draw.text = TextStyle {
        letter_spacing,
        word_spacing,
        kerning,
        ..parsed
    };
}

fn write_word_spacing(draw: &mut DrawState, written: Written) {
    let spacing = match written.text().as_deref() {
        Some("normal") => Some(0.),
        Some(other) => crate::css::length(other, ROOT_FONT_SIZE),
        None => None,
    };
    assign(&mut draw.text.word_spacing, spacing);
}

const PROPERTIES: &[Property] = &[
    Property {
        name: "fillStyle",
        input: Input::Paint,
        read: |d| Reading::Paint(d.fill.clone()),
        write: |d, w| assign(&mut d.fill, w.paint()),
    },
    Property {
        name: "strokeStyle",
        input: Input::Paint,
        read: |d| Reading::Paint(d.stroke.clone()),
        write: |d, w| assign(&mut d.stroke, w.paint()),
    },
    Property {
        name: "font",
        input: Input::Text,
        read: |d| Reading::Text(format!("{}px {}", d.text.size, d.text.families.join(", "))),
        write: write_font,
    },
    Property {
        name: "textAlign",
        input: Input::Text,
        read: |d| Reading::Text(name_of(ALIGNS, d.align).to_owned()),
        write: |d, w| assign(&mut d.align, w.text().and_then(|t| value_of(ALIGNS, &t))),
    },
    Property {
        name: "textBaseline",
        input: Input::Text,
        read: |d| Reading::Text(name_of(BASELINES, d.baseline).to_owned()),
        write: |d, w| {
            assign(
                &mut d.baseline,
                w.text().and_then(|t| value_of(BASELINES, &t)),
            )
        },
    },
    Property {
        name: "direction",
        input: Input::Text,
        read: |d| Reading::Text(name_of(DIRECTIONS, d.rtl).to_owned()),
        write: |d, w| assign(&mut d.rtl, w.text().and_then(|t| value_of(DIRECTIONS, &t))),
    },
    Property {
        name: "globalCompositeOperation",
        input: Input::Text,
        read: |d| Reading::Text(name_of(COMPOSITES, d.composite).to_owned()),
        write: |d, w| {
            assign(
                &mut d.composite,
                w.text().and_then(|t| value_of(COMPOSITES, &t)),
            )
        },
    },
    Property {
        name: "globalAlpha",
        input: Input::Number,
        read: |d| Reading::Number(d.alpha),
        write: |d, w| assign(&mut d.alpha, w.number().filter(|n| (0. ..=1.).contains(n))),
    },
    Property {
        name: "lineWidth",
        input: Input::Number,
        read: |d| Reading::Number(d.line.width),
        write: |d, w| assign(&mut d.line.width, w.number().filter(|n| *n > 0.)),
    },
    Property {
        name: "lineCap",
        input: Input::Text,
        read: |d| Reading::Text(name_of(CAPS, d.line.cap).to_owned()),
        write: |d, w| assign(&mut d.line.cap, w.text().and_then(|t| value_of(CAPS, &t))),
    },
    Property {
        name: "lineJoin",
        input: Input::Text,
        read: |d| Reading::Text(name_of(JOINS, d.line.join).to_owned()),
        write: |d, w| assign(&mut d.line.join, w.text().and_then(|t| value_of(JOINS, &t))),
    },
    Property {
        name: "miterLimit",
        input: Input::Number,
        read: |d| Reading::Number(d.line.miter),
        write: |d, w| assign(&mut d.line.miter, w.number().filter(|n| *n > 0.)),
    },
    Property {
        name: "lineDashOffset",
        input: Input::Number,
        read: |d| Reading::Number(d.line.offset),
        write: |d, w| assign(&mut d.line.offset, w.number()),
    },
    Property {
        name: "shadowColor",
        input: Input::Color,
        read: |d| Reading::Text(css_of(&d.shadow.color)),
        write: |d, w| assign(&mut d.shadow.color, w.color()),
    },
    Property {
        name: "shadowBlur",
        input: Input::Number,
        read: |d| Reading::Number(d.shadow.blur),
        write: |d, w| assign(&mut d.shadow.blur, w.number().filter(|n| *n >= 0.)),
    },
    Property {
        name: "shadowOffsetX",
        input: Input::Number,
        read: |d| Reading::Number(d.shadow.x),
        write: |d, w| assign(&mut d.shadow.x, w.number()),
    },
    Property {
        name: "shadowOffsetY",
        input: Input::Number,
        read: |d| Reading::Number(d.shadow.y),
        write: |d, w| assign(&mut d.shadow.y, w.number()),
    },
    Property {
        name: "imageSmoothingEnabled",
        input: Input::Flag,
        read: |d| Reading::Flag(d.smoothing),
        write: |d, w| assign(&mut d.smoothing, w.flag()),
    },
    Property {
        name: "imageSmoothingQuality",
        input: Input::Text,
        read: |d| Reading::Text(name_of(SMOOTHING, d.quality).to_owned()),
        write: |d, w| {
            assign(
                &mut d.quality,
                w.text().and_then(|t| value_of(SMOOTHING, &t)),
            )
        },
    },
    Property {
        name: "filter",
        input: Input::Text,
        read: |d| Reading::Text(d.filter.clone()),
        write: |d, w| assign(&mut d.filter, w.text()),
    },
    Property {
        name: "fontKerning",
        input: Input::Text,
        read: |d| Reading::Text(name_of(KERNING, d.text.kerning).to_owned()),
        write: |d, w| {
            assign(
                &mut d.text.kerning,
                w.text().and_then(|t| value_of(KERNING, &t)),
            )
        },
    },
    Property {
        name: "letterSpacing",
        input: Input::Text,
        read: |d| pixels(d.text.letter_spacing),
        write: |d, w| assign(&mut d.text.letter_spacing, length(w)),
    },
    Property {
        name: "wordSpacing",
        input: Input::Text,
        read: |d| pixels(d.text.word_spacing),
        write: write_word_spacing,
    },
    Property {
        name: "fontStretch",
        input: Input::Text,
        read: |d| Reading::Text(d.font_stretch.clone()),
        write: |d, w| assign(&mut d.font_stretch, w.text()),
    },
    Property {
        name: "fontVariantCaps",
        input: Input::Text,
        read: |d| Reading::Text(d.font_variant_caps.clone()),
        write: |d, w| assign(&mut d.font_variant_caps, w.text()),
    },
    Property {
        name: "textRendering",
        input: Input::Text,
        read: |d| Reading::Text(d.text_rendering.clone()),
        write: |d, w| assign(&mut d.text_rendering, w.text()),
    },
];

/// Defines every property in the table on the context's prototype.
pub fn install_properties(context: &mut Context, prototype: &JsObject) -> JsResult<()> {
    for (index, property) in PROPERTIES.iter().enumerate() {
        let getter = closure(
            context,
            &format!("get {}", property.name),
            |this, _, index: &usize, cx| read_property(this, cx, *index),
            index,
        );
        let setter = closure(
            context,
            &format!("set {}", property.name),
            |this, args, index: &usize, cx| write_property(this, args, cx, *index),
            index,
        );
        let descriptor = PropertyDescriptor::builder()
            .get(getter)
            .set(setter)
            .enumerable(true)
            .configurable(true);
        prototype.define_property_or_throw(JsString::from(property.name), descriptor, context)?;
    }
    Ok(())
}

fn read_property(this: &JsValue, context: &mut Context, index: usize) -> JsResult<JsValue> {
    let class = (Kind::Context2d, "CanvasRenderingContext2D");
    let reading = read_canvas(this, context, class, |canvas| {
        (PROPERTIES[index].read)(&canvas.draw)
    })?;
    Ok(match reading {
        None => JsValue::undefined(),
        Some(Reading::Text(value)) => text(&value),
        Some(Reading::Number(value)) => value.into(),
        Some(Reading::Flag(value)) => value.into(),
        Some(Reading::Paint(source)) => paint_object(&source, context),
    })
}

fn write_property(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    index: usize,
) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::Context2d, "CanvasRenderingContext2D")?;
    let property = &PROPERTIES[index];
    let Some(written) = written(property.input, &arg(args, 0), context)? else {
        return Ok(JsValue::undefined());
    };
    let state = shared(context);
    if let Some(canvas) = state.borrow_mut().canvas.canvases.get_mut(&id) {
        (property.write)(&mut canvas.draw, written);
    }
    Ok(JsValue::undefined())
}

/// `value` as `input` asks, or `None` when the property would ignore it.
fn written(input: Input, value: &JsValue, context: &mut Context) -> JsResult<Option<Written>> {
    Ok(match input {
        Input::Paint => paint_from(value, context)?.map(Written::Paint),
        Input::Color => match paint_from(value, context)? {
            Some(PaintSource::Color(color, _)) => Some(Written::Color(color)),
            _ => None,
        },
        Input::Number => {
            let number = value.to_number(context)?;
            number.is_finite().then_some(Written::Number(number as f32))
        }
        Input::Flag => Some(Written::Flag(value.to_boolean())),
        Input::Text => Some(Written::Text(string(value, context)?)),
    })
}

/// What `fillStyle = value` means: a color, a gradient or a pattern; `None` leaves it alone.
fn paint_from(value: &JsValue, context: &mut Context) -> JsResult<Option<PaintSource>> {
    if let Some(id) = handle_of(value, Kind::Gradient) {
        return Ok(Some(PaintSource::Gradient(id)));
    }
    if let Some(id) = handle_of(value, Kind::Pattern) {
        return Ok(Some(PaintSource::Pattern(id)));
    }
    if !value.is_string() {
        return Ok(None);
    }
    let written = string(value, context)?;
    Ok(parse_color(&written).map(|(color, css)| PaintSource::Color(color, css)))
}

fn paint_object(source: &PaintSource, context: &mut Context) -> JsValue {
    match source {
        PaintSource::Color(_, css) => text(css),
        PaintSource::Gradient(id) => {
            canvas_object(context, *id, (Kind::Gradient, "CanvasGradient")).into()
        }
        PaintSource::Pattern(id) => {
            canvas_object(context, *id, (Kind::Pattern, "CanvasPattern")).into()
        }
    }
}
