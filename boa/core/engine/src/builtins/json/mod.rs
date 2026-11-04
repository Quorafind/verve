//! Boa's implementation of ECMAScript's global `JSON` object.
//!
//! The `JSON` object contains methods for parsing [JavaScript Object Notation (JSON)][spec]
//! and converting values to JSON. It can't be called or constructed, and aside from its
//! two method properties, it has no interesting functionality of its own.
//!
//! More information:
//!  - [ECMAScript reference][spec]
//!  - [MDN documentation][mdn]
//!  - [JSON specification][json]
//!
//! [spec]: https://tc39.es/ecma262/#sec-json
//! [json]: https://www.json.org/json-en.html
//! [mdn]: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/JSON

use rustc_hash::FxHashSet;

use boa_gc::{Finalize, Trace};
use boa_macros::{js_str, utf16};

use crate::JsExpect;
use crate::{
    Context, JsArgs, JsBigInt, JsData, JsResult, JsString, JsValue,
    builtins::BuiltInObject,
    context::intrinsics::Intrinsics,
    error::JsNativeError,
    js_string,
    object::{IntegrityLevel, JsObject, internal_methods::InternalMethodPropertyContext},
    property::{Attribute, PropertyNameKind},
    realm::Realm,
    string::{CodePoint, StaticJsStrings},
    symbol::JsSymbol,
    value::IntegerOrInfinity,
    vm::native_stack_position,
};

use super::{BuiltInBuilder, IntrinsicObject};

/// Converts a nibble (0-15) to its lowercase hex digit as a UTF-16 code unit.
const fn to_hex_digit(val: u16) -> u16 {
    match val & 0xF {
        v @ 0..=9 => b'0' as u16 + v,
        v => b'a' as u16 + (v - 10),
    }
}

mod parse;
use parse::JsonNode;
#[cfg(test)]
mod tests;

/// Marker struct for the `[[IsRawJSON]]` internal slot.
#[derive(Debug, Trace, Finalize, JsData)]
pub(crate) struct RawJson;

/// JavaScript `JSON` global object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Json;

impl IntrinsicObject for Json {
    fn init(realm: &Realm) {
        let to_string_tag = JsSymbol::to_string_tag();
        let attribute = Attribute::READONLY | Attribute::NON_ENUMERABLE | Attribute::CONFIGURABLE;

        BuiltInBuilder::with_intrinsic::<Self>(realm)
            .static_method(Self::parse, js_string!("parse"), 2)
            .static_method(Self::stringify, js_string!("stringify"), 3)
            .static_method(Self::raw_json, js_string!("rawJSON"), 1)
            .static_method(Self::is_raw_json, js_string!("isRawJSON"), 1)
            .static_property(to_string_tag, Self::NAME, attribute)
            .build();
    }

    fn get(intrinsics: &Intrinsics) -> JsObject {
        intrinsics.objects().json()
    }
}

impl BuiltInObject for Json {
    const NAME: JsString = StaticJsStrings::JSON;
}

impl Json {
    /// `JSON.parse( text[, reviver] )`
    ///
    /// This `JSON` method parses a JSON string, constructing the JavaScript value or object described by the string.
    ///
    /// An optional `reviver` function can be provided to perform a transformation on the resulting object before it is returned.
    ///
    /// More information:
    ///  - [ECMAScript reference][spec]
    ///  - [MDN documentation][mdn]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-json.parse
    /// [mdn]: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/JSON/parse
    pub(crate) fn parse(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
        // 1. Let jsonString be ? ToString(text).
        let json_string = args
            .first()
            .cloned()
            .unwrap_or_default()
            .to_string(context)?;

        // 2. Parse ! StringToCodePoints(jsonString) as a JSON text as specified in ECMA-404.
        //    Throw a SyntaxError exception if it is not a valid JSON text as defined in that specification.
        // 3-10. Evaluate it to a value. The reader does both at once, from the code units.
        let Some(reviver) = args.get_or_undefined(1).as_callable() else {
            return parse::Reader::new(json_string.as_str()).read(context);
        };

        // 11. If IsCallable(reviver) is true, then the text every primitive came from is needed
        // as well, which the reader keeps beside the values it builds.
        let (unfiltered, source_tree) =
            parse::Reader::new(json_string.as_str()).read_with_nodes(context)?;

        // a. Let root be ! OrdinaryObjectCreate(%Object.prototype%).
        let root = JsObject::with_object_proto(context.intrinsics());

        // b. Let rootName be the empty String.
        // c. Perform ! CreateDataPropertyOrThrow(root, rootName, unfiltered).
        root.create_data_property_or_throw(js_string!(), unfiltered, context)
            .js_expect("CreateDataPropertyOrThrow should never throw here")?;

        // d. Return ? InternalizeJSONProperty(root, rootName, reviver).
        Self::internalize_json_property(&root, js_string!(), &reviver, Some(&source_tree), context)
    }

    /// `25.5.1.1 InternalizeJSONProperty ( holder, name, reviver )`
    ///
    /// More information:
    ///  - [ECMAScript reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-internalizejsonproperty
    fn internalize_json_property(
        holder: &JsObject,
        name: JsString,
        reviver: &JsObject,
        source_node: Option<&JsonNode>,
        context: &mut Context,
    ) -> JsResult<JsValue> {
        // The reader nests as deep as memory allows, and this walk of the result recurses on the
        // native stack: past what the runtime lets nested runs take, it throws what V8 throws.
        if context
            .vm
            .native_stack_base
            .saturating_sub(native_stack_position())
            > context.runtime_limits().native_stack_limit()
        {
            return Err(JsNativeError::range()
                .with_message("Maximum call stack size exceeded")
                .into());
        }

        // 1. Let val be ? Get(holder, name).
        let val = holder.get(name.clone(), context)?;

        // 2. If Type(val) is Object, then
        if let Some(obj) = val.as_object() {
            // a. Let isArray be ? IsArray(val).
            // b. If isArray is true, then
            if obj.is_array_abstract()? {
                // i. Let I be 0.
                // ii. Let len be ? LengthOfArrayLike(val).
                // iii. Repeat, while I < len,
                let len = obj.length_of_array_like(context)? as i64;
                let children = match source_node {
                    Some(JsonNode::Array(children)) => Some(children),
                    _ => None,
                };
                for i in 0..len {
                    let child_node = children.and_then(|c| c.get(i as usize));
                    // 1. Let prop be ! ToString(𝔽(I)).
                    // 2. Let newElement be ? InternalizeJSONProperty(val, prop, reviver).
                    let new_element = Self::internalize_json_property(
                        &obj,
                        i.into(),
                        reviver,
                        child_node,
                        context,
                    )?;

                    // 3. If newElement is undefined, then
                    if new_element.is_undefined() {
                        // a. Perform ? val.[[Delete]](prop).
                        obj.__delete__(
                            &i.into(),
                            &mut InternalMethodPropertyContext::new(context),
                        )?;
                    }
                    // 4. Else,
                    else {
                        // a. Perform ? CreateDataProperty(val, prop, newElement).
                        obj.create_data_property(i, new_element, context)?;
                    }
                }
            }
            // c. Else,
            else {
                // i. Let keys be ? EnumerableOwnPropertyNames(val, key).
                let keys = obj.enumerable_own_property_names(PropertyNameKind::Key, context)?;
                let entries = match source_node {
                    Some(JsonNode::Object(entries)) => Some(entries),
                    _ => None,
                };

                // ii. For each String P of keys, do
                for p in keys {
                    // This is safe, because EnumerableOwnPropertyNames with 'key' type only returns strings.
                    let p = p
                        .as_string()
                        .js_expect("EnumerableOwnPropertyNames only returns strings")?;

                    let child_node =
                        entries.and_then(|e| e.iter().rfind(|(k, _)| *k == p).map(|(_, v)| v));

                    // 1. Let newElement be ? InternalizeJSONProperty(val, P, reviver).
                    let new_element = Self::internalize_json_property(
                        &obj,
                        p.clone(),
                        reviver,
                        child_node,
                        context,
                    )?;

                    // 2. If newElement is undefined, then
                    if new_element.is_undefined() {
                        // a. Perform ? val.[[Delete]](P).
                        obj.__delete__(
                            &p.into(),
                            &mut InternalMethodPropertyContext::new(context),
                        )?;
                    }
                    // 3. Else,
                    else {
                        // a. Perform ? CreateDataProperty(val, P, newElement).
                        obj.create_data_property(p, new_element, context)?;
                    }
                }
            }
        }

        // Build the context object for the reviver call.
        // For primitive JSON values: context = { source: "<original text>" }
        // For objects/arrays or modified values: context = {} (no source property)
        // Per spec, source is only provided when the value is still the same
        // primitive that was produced by parsing the original JSON text.
        let ctx_obj = JsObject::with_object_proto(context.intrinsics());
        if let Some(JsonNode::Primitive { source, value }) = source_node
            && JsValue::same_value(&val, value)
        {
            ctx_obj.create_data_property_or_throw(
                js_string!("source"),
                JsValue::from(source.clone()),
                context,
            )?;
        }

        // 3. Return ? Call(reviver, holder, « name, val, context »).
        reviver.call(
            &holder.clone().into(),
            &[name.into(), val, ctx_obj.into()],
            context,
        )
    }

    /// `JSON.rawJSON ( text )`
    ///
    /// Creates a raw JSON object from the given text.
    ///
    /// More information:
    ///  - [ECMAScript reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-json.rawjson
    pub(crate) fn raw_json(
        _: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> JsResult<JsValue> {
        // 1. Let jsonString be ? ToString(text).
        let json_string = args.get_or_undefined(0).to_string(context)?;

        // 2. Throw a SyntaxError exception if jsonString is the empty String, or if
        //    either the first or last code unit of jsonString is any of 0x0009
        //    (CHARACTER TABULATION), 0x000A (LINE FEED), 0x000D (CARRIAGE RETURN), or
        //    0x0020 (SPACE).
        let std_string = json_string
            .to_std_string()
            .map_err(|e| JsNativeError::syntax().with_message(e.to_string()))?;

        if std_string.is_empty() {
            return Err(JsNativeError::syntax()
                .with_message("JSON.rawJSON text must not be the empty string")
                .into());
        }

        let first = std_string.as_bytes()[0];
        let last = std_string.as_bytes()[std_string.len() - 1];
        if matches!(first, b'\t' | b'\n' | b'\r' | b' ')
            || matches!(last, b'\t' | b'\n' | b'\r' | b' ')
        {
            return Err(JsNativeError::syntax()
                .with_message("JSON.rawJSON text must not start or end with whitespace")
                .into());
        }

        // 3. Parse StringToCodePoints(jsonString) as a JSON text as specified in ECMA-404.
        //    Throw a SyntaxError exception if it is not a valid JSON text as defined in that
        //    specification, or if its outermost value is an object or array.
        let parsed: serde_json::Value = serde_json::from_str(&std_string)
            .map_err(|e| JsNativeError::syntax().with_message(e.to_string()))?;

        // Must not be an object or array
        match parsed {
            serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                return Err(JsNativeError::syntax()
                    .with_message("JSON.rawJSON text must not be an object or array")
                    .into());
            }
            _ => {}
        }

        // 3. Let internalSlotsList be « [[IsRawJSON]] ».
        // 4. Let obj be OrdinaryObjectCreate(null, internalSlotsList).
        let obj = JsObject::from_proto_and_data(None::<JsObject>, RawJson);

        // 5. Perform ! CreateDataPropertyOrThrow(obj, "rawJSON", jsonString).
        obj.create_data_property_or_throw(js_string!("rawJSON"), json_string, context)
            .js_expect("CreateDataPropertyOrThrow should never throw here")?;

        // 6. Perform ! SetIntegrityLevel(obj, frozen).
        obj.set_integrity_level(IntegrityLevel::Frozen, context)
            .js_expect("SetIntegrityLevel should never throw here")?;

        // 7. Return obj.
        Ok(obj.into())
    }

    /// `JSON.isRawJSON ( O )`
    ///
    /// Checks if the given value is a raw JSON object.
    ///
    /// More information:
    ///  - [ECMAScript reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-json.israwjson
    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn is_raw_json(
        _: &JsValue,
        args: &[JsValue],
        _context: &mut Context,
    ) -> JsResult<JsValue> {
        // 1. If Type(O) is Object and O has an [[IsRawJSON]] internal slot, return true.
        // 2. Return false.
        let value = args.get_or_undefined(0);
        let result = value.as_object().is_some_and(|obj| obj.is::<RawJson>());
        Ok(result.into())
    }

    /// `JSON.stringify( value[, replacer[, space]] )`
    ///
    /// This `JSON` method converts a JavaScript object or value to a JSON string.
    ///
    /// This method optionally replaces values if a `replacer` function is specified or
    /// optionally including only the specified properties if a replacer array is specified.
    ///
    /// An optional `space` argument can be supplied of type `String` or `Number` that's used to insert
    /// white space into the output JSON string for readability purposes.
    ///
    /// More information:
    ///  - [ECMAScript reference][spec]
    ///  - [MDN documentation][mdn]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-json.stringify
    /// [mdn]: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/JSON/stringify
    pub(crate) fn stringify(
        _: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> JsResult<JsValue> {
        // 1. Let indent be the empty String.
        let indent = js_string!();

        // 2. Let PropertyList and ReplacerFunction be undefined.
        let mut property_list = None;
        let mut replacer_function = None;

        let replacer = args.get_or_undefined(1);

        // 4. If Type(replacer) is Object, then
        if let Some(replacer_obj) = replacer.as_object() {
            // a. If IsCallable(replacer) is true, then
            if replacer_obj.is_callable() {
                // i. Set ReplacerFunction to replacer.
                replacer_function = Some(replacer_obj.clone());
            // b. Else,
            } else {
                // i. Let isArray be ? IsArray(replacer).
                // ii. If isArray is true, then
                if replacer_obj.is_array_abstract()? {
                    // 1. Set PropertyList to a new empty List.
                    let mut property_set = indexmap::IndexSet::new();

                    // 2. Let len be ? LengthOfArrayLike(replacer).
                    let len = replacer_obj.length_of_array_like(context)?;

                    // 3. Let k be 0.
                    let mut k = 0;

                    // 4. Repeat, while k < len,
                    while k < len {
                        // a. Let prop be ! ToString(𝔽(k)).
                        // b. Let v be ? Get(replacer, prop).
                        let v = replacer_obj.get(k, context)?;

                        // c. Let item be undefined.
                        // d. If Type(v) is String, set item to v.
                        // e. Else if Type(v) is Number, set item to ! ToString(v).
                        // f. Else if Type(v) is Object, then
                        // g. If item is not undefined and item is not currently an element of PropertyList, then
                        // i. Append item to the end of PropertyList.
                        if let Some(s) = v.as_string() {
                            property_set.insert(s);
                        } else if v.is_number() {
                            property_set.insert(
                                v.to_string(context)
                                    .js_expect("ToString cannot fail on number value")?,
                            );
                        } else if let Some(obj) = v.as_object()
                            && (obj.is::<JsString>() || obj.is::<f64>())
                        {
                            // i. If v has a [[StringData]] or [[NumberData]] internal slot, set item to ? ToString(v).
                            property_set.insert(v.to_string(context)?);
                        }

                        // h. Set k to k + 1.
                        k += 1;
                    }
                    property_list = Some(property_set.into_iter().collect());
                }
            }
        }

        let mut space = args.get_or_undefined(2).clone();

        // 5. If Type(space) is Object, then
        if let Some(space_obj) = space.as_object() {
            // a. If space has a [[NumberData]] internal slot, then
            if space_obj.is::<f64>() {
                // i. Set space to ? ToNumber(space).
                space = space.to_number(context)?.into();
            }
            // b. Else if space has a [[StringData]] internal slot, then
            else if space_obj.is::<JsString>() {
                // i. Set space to ? ToString(space).
                space = space.to_string(context)?.into();
            }
        }

        // 6. If Type(space) is Number, then
        let gap = if space.is_number() {
            // a. Let spaceMV be ! ToIntegerOrInfinity(space).
            // b. Set spaceMV to min(10, spaceMV).
            // c. If spaceMV < 1, let gap be the empty String; otherwise let gap be the String value containing spaceMV occurrences of the code unit 0x0020 (SPACE).
            match space
                .to_integer_or_infinity(context)
                .js_expect("ToIntegerOrInfinity cannot fail on number")?
            {
                IntegerOrInfinity::PositiveInfinity => js_string!("          "),
                IntegerOrInfinity::NegativeInfinity => js_string!(),
                IntegerOrInfinity::Integer(i) if i < 1 => js_string!(),
                IntegerOrInfinity::Integer(i) => {
                    let mut s = String::new();
                    let i = std::cmp::min(10, i);
                    for _ in 0..i {
                        s.push(' ');
                    }
                    s.into()
                }
            }
        // 7. Else if Type(space) is String, then
        } else if let Some(s) = space.as_string() {
            // a. If the length of space is 10 or less, let gap be space; otherwise let gap be the substring of space from 0 to 10.
            s.get(..10).unwrap_or(s)
        // 8. Else,
        } else {
            // a. Let gap be the empty String.
            js_string!()
        };

        // 9. Let wrapper be ! OrdinaryObjectCreate(%Object.prototype%).
        let wrapper = JsObject::with_object_proto(context.intrinsics());

        // 10. Perform ! CreateDataPropertyOrThrow(wrapper, the empty String, value).
        wrapper
            .create_data_property_or_throw(js_string!(), args.get_or_undefined(0).clone(), context)
            .js_expect("CreateDataPropertyOrThrow should never fail here")?;

        // 11. Let state be the Record { [[ReplacerFunction]]: ReplacerFunction, [[Stack]]: stack, [[Indent]]: indent, [[Gap]]: gap, [[PropertyList]]: PropertyList }.
        let mut state = StateRecord {
            replacer_function,
            stack_set: FxHashSet::default(),
            indent,
            gap,
            property_list,
        };

        // 12. Return ? SerializeJSONProperty(state, the empty String, wrapper).
        Ok(
            Self::serialize_json_property(&mut state, js_string!(), &wrapper, context)?
                .map(Into::into)
                .unwrap_or_default(),
        )
    }

    /// `25.5.2.1 SerializeJSONProperty ( state, key, holder )`
    ///
    /// More information:
    ///  - [ECMAScript reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-serializejsonproperty
    fn serialize_json_property(
        state: &mut StateRecord,
        key: JsString,
        holder: &JsObject,
        context: &mut Context,
    ) -> JsResult<Option<JsString>> {
        // 1. Let value be ? Get(holder, key).
        let mut value = holder.get(key.clone(), context)?;

        // 2. If Type(value) is Object or BigInt, then
        if value.is_object() || value.is_bigint() {
            // a. Let toJSON be ? GetV(value, "toJSON").
            let to_json = value.get_v(js_string!("toJSON"), context)?;

            // b. If IsCallable(toJSON) is true, then
            if let Some(obj) = to_json.as_callable() {
                // i. Set value to ? Call(toJSON, value, « key »).
                value = obj.call(&value, &[key.clone().into()], context)?;
            }
        }

        // 3. If state.[[ReplacerFunction]] is not undefined, then
        if let Some(obj) = &state.replacer_function {
            // a. Set value to ? Call(state.[[ReplacerFunction]], holder, « key, value »).
            value = obj.call(&holder.clone().into(), &[key.into(), value], context)?;
        }

        // 4. If Type(value) is Object, then
        if let Some(obj) = value.as_object() {
            // a. If value has a [[NumberData]] internal slot, then
            if obj.is::<f64>() {
                // i. Set value to ? ToNumber(value).
                value = value.to_number(context)?.into();
            }
            // b. Else if value has a [[StringData]] internal slot, then
            else if obj.is::<JsString>() {
                // i. Set value to ? ToString(value).
                value = value.to_string(context)?.into();
            }
            // c. Else if value has a [[BooleanData]] internal slot, then
            else if let Some(boolean) = obj.downcast_ref::<bool>() {
                // i. Set value to value.[[BooleanData]].
                value = (*boolean).into();
            }
            // d. Else if value has a [[BigIntData]] internal slot, then
            else if let Some(bigint) = obj.downcast_ref::<JsBigInt>() {
                // i. Set value to value.[[BigIntData]].
                value = bigint.clone().into();
            }
            // e. Else if value has a [[IsRawJSON]] internal slot, then
            else if obj.is::<RawJson>() {
                // i. Return the value of the "rawJSON" property of value.
                let raw = obj.get(js_string!("rawJSON"), context)?;
                return Ok(raw.as_string());
            }
        }

        // 5. If value is null, return "null".
        if value.is_null() {
            return Ok(Some(js_string!("null")));
        }

        // 6. If value is true, return "true".
        // 7. If value is false, return "false".
        if value.is_boolean() {
            return Ok(Some(js_string!(if value.to_boolean() {
                "true"
            } else {
                "false"
            })));
        }

        // 8. If Type(value) is String, return QuoteJSONString(value).
        if let Some(s) = value.as_string() {
            return Ok(Some(Self::quote_json_string(&s)));
        }

        // 9. If Type(value) is Number, then
        if let Some(n) = value.as_number() {
            // a. If value is finite, return ! ToString(value).
            if n.is_finite() {
                return Ok(Some(
                    value
                        .to_string(context)
                        .js_expect("ToString should never fail here")?,
                ));
            }

            // b. Return "null".
            return Ok(Some(js_string!("null")));
        }

        // 10. If Type(value) is BigInt, throw a TypeError exception.
        if value.is_bigint() {
            return Err(JsNativeError::typ()
                .with_message("cannot serialize bigint to JSON")
                .into());
        }

        // 11. If Type(value) is Object and IsCallable(value) is false, then
        if let Some(obj) = value.as_object()
            && !obj.is_callable()
        {
            // a. Let isArray be ? IsArray(value).
            // b. If isArray is true, return ? SerializeJSONArray(state, value).
            // c. Return ? SerializeJSONObject(state, value).
            return if obj.is_array_abstract()? {
                Ok(Some(Self::serialize_json_array(state, &obj, context)?))
            } else {
                Ok(Some(Self::serialize_json_object(state, &obj, context)?))
            };
        }

        // 12. Return undefined.
        Ok(None)
    }

    /// `25.5.2.2 QuoteJSONString ( value )`
    ///
    /// More information:
    ///  - [ECMAScript reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-quotejsonstring
    fn quote_json_string(value: &JsString) -> JsString {
        let mut buf = [0; 2];
        // 1. Let product be the String value consisting solely of the code unit 0x0022 (QUOTATION MARK).
        let mut product = Vec::with_capacity(value.len() + 2);
        product.push('"' as u16);

        // 2. For each code point C of ! StringToCodePoints(value), do
        for code_point in value.code_points() {
            match code_point {
                // a. If C is listed in the "Code Point" column of Table 73, then
                // i. Set product to the string-concatenation of product and the
                // escape sequence for C as specified in the "Escape Sequence"
                // column of the corresponding row.
                CodePoint::Unicode('\u{0008}') => product.extend_from_slice(utf16!(r"\b")),
                CodePoint::Unicode('\u{0009}') => product.extend_from_slice(utf16!(r"\t")),
                CodePoint::Unicode('\u{000A}') => product.extend_from_slice(utf16!(r"\n")),
                CodePoint::Unicode('\u{000C}') => product.extend_from_slice(utf16!(r"\f")),
                CodePoint::Unicode('\u{000D}') => product.extend_from_slice(utf16!(r"\r")),
                CodePoint::Unicode('\u{0022}') => product.extend_from_slice(utf16!(r#"\""#)),
                CodePoint::Unicode('\u{005C}') => product.extend_from_slice(utf16!(r"\\")),
                // b. Else if C has a numeric value less than 0x0020 (SPACE), or
                // if C has the same numeric value as a leading surrogate or
                // trailing surrogate, then
                //     i. Let unit be the code unit whose numeric value is that
                //     of C.
                //     ii. Set product to the string-concatenation of product
                //     and UnicodeEscape(unit).
                CodePoint::Unicode(c) if c < '\u{0020}' => {
                    let val = c as u16;
                    product.extend_from_slice(&[
                        '\\' as u16,
                        'u' as u16,
                        to_hex_digit(val >> 12),
                        to_hex_digit((val >> 8) & 0xF),
                        to_hex_digit((val >> 4) & 0xF),
                        to_hex_digit(val & 0xF),
                    ]);
                }
                CodePoint::UnpairedSurrogate(surr) => {
                    product.extend_from_slice(&[
                        '\\' as u16,
                        'u' as u16,
                        to_hex_digit(surr >> 12),
                        to_hex_digit((surr >> 8) & 0xF),
                        to_hex_digit((surr >> 4) & 0xF),
                        to_hex_digit(surr & 0xF),
                    ]);
                }
                // c. Else,
                CodePoint::Unicode(c) => {
                    // i. Set product to the string-concatenation of product and ! UTF16EncodeCodePoint(C).
                    product.extend_from_slice(c.encode_utf16(&mut buf));
                }
            }
        }

        // 3. Set product to the string-concatenation of product and the code unit 0x0022 (QUOTATION MARK).
        product.push('"' as u16);

        // 4. Return product.
        js_string!(&product[..])
    }

    /// The concatenation of `opening`, every piece of `members`, and `closing`, in one
    /// allocation.
    ///
    /// `SerializeJSONObject` and `SerializeJSONArray` collect their members as a list of pieces
    /// and then ask for one string of them; copying each piece into a `Vec<u16>` first, and that
    /// `Vec` again into the result, doubled the bytes moved per member.
    fn concat_members(opening: &JsString, members: &[JsString], closing: &JsString) -> JsString {
        let mut segments = Vec::with_capacity(members.len() + 2);
        segments.push(opening.as_str());
        segments.extend(members.iter().map(JsString::as_str));
        segments.push(closing.as_str());
        JsString::concat_array(&segments)
    }
    /// `25.5.2.4 SerializeJSONObject ( state, value )`
    ///
    /// More information:
    ///  - [ECMAScript reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-serializejsonobject
    fn serialize_json_object(
        state: &mut StateRecord,
        value: &JsObject,
        context: &mut Context,
    ) -> JsResult<JsString> {
        // 1. If state.[[Stack]] contains value, throw a TypeError exception because the structure is cyclical.
        if !state.stack_set.insert(value.clone()) {
            return Err(JsNativeError::typ()
                .with_message("cyclic object value")
                .into());
        }

        // 3. Let stepback be state.[[Indent]].
        let stepback = state.indent.clone();

        // 4. Set state.[[Indent]] to the string-concatenation of state.[[Indent]] and state.[[Gap]].
        state.indent = js_string!(&state.indent, &state.gap);

        // 5. If state.[[PropertyList]] is not undefined, then
        let k = if let Some(p) = &state.property_list {
            // a. Let K be state.[[PropertyList]].
            p.clone()
        // 6. Else,
        } else {
            // a. Let K be ? EnumerableOwnPropertyNames(value, key).
            let keys = value.enumerable_own_property_names(PropertyNameKind::Key, context);
            // 11. Remove value from state.[[Stack]] on error.
            let keys = match keys {
                Ok(k) => k,
                Err(e) => {
                    let removed = state.stack_set.remove(value);
                    debug_assert!(removed);
                    return Err(e);
                }
            };
            // Unwrap is safe, because EnumerableOwnPropertyNames with kind "key" only returns string values.
            match keys
                .iter()
                .map(|v| {
                    v.to_string(context)
                        .js_expect("EnumerableOwnPropertyNames only returns strings")
                })
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(k) => k,
                Err(e) => {
                    let removed = state.stack_set.remove(value);
                    debug_assert!(removed);
                    return Err(e.into());
                }
            }
        };

        // 7. Let partial be a new empty List.
        //
        // A member is kept as its pieces — the quoted key, the colon, the value — instead of
        // being copied into a `Vec<u16>` first and into the result after that. One allocation at
        // the end copies each piece once.
        let separator = if state.gap.is_empty() {
            js_string!(",")
        } else {
            js_string!(js_str!(",\n"), &state.indent)
        };
        let colon = if state.gap.is_empty() {
            js_string!(":")
        } else {
            js_string!(": ")
        };
        let mut partial: Vec<JsString> = Vec::with_capacity(k.len() * 3);

        // 8. For each element P of K, do
        for p in &k {
            // a. Let strP be ? SerializeJSONProperty(state, P, value).
            let str_p = Self::serialize_json_property(state, p.clone(), value, context);
            let str_p = match str_p {
                Ok(v) => v,
                Err(e) => {
                    let removed = state.stack_set.remove(value);
                    debug_assert!(removed);
                    return Err(e);
                }
            };

            // b. If strP is not undefined, then
            if let Some(str_p) = str_p {
                // i. Let member be QuoteJSONString(P).
                // ii–iv. … and ":", the space a gap puts after it, and strP.
                if !partial.is_empty() {
                    partial.push(separator.clone());
                }
                partial.push(Self::quote_json_string(p));
                partial.push(colon.clone());
                partial.push(str_p);
            }
        }

        // 9. If partial is empty, then
        let r#final = if partial.is_empty() {
            // a. Let final be "{}".
            js_string!("{}")
        // 10. Else,
        } else if state.gap.is_empty() {
            // a. i–ii. Let final be the string-concatenation of "{", the members separated by
            // commas, and "}", in one allocation.
            Self::concat_members(&js_string!("{"), &partial, &js_string!("}"))
        } else {
            // b. i. Set separator to the comma, the line feed and state.[[Indent]] (above).
            // ii–iii. … and final to "{", a line feed, the indent, the members, a line feed,
            // stepback and "}".
            let opening = js_string!(js_str!("{\n"), &state.indent);
            let closing = js_string!(js_str!("\n"), &stepback, js_str!("}"));
            Self::concat_members(&opening, &partial, &closing)
        };

        // 12. Set state.[[Indent]] to stepback.
        state.indent = stepback;

        // 11. Remove value from state.[[Stack]].
        let removed = state.stack_set.remove(value);
        debug_assert!(removed);

        // 13. Return final.
        Ok(r#final)
    }

    /// `25.5.2.5 SerializeJSONArray ( state, value )`
    ///
    /// More information:
    ///  - [ECMAScript reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-serializejsonarray
    fn serialize_json_array(
        state: &mut StateRecord,
        value: &JsObject,
        context: &mut Context,
    ) -> JsResult<JsString> {
        // 1. If state.[[Stack]] contains value, throw a TypeError exception because the structure is cyclical.
        if !state.stack_set.insert(value.clone()) {
            return Err(JsNativeError::typ()
                .with_message("cyclic object value")
                .into());
        }

        // 3. Let stepback be state.[[Indent]].
        let stepback = state.indent.clone();

        // 4. Set state.[[Indent]] to the string-concatenation of state.[[Indent]] and state.[[Gap]].
        state.indent = js_string!(&state.indent, &state.gap);

        // 6. Let len be ? LengthOfArrayLike(value).
        let len = match value.length_of_array_like(context) {
            Ok(n) => n,
            Err(e) => {
                let removed = state.stack_set.remove(value);
                debug_assert!(removed);
                return Err(e);
            }
        };

        // 5. Let partial be a new empty List.
        let separator = if state.gap.is_empty() {
            js_string!(",")
        } else {
            js_string!(js_str!(",\n"), &state.indent)
        };
        let mut partial: Vec<JsString> = Vec::with_capacity(len as usize * 2);

        // 7. Let index be 0.
        let mut index = 0;

        // 8. Repeat, while index < len,
        while index < len {
            // a. Let strP be ? SerializeJSONProperty(state, ! ToString(𝔽(index)), value).
            let str_p = Self::serialize_json_property(state, index.into(), value, context);
            let str_p = match str_p {
                Ok(v) => v,
                Err(e) => {
                    let removed = state.stack_set.remove(value);
                    debug_assert!(removed);
                    return Err(e);
                }
            };

            // b. If strP is undefined, then
            if let Some(str_p) = str_p {
                // i. Append strP to partial.
                if !partial.is_empty() {
                    partial.push(separator.clone());
                }
                partial.push(str_p);
            // c. Else,
            } else {
                // i. Append "null" to partial.
                if !partial.is_empty() {
                    partial.push(separator.clone());
                }
                partial.push(js_string!("null"));
            }

            // d. Set index to index + 1.
            index += 1;
        }

        // 9. If partial is empty, then
        let r#final = if partial.is_empty() {
            // a. Let final be "[]".
            js_string!("[]")
        // 10. Else,
        } else if state.gap.is_empty() {
            // a. i–ii. Let final be the string-concatenation of "[", the elements separated by
            // commas, and "]", in one allocation.
            Self::concat_members(&js_string!("["), &partial, &js_string!("]"))
        } else {
            // b. i. Set separator to the comma, the line feed and state.[[Indent]] (above).
            // ii–iii. … and final to "[", a line feed, the indent, the elements, a line feed,
            // stepback and "]".
            let opening = js_string!(js_str!("[\n"), &state.indent);
            let closing = js_string!(js_str!("\n"), &stepback, js_str!("]"));
            Self::concat_members(&opening, &partial, &closing)
        };

        // 12. Set state.[[Indent]] to stepback.
        state.indent = stepback;

        // 11. Remove value from state.[[Stack]].
        let removed = state.stack_set.remove(value);
        debug_assert!(removed);

        // 13. Return final.
        Ok(r#final)
    }
}

struct StateRecord {
    replacer_function: Option<JsObject>,
    stack_set: FxHashSet<JsObject>,
    indent: JsString,
    gap: JsString,
    property_list: Option<Vec<JsString>>,
}
