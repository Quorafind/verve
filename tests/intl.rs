//! `Intl` as plugins use it — English-shaped, without ICU: dates and numbers formatted as
//! Chromium's `en-US` writes them, text sorted as people read it, plurals, relative times,
//! lists, display names, locales, and text cut into graphemes, words and sentences.

use runtime_x::RuntimeX;

/// Each line of `script` (an expression) evaluated, as a string; an error as `ERR <message>`.
fn each(script: &str) -> Vec<String> {
    let mut rt = RuntimeX::new();
    script
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|line| match rt.eval(&format!("String({line})")) {
            Ok(value) => runtime_x::print(&value),
            Err(error) => format!("ERR {error}"),
        })
        .collect()
}

#[test]
fn dates_as_chromium_writes_them_in_english() {
    let results = each(
        r#"
        new Intl.DateTimeFormat('en-US', { timeZone: 'UTC' }).format(new Date(Date.UTC(2024, 0, 5, 13, 4, 5)))
        new Date(Date.UTC(2024, 0, 5, 13, 4, 5)).toLocaleString('en-US', { timeZone: 'UTC' })
        new Date(Date.UTC(2024, 0, 5)).toLocaleDateString('en-US', { timeZone: 'UTC', weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' })
        new Date(Date.UTC(2024, 0, 5, 13, 4, 5)).toLocaleTimeString('en-US', { timeZone: 'UTC', hour12: false })
        new Date(Date.UTC(2024, 0, 5, 9, 4)).toLocaleTimeString('en-US', { timeZone: 'UTC', hour: '2-digit', minute: '2-digit' })
        new Intl.DateTimeFormat('en', { timeZone: 'UTC', dateStyle: 'full', timeStyle: 'short' }).format(Date.UTC(2024, 0, 5, 13, 4))
        new Intl.DateTimeFormat('en', { timeZone: 'UTC', dateStyle: 'medium' }).format(Date.UTC(2024, 0, 5))
        new Intl.DateTimeFormat('en', { timeZone: 'UTC', dateStyle: 'short' }).format(Date.UTC(2024, 0, 5))
        new Intl.DateTimeFormat('en', { timeZone: 'UTC', month: 'long', year: 'numeric' }).format(Date.UTC(2024, 0, 5))
        new Intl.DateTimeFormat('en', { timeZone: 'UTC', weekday: 'short' }).format(Date.UTC(2024, 0, 5))
        new Intl.DateTimeFormat('en', { timeZone: 'America/New_York', hour: 'numeric', minute: '2-digit', timeZoneName: 'short' }).format(Date.UTC(2024, 6, 5, 13, 4))
        new Intl.DateTimeFormat('en', { timeZone: 'Europe/Berlin', hour: 'numeric', hourCycle: 'h23' }).format(Date.UTC(2024, 6, 5, 13))
        new Intl.DateTimeFormat('en', { timeZone: '+05:30', timeStyle: 'long' }).format(Date.UTC(2024, 0, 5, 0, 0))
        new Intl.DateTimeFormat('en', { timeZone: 'UTC' }).formatToParts(0).map(p => p.type).join()
        new Intl.DateTimeFormat('en', { timeZone: 'UTC', fractionalSecondDigits: 2, second: 'numeric', minute: 'numeric', hour: 'numeric' }).format(Date.UTC(2024, 0, 5, 1, 2, 3, 456))
        new Date(NaN).toLocaleDateString()
        new Intl.DateTimeFormat('fr-FR').resolvedOptions().locale
        Intl.DateTimeFormat.supportedLocalesOf(['fr', 'en-GB']).join()
        typeof Intl.DateTimeFormat().resolvedOptions().timeZone
        new Intl.DateTimeFormat('en', { timeZone: 'utc' }).resolvedOptions().timeZone
        new Intl.DateTimeFormat('en', { timeZone: 'Mars/Olympus_Mons' }).resolvedOptions().timeZone
        new Intl.DateTimeFormat('en', { timeZone: 'not a zone' })
        new Intl.DateTimeFormat('en', { dateStyle: 'short', year: 'numeric' })
        new Intl.DateTimeFormat('en', { timeZone: 'UTC' }).format(NaN)
        "#,
    );
    assert_eq!(
        results,
        [
            "1/5/2024",
            "1/5/2024, 1:04:05\u{202f}PM",
            "Friday, January 5, 2024",
            "13:04:05",
            "09:04\u{202f}AM",
            "Friday, January 5, 2024 at 1:04\u{202f}PM",
            "Jan 5, 2024",
            "1/5/24",
            "January 2024",
            "Fri",
            "9:04\u{202f}AM EDT",
            "15",
            "5:30:00\u{202f}AM GMT+5:30",
            "month,literal,day,literal,year",
            "1:02:03.45\u{202f}AM",
            "Invalid Date",
            "en-US",
            "en-GB",
            "string",
            "UTC",
            "Mars/Olympus_Mons",
            "ERR RangeError: Invalid time zone specified: not a zone",
            "ERR TypeError: Can't set option when dateStyle or timeStyle is used",
            "ERR RangeError: Invalid time value",
        ]
    );
}

#[test]
fn numbers_as_english_writes_them() {
    let results = each(
        r#"
        (1234567.891).toLocaleString()
        (1.005).toLocaleString('en', { maximumFractionDigits: 2 })
        (0.256).toLocaleString('en-US', { style: 'percent' })
        new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(-1234.5)
        new Intl.NumberFormat('en-US', { style: 'currency', currency: 'EUR', currencyDisplay: 'code' }).format(3)
        new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD', currencyDisplay: 'name' }).format(1)
        new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD', currencySign: 'accounting' }).format(-5)
        new Intl.NumberFormat('en-US', { style: 'currency', currency: 'JPY' }).format(1234.5)
        new Intl.NumberFormat('en', { notation: 'compact' }).format(1234567)
        new Intl.NumberFormat('en', { notation: 'compact', compactDisplay: 'long' }).format(12300)
        new Intl.NumberFormat('en', { notation: 'compact' }).format(1234)
        new Intl.NumberFormat('en', { notation: 'scientific' }).format(123456)
        new Intl.NumberFormat('en', { notation: 'engineering' }).format(123456)
        new Intl.NumberFormat('en', { style: 'unit', unit: 'megabyte', maximumFractionDigits: 1 }).format(12.345)
        new Intl.NumberFormat('en', { style: 'unit', unit: 'kilometer-per-hour' }).format(50)
        new Intl.NumberFormat('en', { style: 'unit', unit: 'day', unitDisplay: 'long' }).format(2)
        new Intl.NumberFormat('en', { minimumIntegerDigits: 2 }).format(5)
        new Intl.NumberFormat('en', { minimumFractionDigits: 2 }).format(5)
        new Intl.NumberFormat('en', { maximumSignificantDigits: 3 }).format(1234.5)
        new Intl.NumberFormat('en', { signDisplay: 'always' }).format(5)
        new Intl.NumberFormat('en', { signDisplay: 'exceptZero' }).format(0)
        new Intl.NumberFormat('en', { useGrouping: false }).format(12345)
        new Intl.NumberFormat('en').format(NaN) + ' ' + new Intl.NumberFormat('en').format(-Infinity)
        new Intl.NumberFormat('en').formatToParts(-1234.5).map(p => p.type).join()
        new Intl.NumberFormat('en').formatRange(3, 5)
        (12345678901234567890n).toLocaleString()
        [1, 2, 3].map(new Intl.NumberFormat('en', { minimumFractionDigits: 1 }).format).join(' ')
        new Intl.NumberFormat('en', { style: 'currency' })
        new Intl.NumberFormat('en', { style: 'unit', unit: 'parsec' })
        new Intl.NumberFormat('en', { style: 'currency', currency: 'GBP' }).resolvedOptions().maximumFractionDigits
        "#,
    );
    assert_eq!(
        results,
        [
            "1,234,567.891",
            "1.01",
            "26%",
            "-$1,234.50",
            "EUR\u{a0}3.00",
            "1.00 US dollars",
            "($5.00)",
            "¥1,235",
            "1.2M",
            "12 thousand",
            "1.2K",
            "1.235E5",
            "123.456E3",
            "12.3 MB",
            "50 km/h",
            "2 days",
            "05",
            "5.00",
            "1,230",
            "+5",
            "0",
            "12345",
            "NaN -∞",
            "minusSign,integer,group,integer,decimal,fraction",
            "3–5",
            "12,345,678,901,234,567,890",
            "1.0 2.0 3.0",
            "ERR TypeError: Currency code is required with currency style.",
            "ERR RangeError: Invalid unit argument for Intl.NumberFormat() 'parsec'",
            "2",
        ]
    );
}

#[test]
fn text_sorts_plurals_relative_times_lists_and_names() {
    let results = each(
        r#"
        ['b', 'a', 'C', 'é', 'A'].sort(new Intl.Collator().compare).join()
        ['item10', 'item9', 'item1'].sort(new Intl.Collator(undefined, { numeric: true }).compare).join()
        ['b', 'a'].sort(Intl.Collator('en', { sensitivity: 'base' }).compare).join()
        'a'.localeCompare('B')
        'résumé'.localeCompare('resume', undefined, { sensitivity: 'base' })
        new Intl.Collator('en', { sensitivity: 'base' }).resolvedOptions().sensitivity
        ['one', 'two'].map(n => new Intl.PluralRules('en').select(n === 'one' ? 1 : 2)).join()
        [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101].map(n => new Intl.PluralRules('en', { type: 'ordinal' }).select(n)).join()
        new Intl.PluralRules('en', { minimumFractionDigits: 1 }).select(1)
        new Intl.RelativeTimeFormat('en', { numeric: 'auto' }).format(-1, 'day')
        new Intl.RelativeTimeFormat('en', { numeric: 'auto' }).format(1, 'week')
        new Intl.RelativeTimeFormat('en').format(3, 'hours')
        new Intl.RelativeTimeFormat('en').format(-1, 'minute')
        new Intl.RelativeTimeFormat('en', { style: 'short' }).format(-2, 'month')
        new Intl.RelativeTimeFormat('en').formatToParts(10, 'seconds').map(p => p.type + (p.unit ? ':' + p.unit : '')).join()
        new Intl.RelativeTimeFormat('en').format(1, 'fortnight')
        new Intl.ListFormat('en').format(['a', 'b', 'c'])
        new Intl.ListFormat('en').format(['a', 'b'])
        new Intl.ListFormat('en', { type: 'disjunction' }).format(['a', 'b', 'c'])
        new Intl.ListFormat('en', { style: 'short' }).format(['a', 'b', 'c'])
        new Intl.ListFormat('en', { type: 'unit', style: 'narrow' }).format(['5 ft', '2 in'])
        new Intl.DisplayNames(['en'], { type: 'language' }).of('fr-CA')
        new Intl.DisplayNames(['en'], { type: 'region' }).of('jp')
        new Intl.DisplayNames(['en'], { type: 'currency' }).of('EUR')
        new Intl.DisplayNames(['en'], { type: 'region', fallback: 'none' }).of('ZZ')
        new Intl.DisplayNames(['en'], { type: 'script' }).of('Cyrl')
        "#,
    );
    assert_eq!(
        results,
        [
            "a,A,b,C,é",
            "item1,item9,item10",
            "a,b",
            "-1",
            "0",
            "base",
            "one,other",
            "one,two,few,other,other,other,other,one,two,few,one",
            "other",
            "yesterday",
            "next week",
            "in 3 hours",
            "1 minute ago",
            "2 mo. ago",
            "literal,integer:second,literal",
            "ERR RangeError: Invalid unit argument for format() 'fortnight'",
            "a, b, and c",
            "a and b",
            "a, b, or c",
            "a, b, & c",
            "5 ft 2 in",
            "French (Canada)",
            "Japan",
            "Euro",
            "undefined",
            "Cyrillic",
        ]
    );
}

#[test]
fn locales_and_segments() {
    let results = each(
        r#"
        new Intl.Locale('zh-hant-tw').baseName
        new Intl.Locale('zh').maximize().toString()
        new Intl.Locale('en-Latn-US').minimize().toString()
        new Intl.Locale('en-US', { hourCycle: 'h23' }).toString()
        new Intl.Locale('en-US-u-ca-gregory').calendar
        new Intl.Locale('ar').textInfo.direction
        new Intl.Locale('en-US').weekInfo.firstDay + ' ' + new Intl.Locale('de-DE').getWeekInfo().firstDay
        Intl.getCanonicalLocales(['EN-us', 'zh_hans_cn']).join()
        Intl.getCanonicalLocales('not a locale!')
        Intl.supportedValuesOf('currency').includes('USD')
        [...new Intl.Segmenter(void 0, { granularity: 'grapheme' }).segment('a👍🏽é🇯🇵')].map(s => s.segment + '@' + s.index).join('|')
        [...new Intl.Segmenter('en', { granularity: 'word' }).segment('Hi, you two.')].filter(s => s.isWordLike).map(s => s.segment).join('|')
        Array.from(new Intl.Segmenter().segment('xyz'), ({ segment }) => segment).length
        new Intl.Segmenter('en', { granularity: 'grapheme' }).segment('xy')[Symbol.iterator]().next().value.segment
        new Intl.Segmenter('en', { granularity: 'sentence' }).segment('One. Two! Three?').containing(6).segment
        new Intl.Segmenter('en', { granularity: 'word' }).segment('abc def').containing(99)
        new Intl.Segmenter().resolvedOptions().granularity
        Object.prototype.toString.call(new Intl.Segmenter())
        typeof Intl.Segmenter.supportedLocalesOf
        "#,
    );
    assert_eq!(
        results,
        [
            "zh-Hant-TW",
            "zh-Hans-CN",
            "en",
            "en-US-u-hc-h23",
            "gregory",
            "rtl",
            "7 1",
            "en-US,zh-Hans-CN",
            "ERR RangeError: Incorrect locale information provided: not a locale!",
            "true",
            "a@0|👍🏽@1|é@5|🇯🇵@6",
            "Hi|you|two",
            "3",
            "x",
            "Two! ",
            "undefined",
            "grapheme",
            "[object Intl.Segmenter]",
            "function",
        ]
    );
}
