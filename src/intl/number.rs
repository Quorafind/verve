//! `Intl.NumberFormat`, and `Number.prototype.toLocaleString`: numbers as English writes them.
//!
//! A number is read as the shortest decimal that is still that number — `1.005` is `1.005`,
//! as ICU reads it, not the binary fraction just under it — and rounded half away from zero, in
//! decimal digits, so nothing is lost to floating point on the way. Styles (decimal, percent,
//! currency, unit), notations (standard, scientific, engineering, compact), digit options,
//! grouping and sign display are all here; the words and symbols are English's.

use boa_engine::{Context, Finalize, JsData, JsObject, JsResult, JsValue, Trace, js_string};

use super::{
    Accessors, Methods, bound, data_of, joined, option, option_number, option_str, options_of,
    parts_array, record, resolve_locale,
};
use crate::native::{arg, js_text, range_error, text_of, type_error};

// ─── Decimals ───────────────────────────────────────────────────────────────────────────────

/// A non-negative decimal: its digits `0.d₀d₁d₂…` times ten to the `point` — `point` is how
/// many of the digits are before the decimal point. No trailing zeros; zero has no digits.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Decimal {
    digits: Vec<u8>,
    point: i32,
}

impl Decimal {
    /// The shortest decimal that reads back as `value` (finite, the sign aside).
    pub(crate) fn of(value: f64) -> Decimal {
        let value = value.abs();
        if value == 0. {
            return Decimal {
                digits: Vec::new(),
                point: 1,
            };
        }
        let text = format!("{value:e}");
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        let exponent: i32 = exponent.parse().unwrap_or(0);
        let digits: Vec<u8> = mantissa
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(|b| b - b'0')
            .collect();
        let mut out = Decimal {
            digits,
            point: exponent + 1,
        };
        out.trim();
        out
    }

    /// The decimal a string of digits is: a `BigInt`'s.
    fn of_digits(text: &str) -> Decimal {
        let digits: Vec<u8> = text
            .trim_start_matches('0')
            .bytes()
            .map(|b| b - b'0')
            .collect();
        let point = digits.len() as i32;
        let mut out = Decimal {
            digits,
            point: point.max(1),
        };
        out.trim();
        out
    }

    fn trim(&mut self) {
        while self.digits.last() == Some(&0) {
            self.digits.pop();
        }
        while self.digits.first() == Some(&0) {
            self.digits.remove(0);
            self.point -= 1;
        }
        if self.digits.is_empty() {
            self.point = 1;
        }
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// The power of ten of its first digit: `1234` is 3.
    fn magnitude(&self) -> i32 {
        self.point - 1
    }

    /// Rounded, half away from zero, to the first `keep` digits.
    fn round_at(&mut self, keep: i32) {
        if keep >= self.digits.len() as i32 {
            return;
        }
        if keep < 0 {
            *self = Decimal {
                digits: Vec::new(),
                point: 1,
            };
            return;
        }
        let keep = keep as usize;
        let up = self.digits[keep] >= 5;
        self.digits.truncate(keep);
        if up {
            let mut at = keep;
            loop {
                if at == 0 {
                    self.digits.insert(0, 1);
                    self.point += 1;
                    break;
                }
                at -= 1;
                if self.digits[at] == 9 {
                    self.digits[at] = 0;
                } else {
                    self.digits[at] += 1;
                    break;
                }
            }
        }
        self.trim();
    }

    fn round_fraction(&mut self, max_frac: usize) {
        let keep = self.point + max_frac as i32;
        self.round_at(keep);
    }

    fn round_significant(&mut self, max_sig: usize) {
        self.round_at(max_sig as i32);
    }

    /// How many significant digits it shows.
    fn significant(&self) -> usize {
        self.digits.len()
    }

    /// Its whole part and its fraction, as digits: at least `min_int` whole digits and
    /// `min_frac` fraction digits, zeros added.
    fn texts(&self, min_int: usize, min_frac: usize) -> (String, String) {
        let mut integer = String::new();
        for at in 0..self.point.max(0) {
            integer.push(char::from(
                b'0' + self.digits.get(at as usize).copied().unwrap_or(0),
            ));
        }
        if integer.is_empty() {
            integer.push('0');
        }
        while integer.len() < min_int {
            integer.insert(0, '0');
        }
        let mut fraction = String::new();
        for _ in self.point..0 {
            fraction.push('0');
        }
        for digit in self.digits.iter().skip(self.point.max(0) as usize) {
            fraction.push(char::from(b'0' + digit));
        }
        while fraction.len() < min_frac {
            fraction.push('0');
        }
        (integer, fraction)
    }

    fn shift(&mut self, by: i32) {
        if !self.is_zero() {
            self.point += by;
        }
    }
}

// ─── Digit options ──────────────────────────────────────────────────────────────────────────

/// How many digits a number is shown with.
#[derive(Clone, Debug)]
pub(crate) struct Digits {
    pub(crate) min_int: usize,
    pub(crate) min_frac: usize,
    pub(crate) max_frac: usize,
    pub(crate) min_sig: Option<usize>,
    pub(crate) max_sig: Option<usize>,
    /// Compact notation's own rounding, with no digit options given: whole numbers, or two
    /// significant digits, whichever shows more (`1.2K`, `12K`, `123K`).
    pub(crate) compact: bool,
}

impl Default for Digits {
    fn default() -> Self {
        Digits {
            min_int: 1,
            min_frac: 0,
            max_frac: 3,
            min_sig: None,
            max_sig: None,
            compact: false,
        }
    }
}

impl Digits {
    /// What `resolvedOptions` says of them.
    pub(crate) fn resolved(&self) -> Vec<(&'static str, JsValue)> {
        let mut out = vec![("minimumIntegerDigits", self.min_int.into())];
        match (self.min_sig, self.max_sig) {
            (Some(min), Some(max)) => {
                out.push(("minimumSignificantDigits", min.into()));
                out.push(("maximumSignificantDigits", max.into()));
            }
            _ => {
                out.push(("minimumFractionDigits", self.min_frac.into()));
                out.push(("maximumFractionDigits", self.max_frac.into()));
            }
        }
        out
    }
}

/// The digit options, with the style's defaults for the fraction.
pub(crate) fn digit_options(
    options: &JsValue,
    min_frac: usize,
    max_frac: usize,
    context: &mut Context,
) -> JsResult<Digits> {
    let min_int = option_number(options, "minimumIntegerDigits", 1, 21, context)?.unwrap_or(1);
    let mnfd = option_number(options, "minimumFractionDigits", 0, 100, context)?;
    let mxfd = option_number(options, "maximumFractionDigits", 0, 100, context)?;
    let mnsd = option_number(options, "minimumSignificantDigits", 1, 21, context)?;
    let mxsd = option_number(options, "maximumSignificantDigits", 1, 21, context)?;
    let mut digits = Digits {
        min_int,
        min_frac,
        max_frac: max_frac.max(min_frac),
        ..Digits::default()
    };
    if mnsd.is_some() || mxsd.is_some() {
        let min = mnsd.unwrap_or(1);
        let max = mxsd.unwrap_or(21);
        if min > max {
            return Err(range_error(
                "maximumSignificantDigits value is out of range.",
            ));
        }
        digits.min_sig = Some(min);
        digits.max_sig = Some(max);
    } else if mnfd.is_some() || mxfd.is_some() {
        let min = mnfd.unwrap_or_else(|| min_frac.min(mxfd.unwrap_or(min_frac)));
        let max = mxfd.unwrap_or_else(|| max_frac.max(min));
        if min > max {
            return Err(range_error("maximumFractionDigits value is out of range."));
        }
        digits.min_frac = min;
        digits.max_frac = max;
    }
    Ok(digits)
}

/// A number as shown: its whole part and its fraction.
pub(crate) struct Rounded {
    pub(crate) integer: String,
    pub(crate) fraction: String,
    decimal: Decimal,
}

impl Rounded {
    /// `1234.5`: the digits, ungrouped.
    pub(crate) fn joined(&self) -> String {
        if self.fraction.is_empty() {
            self.integer.clone()
        } else {
            format!("{}.{}", self.integer, self.fraction)
        }
    }
}

fn round_decimal(decimal: &Decimal, digits: &Digits) -> Rounded {
    let mut out = decimal.clone();
    let mut min_frac = digits.min_frac;
    if let (Some(min_sig), Some(max_sig)) = (digits.min_sig, digits.max_sig) {
        out.round_significant(max_sig);
        // Zeros after the last digit, up to the least significant digits asked for; leading
        // zeros after the point do not count.
        let wanted = min_sig.max(out.significant()) as i32;
        min_frac = if out.is_zero() {
            min_sig.saturating_sub(1)
        } else if out.point > 0 {
            (wanted - out.point).max(0) as usize
        } else {
            (wanted - out.point) as usize
        };
    } else if digits.compact {
        let mut whole = decimal.clone();
        whole.round_fraction(0);
        let mut two = decimal.clone();
        two.round_significant(2);
        out = if two.significant() > whole.significant() {
            two
        } else {
            whole
        };
        min_frac = 0;
    } else {
        out.round_fraction(digits.max_frac);
    }
    let (integer, fraction) = out.texts(digits.min_int, min_frac);
    Rounded {
        integer,
        fraction,
        decimal: out,
    }
}

/// `value`, rounded as `digits` say.
pub(crate) fn round(value: f64, digits: &Digits) -> Rounded {
    round_decimal(&Decimal::of(value), digits)
}

/// The whole part of `number` (digits, a point, digits) grouped by thousands.
pub(crate) fn group_integer(number: &str, grouping: &str) -> String {
    let (integer, rest) = match number.find('.') {
        Some(at) => (&number[..at], &number[at..]),
        None => (number, ""),
    };
    format!("{}{rest}", group_parts(integer, grouping).concat())
}

/// The whole part as `integer` and `group` parts.
fn group_parts(integer: &str, grouping: &str) -> Vec<String> {
    let enough = match grouping {
        "false" => usize::MAX,
        "min2" => 5,
        _ => 4,
    };
    if integer.len() < enough {
        return vec![integer.to_owned()];
    }
    let mut out = Vec::new();
    let first = integer.len() % 3;
    if first > 0 {
        out.push(integer[..first].to_owned());
    }
    for chunk in integer.as_bytes()[first..].chunks(3) {
        if !out.is_empty() {
            out.push(",".to_owned());
        }
        out.push(String::from_utf8_lossy(chunk).into_owned());
    }
    out
}

// ─── Currencies and units ───────────────────────────────────────────────────────────────────

/// Symbols English writes before an amount: the symbol, and the narrow one.
const SYMBOLS: &[(&str, &str, &str)] = &[
    ("USD", "$", "$"),
    ("EUR", "€", "€"),
    ("GBP", "£", "£"),
    ("JPY", "¥", "¥"),
    ("CNY", "CN¥", "¥"),
    ("INR", "₹", "₹"),
    ("KRW", "₩", "₩"),
    ("CAD", "CA$", "$"),
    ("AUD", "A$", "$"),
    ("NZD", "NZ$", "$"),
    ("HKD", "HK$", "$"),
    ("MXN", "MX$", "$"),
    ("TWD", "NT$", "$"),
    ("SGD", "SGD", "$"),
    ("BRL", "R$", "R$"),
    ("ILS", "₪", "₪"),
    ("VND", "₫", "₫"),
    ("PHP", "₱", "₱"),
    ("RUB", "RUB", "₽"),
    ("TRY", "TRY", "₺"),
    ("PLN", "PLN", "zł"),
    ("UAH", "UAH", "₴"),
    ("THB", "THB", "฿"),
    ("NGN", "NGN", "₦"),
    ("XAF", "FCFA", "FCFA"),
];

/// How many fraction digits a currency is written with.
fn currency_digits(code: &str) -> usize {
    match code {
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "PYG" | "XAF" | "XOF" | "KMF" | "RWF" => 0,
        "BHD" | "KWD" | "JOD" | "OMR" | "TND" | "IQD" | "LYD" => 3,
        _ => 2,
    }
}

/// A unit: its short form (singular, plural), narrow form, and long names (singular, plural).
struct Unit {
    id: &'static str,
    short: (&'static str, &'static str),
    narrow: &'static str,
    long: (&'static str, &'static str),
}

const fn unit(
    id: &'static str,
    short: (&'static str, &'static str),
    narrow: &'static str,
    long: (&'static str, &'static str),
) -> Unit {
    Unit {
        id,
        short,
        narrow,
        long,
    }
}

const UNITS: &[Unit] = &[
    unit("acre", ("ac", "ac"), "ac", ("acre", "acres")),
    unit("bit", ("bit", "bit"), "bit", ("bit", "bits")),
    unit("byte", ("byte", "byte"), "B", ("byte", "bytes")),
    unit(
        "celsius",
        ("°C", "°C"),
        "°C",
        ("degree Celsius", "degrees Celsius"),
    ),
    unit(
        "centimeter",
        ("cm", "cm"),
        "cm",
        ("centimeter", "centimeters"),
    ),
    unit("day", ("day", "days"), "d", ("day", "days")),
    unit("degree", ("deg", "deg"), "°", ("degree", "degrees")),
    unit(
        "fahrenheit",
        ("°F", "°F"),
        "°",
        ("degree Fahrenheit", "degrees Fahrenheit"),
    ),
    unit(
        "fluid-ounce",
        ("fl oz", "fl oz"),
        "fl oz",
        ("fluid ounce", "fluid ounces"),
    ),
    unit("foot", ("ft", "ft"), "′", ("foot", "feet")),
    unit("gallon", ("gal", "gal"), "gal", ("gallon", "gallons")),
    unit("gigabit", ("Gb", "Gb"), "Gb", ("gigabit", "gigabits")),
    unit("gigabyte", ("GB", "GB"), "GB", ("gigabyte", "gigabytes")),
    unit("gram", ("g", "g"), "g", ("gram", "grams")),
    unit("hectare", ("ha", "ha"), "ha", ("hectare", "hectares")),
    unit("hour", ("hr", "hr"), "h", ("hour", "hours")),
    unit("inch", ("in", "in"), "″", ("inch", "inches")),
    unit("kilobit", ("kb", "kb"), "kb", ("kilobit", "kilobits")),
    unit("kilobyte", ("kB", "kB"), "kB", ("kilobyte", "kilobytes")),
    unit("kilogram", ("kg", "kg"), "kg", ("kilogram", "kilograms")),
    unit("kilometer", ("km", "km"), "km", ("kilometer", "kilometers")),
    unit("liter", ("L", "L"), "L", ("liter", "liters")),
    unit("megabit", ("Mb", "Mb"), "Mb", ("megabit", "megabits")),
    unit("megabyte", ("MB", "MB"), "MB", ("megabyte", "megabytes")),
    unit("meter", ("m", "m"), "m", ("meter", "meters")),
    unit(
        "microsecond",
        ("μs", "μs"),
        "μs",
        ("microsecond", "microseconds"),
    ),
    unit("mile", ("mi", "mi"), "mi", ("mile", "miles")),
    unit(
        "mile-scandinavian",
        ("smi", "smi"),
        "smi",
        ("mile-scandinavian", "miles-scandinavian"),
    ),
    unit(
        "milliliter",
        ("mL", "mL"),
        "mL",
        ("milliliter", "milliliters"),
    ),
    unit(
        "millimeter",
        ("mm", "mm"),
        "mm",
        ("millimeter", "millimeters"),
    ),
    unit(
        "millisecond",
        ("ms", "ms"),
        "ms",
        ("millisecond", "milliseconds"),
    ),
    unit("minute", ("min", "min"), "m", ("minute", "minutes")),
    unit("month", ("mth", "mths"), "m", ("month", "months")),
    unit(
        "nanosecond",
        ("ns", "ns"),
        "ns",
        ("nanosecond", "nanoseconds"),
    ),
    unit("ounce", ("oz", "oz"), "oz", ("ounce", "ounces")),
    unit("percent", ("%", "%"), "%", ("percent", "percent")),
    unit("petabyte", ("PB", "PB"), "PB", ("petabyte", "petabytes")),
    unit("pound", ("lb", "lb"), "#", ("pound", "pounds")),
    unit("second", ("sec", "sec"), "s", ("second", "seconds")),
    unit("stone", ("st", "st"), "st", ("stone", "stones")),
    unit("terabit", ("Tb", "Tb"), "Tb", ("terabit", "terabits")),
    unit("terabyte", ("TB", "TB"), "TB", ("terabyte", "terabytes")),
    unit("week", ("wk", "wks"), "w", ("week", "weeks")),
    unit("yard", ("yd", "yd"), "yd", ("yard", "yards")),
    unit("year", ("yr", "yrs"), "y", ("year", "years")),
];

pub(crate) fn unit_names() -> Vec<&'static str> {
    UNITS.iter().map(|u| u.id).collect()
}

fn find_unit(id: &str) -> Option<&'static Unit> {
    UNITS.iter().find(|u| u.id == id)
}

/// Whether `id` names a unit: one of the list, or two of them joined by `-per-`.
fn valid_unit(id: &str) -> bool {
    match id.split_once("-per-") {
        Some((a, b)) => find_unit(a).is_some() && find_unit(b).is_some(),
        None => find_unit(id).is_some(),
    }
}

/// The unit as written after `count`: its words, and whether a space goes between.
fn unit_text(id: &str, display: &str, one: bool) -> (String, bool) {
    if let Some((a, b)) = id.split_once("-per-") {
        let (a, b) = (find_unit(a).expect("a unit"), find_unit(b).expect("a unit"));
        return match display {
            "long" => (
                format!("{} per {}", if one { a.long.0 } else { a.long.1 }, b.long.0),
                true,
            ),
            _ if id == "kilometer-per-hour" => ("km/h".to_owned(), display != "narrow"),
            _ if id == "mile-per-hour" => ("mph".to_owned(), display != "narrow"),
            "narrow" => (format!("{}/{}", a.narrow, b.narrow), false),
            _ => (format!("{}/{}", a.short.0, b.short.0), true),
        };
    }
    let unit = find_unit(id).expect("a unit");
    match display {
        "long" => (
            (if one { unit.long.0 } else { unit.long.1 }).to_owned(),
            true,
        ),
        "narrow" => (unit.narrow.to_owned(), false),
        _ => {
            let text = if one { unit.short.0 } else { unit.short.1 };
            // `5%`, `5°C`: no space before a sign.
            let spaced = !text.starts_with('%') && !text.starts_with('°');
            (text.to_owned(), spaced)
        }
    }
}

// ─── NumberFormat ───────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub(crate) struct NumberOptions {
    locale: String,
    style: String,
    currency: Option<String>,
    currency_display: String,
    currency_sign: String,
    unit: Option<String>,
    unit_display: String,
    notation: String,
    compact_display: String,
    sign_display: String,
    grouping: String,
    digits: Digits,
}

#[derive(Clone, Trace, Finalize, JsData)]
struct NumberFormatData {
    #[unsafe_ignore_trace]
    options: NumberOptions,
}

/// Reads a `NumberFormat`'s options, as its constructor and `toLocaleString` are given them.
pub(crate) fn number_options(
    locales: &JsValue,
    options: &JsValue,
    context: &mut Context,
) -> JsResult<NumberOptions> {
    let tag = resolve_locale(locales, context)?;
    let options = options_of(options, context)?;
    let style = option_str(
        &options,
        "style",
        &["decimal", "percent", "currency", "unit"],
        context,
    )?
    .unwrap_or_else(|| "decimal".into());
    let currency = option_str(&options, "currency", &[], context)?;
    if let Some(code) = &currency
        && (code.len() != 3 || !code.bytes().all(|b| b.is_ascii_alphabetic()))
    {
        return Err(range_error(format!("Invalid currency code : {code}")));
    }
    let currency = currency.map(|c| c.to_ascii_uppercase());
    let currency_display = option_str(
        &options,
        "currencyDisplay",
        &["code", "symbol", "narrowSymbol", "name"],
        context,
    )?
    .unwrap_or_else(|| "symbol".into());
    let currency_sign = option_str(
        &options,
        "currencySign",
        &["standard", "accounting"],
        context,
    )?
    .unwrap_or_else(|| "standard".into());
    let unit = option_str(&options, "unit", &[], context)?;
    if let Some(unit) = &unit
        && !valid_unit(unit)
    {
        return Err(range_error(format!(
            "Invalid unit argument for Intl.NumberFormat() '{unit}'"
        )));
    }
    let unit_display = option_str(
        &options,
        "unitDisplay",
        &["short", "narrow", "long"],
        context,
    )?
    .unwrap_or_else(|| "short".into());
    if style == "currency" && currency.is_none() {
        return Err(type_error("Currency code is required with currency style."));
    }
    if style == "unit" && unit.is_none() {
        return Err(type_error("Unit is required with unit style."));
    }
    let notation = option_str(
        &options,
        "notation",
        &["standard", "scientific", "engineering", "compact"],
        context,
    )?
    .unwrap_or_else(|| "standard".into());
    let (min_frac, max_frac) = match style.as_str() {
        "currency" if notation != "compact" => {
            let cd = currency_digits(currency.as_deref().unwrap_or("USD"));
            (cd, cd)
        }
        "percent" => (0, 0),
        _ => (0, 3),
    };
    let mut digits = super::number::digit_options(&options, min_frac, max_frac, context)?;
    let given = [
        "minimumFractionDigits",
        "maximumFractionDigits",
        "minimumSignificantDigits",
        "maximumSignificantDigits",
    ];
    let mut any = false;
    for key in given {
        any |= !option(&options, key, context)?.is_undefined();
    }
    if notation == "compact" && !any {
        digits.compact = true;
    }
    let compact_display = option_str(&options, "compactDisplay", &["short", "long"], context)?
        .unwrap_or_else(|| "short".into());
    let grouping_value = option(&options, "useGrouping", context)?;
    let grouping = if grouping_value.is_undefined() {
        if notation == "compact" {
            "min2".to_owned()
        } else {
            "auto".to_owned()
        }
    } else if grouping_value.is_boolean() {
        if grouping_value.to_boolean() {
            "always".to_owned()
        } else {
            "false".to_owned()
        }
    } else {
        let text = text_of(&grouping_value, context)?;
        match text.as_str() {
            "always" | "auto" | "min2" => text,
            "true" => "always".to_owned(),
            "false" => "false".to_owned(),
            _ => {
                return Err(range_error(format!(
                    "Value {text} out of range for Intl.NumberFormat options property useGrouping"
                )));
            }
        }
    };
    let sign_display = option_str(
        &options,
        "signDisplay",
        &["auto", "never", "always", "exceptZero", "negative"],
        context,
    )?
    .unwrap_or_else(|| "auto".into());
    Ok(NumberOptions {
        locale: tag.base_name(),
        style,
        currency,
        currency_display,
        currency_sign,
        unit,
        unit_display,
        notation,
        compact_display,
        sign_display,
        grouping,
        digits,
    })
}

/// What `format` is given, as a number: a decimal (with its sign), or not a finite one.
enum Amount {
    Finite(bool, Decimal),
    Infinite(bool),
    NaN,
}

fn amount_of(value: &JsValue, context: &mut Context) -> JsResult<Amount> {
    if let Some(big) = value.as_bigint() {
        let text = big.to_string_radix(10);
        let negative = text.starts_with('-');
        return Ok(Amount::Finite(
            negative,
            Decimal::of_digits(text.trim_start_matches('-')),
        ));
    }
    let n = value.to_number(context)?;
    Ok(if n.is_nan() {
        Amount::NaN
    } else if n.is_infinite() {
        Amount::Infinite(n < 0.)
    } else {
        Amount::Finite(n.is_sign_negative(), Decimal::of(n))
    })
}

/// The compact suffix for a power of a thousand: `K`, `M`… or ` thousand`, ` million`…
fn compact_suffix(exponent: i32, long: bool) -> &'static str {
    match (exponent, long) {
        (3, false) => "K",
        (6, false) => "M",
        (9, false) => "B",
        (12, false) => "T",
        (3, true) => " thousand",
        (6, true) => " million",
        (9, true) => " billion",
        (12, true) => " trillion",
        _ => "",
    }
}

/// Formats `amount` as `options` say, in parts.
fn format_parts(options: &NumberOptions, amount: Amount) -> Vec<(&'static str, String)> {
    let (negative, number_parts, rounded_zero, one) = match amount {
        Amount::NaN => (false, vec![("nan", "NaN".to_owned())], false, false),
        Amount::Infinite(negative) => (negative, vec![("infinity", "∞".to_owned())], false, false),
        Amount::Finite(negative, mut decimal) => {
            if options.style == "percent" {
                decimal.shift(2);
            }
            let (parts, zero, one) = finite_parts(options, &decimal);
            (negative, parts, zero, one)
        }
    };
    let sign = match options.sign_display.as_str() {
        "never" => None,
        "always" => Some(if negative { "-" } else { "+" }),
        "exceptZero" if rounded_zero => None,
        "exceptZero" => Some(if negative { "-" } else { "+" }),
        "negative" if negative && !rounded_zero => Some("-"),
        "negative" => None,
        _ => negative.then_some("-"),
    };
    let accounting =
        options.currency_sign == "accounting" && options.style == "currency" && sign == Some("-");
    let mut out: Vec<(&'static str, String)> = Vec::new();
    if accounting {
        out.push(("literal", "(".to_owned()));
    } else if let Some(sign) = sign {
        out.push((
            if sign == "-" { "minusSign" } else { "plusSign" },
            sign.to_owned(),
        ));
    }
    match options.style.as_str() {
        "currency" => {
            let code = options.currency.clone().unwrap_or_default();
            let symbol = SYMBOLS.iter().find(|(c, ..)| *c == code);
            match options.currency_display.as_str() {
                "name" => {
                    out.extend(number_parts);
                    let names = super::CURRENCIES.iter().find(|(c, ..)| *c == code);
                    let name = match names {
                        Some((_, singular, plural)) => {
                            if one { *singular } else { *plural }.to_owned()
                        }
                        None => code,
                    };
                    out.push(("literal", " ".to_owned()));
                    out.push(("currency", name));
                }
                display => {
                    let text = match (display, symbol) {
                        ("symbol", Some((_, symbol, _))) => (*symbol).to_owned(),
                        ("narrowSymbol", Some((_, _, narrow))) => (*narrow).to_owned(),
                        _ => code.clone(),
                    };
                    // A code, or a symbol that is letters, stands apart from the amount.
                    let apart = text.chars().last().is_some_and(char::is_alphabetic);
                    out.push(("currency", text));
                    if apart {
                        out.push(("literal", "\u{a0}".to_owned()));
                    }
                    out.extend(number_parts);
                }
            }
        }
        "percent" => {
            out.extend(number_parts);
            out.push(("percentSign", "%".to_owned()));
        }
        "unit" => {
            out.extend(number_parts);
            let (text, spaced) = unit_text(
                options.unit.as_deref().unwrap_or("byte"),
                &options.unit_display,
                one,
            );
            if spaced {
                out.push(("literal", " ".to_owned()));
            }
            out.push(("unit", text));
        }
        _ => out.extend(number_parts),
    }
    if accounting {
        out.push(("literal", ")".to_owned()));
    }
    out
}

/// A finite number's own parts — digits, groups, the point, the fraction, a compact suffix or
/// an exponent — and whether it rounded to zero, and to exactly one.
fn finite_parts(
    options: &NumberOptions,
    decimal: &Decimal,
) -> (Vec<(&'static str, String)>, bool, bool) {
    let digits = &options.digits;
    let mut exponent = 0;
    let mut suffix = "";
    let rounded = match options.notation.as_str() {
        "compact" if !decimal.is_zero() && decimal.magnitude() >= 3 => {
            let mut power = (decimal.magnitude() / 3 * 3).min(12);
            loop {
                let mut scaled = decimal.clone();
                scaled.shift(-power);
                let rounded = round_decimal(&scaled, digits);
                // `999.95K` rounds to `1000K`: that is `1M`.
                if rounded.decimal.point > 3 && power < 12 {
                    power += 3;
                    continue;
                }
                suffix = compact_suffix(power, options.compact_display == "long");
                break rounded;
            }
        }
        "scientific" | "engineering" if !decimal.is_zero() => {
            let step = if options.notation == "engineering" {
                3
            } else {
                1
            };
            let mut power = decimal.magnitude().div_euclid(step) * step;
            loop {
                let mut scaled = decimal.clone();
                scaled.shift(-power);
                let rounded = round_decimal(&scaled, digits);
                if rounded.decimal.point > step {
                    power += step;
                    continue;
                }
                exponent = power;
                break rounded;
            }
        }
        _ => round_decimal(decimal, digits),
    };
    let zero = rounded.decimal.is_zero();
    let one =
        rounded.integer == "1" && rounded.fraction.is_empty() && suffix.is_empty() && exponent == 0;
    let mut parts: Vec<(&'static str, String)> = Vec::new();
    for piece in group_parts(&rounded.integer, &options.grouping) {
        parts.push((if piece == "," { "group" } else { "integer" }, piece));
    }
    if !rounded.fraction.is_empty() {
        parts.push(("decimal", ".".to_owned()));
        parts.push(("fraction", rounded.fraction.clone()));
    }
    if !suffix.is_empty() {
        if let Some(word) = suffix.strip_prefix(' ') {
            parts.push(("literal", " ".to_owned()));
            parts.push(("compact", word.to_owned()));
        } else {
            parts.push(("compact", suffix.to_owned()));
        }
    }
    if matches!(options.notation.as_str(), "scientific" | "engineering") {
        parts.push(("exponentSeparator", "E".to_owned()));
        if exponent < 0 {
            parts.push(("exponentMinusSign", "-".to_owned()));
        }
        parts.push(("exponentInteger", exponent.abs().to_string()));
    }
    (parts, zero, one)
}

pub(crate) fn make(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let options = number_options(&arg(args, 0), &arg(args, 1), context)?;
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        NumberFormatData { options },
    ))
}

fn format_value(
    options: &NumberOptions,
    value: &JsValue,
    context: &mut Context,
) -> JsResult<Vec<(&'static str, String)>> {
    let amount = amount_of(value, context)?;
    Ok(format_parts(options, amount))
}

fn number_format(
    _: &JsValue,
    args: &[JsValue],
    format: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let data: NumberFormatData = data_of(&format.clone().into(), "NumberFormat.prototype.format")?;
    let parts = format_value(&data.options, &arg(args, 0), context)?;
    Ok(js_text(&joined(&parts)))
}

/// A range: `3–5`, or `$3.00 – $5.00` when each end carries a unit or a currency; `~5` when
/// both ends come out the same.
fn range_parts(
    options: &NumberOptions,
    start: &JsValue,
    end: &JsValue,
    context: &mut Context,
) -> JsResult<Vec<(&'static str, String, &'static str)>> {
    if start.is_undefined() || end.is_undefined() {
        return Err(type_error("start or end is undefined"));
    }
    let a = format_value(options, start, context)?;
    let b = format_value(options, end, context)?;
    if joined(&a) == joined(&b) {
        let mut out = vec![("approximatelySign", "~".to_owned(), "shared")];
        out.extend(a.into_iter().map(|(k, v)| (k, v, "shared")));
        return Ok(out);
    }
    let separator = if matches!(options.style.as_str(), "currency" | "unit" | "percent") {
        " – "
    } else {
        "–"
    };
    let mut out: Vec<(&'static str, String, &'static str)> =
        a.into_iter().map(|(k, v)| (k, v, "startRange")).collect();
    out.push(("literal", separator.to_owned(), "shared"));
    out.extend(b.into_iter().map(|(k, v)| (k, v, "endRange")));
    Ok(out)
}

pub(crate) const METHODS: Methods = &[
    ("formatToParts", 1, |this, args, cx| {
        let data: NumberFormatData = data_of(this, "NumberFormat.prototype.formatToParts")?;
        let parts = format_value(&data.options, &arg(args, 0), cx)?;
        parts_array(cx, &parts)
    }),
    ("formatRange", 2, |this, args, cx| {
        let data: NumberFormatData = data_of(this, "NumberFormat.prototype.formatRange")?;
        let parts = range_parts(&data.options, &arg(args, 0), &arg(args, 1), cx)?;
        Ok(js_text(
            &parts.iter().map(|(_, v, _)| v.as_str()).collect::<String>(),
        ))
    }),
    ("formatRangeToParts", 2, |this, args, cx| {
        let data: NumberFormatData = data_of(this, "NumberFormat.prototype.formatRangeToParts")?;
        let parts = range_parts(&data.options, &arg(args, 0), &arg(args, 1), cx)?;
        let flat: Vec<(&str, String)> = parts.iter().map(|(k, v, _)| (*k, v.clone())).collect();
        let list = parts_array(cx, &flat)?;
        for (item, (_, _, source)) in crate::native::list_of(&list, cx)?.iter().zip(&parts) {
            if let Some(item) = item.as_object() {
                item.set(js_string!("source"), js_text(source), false, cx)?;
            }
        }
        Ok(list)
    }),
    ("resolvedOptions", 0, |this, _, cx| {
        let data: NumberFormatData = data_of(this, "NumberFormat.prototype.resolvedOptions")?;
        let o = data.options.clone();
        let currency = o.style == "currency";
        let unit = o.style == "unit";
        let text = |s: &str, when: bool| {
            if when {
                js_text(s)
            } else {
                JsValue::undefined()
            }
        };
        let mut fields = vec![
            ("locale", js_text(&o.locale)),
            ("numberingSystem", js_text("latn")),
            ("style", js_text(&o.style)),
            (
                "currency",
                text(o.currency.as_deref().unwrap_or(""), currency),
            ),
            ("currencyDisplay", text(&o.currency_display, currency)),
            ("currencySign", text(&o.currency_sign, currency)),
            ("unit", text(o.unit.as_deref().unwrap_or(""), unit)),
            ("unitDisplay", text(&o.unit_display, unit)),
        ];
        fields.extend(o.digits.resolved());
        fields.push((
            "useGrouping",
            if o.grouping == "false" {
                false.into()
            } else {
                js_text(&o.grouping)
            },
        ));
        fields.push(("notation", js_text(&o.notation)));
        fields.push((
            "compactDisplay",
            text(&o.compact_display, o.notation == "compact"),
        ));
        fields.push(("signDisplay", js_text(&o.sign_display)));
        fields.push(("roundingIncrement", 1.into()));
        fields.push(("roundingMode", js_text("halfExpand")));
        fields.push(("roundingPriority", js_text("auto")));
        fields.push(("trailingZeroDisplay", js_text("auto")));
        record(cx, fields)
    }),
];

pub(crate) const ACCESSORS: Accessors = &[(
    "format",
    |this, _, cx| bound(this, "format", number_format, cx),
    None,
)];

/// `Number.prototype.toLocaleString(locales, options)`, and `BigInt`'s.
pub(crate) fn to_locale(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let value = if this.is_number() || this.is_bigint() {
        this.clone()
    } else {
        // A `Number` or `BigInt` wrapper: its primitive.
        let value_of = crate::native::get(this, "valueOf", context)?;
        let primitive = crate::call(context, &value_of, this, &[])?;
        if !primitive.is_number() && !primitive.is_bigint() {
            return Err(type_error(
                "toLocaleString called on a value that is not a number",
            ));
        }
        primitive
    };
    let options = number_options(&arg(args, 0), &arg(args, 1), context)?;
    let parts = format_value(&options, &value, context)?;
    Ok(js_text(&joined(&parts)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(style: &str, f: impl FnOnce(&mut NumberOptions)) -> NumberOptions {
        let mut options = NumberOptions {
            locale: "en-US".into(),
            style: style.into(),
            currency: None,
            currency_display: "symbol".into(),
            currency_sign: "standard".into(),
            unit: None,
            unit_display: "short".into(),
            notation: "standard".into(),
            compact_display: "short".into(),
            sign_display: "auto".into(),
            grouping: "auto".into(),
            digits: Digits::default(),
        };
        f(&mut options);
        options
    }

    fn show(options: &NumberOptions, n: f64) -> String {
        joined(&format_parts(
            options,
            Amount::Finite(n.is_sign_negative(), Decimal::of(n)),
        ))
    }

    #[test]
    fn decimals_round_half_away_from_zero_in_decimal() {
        let plain = with("decimal", |_| {});
        assert_eq!(show(&plain, 1234567.891), "1,234,567.891");
        assert_eq!(show(&plain, 1.0005), "1.001");
        assert_eq!(show(&plain, -0.0001), "-0");
        assert_eq!(show(&plain, 0.1 + 0.2), "0.3");
        let two = with("decimal", |o| {
            o.digits = Digits {
                min_frac: 2,
                max_frac: 2,
                ..Digits::default()
            }
        });
        assert_eq!(show(&two, 1.005), "1.01");
        assert_eq!(show(&two, 9.999), "10.00");
        let sig = with("decimal", |o| {
            o.digits = Digits {
                min_sig: Some(1),
                max_sig: Some(3),
                ..Digits::default()
            }
        });
        assert_eq!(show(&sig, 123456.), "123,000");
        assert_eq!(show(&sig, 0.012345), "0.0123");
    }

    #[test]
    fn styles_and_notations() {
        let usd = with("currency", |o| {
            o.currency = Some("USD".into());
            o.digits = Digits {
                min_frac: 2,
                max_frac: 2,
                ..Digits::default()
            };
        });
        assert_eq!(show(&usd, -1234.5), "-$1,234.50");
        let percent = with("percent", |o| {
            o.digits = Digits {
                max_frac: 0,
                ..Digits::default()
            }
        });
        assert_eq!(show(&percent, 0.256), "26%");
        let compact = with("decimal", |o| {
            o.notation = "compact".into();
            o.grouping = "min2".into();
            o.digits.compact = true;
        });
        assert_eq!(show(&compact, 1234.), "1.2K");
        assert_eq!(show(&compact, 12345.), "12K");
        assert_eq!(show(&compact, 123456.), "123K");
        assert_eq!(show(&compact, 999999.), "1M");
        assert_eq!(show(&compact, 999.), "999");
        let scientific = with("decimal", |o| o.notation = "scientific".into());
        assert_eq!(show(&scientific, 123456.), "1.235E5");
        assert_eq!(show(&scientific, 0.00012), "1.2E-4");
        let kb = with("unit", |o| o.unit = Some("kilobyte".into()));
        assert_eq!(show(&kb, 5.), "5 kB");
        let long = with("unit", |o| {
            o.unit = Some("day".into());
            o.unit_display = "long".into();
        });
        assert_eq!(show(&long, 1.), "1 day");
        assert_eq!(show(&long, 2.), "2 days");
    }
}
