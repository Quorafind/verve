//! `Intl.DateTimeFormat`, and `Date.prototype.toLocaleString` and its kin: dates and times as
//! English (`en-US`) writes them — `1/5/2024, 1:04:05 PM`, `Friday, January 5, 2024`.
//!
//! Times are shown in the system's own zone (its offset as Boa's `Date` reads it), in UTC, at
//! a fixed offset (`+05:30`), or in one of the zones most people live in, by their standard
//! offset and their daylight-saving rule (the United States', Europe's, south-eastern
//! Australia's, New Zealand's). A zone outside that list is accepted and shown in the system's
//! zone: there is no time zone database here.

use std::sync::OnceLock;

use boa_engine::{
    Context, Finalize, JsData, JsObject, JsResult, JsValue, Trace, object::builtins::JsDate,
};

use super::{
    Accessors, Methods, bound, data_of, joined, option_bool, option_number, option_str, options_of,
    parts_array, record, resolve_locale,
};
use crate::native::{arg, js_text, range_error, type_error};

// ─── Calendar arithmetic ────────────────────────────────────────────────────────────────────

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The day of the week, Sunday 0.
fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The day of the month of the `n`th Sunday.
fn nth_sunday(year: i64, month: u32, n: u32) -> u32 {
    let first = weekday(days_from_civil(year, month, 1));
    1 + (7 - first) % 7 + 7 * (n - 1)
}

fn last_sunday(year: i64, month: u32) -> u32 {
    let last = days_in_month(year, month);
    last - weekday(days_from_civil(year, month, last))
}

/// Milliseconds since the epoch of `hour` o'clock on a date, read as UTC.
fn at(year: i64, month: u32, day: u32, hour: i64) -> f64 {
    ((days_from_civil(year, month, day) * 24 + hour) * 3_600_000) as f64
}

// ─── Time zones ─────────────────────────────────────────────────────────────────────────────

/// When a zone keeps daylight-saving time.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Rule {
    Never,
    /// Second Sunday of March to first Sunday of November, at 2:00 local time.
    UnitedStates,
    /// Last Sunday of March to last Sunday of October, at 1:00 UTC.
    Europe,
    /// First Sunday of October to first Sunday of April, at 2:00 standard time.
    SouthAustralia,
    /// Last Sunday of September to first Sunday of April, at 2:00 standard time.
    NewZealand,
}

/// A zone: its IANA name, standard offset in minutes east of UTC, rule, and — where English
/// has them — its names, standard and daylight, short and long.
struct ZoneInfo {
    name: &'static str,
    offset: i32,
    rule: Rule,
    names: Option<(&'static str, &'static str, &'static str, &'static str)>,
}

const fn zone(name: &'static str, offset: i32, rule: Rule) -> ZoneInfo {
    ZoneInfo {
        name,
        offset,
        rule,
        names: None,
    }
}

const fn us_zone(
    name: &'static str,
    offset: i32,
    rule: Rule,
    names: (&'static str, &'static str, &'static str, &'static str),
) -> ZoneInfo {
    ZoneInfo {
        name,
        offset,
        rule,
        names: Some(names),
    }
}

const ZONES: &[ZoneInfo] = &[
    us_zone(
        "America/New_York",
        -300,
        Rule::UnitedStates,
        (
            "EST",
            "EDT",
            "Eastern Standard Time",
            "Eastern Daylight Time",
        ),
    ),
    us_zone(
        "America/Detroit",
        -300,
        Rule::UnitedStates,
        (
            "EST",
            "EDT",
            "Eastern Standard Time",
            "Eastern Daylight Time",
        ),
    ),
    us_zone(
        "America/Toronto",
        -300,
        Rule::UnitedStates,
        (
            "EST",
            "EDT",
            "Eastern Standard Time",
            "Eastern Daylight Time",
        ),
    ),
    us_zone(
        "America/Chicago",
        -360,
        Rule::UnitedStates,
        (
            "CST",
            "CDT",
            "Central Standard Time",
            "Central Daylight Time",
        ),
    ),
    us_zone(
        "America/Winnipeg",
        -360,
        Rule::UnitedStates,
        (
            "CST",
            "CDT",
            "Central Standard Time",
            "Central Daylight Time",
        ),
    ),
    us_zone(
        "America/Denver",
        -420,
        Rule::UnitedStates,
        (
            "MST",
            "MDT",
            "Mountain Standard Time",
            "Mountain Daylight Time",
        ),
    ),
    us_zone(
        "America/Edmonton",
        -420,
        Rule::UnitedStates,
        (
            "MST",
            "MDT",
            "Mountain Standard Time",
            "Mountain Daylight Time",
        ),
    ),
    us_zone(
        "America/Phoenix",
        -420,
        Rule::Never,
        (
            "MST",
            "MST",
            "Mountain Standard Time",
            "Mountain Standard Time",
        ),
    ),
    us_zone(
        "America/Los_Angeles",
        -480,
        Rule::UnitedStates,
        (
            "PST",
            "PDT",
            "Pacific Standard Time",
            "Pacific Daylight Time",
        ),
    ),
    us_zone(
        "America/Vancouver",
        -480,
        Rule::UnitedStates,
        (
            "PST",
            "PDT",
            "Pacific Standard Time",
            "Pacific Daylight Time",
        ),
    ),
    us_zone(
        "America/Anchorage",
        -540,
        Rule::UnitedStates,
        (
            "AKST",
            "AKDT",
            "Alaska Standard Time",
            "Alaska Daylight Time",
        ),
    ),
    us_zone(
        "Pacific/Honolulu",
        -600,
        Rule::Never,
        (
            "HST",
            "HST",
            "Hawaii-Aleutian Standard Time",
            "Hawaii-Aleutian Standard Time",
        ),
    ),
    zone("America/Halifax", -240, Rule::UnitedStates),
    zone("America/St_Johns", -210, Rule::UnitedStates),
    zone("America/Mexico_City", -360, Rule::Never),
    zone("America/Bogota", -300, Rule::Never),
    zone("America/Lima", -300, Rule::Never),
    zone("America/Caracas", -240, Rule::Never),
    zone("America/Sao_Paulo", -180, Rule::Never),
    zone("America/Argentina/Buenos_Aires", -180, Rule::Never),
    zone("Atlantic/Reykjavik", 0, Rule::Never),
    zone("Europe/London", 0, Rule::Europe),
    zone("Europe/Dublin", 0, Rule::Europe),
    zone("Europe/Lisbon", 0, Rule::Europe),
    zone("Europe/Paris", 60, Rule::Europe),
    zone("Europe/Berlin", 60, Rule::Europe),
    zone("Europe/Madrid", 60, Rule::Europe),
    zone("Europe/Rome", 60, Rule::Europe),
    zone("Europe/Amsterdam", 60, Rule::Europe),
    zone("Europe/Brussels", 60, Rule::Europe),
    zone("Europe/Vienna", 60, Rule::Europe),
    zone("Europe/Zurich", 60, Rule::Europe),
    zone("Europe/Stockholm", 60, Rule::Europe),
    zone("Europe/Oslo", 60, Rule::Europe),
    zone("Europe/Copenhagen", 60, Rule::Europe),
    zone("Europe/Warsaw", 60, Rule::Europe),
    zone("Europe/Prague", 60, Rule::Europe),
    zone("Europe/Budapest", 60, Rule::Europe),
    zone("Europe/Belgrade", 60, Rule::Europe),
    zone("Europe/Athens", 120, Rule::Europe),
    zone("Europe/Helsinki", 120, Rule::Europe),
    zone("Europe/Kyiv", 120, Rule::Europe),
    zone("Europe/Kiev", 120, Rule::Europe),
    zone("Europe/Bucharest", 120, Rule::Europe),
    zone("Europe/Sofia", 120, Rule::Europe),
    zone("Europe/Riga", 120, Rule::Europe),
    zone("Europe/Vilnius", 120, Rule::Europe),
    zone("Europe/Tallinn", 120, Rule::Europe),
    zone("Europe/Istanbul", 180, Rule::Never),
    zone("Europe/Moscow", 180, Rule::Never),
    zone("Africa/Lagos", 60, Rule::Never),
    zone("Africa/Johannesburg", 120, Rule::Never),
    zone("Africa/Nairobi", 180, Rule::Never),
    zone("Asia/Dubai", 240, Rule::Never),
    zone("Asia/Tehran", 210, Rule::Never),
    zone("Asia/Karachi", 300, Rule::Never),
    zone("Asia/Kolkata", 330, Rule::Never),
    zone("Asia/Calcutta", 330, Rule::Never),
    zone("Asia/Kathmandu", 345, Rule::Never),
    zone("Asia/Dhaka", 360, Rule::Never),
    zone("Asia/Bangkok", 420, Rule::Never),
    zone("Asia/Jakarta", 420, Rule::Never),
    zone("Asia/Ho_Chi_Minh", 420, Rule::Never),
    zone("Asia/Shanghai", 480, Rule::Never),
    zone("Asia/Hong_Kong", 480, Rule::Never),
    zone("Asia/Taipei", 480, Rule::Never),
    zone("Asia/Singapore", 480, Rule::Never),
    zone("Asia/Manila", 480, Rule::Never),
    zone("Asia/Kuala_Lumpur", 480, Rule::Never),
    zone("Australia/Perth", 480, Rule::Never),
    zone("Asia/Seoul", 540, Rule::Never),
    zone("Asia/Tokyo", 540, Rule::Never),
    zone("Australia/Darwin", 570, Rule::Never),
    zone("Australia/Adelaide", 570, Rule::SouthAustralia),
    zone("Australia/Brisbane", 600, Rule::Never),
    zone("Australia/Sydney", 600, Rule::SouthAustralia),
    zone("Australia/Melbourne", 600, Rule::SouthAustralia),
    zone("Australia/Hobart", 600, Rule::SouthAustralia),
    zone("Pacific/Auckland", 720, Rule::NewZealand),
];

pub(crate) fn zone_names() -> Vec<&'static str> {
    let mut names: Vec<&str> = ZONES.iter().map(|z| z.name).collect();
    names.push("UTC");
    names.sort_unstable();
    names
}

/// The zone the system's clock is set to, by its IANA name.
fn system_zone() -> &'static str {
    static ZONE: OnceLock<String> = OnceLock::new();
    ZONE.get_or_init(|| iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into()))
}

/// Where a time is shown.
#[derive(Clone, Debug)]
enum Zone {
    /// The system's zone, as Boa's `Date` reads its offsets.
    Local,
    Fixed(i32),
    Ruled(usize),
}

/// Whether the zone `info` keeps daylight time at `t` (milliseconds since the epoch).
fn in_daylight(info: &ZoneInfo, t: f64) -> bool {
    let std = f64::from(info.offset) * 60_000.;
    let (year, ..) = civil_from_days(((t + std) / 86_400_000.).floor() as i64);
    let hour = 3_600_000.;
    match info.rule {
        Rule::Never => false,
        Rule::UnitedStates => {
            let start = at(year, 3, nth_sunday(year, 3, 2), 2) - std;
            let end = at(year, 11, nth_sunday(year, 11, 1), 2) - std - hour;
            t >= start && t < end
        }
        Rule::Europe => {
            let start = at(year, 3, last_sunday(year, 3), 1);
            let end = at(year, 10, last_sunday(year, 10), 1);
            t >= start && t < end
        }
        Rule::SouthAustralia => {
            let start = at(year, 10, nth_sunday(year, 10, 1), 2) - std;
            let end = at(year, 4, nth_sunday(year, 4, 1), 3) - std - hour;
            t >= start || t < end
        }
        Rule::NewZealand => {
            let start = at(year, 9, last_sunday(year, 9), 2) - std;
            let end = at(year, 4, nth_sunday(year, 4, 1), 3) - std - hour;
            t >= start || t < end
        }
    }
}

/// The zone a `timeZone` option names, and its canonical name.
fn resolve_zone(name: Option<String>) -> JsResult<(String, Zone)> {
    let Some(name) = name else {
        return Ok((system_zone().to_owned(), Zone::Local));
    };
    let upper = name.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "UTC"
            | "ETC/UTC"
            | "GMT"
            | "ETC/GMT"
            | "UCT"
            | "ETC/UCT"
            | "Z"
            | "ZULU"
            | "ETC/ZULU"
            | "ETC/GMT0"
            | "GMT0"
    ) {
        return Ok(("UTC".into(), Zone::Fixed(0)));
    }
    // `+05:30`, `-0800`, `+05`.
    if let Some(sign) = name.chars().next().filter(|c| *c == '+' || *c == '-') {
        let digits: String = name[1..].chars().filter(char::is_ascii_digit).collect();
        if (digits.len() == 2 || digits.len() == 4)
            && name[1..].chars().all(|c| c.is_ascii_digit() || c == ':')
        {
            let hours: i32 = digits[..2].parse().unwrap_or(0);
            let minutes: i32 = digits.get(2..).and_then(|m| m.parse().ok()).unwrap_or(0);
            if hours <= 23 && minutes <= 59 {
                let total = (hours * 60 + minutes) * if sign == '-' { -1 } else { 1 };
                return Ok((format!("{sign}{hours:02}:{minutes:02}"), Zone::Fixed(total)));
            }
        }
        return Err(range_error(format!("Invalid time zone specified: {name}")));
    }
    // `Etc/GMT+5` is five hours west: its sign is POSIX's, the other way about.
    if let Some(rest) = upper.strip_prefix("ETC/GMT")
        && let Ok(hours) = rest.parse::<i32>()
        && (-14..=12).contains(&hours)
    {
        return Ok((format!("Etc/GMT{rest}"), Zone::Fixed(-hours * 60)));
    }
    if name.eq_ignore_ascii_case(system_zone()) {
        return Ok((system_zone().to_owned(), Zone::Local));
    }
    if let Some(at) = ZONES
        .iter()
        .position(|z| z.name.eq_ignore_ascii_case(&name))
    {
        return Ok((ZONES[at].name.to_owned(), Zone::Ruled(at)));
    }
    let shaped = name.contains('/')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_-+".contains(c));
    if shaped {
        return Ok((name, Zone::Local));
    }
    Err(range_error(format!("Invalid time zone specified: {name}")))
}

/// Minutes east of UTC in `zone` at `t`, and whether that is daylight time.
fn offset_at(zone: &Zone, t: f64, context: &mut Context) -> JsResult<(i32, bool)> {
    Ok(match zone {
        Zone::Fixed(offset) => (*offset, false),
        Zone::Ruled(at) => {
            let info = &ZONES[*at];
            let daylight = in_daylight(info, t);
            (info.offset + if daylight { 60 } else { 0 }, daylight)
        }
        Zone::Local => {
            let date = JsDate::new(context);
            date.set_time(t, context)?;
            let offset = -date.get_timezone_offset(context)?.to_number(context)? as i32;
            // Daylight when ahead of the zone's offset in January and in July both.
            let (year, ..) = civil_from_days((t / 86_400_000.).floor() as i64);
            let mut standard = offset;
            for month in [1, 7] {
                date.set_time(at(year, month, 1, 0), context)?;
                standard =
                    standard.min(-date.get_timezone_offset(context)?.to_number(context)? as i32);
            }
            (offset, offset > standard)
        }
    })
}

// ─── Options ────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
struct Fields {
    weekday: Option<String>,
    era: Option<String>,
    year: Option<String>,
    month: Option<String>,
    day: Option<String>,
    day_period: Option<String>,
    hour: Option<String>,
    minute: Option<String>,
    second: Option<String>,
    fractional: Option<usize>,
    zone_name: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct DateOptions {
    locale: String,
    zone_label: String,
    zone: Zone,
    fields: Fields,
    hour_cycle: String,
    date_style: Option<String>,
    time_style: Option<String>,
}

#[derive(Clone, Trace, Finalize, JsData)]
struct DateFormatData {
    #[unsafe_ignore_trace]
    options: DateOptions,
}

/// What a format shows when told nothing: `DateTimeFormat` and `toLocaleDateString` the
/// date, `toLocaleTimeString` the time, `toLocaleString` both.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Defaults {
    Date,
    Time,
    All,
}

const TEXT: &[&str] = &["narrow", "short", "long"];
const NUMBER: &[&str] = &["numeric", "2-digit"];
const STYLES: &[&str] = &["full", "long", "medium", "short"];

fn date_options(
    locales: &JsValue,
    options: &JsValue,
    required: Defaults,
    defaults: Defaults,
    context: &mut Context,
) -> JsResult<DateOptions> {
    let tag = resolve_locale(locales, context)?;
    let options = options_of(options, context)?;
    let hour12 = option_bool(&options, "hour12", context)?;
    let hour_cycle = option_str(
        &options,
        "hourCycle",
        &["h11", "h12", "h23", "h24"],
        context,
    )?;
    let zone_option = option_str(&options, "timeZone", &[], context)?;
    let mut fields = Fields {
        weekday: option_str(&options, "weekday", TEXT, context)?,
        era: option_str(&options, "era", TEXT, context)?,
        year: option_str(&options, "year", NUMBER, context)?,
        month: option_str(
            &options,
            "month",
            &["numeric", "2-digit", "narrow", "short", "long"],
            context,
        )?,
        day: option_str(&options, "day", NUMBER, context)?,
        day_period: option_str(&options, "dayPeriod", TEXT, context)?,
        hour: option_str(&options, "hour", NUMBER, context)?,
        minute: option_str(&options, "minute", NUMBER, context)?,
        second: option_str(&options, "second", NUMBER, context)?,
        fractional: option_number(&options, "fractionalSecondDigits", 1, 3, context)?,
        zone_name: option_str(
            &options,
            "timeZoneName",
            &[
                "short",
                "long",
                "shortOffset",
                "longOffset",
                "shortGeneric",
                "longGeneric",
            ],
            context,
        )?,
    };
    let date_style = option_str(&options, "dateStyle", STYLES, context)?;
    let time_style = option_str(&options, "timeStyle", STYLES, context)?;
    let has_date = fields.weekday.is_some()
        || fields.year.is_some()
        || fields.month.is_some()
        || fields.day.is_some();
    let has_time = fields.day_period.is_some()
        || fields.hour.is_some()
        || fields.minute.is_some()
        || fields.second.is_some()
        || fields.fractional.is_some();
    if date_style.is_some() || time_style.is_some() {
        if has_date || has_time || fields.era.is_some() || fields.zone_name.is_some() {
            return Err(type_error(
                "Can't set option when dateStyle or timeStyle is used",
            ));
        }
        if required == Defaults::Date && date_style.is_none() {
            return Err(type_error("Invalid option : timeStyle without dateStyle"));
        }
        if required == Defaults::Time && time_style.is_none() {
            return Err(type_error("Invalid option : dateStyle without timeStyle"));
        }
    } else {
        let needs = match required {
            Defaults::Date => !has_date,
            Defaults::Time => !has_time,
            Defaults::All => !has_date && !has_time,
        };
        if needs && matches!(defaults, Defaults::Date | Defaults::All) {
            fields.year = Some("numeric".into());
            fields.month = Some("numeric".into());
            fields.day = Some("numeric".into());
        }
        if needs && matches!(defaults, Defaults::Time | Defaults::All) {
            fields.hour = Some("numeric".into());
            fields.minute = Some("numeric".into());
            fields.second = Some("numeric".into());
        }
    }
    let hour_cycle = match (hour12, hour_cycle) {
        (Some(true), _) => "h12".to_owned(),
        (Some(false), _) => "h23".to_owned(),
        (None, Some(cycle)) => cycle,
        (None, None) => tag
            .keyword("hc")
            .filter(|c| ["h11", "h12", "h23", "h24"].contains(c))
            .unwrap_or("h12")
            .to_owned(),
    };
    let (zone_label, zone) = resolve_zone(zone_option)?;
    Ok(DateOptions {
        locale: tag.base_name(),
        zone_label,
        zone,
        fields,
        hour_cycle,
        date_style,
        time_style,
    })
}

/// The fields a style shows.
fn effective(options: &DateOptions) -> Fields {
    let mut fields = options.fields.clone();
    if let Some(style) = &options.date_style {
        let (weekday, month, year) = match style.as_str() {
            "full" => (Some("long"), "long", "numeric"),
            "long" => (None, "long", "numeric"),
            "medium" => (None, "short", "numeric"),
            _ => (None, "numeric", "2-digit"),
        };
        fields.weekday = weekday.map(str::to_owned);
        fields.month = Some(month.into());
        fields.day = Some("numeric".into());
        fields.year = Some(year.into());
    }
    if let Some(style) = &options.time_style {
        fields.hour = Some("numeric".into());
        fields.minute = Some("2-digit".into());
        if style != "short" {
            fields.second = Some("2-digit".into());
        }
        fields.zone_name = match style.as_str() {
            "full" => Some("long".into()),
            "long" => Some("short".into()),
            _ => None,
        };
    }
    fields
}

// ─── Formatting ─────────────────────────────────────────────────────────────────────────────

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

fn named(name: &str, width: &str) -> String {
    match width {
        "long" => name.to_owned(),
        "narrow" => name.chars().take(1).collect(),
        _ => name.chars().take(3).collect(),
    }
}

fn numeric(value: i64, width: &str) -> String {
    if width == "2-digit" {
        format!("{:02}", value.rem_euclid(100))
    } else {
        value.to_string()
    }
}

/// `GMT+8`, `GMT-5:30`, `UTC` — or `GMT+08:00` long.
fn offset_name(offset: i32, long: bool, utc: &str) -> String {
    if offset == 0 {
        return utc.to_owned();
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let (hours, minutes) = (offset.abs() / 60, offset.abs() % 60);
    match (long, minutes) {
        (true, _) => format!("GMT{sign}{hours:02}:{minutes:02}"),
        (false, 0) => format!("GMT{sign}{hours}"),
        (false, _) => format!("GMT{sign}{hours}:{minutes:02}"),
    }
}

fn zone_name(options: &DateOptions, kind: &str, offset: i32, daylight: bool) -> String {
    let info = match &options.zone {
        Zone::Ruled(at) => Some(&ZONES[*at]),
        Zone::Local => ZONES.iter().find(|z| z.name == options.zone_label),
        Zone::Fixed(_) => None,
    };
    if let Some((short_std, short_dst, long_std, long_dst)) = info.and_then(|z| z.names) {
        match kind {
            "short" | "shortGeneric" => {
                return (if daylight { short_dst } else { short_std }).to_owned();
            }
            "long" | "longGeneric" => {
                return (if daylight { long_dst } else { long_std }).to_owned();
            }
            _ => {}
        }
    }
    match kind {
        "long" if offset == 0 && options.zone_label == "UTC" => {
            "Coordinated Universal Time".to_owned()
        }
        "short" | "shortGeneric" => offset_name(
            offset,
            false,
            if options.zone_label == "UTC" {
                "UTC"
            } else {
                "GMT"
            },
        ),
        "shortOffset" => offset_name(offset, false, "GMT"),
        _ => offset_name(offset, true, "GMT"),
    }
}

/// `t` (milliseconds since the epoch) formatted as `options` say, in parts.
fn format_parts(
    options: &DateOptions,
    t: f64,
    context: &mut Context,
) -> JsResult<Vec<(&'static str, String)>> {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return Err(range_error("Invalid time value"));
    }
    let (offset, daylight) = offset_at(&options.zone, t, context)?;
    let local = t + f64::from(offset) * 60_000.;
    let days = (local / 86_400_000.).floor() as i64;
    let (year, month, day) = civil_from_days(days);
    let in_day = local - days as f64 * 86_400_000.;
    let (hour, minute, second, milli) = (
        (in_day / 3_600_000.) as i64,
        (in_day / 60_000.) as i64 % 60,
        (in_day / 1000.) as i64 % 60,
        in_day as i64 % 1000,
    );
    let fields = effective(options);
    let mut parts: Vec<(&'static str, String)> = Vec::new();

    // The date.
    let textual = fields.month.as_deref().is_some_and(|m| TEXT.contains(&m));
    let year_text = fields
        .year
        .as_deref()
        .map(|w| numeric(if year <= 0 { 1 - year } else { year }, w));
    let month_text = fields.month.as_deref().map(|w| {
        if textual {
            named(MONTHS[month as usize - 1], w)
        } else {
            numeric(i64::from(month), w)
        }
    });
    let day_text = fields.day.as_deref().map(|w| numeric(i64::from(day), w));
    let has_date_parts = year_text.is_some() || month_text.is_some() || day_text.is_some();
    if let Some(width) = &fields.weekday {
        parts.push(("weekday", named(WEEKDAYS[weekday(days) as usize], width)));
        if has_date_parts {
            parts.push(("literal", ", ".into()));
        }
    }
    if textual {
        parts.push(("month", month_text.clone().unwrap_or_default()));
        if let Some(day) = &day_text {
            parts.push(("literal", " ".into()));
            parts.push(("day", day.clone()));
        }
        if let Some(year) = &year_text {
            parts.push((
                "literal",
                if day_text.is_some() { ", " } else { " " }.into(),
            ));
            parts.push(("year", year.clone()));
        }
    } else {
        let mut first = true;
        for (kind, text) in [
            ("month", &month_text),
            ("day", &day_text),
            ("year", &year_text),
        ] {
            if let Some(text) = text {
                if !first {
                    parts.push(("literal", "/".into()));
                }
                parts.push((kind, text.clone()));
                first = false;
            }
        }
    }
    if let Some(width) = &fields.era {
        let era = match (year > 0, width.as_str()) {
            (true, "long") => "Anno Domini",
            (false, "long") => "Before Christ",
            (true, "narrow") => "A",
            (false, "narrow") => "B",
            (true, _) => "AD",
            (false, _) => "BC",
        };
        if !parts.is_empty() {
            parts.push(("literal", " ".into()));
        }
        parts.push(("era", era.into()));
    }

    // The time.
    let twelve = matches!(options.hour_cycle.as_str(), "h11" | "h12");
    let has_time = fields.hour.is_some() || fields.minute.is_some() || fields.second.is_some();
    if has_time {
        if !parts.is_empty() {
            let at = matches!(options.date_style.as_deref(), Some("full" | "long"))
                && options.time_style.is_some();
            parts.push(("literal", if at { " at " } else { ", " }.into()));
        }
        let mut first = true;
        if let Some(width) = &fields.hour {
            let shown = match options.hour_cycle.as_str() {
                "h11" => hour % 12,
                "h12" if hour % 12 == 0 => 12,
                "h12" => hour % 12,
                "h24" if hour == 0 => 24,
                _ => hour,
            };
            let pad = width == "2-digit" || !twelve;
            parts.push((
                "hour",
                if pad {
                    format!("{shown:02}")
                } else {
                    shown.to_string()
                },
            ));
            first = false;
        }
        if fields.minute.is_some() {
            if !first {
                parts.push(("literal", ":".into()));
            }
            let pad = !first || fields.minute.as_deref() == Some("2-digit");
            parts.push((
                "minute",
                if pad {
                    format!("{minute:02}")
                } else {
                    minute.to_string()
                },
            ));
            first = false;
        }
        if fields.second.is_some() {
            if !first {
                parts.push(("literal", ":".into()));
            }
            let pad = !first || fields.second.as_deref() == Some("2-digit");
            parts.push((
                "second",
                if pad {
                    format!("{second:02}")
                } else {
                    second.to_string()
                },
            ));
            if let Some(digits) = fields.fractional {
                parts.push(("literal", ".".into()));
                parts.push((
                    "fractionalSecond",
                    format!("{milli:03}")[..digits].to_owned(),
                ));
            }
        }
        if twelve && fields.hour.is_some() {
            // A narrow no-break space before `AM`, as Chromium writes it.
            parts.push(("literal", "\u{202f}".into()));
            parts.push(("dayPeriod", if hour < 12 { "AM" } else { "PM" }.into()));
        }
    }
    if let Some(kind) = &fields.zone_name {
        if !parts.is_empty() {
            parts.push(("literal", " ".into()));
        }
        parts.push(("timeZoneName", zone_name(options, kind, offset, daylight)));
    }
    Ok(parts)
}

/// The time a `format` is given: a `Date`, a number, or now.
fn time_of(value: &JsValue, context: &mut Context) -> JsResult<f64> {
    if value.is_undefined() {
        return Ok(web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0., |d| d.as_millis() as f64));
    }
    value.to_number(context)
}

pub(crate) fn make(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let options = date_options(
        &arg(args, 0),
        &arg(args, 1),
        Defaults::All,
        Defaults::Date,
        context,
    )?;
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        DateFormatData { options },
    ))
}

fn date_format(
    _: &JsValue,
    args: &[JsValue],
    format: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let data: DateFormatData = data_of(&format.clone().into(), "DateTimeFormat.prototype.format")?;
    let t = time_of(&arg(args, 0), context)?;
    Ok(js_text(&joined(&format_parts(&data.options, t, context)?)))
}

/// A range: both ends, apart by an en dash between thin spaces — or one, when they read alike.
fn range_parts(
    options: &DateOptions,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<Vec<(&'static str, String, &'static str)>> {
    if arg(args, 0).is_undefined() || arg(args, 1).is_undefined() {
        return Err(type_error("startDate or endDate is undefined"));
    }
    let start = format_parts(options, time_of(&arg(args, 0), context)?, context)?;
    let end = format_parts(options, time_of(&arg(args, 1), context)?, context)?;
    if joined(&start) == joined(&end) {
        return Ok(start.into_iter().map(|(k, v)| (k, v, "shared")).collect());
    }
    let mut out: Vec<(&'static str, String, &'static str)> = start
        .into_iter()
        .map(|(k, v)| (k, v, "startRange"))
        .collect();
    out.push(("literal", "\u{2009}–\u{2009}".into(), "shared"));
    out.extend(end.into_iter().map(|(k, v)| (k, v, "endRange")));
    Ok(out)
}

pub(crate) const METHODS: Methods = &[
    ("formatToParts", 1, |this, args, cx| {
        let data: DateFormatData = data_of(this, "DateTimeFormat.prototype.formatToParts")?;
        let t = time_of(&arg(args, 0), cx)?;
        let parts = format_parts(&data.options, t, cx)?;
        parts_array(cx, &parts)
    }),
    ("formatRange", 2, |this, args, cx| {
        let data: DateFormatData = data_of(this, "DateTimeFormat.prototype.formatRange")?;
        let parts = range_parts(&data.options, args, cx)?;
        Ok(js_text(
            &parts.iter().map(|(_, v, _)| v.as_str()).collect::<String>(),
        ))
    }),
    ("formatRangeToParts", 2, |this, args, cx| {
        let data: DateFormatData = data_of(this, "DateTimeFormat.prototype.formatRangeToParts")?;
        let parts = range_parts(&data.options, args, cx)?;
        let flat: Vec<(&str, String)> = parts.iter().map(|(k, v, _)| (*k, v.clone())).collect();
        let list = parts_array(cx, &flat)?;
        for (item, (_, _, source)) in crate::native::list_of(&list, cx)?.iter().zip(&parts) {
            if let Some(item) = item.as_object() {
                item.set(boa_engine::js_string!("source"), js_text(source), false, cx)?;
            }
        }
        Ok(list)
    }),
    ("resolvedOptions", 0, |this, _, cx| {
        let data: DateFormatData = data_of(this, "DateTimeFormat.prototype.resolvedOptions")?;
        let o = data.options.clone();
        let text = |s: &Option<String>| s.as_deref().map_or(JsValue::undefined(), js_text);
        let mut fields = vec![
            ("locale", js_text(&o.locale)),
            ("calendar", js_text("gregory")),
            ("numberingSystem", js_text("latn")),
            ("timeZone", js_text(&o.zone_label)),
        ];
        let shown = effective(&o);
        if shown.hour.is_some() {
            fields.push(("hourCycle", js_text(&o.hour_cycle)));
            fields.push((
                "hour12",
                matches!(o.hour_cycle.as_str(), "h11" | "h12").into(),
            ));
        }
        if o.date_style.is_some() || o.time_style.is_some() {
            fields.push(("dateStyle", text(&o.date_style)));
            fields.push(("timeStyle", text(&o.time_style)));
        } else {
            let f = &o.fields;
            // With the hour, minutes and seconds always show two digits.
            let two = |given: &Option<String>| given.as_ref().map(|_| "2-digit".to_owned());
            let (minute, second) = if f.hour.is_some() {
                (two(&f.minute), two(&f.second))
            } else {
                (f.minute.clone(), f.second.clone())
            };
            let hour = if f.hour.is_some() && !matches!(o.hour_cycle.as_str(), "h11" | "h12") {
                Some("2-digit".to_owned())
            } else {
                f.hour.clone()
            };
            fields.extend([
                ("weekday", text(&f.weekday)),
                ("era", text(&f.era)),
                ("year", text(&f.year)),
                ("month", text(&f.month)),
                ("day", text(&f.day)),
                ("hour", text(&hour)),
                ("minute", text(&minute)),
                ("second", text(&second)),
                (
                    "fractionalSecondDigits",
                    f.fractional.map_or(JsValue::undefined(), JsValue::from),
                ),
                ("timeZoneName", text(&f.zone_name)),
            ]);
        }
        record(cx, fields)
    }),
];

pub(crate) const ACCESSORS: Accessors = &[(
    "format",
    |this, _, cx| bound(this, "format", date_format, cx),
    None,
)];

/// `Date.prototype.toLocaleString` (`toLocaleDateString`, `toLocaleTimeString`): the date
/// formatted with the locales and options given, or `Invalid Date`.
pub(crate) fn to_locale(
    this: &JsValue,
    args: &[JsValue],
    defaults: Defaults,
    context: &mut Context,
) -> JsResult<JsValue> {
    let date = this
        .as_object()
        .and_then(|o| JsDate::from_object(o.clone()).ok())
        .ok_or_else(|| type_error("this is not a Date object."))?;
    let t = date.get_time(context)?.to_number(context)?;
    if t.is_nan() {
        return Ok(js_text("Invalid Date"));
    }
    let options = date_options(&arg(args, 0), &arg(args, 1), defaults, defaults, context)?;
    Ok(js_text(&joined(&format_parts(&options, t, context)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_dates_agree() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(weekday(days_from_civil(2024, 1, 5)), 5);
        assert_eq!(nth_sunday(2024, 3, 2), 10);
        assert_eq!(last_sunday(2024, 10), 27);
    }

    #[test]
    fn daylight_rules_turn_on_their_days() {
        let new_york = ZONES.iter().find(|z| z.name == "America/New_York").unwrap();
        assert!(!in_daylight(new_york, at(2024, 3, 10, 6)));
        assert!(in_daylight(new_york, at(2024, 3, 10, 8)));
        assert!(in_daylight(new_york, at(2024, 11, 3, 5)));
        assert!(!in_daylight(new_york, at(2024, 11, 3, 7)));
        let london = ZONES.iter().find(|z| z.name == "Europe/London").unwrap();
        assert!(in_daylight(london, at(2024, 7, 1, 12)));
        assert!(!in_daylight(london, at(2024, 12, 1, 12)));
        let sydney = ZONES.iter().find(|z| z.name == "Australia/Sydney").unwrap();
        assert!(in_daylight(sydney, at(2024, 1, 1, 0)));
        assert!(!in_daylight(sydney, at(2024, 7, 1, 0)));
    }
}
