//! This module implements the `ArrayIterator` object.
//!
//! More information:
//!  - [ECMAScript reference][spec]
//!
//! [spec]: https://tc39.es/ecma262/#sec-array-iterator-objects

#[cfg(feature = "jit")]
use crate::builtins::iterable::Declined;
use crate::{
    Context, JsData, JsResult,
    builtins::{
        BuiltInBuilder, IntrinsicObject, JsValue,
        iterable::{Step, step_to_result_object},
        typed_array::TypedArray,
    },
    context::intrinsics::Intrinsics,
    error::JsNativeError,
    js_string,
    object::{JsObject, RefMut},
    property::{Attribute, PropertyNameKind},
    realm::Realm,
    symbol::JsSymbol,
};
use boa_gc::{Finalize, Trace};

/// The Array Iterator object represents an iteration over an array. It implements the iterator protocol.
///
/// More information:
///  - [ECMAScript reference][spec]
///
/// [spec]: https://tc39.es/ecma262/#sec-array-iterator-objects
#[derive(Debug, Clone, Finalize, Trace, JsData)]
pub(crate) struct ArrayIterator {
    array: JsObject,
    next_index: u64,
    #[unsafe_ignore_trace]
    kind: PropertyNameKind,
    done: bool,
}

impl IntrinsicObject for ArrayIterator {
    fn init(realm: &Realm) {
        BuiltInBuilder::with_intrinsic::<Self>(realm)
            .prototype(realm.intrinsics().constructors().iterator().prototype())
            .static_method(Self::next, js_string!("next"), 0)
            .static_property(
                JsSymbol::to_string_tag(),
                js_string!("Array Iterator"),
                Attribute::CONFIGURABLE,
            )
            .build();
    }

    fn get(intrinsics: &Intrinsics) -> JsObject {
        intrinsics.objects().iterator_prototypes().array()
    }
}

impl ArrayIterator {
    fn new(array: JsObject, kind: PropertyNameKind) -> Self {
        Self {
            array,
            kind,
            next_index: 0,
            done: false,
        }
    }

    /// `CreateArrayIterator( array, kind )`
    ///
    /// Creates a new iterator over the given array.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-createarrayiterator
    pub(crate) fn create_array_iterator(
        array: JsObject,
        kind: PropertyNameKind,
        context: &Context,
    ) -> JsValue {
        let array_iterator = JsObject::from_proto_and_data_with_shared_shape(
            context.root_shape(),
            context.intrinsics().objects().iterator_prototypes().array(),
            Self::new(array, kind),
        );
        array_iterator.into()
    }

    /// %ArrayIteratorPrototype%.next( )
    ///
    /// Gets the next result in the array.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-%arrayiteratorprototype%.next
    pub(crate) fn next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
        let object = this
            .as_object()
            .ok_or_else(|| JsNativeError::typ().with_message("`this` is not an ArrayIterator"))?;
        let step = Self::advance(&object, context)?.map(|step| step.into_value(context));
        Ok(step_to_result_object(step, context))
    }

    /// [`advance`](Self::advance) for the common case that needs no JavaScript: a value or entry
    /// iterator over a plain array, whose `length` is an integer and whose next element is an
    /// ordinary dense one. [`Declined`], with nothing changed, for any other.
    #[cfg(feature = "jit")]
    pub(crate) fn advance_dense(object: &JsObject) -> Result<Option<Step>, Declined> {
        let mut iterator = object.try_downcast_mut::<Self>().ok_or(Declined)?;
        let kind = iterator.kind;
        if matches!(kind, PropertyNameKind::Key) || !iterator.array.is_array() {
            return Err(Declined);
        }
        if iterator.done {
            return Ok(None);
        }
        let index = iterator.next_index;
        let array = iterator.array.try_borrow().map_err(|_| Declined)?;
        let length = array
            .properties()
            .storage
            .first()
            .and_then(JsValue::as_i32)
            .and_then(|length| u64::try_from(length).ok())
            .ok_or(Declined)?;
        if index >= length {
            drop(array);
            iterator.done = true;
            return Ok(None);
        }
        let value = u32::try_from(index)
            .ok()
            .and_then(|index| array.properties().get_dense_property(index))
            .ok_or(Declined)?;
        drop(array);
        iterator.next_index = index + 1;
        Ok(Some(match kind {
            PropertyNameKind::KeyAndValue => Step::Entry(index.into(), value),
            _ => Step::Value(value),
        }))
    }

    /// The body of `next` without the result object: the next value, or `None` once the
    /// iterator is exhausted. Every read `next` makes of the array happens here. An entry
    /// iterator answers the index and the element, not the array of them.
    ///
    /// The iterator is not borrowed while the array is read, since reading `length` or an
    /// element may run user code (a Proxy trap, a getter) that calls `next` on this same
    /// iterator, or that iterates the iterator itself. As in V8, the index is read first, the
    /// step stores `index + 1` once `length` is known and before the element is read, and what a
    /// re-entrant call stored in between is overwritten.
    pub(crate) fn advance(object: &JsObject, context: &mut Context) -> JsResult<Option<Step>> {
        let (array, index, kind) = {
            let iterator = Self::borrow_mut(object)?;
            if iterator.done {
                return Ok(None);
            }
            (iterator.array.clone(), iterator.next_index, iterator.kind)
        };

        let len = if let Some(f) = array.downcast_ref::<TypedArray>() {
            let buf = f.viewed_array_buffer().as_buffer();
            let Some(buf) = buf
                .bytes(std::sync::atomic::Ordering::SeqCst)
                .filter(|buf| !f.is_out_of_bounds(buf.len()))
            else {
                return Err(JsNativeError::typ()
                    .with_message("Cannot get value from out of bounds typed array")
                    .into());
            };

            f.array_length(buf.len())
        } else {
            array.length_of_array_like(context)?
        };

        {
            let mut iterator = Self::borrow_mut(object)?;
            if index >= len {
                iterator.done = true;
                return Ok(None);
            }
            iterator.next_index = index + 1;
            iterator.done = false;
        }
        match kind {
            PropertyNameKind::Key => Ok(Some(Step::Value(index.into()))),
            PropertyNameKind::Value => array.get(index, context).map(|v| Some(Step::Value(v))),
            PropertyNameKind::KeyAndValue => {
                let element_value = array.get(index, context)?;
                Ok(Some(Step::Entry(index.into(), element_value)))
            }
        }
    }

    fn borrow_mut(object: &JsObject) -> JsResult<RefMut<'_, Self>> {
        object.downcast_mut::<Self>().ok_or_else(|| {
            JsNativeError::typ()
                .with_message("`this` is not an ArrayIterator")
                .into()
        })
    }
}
