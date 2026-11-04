#![allow(clippy::redundant_clone)]

use std::hash::{BuildHasher, BuildHasherDefault, Hash};

use crate::{
    CodePoint, CommonJsStringBuilder, JsStr, JsString, JsStringKind, Latin1JsStringBuilder,
    ROPE_MIN_LEN, StaticJsStrings, StaticString, Utf16JsStringBuilder,
};

use rustc_hash::FxHasher;

fn hash_value<T: Hash>(value: &T) -> u64 {
    BuildHasherDefault::<FxHasher>::default().hash_one(value)
}

const fn ascii_to_utf16<const LEN: usize>(ascii: &[u8; LEN]) -> [u16; LEN] {
    let mut array = [0; LEN];
    let mut i = 0;
    while i < LEN {
        array[i] = ascii[i] as u16;
        i += 1;
    }
    array
}

#[test]
fn empty() {
    let s = StaticJsStrings::EMPTY_STRING;
    assert_eq!(&s, &[]);
}

#[test]
fn refcount() {
    let x = JsString::from("Hello world");
    assert_eq!(x.refcount(), Some(1));

    {
        let y = x.clone();
        assert_eq!(x.refcount(), Some(2));
        assert_eq!(y.refcount(), Some(2));

        {
            let z = y.clone();
            assert_eq!(x.refcount(), Some(3));
            assert_eq!(y.refcount(), Some(3));
            assert_eq!(z.refcount(), Some(3));
        }

        assert_eq!(x.refcount(), Some(2));
        assert_eq!(y.refcount(), Some(2));
    }

    assert_eq!(x.refcount(), Some(1));
}

#[test]
fn static_refcount() {
    let x = StaticJsStrings::EMPTY_STRING;
    assert_eq!(x.refcount(), None);

    {
        let y = x.clone();
        assert_eq!(x.refcount(), None);
        assert_eq!(y.refcount(), None);
    };

    assert_eq!(x.refcount(), None);
}

#[test]
fn ptr_eq() {
    let x = JsString::from("Hello");
    let y = x.clone();

    assert!(!x.is_static());

    assert_eq!(x.ptr.addr(), y.ptr.addr());

    let z = JsString::from("Hello");
    assert_ne!(x.ptr.addr(), z.ptr.addr());
    assert_ne!(y.ptr.addr(), z.ptr.addr());
}

#[test]
fn static_ptr_eq() {
    let x = StaticJsStrings::EMPTY_STRING;
    let y = x.clone();

    assert!(x.is_static());

    assert_eq!(x.ptr.addr(), y.ptr.addr());

    let z = StaticJsStrings::EMPTY_STRING;
    assert_eq!(x.ptr.addr(), z.ptr.addr());
    assert_eq!(y.ptr.addr(), z.ptr.addr());
}

#[test]
fn as_str() {
    const HELLO: &[u16] = &ascii_to_utf16(b"Hello");
    let x = JsString::from(HELLO);

    assert_eq!(&x, HELLO);
}

#[test]
fn hash() {
    const HELLOWORLD: JsStr<'_> = JsStr::latin1("Hello World!".as_bytes());
    let x = JsString::from(HELLOWORLD);

    assert_eq!(x.as_str(), HELLOWORLD);

    assert!(HELLOWORLD.is_latin1());
    assert!(x.as_str().is_latin1());

    let s_hash = hash_value(&HELLOWORLD);
    let x_hash = hash_value(&x);

    assert_eq!(s_hash, x_hash);
}

#[test]
fn concat() {
    const Y: &[u16] = &ascii_to_utf16(b", ");
    const W: &[u16] = &ascii_to_utf16(b"!");

    let x = JsString::from("hello");
    let z = JsString::from("world");

    let xy = JsString::concat(x.as_str(), JsString::from(Y).as_str());
    assert_eq!(&xy, &ascii_to_utf16(b"hello, "));
    assert_eq!(xy.refcount(), Some(1));

    let xyz = JsString::concat(xy.as_str(), z.as_str());
    assert_eq!(&xyz, &ascii_to_utf16(b"hello, world"));
    assert_eq!(xyz.refcount(), Some(1));

    let xyzw = JsString::concat(xyz.as_str(), JsString::from(W).as_str());
    assert_eq!(&xyzw, &ascii_to_utf16(b"hello, world!"));
    assert_eq!(xyzw.refcount(), Some(1));
}

#[test]
fn trim_start_non_ascii_to_ascii() {
    let s = "\u{2029}abc";
    let x = JsString::from(s);

    let y = x.trim_start();

    assert_eq!(&y, s.trim_start());
}

#[test]
fn conversion_to_known_static_js_string() {
    const JS_STR_U8: &JsStr<'_> = &JsStr::latin1("length".as_bytes());
    const JS_STR_U16: &JsStr<'_> = &JsStr::utf16(&ascii_to_utf16(b"length"));

    assert!(JS_STR_U8.is_latin1());
    assert!(!JS_STR_U16.is_latin1());

    assert_eq!(JS_STR_U8, JS_STR_U8);
    assert_eq!(JS_STR_U16, JS_STR_U16);

    assert_eq!(JS_STR_U8, JS_STR_U16);
    assert_eq!(JS_STR_U16, JS_STR_U8);

    assert_eq!(hash_value(JS_STR_U8), hash_value(JS_STR_U16));

    let string = StaticJsStrings::get_string(JS_STR_U8);

    assert!(string.is_some());
    assert!(string.unwrap().as_str().is_latin1());

    let string = StaticJsStrings::get_string(JS_STR_U16);

    assert!(string.is_some());
    assert!(string.unwrap().as_str().is_latin1());
}

#[test]
fn to_std_string_escaped() {
    assert_eq!(
        JsString::from("Hello, \u{1D49E} world!").to_std_string_escaped(),
        "Hello, \u{1D49E} world!"
    );

    assert_eq!(
        JsString::from("Hello, world!").to_std_string_escaped(),
        "Hello, world!"
    );

    // 15 should not be escaped.
    let unpaired_surrogates: [u16; 3] = [0xDC58, 0xD83C, 0x0015];
    assert_eq!(
        JsString::from(&unpaired_surrogates).to_std_string_escaped(),
        "\\uDC58\\uD83C\u{15}"
    );
}

#[test]
fn from_static_js_string() {
    static STATIC_HELLO_WORLD: StaticString =
        StaticString::new(JsStr::latin1("hello world".as_bytes()));
    static STATIC_EMOJIS: StaticString = StaticString::new(JsStr::utf16(&[
        0xD83C, 0xDFB9, 0xD83C, 0xDFB6, 0xD83C, 0xDFB5,
    ])); // 🎹🎶🎵

    let latin1 = JsString::from_static(&STATIC_HELLO_WORLD);
    let utf16 = JsString::from_static(&STATIC_EMOJIS);

    // content compare
    assert_eq!(latin1, "hello world");
    assert_eq!(utf16, "🎹🎶🎵");

    // refcount check
    let clone = latin1.clone();

    assert_eq!(clone, latin1);

    let clone = utf16.clone();

    assert_eq!(clone, utf16);

    assert!(latin1.refcount().is_none());
    assert!(utf16.refcount().is_none());

    // `is_latin1` check
    assert!(latin1.as_str().is_latin1());
    assert!(!utf16.as_str().is_latin1());
}

#[test]
fn compare_static_and_dynamic_js_string() {
    static STATIC_HELLO_WORLD: StaticString =
        StaticString::new(JsStr::latin1("hello world".as_bytes()));
    static STATIC_EMOJIS: StaticString = StaticString::new(JsStr::utf16(&[
        0xD83C, 0xDFB9, 0xD83C, 0xDFB6, 0xD83C, 0xDFB5,
    ])); // 🎹🎶🎵

    let static_latin1 = JsString::from_static(&STATIC_HELLO_WORLD);
    let static_utf16 = JsString::from_static(&STATIC_EMOJIS);

    let dynamic_latin1 = JsString::from(JsStr::latin1("hello world".as_bytes()));
    let dynamic_utf16 = JsString::from(&[0xD83C, 0xDFB9, 0xD83C, 0xDFB6, 0xD83C, 0xDFB5]);

    // content compare
    assert_eq!(static_latin1, dynamic_latin1);
    assert_eq!(static_utf16, dynamic_utf16);

    // length check
    assert_eq!(static_latin1.len(), dynamic_latin1.len());
    assert_eq!(static_utf16.len(), dynamic_utf16.len());

    // `is_static` check
    assert!(static_latin1.is_static());
    assert!(static_utf16.is_static());
    assert!(!dynamic_latin1.is_static());
    assert!(!dynamic_utf16.is_static());
}

#[test]
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::undocumented_unsafe_blocks)]
fn js_string_builder() {
    let s = "2024年5月21日";
    let utf16 = s.encode_utf16().collect::<Vec<_>>();
    let s_utf16 = utf16.as_slice();
    let ascii = "Lorem ipsum dolor sit amet";
    let s_ascii = ascii.as_bytes();
    let latin1_as_utf8_literal = "Déjà vu";
    let s_latin1_literal: &[u8] = &[
        b'D', 0xE9, /* é */
        b'j', 0xE0, /* à */
        b' ', b'v', b'u',
    ];

    // latin1 builder -- test

    // push ascii
    let mut builder = Latin1JsStringBuilder::new();
    for &code in s_ascii {
        builder.push(code);
    }
    let s_builder = builder.build().unwrap_or_default();
    assert_eq!(s_builder, ascii);

    // push latin1
    let mut builder = Latin1JsStringBuilder::new();
    for &code in s_latin1_literal {
        builder.push(code);
    }
    let s_builder = unsafe { builder.build_as_latin1() };
    assert_eq!(
        s_builder.to_std_string().unwrap_or_default(),
        latin1_as_utf8_literal
    );

    // from_iter ascii
    let s_builder = s_ascii
        .iter()
        .copied()
        .collect::<Latin1JsStringBuilder>()
        .build()
        .unwrap_or_default();
    assert_eq!(s_builder.to_std_string().unwrap_or_default(), ascii);

    // from_iter latin1
    let s_builder = unsafe {
        s_latin1_literal
            .iter()
            .copied()
            .collect::<Latin1JsStringBuilder>()
            .build_as_latin1()
    };
    assert_eq!(
        s_builder.to_std_string().unwrap_or_default(),
        latin1_as_utf8_literal
    );

    // extend_from_slice ascii
    let mut builder = Latin1JsStringBuilder::new();
    builder.extend_from_slice(s_ascii);
    let s_builder = builder.build().unwrap_or_default();
    assert_eq!(s_builder.to_std_string().unwrap_or_default(), ascii);

    // extend_from_slice latin1
    let mut builder = Latin1JsStringBuilder::new();
    builder.extend_from_slice(s_latin1_literal);
    let s_builder = unsafe { builder.build_as_latin1() };
    assert_eq!(
        s_builder.to_std_string().unwrap_or_default(),
        latin1_as_utf8_literal
    );

    // build from utf16 encoded string
    let s_builder = s
        .as_bytes()
        .iter()
        .copied()
        .collect::<Latin1JsStringBuilder>()
        .build();
    assert_eq!(None, s_builder);

    let s_builder = s_utf16
        .iter()
        .copied()
        .map(|v| v as u8)
        .collect::<Latin1JsStringBuilder>()
        .build();
    assert_eq!(None, s_builder);

    // utf16 builder -- test

    // push
    let mut builder = Utf16JsStringBuilder::new();
    for &code in s_utf16 {
        builder.push(code);
    }
    let s_builder = builder.build();
    assert_eq!(s_builder.to_std_string().unwrap_or_default(), s);

    // from_iter
    let s_builder = s_utf16
        .iter()
        .copied()
        .collect::<Utf16JsStringBuilder>()
        .build();
    assert_eq!(s_builder.to_std_string().unwrap_or_default(), s);

    // extend_from_slice
    let mut builder = Utf16JsStringBuilder::new();
    builder.extend_from_slice(s_utf16);
    let s_builder = builder.build();
    assert_eq!(s_builder.to_std_string().unwrap_or_default(), s);
}

#[test]
fn clone_builder() {
    // latin1 builder -- test
    let origin = Latin1JsStringBuilder::from(&b"0123456789"[..]);
    let empty_origin = Latin1JsStringBuilder::new();

    // clone == origin
    let cloned = origin.clone();
    assert_eq!(origin, cloned);

    // clone_from == origin
    let mut cloned_from = Latin1JsStringBuilder::new();
    cloned_from.clone_from(&origin);
    assert_eq!(origin, cloned_from);

    // clone == origin(empty)
    let cloned = empty_origin.clone();
    assert_eq!(empty_origin, cloned);

    // clone_from == origin(empty)

    cloned_from.clone_from(&empty_origin);
    assert!(cloned_from.capacity() > 0); // Should not be reallocated so the capacity is preserved.
    assert_eq!(empty_origin, cloned_from);

    // clone_from(empty) == origin(empty)
    let mut cloned_from = Latin1JsStringBuilder::new();
    cloned_from.clone_from(&empty_origin);
    assert_eq!(cloned_from.capacity(), 0);
    assert_eq!(empty_origin, cloned_from);

    // utf16 builder -- test
    let s = "2024年5月21日";

    let origin = Utf16JsStringBuilder::from(s.encode_utf16().collect::<Vec<_>>().as_slice());
    let empty_origin = Utf16JsStringBuilder::new();
    // clone == origin
    let cloned = origin.clone();
    assert_eq!(origin, cloned);

    // clone_from == origin(empty)
    let mut cloned_from = Utf16JsStringBuilder::new();
    cloned_from.clone_from(&origin);

    assert_eq!(origin, cloned_from);
    // clone == origin(empty)
    let cloned = empty_origin.clone();
    assert_eq!(empty_origin, cloned);

    // clone_from == origin(empty)

    cloned_from.clone_from(&empty_origin);
    assert!(cloned_from.capacity() > 0); // should not be reallocated so the capacity is preserved.
    assert_eq!(empty_origin, cloned_from);

    // clone_from(empty) == origin(empty)
    let mut cloned_from = Utf16JsStringBuilder::new();
    cloned_from.clone_from(&empty_origin);
    assert_eq!(cloned_from.capacity(), 0);
    assert_eq!(empty_origin, cloned_from);
}

#[test]
fn common_js_string_builder() {
    let utf16 = "2024年5月21日".encode_utf16().collect::<Vec<_>>();
    let s_utf16 = utf16.as_slice();
    let s = "Lorem ipsum dolor sit amet";
    let js_str_utf16 = JsStr::utf16(s_utf16);
    let js_str_ascii = JsStr::latin1(s.as_bytes());
    let latin1_bytes = [
        b'D', 0xE9, /* é */
        b'j', 0xE0, /* à */
        b' ', b'v', b'u',
    ];
    let ch = '🎹';
    let mut builder = CommonJsStringBuilder::with_capacity(10);
    builder += ch;
    builder += s;
    builder += js_str_utf16;
    builder += js_str_ascii;
    builder += ch;
    assert_eq!(builder.len(), 5);
    let js_string = builder.build_from_utf16();
    assert_eq!(
        js_string,
        "🎹Lorem ipsum dolor sit amet2024年5月21日Lorem ipsum dolor sit amet🎹"
    );
    let mut builder = CommonJsStringBuilder::new();
    for b in latin1_bytes {
        builder += b;
    }
    builder += s_utf16;
    builder += ch;
    let js_string = builder.build();
    assert_eq!(
        js_string.to_std_string().unwrap_or_default(),
        "Déjà vu2024年5月21日🎹"
    );
}

#[test]
fn code_points_optimization() {
    // Test Latin1 optimization with extended Latin1 characters
    let latin1_str = JsStr::latin1(b"Caf\xe9 na\xefve"); // "Café naïve" in Latin1 encoding
    let latin1_points: Vec<CodePoint> = latin1_str.code_points().collect();
    let expected_latin1: Vec<CodePoint> = "Café naïve".chars().map(CodePoint::Unicode).collect();
    assert_eq!(latin1_points, expected_latin1);

    // Test UTF-16 behavior unchanged (including non-ASCII)
    let utf16_str = JsStr::utf16(&[
        0x0043, 0x0061, 0x0066, 0x00E9, // "Café"
        0x0020, // space
        0x006E, 0x0061, 0x00EF, 0x0076, 0x0065, // "naïve"
    ]);
    let utf16_points: Vec<CodePoint> = utf16_str.code_points().collect();
    assert_eq!(latin1_points, utf16_points); // Same result for same content
}

#[test]
fn slice() {
    let sliced = {
        let base_str = JsString::from("Hello World");
        assert_eq!(base_str.kind(), JsStringKind::Latin1Sequence);

        base_str.slice(1, 5)
    };
    assert_eq!(sliced, JsString::from("ello"));
    assert_eq!(sliced.kind(), JsStringKind::Slice);

    let sliced2 = sliced.slice(1, 3);
    drop(sliced);
    assert_eq!(sliced2, JsString::from("ll"));
    assert_eq!(sliced2.kind(), JsStringKind::Slice);

    let sliced3 = sliced2.slice(0, 2);
    drop(sliced2);
    assert_eq!(sliced3, JsString::from("ll"));
    assert_eq!(sliced3.kind(), JsStringKind::Slice);

    let sliced4 = sliced3.slice(0, 2);
    drop(sliced3);
    assert_eq!(sliced4, JsString::from("ll"));
    assert_eq!(sliced4.kind(), JsStringKind::Slice);

    let sliced4 = sliced4.slice(0, 2);
    assert_eq!(sliced4, JsString::from("ll"));
    assert_eq!(sliced4.kind(), JsStringKind::Slice);

    let sliced5 = sliced4.slice(1, 1);
    assert_eq!(sliced5, JsString::from(""));
    assert_eq!(sliced5.kind(), JsStringKind::Static);

    assert_eq!(sliced5.slice(4, 4), StaticJsStrings::EMPTY_STRING);
}

#[test]
fn split() {
    let base_str = JsString::from("Hello World");
    assert_eq!(base_str.kind(), JsStringKind::Latin1Sequence);

    let str1 = base_str.slice(0, 5);
    let str2 = base_str.slice(6, base_str.len());

    assert_eq!(str1, JsString::from("Hello"));
    assert_eq!(str2, JsString::from("World"));

    let str3 = str1.clone();
    drop(str1);
    assert_eq!(str3, JsString::from("Hello"));
    drop(base_str);
    assert_eq!(str3, JsString::from("Hello"));
}

#[test]
fn trim() {
    // Very basic test for trimming. The extensive testing is done by `boa_engine`.
    let base_str = JsString::from(" \u{000B} Hello World \t ");
    assert_eq!(base_str.trim(), JsString::from("Hello World"));
}

#[test]
fn starts_with_and_ends_with_basic() {
    let basic = JsString::from("abcdef");
    let start_needle = JsStr::latin1("abc".as_bytes());
    assert!(basic.starts_with(start_needle));
    assert!(!basic.ends_with(start_needle));

    let end_needle = JsStr::latin1("def".as_bytes());
    assert!(!basic.starts_with(end_needle));
    assert!(basic.ends_with(end_needle));
}

#[test]
fn index_of_starts_at_from_index() {
    let haystack = JsString::from("aXbXc");
    let needle = JsStr::latin1(b"X");
    assert_eq!(haystack.index_of(needle, 0), Some(1));
    assert_eq!(haystack.index_of(needle, 2), Some(3));
    assert_eq!(haystack.index_of(needle, 4), None);
    assert_eq!(haystack.index_of(needle, 5), None);

    // A needle longer than what is left has no candidate, whatever the index.
    let longer = JsStr::latin1(b"XbXcX");
    assert_eq!(haystack.index_of(longer, 0), None);
    assert_eq!(haystack.index_of(longer, 1), None);

    // An empty needle is found at the index itself, up to the length.
    let empty = JsStr::latin1(b"");
    assert_eq!(haystack.index_of(empty, 3), Some(3));
    assert_eq!(haystack.index_of(empty, 5), Some(5));
    assert_eq!(haystack.index_of(empty, 6), None);
}

#[test]
fn index_of_compares_utf16_and_latin1_by_code_unit() {
    // `µµµx` fits Latin-1, so both this haystack and its needle are Latin-1.
    let latin1 = JsString::from("µµµx");
    let narrow_needle = JsStr::latin1(b"x");
    assert_eq!(latin1.index_of(narrow_needle, 0), Some(3));
    assert_eq!(latin1.index_of(narrow_needle, 3), Some(3));
    assert_eq!(latin1.index_of(narrow_needle, 4), None);

    // A haystack and a needle in different encodings are compared code unit by code unit.
    let narrow = JsString::from("abc");
    let wide_needle = JsStr::utf16(&[0x0062, 0x0063]);
    assert_eq!(narrow.index_of(wide_needle, 0), Some(1));
    assert_eq!(narrow.index_of(wide_needle, 2), None);
    let wide = JsString::from("µµµx");
    assert_eq!(wide.index_of(narrow_needle, 0), Some(3));
    assert_eq!(wide.index_of(narrow_needle, 4), None);

    // A surrogate pair is two code units in either encoding of the needle.
    let emoji = JsString::from("😀😀x");
    let pair = JsStr::utf16(&[0xD83D, 0xDE00]);
    assert_eq!(emoji.index_of(pair, 0), Some(0));
    assert_eq!(emoji.index_of(pair, 1), Some(2));
    assert_eq!(emoji.index_of(JsStr::latin1(b"x"), 0), Some(4));
}

/// Two strings with the same text are not the same allocation, which is what `ptr_eq` says.
#[test]
fn equal_strings_from_different_allocations_are_not_one_string() {
    let first = JsString::from("µµµx");
    let second = JsString::from("µµµx");

    assert_eq!(first, second);
    assert!(!first.ptr_eq(&second));
    assert!(first.ptr_eq(&first.clone()));
}

#[test]
fn ptr_eq_tells_one_allocation_from_an_equal_string() {
    let x = JsString::from("Hello");
    let clone = x.clone();
    let equal = JsString::from("Hello");

    assert!(x.ptr_eq(&clone));
    assert!(!x.ptr_eq(&equal));
}

#[test]
fn concat_with_an_int32() {
    let text = JsString::from("ab");
    for n in [0, 7, -1, 12345, i32::MAX, i32::MIN] {
        assert_eq!(
            JsString::concat_i32(&text, n),
            JsString::from(format!("ab{n}"))
        );
        assert_eq!(
            JsString::concat_i32_before(n, &text),
            JsString::from(format!("{n}ab"))
        );
    }
    let wide = JsString::from(&[0x20ac_u16, 0x61][..]);
    assert_eq!(
        JsString::concat_i32(&wide, 42).to_vec(),
        [0x20ac, 0x61, 0x34, 0x32]
    );
    assert_eq!(
        JsString::concat_i32_before(42, &wide).to_vec(),
        [0x34, 0x32, 0x20ac, 0x61]
    );
    let empty = JsString::default();
    assert_eq!(JsString::concat_i32(&empty, -5), JsString::from("-5"));
    // Around the length at which the result is a rope.
    for len in [
        ROPE_MIN_LEN - 3,
        ROPE_MIN_LEN - 2,
        ROPE_MIN_LEN - 1,
        ROPE_MIN_LEN,
    ] {
        let base = JsString::from("x".repeat(len - 2));
        let want = format!("{}12", "x".repeat(len - 2));
        assert_eq!(
            JsString::concat_i32(&base, 12).to_std_string().unwrap(),
            want
        );
        let want = format!("12{}", "x".repeat(len - 2));
        assert_eq!(
            JsString::concat_i32_before(12, &base)
                .to_std_string()
                .unwrap(),
            want
        );
    }
}

mod rope {
    use crate::vtable::RopeString;
    use crate::{JsString, JsStringKind, ROPE_MIN_LEN};

    /// Chain depth of the deep tests: a million, or little enough for Miri to finish.
    const DEEP: usize = if cfg!(miri) { 2_000 } else { 1_000_000 };

    fn latin1(n: usize, c: char) -> JsString {
        JsString::from(c.to_string().repeat(n))
    }

    fn utf16(n: usize, unit: u16) -> JsString {
        JsString::from(vec![unit; n].as_slice())
    }

    fn data_ptr(s: &JsString) -> *const u8 {
        match s.variant() {
            crate::JsStrVariant::Latin1(b) => b.as_ptr(),
            crate::JsStrVariant::Utf16(u) => u.as_ptr().cast(),
        }
    }

    fn is_flat(s: &JsString) -> bool {
        RopeString::flat_of(s).is_some()
    }

    #[test]
    fn node_size() {
        // Reported in docs/electron/size.md; a bigger node costs every append that much more.
        assert_eq!(size_of::<RopeString>(), 96);
    }

    #[test]
    fn short_results_are_copied() {
        let s = JsString::concat_lazy(&latin1(10, 'a'), &latin1(10, 'b'));
        assert_eq!(s.kind(), JsStringKind::Latin1Sequence);
        let s = JsString::concat_lazy(&latin1(ROPE_MIN_LEN - 2, 'a'), &latin1(1, 'b'));
        assert_eq!(s.kind(), JsStringKind::Latin1Sequence);
        let s = JsString::concat_lazy(&latin1(ROPE_MIN_LEN, 'a'), &latin1(1, 'b'));
        assert_eq!(s.kind(), JsStringKind::Rope);
    }

    #[test]
    fn empty_sides_return_the_other() {
        let a = latin1(300, 'a');
        let e = JsString::default();
        assert!(JsString::concat_lazy(&a, &e).ptr_eq(&a));
        assert!(JsString::concat_lazy(&e, &a).ptr_eq(&a));
        assert!(JsString::concat_lazy(&e, &e).is_empty());
    }

    #[test]
    fn latin1_content_and_width() {
        let (a, b) = (latin1(200, 'a'), latin1(100, '\u{e9}'));
        let s = JsString::concat_lazy(&a, &b);
        assert_eq!(s.len(), 300);
        assert!(!is_flat(&s));
        assert!(s.as_str().is_latin1());
        assert_eq!(
            s.to_std_string().unwrap(),
            format!("{}{}", "a".repeat(200), "\u{e9}".repeat(100))
        );
        assert!(is_flat(&s));
    }

    #[test]
    fn mixed_width_content() {
        let a = latin1(200, '\u{ff}');
        let b = utf16(100, 0x20ac);
        let s = JsString::concat_lazy(&a, &b);
        assert!(!s.as_str().is_latin1());
        let t = JsString::concat_lazy(&b, &a);
        let mut want = vec![0xffu16; 200];
        want.extend(vec![0x20ac; 100]);
        assert_eq!(s.to_vec(), want);
        want.rotate_left(200);
        assert_eq!(t.to_vec(), want);
    }

    #[test]
    fn lone_surrogates_at_the_boundary() {
        let hi = JsString::from(&[&[0x61u16; 300][..], &[0xD83D]].concat()[..]);
        let lo = JsString::from(&[0xDE00u16][..]);
        let s = JsString::concat_lazy(&hi, &lo);
        // The halves join into a valid pair only once flattened.
        assert_eq!(s.code_points().count(), 301);
        assert_eq!(s.code_point_at(300).as_char(), Some('\u{1F600}'));
    }

    #[test]
    fn len_does_not_flatten() {
        let s = JsString::concat_lazy(&latin1(300, 'a'), &latin1(5, 'b'));
        let t = JsString::concat_lazy(&s, &latin1(5, 'c'));
        assert_eq!(t.len(), 310);
        assert!(!t.is_empty());
        let _clone = t.clone();
        assert!(!is_flat(&t));
        assert!(!is_flat(&s));
    }

    #[test]
    fn flattens_once() {
        let s = JsString::concat_lazy(&latin1(300, 'a'), &latin1(5, 'b'));
        let first = data_ptr(&s);
        assert!(is_flat(&s));
        assert_eq!(data_ptr(&s), first);
        assert_eq!(data_ptr(&s.clone()), first);
    }

    #[test]
    fn ptr_eq_and_refcount() {
        let s = JsString::concat_lazy(&latin1(300, 'a'), &latin1(5, 'b'));
        assert_eq!(s.refcount(), Some(1));
        let c = s.clone();
        assert!(c.ptr_eq(&s));
        assert_eq!(s.refcount(), Some(2));
        drop(c);
        assert_eq!(s.refcount(), Some(1));
    }

    #[test]
    fn halves_stay_shared() {
        let a = latin1(300, 'a');
        let s = JsString::concat_lazy(&a, &latin1(5, 'b'));
        assert_eq!(a.refcount(), Some(2));
        let _ = s.as_str();
        // Flattening releases the halves.
        assert_eq!(a.refcount(), Some(1));
    }

    #[test]
    fn slice_and_clone_after_flatten() {
        let s = JsString::concat_lazy(&latin1(300, 'a'), &latin1(300, 'b'));
        let sub = s.slice(295, 305);
        assert_eq!(sub.to_std_string().unwrap(), "aaaaabbbbb");
        assert_eq!(sub.kind(), JsStringKind::Slice);
        drop(s);
        assert_eq!(sub.to_std_string().unwrap(), "aaaaabbbbb");
    }

    #[test]
    fn rope_of_ropes_and_dag() {
        let s = JsString::concat_lazy(&latin1(300, 'a'), &latin1(1, 'b'));
        let t = JsString::concat_lazy(&s, &s);
        let u = JsString::concat_lazy(&t, &utf16(1, 0x100));
        assert_eq!(u.len(), 603);
        let mut want = "a".repeat(300);
        want.push('b');
        let mut units: Vec<u16> = want.encode_utf16().collect();
        units.extend(units.clone());
        units.push(0x100);
        assert_eq!(u.to_vec(), units);
    }

    #[test]
    fn deep_left_chain() {
        let mut s = latin1(ROPE_MIN_LEN, 'x');
        let piece = latin1(1, 'y');
        for _ in 0..DEEP {
            s = JsString::concat_lazy(&s, &piece);
        }
        assert_eq!(s.len(), ROPE_MIN_LEN + DEEP);
        let flat = s.as_str();
        assert_eq!(flat.get(ROPE_MIN_LEN + DEEP - 1), Some(u16::from(b'y')));
        assert_eq!(flat.get(0), Some(u16::from(b'x')));
    }

    #[test]
    fn deep_chains_drop_without_flattening() {
        let mut left = latin1(ROPE_MIN_LEN, 'x');
        let mut right = latin1(ROPE_MIN_LEN, 'z');
        let piece = latin1(1, 'y');
        for _ in 0..DEEP {
            left = JsString::concat_lazy(&left, &piece);
            right = JsString::concat_lazy(&piece, &right);
        }
        let both = JsString::concat_lazy(&left, &right);
        assert_eq!(both.len(), 2 * ROPE_MIN_LEN + 2 * DEEP);
        drop(both);
        drop(left);
        drop(right);
    }

    #[test]
    fn deep_right_chain_flattens() {
        let mut s = latin1(ROPE_MIN_LEN, 'x');
        let letter = |i: usize| b'a' + u8::try_from(i % 26).unwrap();
        for i in 0..DEEP {
            let piece = JsString::from(char::from(letter(i)).to_string());
            s = JsString::concat_lazy(&piece, &s);
        }
        let v = s.to_vec();
        assert_eq!(v.len(), ROPE_MIN_LEN + DEEP);
        assert_eq!(v[0], u16::from(letter(DEEP - 1)));
        assert_eq!(v[DEEP], u16::from(b'x'));
    }

    /// A rope that has taken two short appends, so that it ends in a shared buffer.
    fn with_buffer(base: &JsString) -> JsString {
        let s = JsString::concat_lazy(base, &latin1(1, 'x'));
        JsString::concat_lazy(&s, &latin1(1, 'y'))
    }

    fn text(s: &JsString) -> String {
        s.to_std_string().unwrap()
    }

    #[test]
    fn short_appends_share_one_buffer() {
        let base = latin1(300, 'a');
        let s = with_buffer(&base);
        // Appending to a rope that ends in a buffer adds no reference to the buffer's owner other
        // than the new rope's, and the old rope keeps its length and content.
        let t = JsString::concat_lazy(&s, &latin1(2, 'z'));
        assert_eq!((s.len(), t.len()), (302, 304));
        assert_eq!(base.refcount(), Some(3));
        assert_eq!(text(&s), format!("{}xy", "a".repeat(300)));
        assert_eq!(text(&t), format!("{}xyzz", "a".repeat(300)));
    }

    #[test]
    fn shared_rope_is_not_modified_by_appends() {
        let base = latin1(300, 'a');
        let s = with_buffer(&base);
        let one = JsString::concat_lazy(&s, &latin1(1, '1'));
        let two = JsString::concat_lazy(&s, &latin1(1, '2'));
        let one3 = JsString::concat_lazy(&one, &latin1(1, '3'));
        let one4 = JsString::concat_lazy(&one, &latin1(1, '4'));
        let two5 = JsString::concat_lazy(&two, &latin1(1, '5'));
        let head = "a".repeat(300);
        // Read in an order other than the order of creation.
        assert_eq!(text(&two5), format!("{head}xy25"));
        assert_eq!(text(&one4), format!("{head}xy14"));
        assert_eq!(text(&s), format!("{head}xy"));
        assert_eq!(text(&one3), format!("{head}xy13"));
        assert_eq!(text(&two), format!("{head}xy2"));
        assert_eq!(text(&one), format!("{head}xy1"));
    }

    #[test]
    fn appends_after_a_read_and_across_widths() {
        let base = latin1(300, 'a');
        let s = with_buffer(&base);
        let wide = JsString::concat_lazy(&s, &utf16(1, 0x20ac));
        let later = JsString::concat_lazy(&wide, &latin1(1, 'q'));
        assert_eq!(wide.len(), 303);
        assert_eq!(later.to_vec().last(), Some(&u16::from(b'q')));
        assert_eq!(wide.to_vec()[302], 0x20ac);
        // A rope that has been read is flat; the next append starts over from it.
        assert_eq!(text(&s).len(), 302);
        let u = JsString::concat_lazy(&s, &latin1(1, 'u'));
        let v = JsString::concat_lazy(&u, &latin1(1, 'v'));
        assert_eq!(text(&v), format!("{}xyuv", "a".repeat(300)));
        // A piece too long for the buffer gets a node of its own and the buffer is left alone.
        let long = JsString::concat_lazy(&v, &latin1(100, 'l'));
        assert_eq!(long.len(), 404);
        assert_eq!(
            text(&long),
            format!("{}xyuv{}", "a".repeat(300), "l".repeat(100))
        );
        assert_eq!(text(&v), format!("{}xyuv", "a".repeat(300)));
    }

    #[test]
    fn deep_chains_of_long_pieces_still_flatten_and_drop() {
        let mut s = latin1(ROPE_MIN_LEN, 'x');
        let piece = latin1(100, 'y');
        for _ in 0..DEEP / 10 {
            s = JsString::concat_lazy(&s, &piece);
        }
        assert_eq!(s.len(), ROPE_MIN_LEN + 100 * (DEEP / 10));
        assert_eq!(s.to_vec().len(), s.len());
    }

    #[test]
    fn equality_hash_and_order() {
        let a = latin1(300, 'a');
        let s = JsString::concat_lazy(&latin1(150, 'a'), &latin1(150, 'a'));
        assert_eq!(s, a);
        assert_eq!(super::hash_value(&s), super::hash_value(&a));
        let t = JsString::concat_lazy(&a, &latin1(1, 'b'));
        assert!(s < t);
    }
}
