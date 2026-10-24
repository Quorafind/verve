//! A small `Intl`, natively: what plugins use it for — sorting text people read
//! (`Intl.Collator`, numeric and case-insensitive), formatting numbers and dates for display,
//! choosing a plural, and saying how long ago. Without ICU's data: comparison is by Unicode
//! code point after folding case, and formats are English-shaped whatever the locale asked.

use std::cmp::Ordering;

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string,
    object::{FunctionObjectBuilder, ObjectInitializer},
    property::Attribute,
};

fn arg(args: &[JsValue], i: usize) -> JsValue {
    args.get(i).cloned().unwrap_or_default()
}

fn option(options: &JsValue, key: &str, context: &mut Context) -> JsResult<JsValue> {
    match options.as_object() {
        Some(o) => o.get(JsString::from(key), context),
        None => Ok(JsValue::undefined()),
    }
}

fn option_str(options: &JsValue, key: &str, context: &mut Context) -> JsResult<Option<String>> {
    let value = option(options, key, context)?;
    if value.is_undefined() {
        return Ok(None);
    }
    Ok(Some(value.to_string(context)?.to_std_string_escaped()))
}

/// Text split into runs of digits and runs of everything else, for numeric comparison.
fn runs(text: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut digits = None;
    for (i, c) in text.char_indices() {
        let d = c.is_ascii_digit();
        match digits {
            Some(previous) if previous != d => {
                out.push((previous, &text[start..i]));
                start = i;
            }
            _ => {}
        }
        digits = Some(d);
    }
    if let Some(d) = digits {
        out.push((d, &text[start..]));
    }
    out
}

/// Folds what a sensitivity says does not count.
fn fold(text: &str, sensitivity: &str) -> String {
    match sensitivity {
        "base" | "accent" => text.to_lowercase(),
        _ => text.to_owned(),
    }
}

pub fn collate(a: &str, b: &str, numeric: bool, sensitivity: &str) -> Ordering {
    let primary = |a: &str, b: &str| -> Ordering {
        let (fa, fb) = (a.to_lowercase(), b.to_lowercase());
        if !numeric {
            return fa.cmp(&fb);
        }
        let (ra, rb) = (runs(&fa), runs(&fb));
        for (x, y) in ra.iter().zip(rb.iter()) {
            let order = match (x.0, y.0) {
                (true, true) => {
                    let (nx, ny) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
                    nx.len().cmp(&ny.len()).then_with(|| nx.cmp(ny))
                }
                _ => x.1.cmp(y.1),
            };
            if order != Ordering::Equal {
                return order;
            }
        }
        ra.len().cmp(&rb.len())
    };
    let order = primary(a, b);
    if order != Ordering::Equal || matches!(sensitivity, "base" | "accent") {
        return order;
    }
    // Same letters: lower case before upper, as ICU's default does.
    let (fa, fb) = (fold(a, sensitivity), fold(b, sensitivity));
    fb.cmp(&fa)
}

fn method(context: &mut Context, name: &str, length: usize, f: NativeFunction) -> JsObject {
    FunctionObjectBuilder::new(context.realm(), f)
        .name(JsString::from(name))
        .length(length)
        .build()
        .into()
}

fn collator(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let options = arg(args, 1);
    let numeric = option(&options, "numeric", context)?.to_boolean();
    let sensitivity =
        option_str(&options, "sensitivity", context)?.unwrap_or_else(|| "variant".into());
    let settings = ObjectInitializer::new(context)
        .property(js_string!("numeric"), numeric, Attribute::all())
        .property(
            js_string!("sensitivity"),
            JsString::from(sensitivity.as_str()),
            Attribute::all(),
        )
        .property(js_string!("locale"), js_string!("en-US"), Attribute::all())
        .property(js_string!("usage"), js_string!("sort"), Attribute::all())
        .build();
    let compare = method(
        context,
        "compare",
        2,
        NativeFunction::from_copy_closure_with_captures(
            |_, args, settings: &JsObject, context| {
                let a = arg(args, 0).to_string(context)?.to_std_string_escaped();
                let b = arg(args, 1).to_string(context)?.to_std_string_escaped();
                let numeric = settings.get(js_string!("numeric"), context)?.to_boolean();
                let sensitivity = settings
                    .get(js_string!("sensitivity"), context)?
                    .to_string(context)?
                    .to_std_string_escaped();
                Ok(match collate(&a, &b, numeric, &sensitivity) {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                }
                .into())
            },
            settings.clone(),
        ),
    );
    let resolved = method(
        context,
        "resolvedOptions",
        0,
        NativeFunction::from_copy_closure_with_captures(
            |_, _, settings: &JsObject, _| Ok(settings.clone().into()),
            settings,
        ),
    );
    Ok(ObjectInitializer::new(context)
        .property(js_string!("compare"), compare, Attribute::all())
        .property(js_string!("resolvedOptions"), resolved, Attribute::all())
        .build()
        .into())
}

/// `1234.5` as `1,234.5`, with at most `max` and at least `min` digits after the point.
pub fn group(value: f64, min: usize, max: usize) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            "NaN".into()
        } else if value > 0. {
            "∞".into()
        } else {
            "-∞".into()
        };
    }
    // Half away from zero, as Intl rounds; Rust's formatting rounds half to even.
    let scale = 10f64.powi(max as i32);
    let fixed = format!("{:.*}", max, (value.abs() * scale).round() / scale);
    let (whole, mut fraction) = match fixed.split_once('.') {
        Some((w, f)) => (w.to_owned(), f.to_owned()),
        None => (fixed.clone(), String::new()),
    };
    while fraction.len() > min && fraction.ends_with('0') {
        fraction.pop();
    }
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let sign = if value < 0. && (whole != "0" || fraction.chars().any(|c| c != '0')) {
        "-"
    } else {
        ""
    };
    if fraction.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{fraction}")
    }
}

fn number_format(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let options = arg(args, 1);
    let style = option_str(&options, "style", context)?.unwrap_or_else(|| "decimal".into());
    let min = option(&options, "minimumFractionDigits", context)?
        .as_number()
        .unwrap_or(0.) as usize;
    let max = option(&options, "maximumFractionDigits", context)?
        .as_number()
        .map_or(if style == "percent" { 0 } else { 3 }, |n| n as usize)
        .max(min);
    let settings = ObjectInitializer::new(context)
        .property(
            js_string!("style"),
            JsString::from(style.as_str()),
            Attribute::all(),
        )
        .property(
            js_string!("minimumFractionDigits"),
            min as f64,
            Attribute::all(),
        )
        .property(
            js_string!("maximumFractionDigits"),
            max as f64,
            Attribute::all(),
        )
        .property(js_string!("locale"), js_string!("en-US"), Attribute::all())
        .build();
    let format = method(
        context,
        "format",
        1,
        NativeFunction::from_copy_closure_with_captures(
            |_, args, settings: &JsObject, context| {
                let value = arg(args, 0).to_number(context)?;
                let style = settings
                    .get(js_string!("style"), context)?
                    .to_string(context)?
                    .to_std_string_escaped();
                let min = settings
                    .get(js_string!("minimumFractionDigits"), context)?
                    .to_number(context)? as usize;
                let max = settings
                    .get(js_string!("maximumFractionDigits"), context)?
                    .to_number(context)? as usize;
                let out = match style.as_str() {
                    "percent" => format!("{}%", group(value * 100., min, max)),
                    _ => group(value, min, max),
                };
                Ok(JsString::from(out.as_str()).into())
            },
            settings.clone(),
        ),
    );
    let resolved = method(
        context,
        "resolvedOptions",
        0,
        NativeFunction::from_copy_closure_with_captures(
            |_, _, settings: &JsObject, _| Ok(settings.clone().into()),
            settings,
        ),
    );
    Ok(ObjectInitializer::new(context)
        .property(js_string!("format"), format, Attribute::all())
        .property(js_string!("resolvedOptions"), resolved, Attribute::all())
        .build()
        .into())
}

fn date_time_format(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let format = method(
        context,
        "format",
        1,
        NativeFunction::from_fn_ptr(|_, args, context| {
            // The date's own `toLocaleDateString`, which Boa answers without ICU.
            let date = arg(args, 0);
            let date = if date.is_undefined() {
                let now = context.intrinsics().constructors().date().constructor();
                now.construct(&[], None, context)?.into()
            } else {
                date
            };
            let method = date
                .as_object()
                .map(|o| o.get(js_string!("toLocaleDateString"), context))
                .transpose()?;
            match method {
                Some(method) => crate::call(context, &method, &date, &[]),
                None => Ok(js_string!("").into()),
            }
        }),
    );
    let resolved = method(
        context,
        "resolvedOptions",
        0,
        NativeFunction::from_fn_ptr(|_, _, context| {
            Ok(ObjectInitializer::new(context)
                .property(js_string!("locale"), js_string!("en-US"), Attribute::all())
                .property(
                    js_string!("calendar"),
                    js_string!("gregory"),
                    Attribute::all(),
                )
                .property(
                    js_string!("numberingSystem"),
                    js_string!("latn"),
                    Attribute::all(),
                )
                .property(js_string!("timeZone"), js_string!("UTC"), Attribute::all())
                .build()
                .into())
        }),
    );
    Ok(ObjectInitializer::new(context)
        .property(js_string!("format"), format, Attribute::all())
        .property(js_string!("resolvedOptions"), resolved, Attribute::all())
        .build()
        .into())
}

fn plural_rules(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let select = method(
        context,
        "select",
        1,
        NativeFunction::from_fn_ptr(|_, args, context| {
            let n = arg(args, 0).to_number(context)?;
            Ok(if n == 1. {
                js_string!("one")
            } else {
                js_string!("other")
            }
            .into())
        }),
    );
    Ok(ObjectInitializer::new(context)
        .property(js_string!("select"), select, Attribute::all())
        .build()
        .into())
}

fn relative_time_format(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let format = method(
        context,
        "format",
        2,
        NativeFunction::from_fn_ptr(|_, args, context| {
            let n = arg(args, 0).to_number(context)?;
            let unit = arg(args, 1).to_string(context)?.to_std_string_escaped();
            let unit = unit.trim_end_matches('s');
            let count = n.abs();
            let unit = if count == 1. {
                unit.to_owned()
            } else {
                format!("{unit}s")
            };
            let out = if n < 0. {
                format!("{count} {unit} ago")
            } else {
                format!("in {count} {unit}")
            };
            Ok(JsString::from(out.as_str()).into())
        }),
    );
    Ok(ObjectInitializer::new(context)
        .property(js_string!("format"), format, Attribute::all())
        .build()
        .into())
}

pub fn install(context: &mut Context) -> JsResult<()> {
    let constructors: [(&str, boa_engine::native_function::NativeFunctionPointer); 5] = [
        ("Collator", collator),
        ("NumberFormat", number_format),
        ("DateTimeFormat", date_time_format),
        ("PluralRules", plural_rules),
        ("RelativeTimeFormat", relative_time_format),
    ];
    let mut objects = Vec::new();
    for (name, f) in constructors {
        let function: JsObject =
            FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
                .name(JsString::from(name))
                .length(2)
                .constructor(true)
                .build()
                .into();
        objects.push((name, function));
    }
    let mut intl = ObjectInitializer::new(context);
    for (name, function) in objects {
        intl.property(
            JsString::from(name),
            function,
            Attribute::WRITABLE | Attribute::CONFIGURABLE,
        );
    }
    let intl = intl.build();
    context.register_global_property(
        js_string!("Intl"),
        intl,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_sorted_as_people_read_it() {
        assert_eq!(
            collate("item 10", "item 9", true, "base"),
            Ordering::Greater
        );
        assert_eq!(collate("item 10", "item 9", false, "base"), Ordering::Less);
        assert_eq!(collate("apple", "Banana", false, "variant"), Ordering::Less);
        assert_eq!(collate("a", "A", false, "base"), Ordering::Equal);
        assert_eq!(collate("a", "A", false, "variant"), Ordering::Less);
        assert_eq!(group(1234567.891, 0, 2), "1,234,567.89");
        assert_eq!(group(-0.5, 0, 0), "-1");
    }
}
