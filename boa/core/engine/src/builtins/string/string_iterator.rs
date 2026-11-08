//! This module implements the `StringIterator` object.
//!
//! More information:
//!  - [ECMAScript reference][spec]
//!
//! [spec]: https://tc39.es/ecma262/#sec-string-iterator-objects

use crate::{
    Context, JsData, JsResult, JsString, JsValue,
    builtins::{BuiltInBuilder, IntrinsicObject, iterable::step_to_result_object},
    context::intrinsics::Intrinsics,
    error::JsNativeError,
    js_string,
    object::JsObject,
    property::Attribute,
    realm::Realm,
    string::{CodePoint, StaticJsStrings},
    symbol::JsSymbol,
};
use boa_gc::{Finalize, Trace};

/// The `StringIterator` object represents an iteration over a string. It implements the iterator protocol.
///
/// More information:
///  - [ECMAScript reference][spec]
///
/// [spec]: https://tc39.es/ecma262/#sec-string-iterator-objects
#[derive(Debug, Clone, Finalize, Trace, JsData)]
pub(crate) struct StringIterator {
    string: JsString,
    next_index: usize,
}

impl IntrinsicObject for StringIterator {
    fn init(realm: &Realm) {
        BuiltInBuilder::with_intrinsic::<Self>(realm)
            .prototype(realm.intrinsics().constructors().iterator().prototype())
            .static_method(Self::next, js_string!("next"), 0)
            .static_property(
                JsSymbol::to_string_tag(),
                js_string!("String Iterator"),
                Attribute::CONFIGURABLE,
            )
            .build();
    }

    fn get(intrinsics: &Intrinsics) -> JsObject {
        intrinsics.objects().iterator_prototypes().string()
    }
}

impl StringIterator {
    /// Create a new `StringIterator`.
    pub(crate) fn create_string_iterator(string: JsString, context: &mut Context) -> JsObject {
        JsObject::from_proto_and_data_with_shared_shape(
            context.root_shape(),
            context
                .intrinsics()
                .objects()
                .iterator_prototypes()
                .string(),
            Self {
                string,
                next_index: 0,
            },
        )
        .upcast()
    }

    /// `StringIterator.prototype.next( )`
    pub(crate) fn next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
        let object = this
            .as_object()
            .ok_or_else(|| JsNativeError::typ().with_message("`this` is not an ArrayIterator"))?;
        let step = Self::advance(&object, context)?;
        Ok(step_to_result_object(step, context))
    }

    /// The body of `next` without the result object: the next code point as a string, or `None`
    /// once the string is exhausted.
    pub(crate) fn advance(object: &JsObject, _: &mut Context) -> JsResult<Option<JsValue>> {
        let mut string_iterator = object
            .downcast_mut::<Self>()
            .ok_or_else(|| JsNativeError::typ().with_message("`this` is not an ArrayIterator"))?;

        if string_iterator.string.is_empty() {
            return Ok(None);
        }
        let native_string = &string_iterator.string;
        let len = native_string.len();
        let position = string_iterator.next_index;
        if position >= len {
            string_iterator.string = js_string!();
            return Ok(None);
        }
        let code_point = native_string.code_point_at(position);
        let end = position + code_point.code_unit_count();
        // A character of one Latin-1 code unit is a static string; the rest are slices.
        let result = match code_point {
            CodePoint::Unicode(c) if u8::try_from(c).is_ok() => {
                StaticJsStrings::latin1_char(c as u8)
            }
            _ => native_string.slice(position, end),
        };
        string_iterator.next_index = end;
        Ok(Some(result.into()))
    }
}
