//! This module implements the `SetIterator` object.
//!
//! More information:
//!  - [ECMAScript reference][spec]
//!
//! [spec]: https://tc39.es/ecma262/#sec-set-iterator-objects

use super::ordered_set::OrderedSet;
use crate::{
    Context, JsData, JsResult,
    builtins::{Array, BuiltInBuilder, IntrinsicObject, JsValue, iterable::step_to_result_object},
    context::intrinsics::Intrinsics,
    error::JsNativeError,
    js_string,
    object::JsObject,
    property::{Attribute, PropertyNameKind},
    realm::Realm,
    symbol::JsSymbol,
};
use boa_gc::{Finalize, Trace};

#[derive(Debug, Trace)]
struct SetIteratorLock(Option<JsObject<OrderedSet>>);

impl SetIteratorLock {
    fn new(js_object: JsObject<OrderedSet>) -> Self {
        js_object.borrow_mut().data_mut().lock();
        Self(Some(js_object))
    }

    /// The lock is taken out of the iterator at the end of the set, so dropping it as a plain value
    /// cannot reach `Finalize`; releasing it here keeps the count right. An iterator that is
    /// abandoned half-way is finalized instead.
    fn release(mut self) {
        if let Some(js_object) = self.0.take() {
            js_object.borrow_mut().data_mut().unlock();
        }
    }
}

impl Finalize for SetIteratorLock {
    fn finalize(&self) {
        if let Some(js_object) = &self.0 {
            js_object.borrow_mut().data_mut().unlock();
        }
    }
}

/// The Set Iterator object represents an iteration over a set. It implements the iterator protocol.
///
/// More information:
///  - [ECMAScript reference][spec]
///
/// [spec]: https://tc39.es/ecma262/#sec-set-iterator-objects
#[derive(Debug, Finalize, Trace, JsData)]
pub(crate) struct SetIterator {
    iterated_set: Option<SetIteratorLock>,
    next_index: usize,
    #[unsafe_ignore_trace]
    iteration_kind: PropertyNameKind,
}

impl IntrinsicObject for SetIterator {
    fn init(realm: &Realm) {
        BuiltInBuilder::with_intrinsic::<Self>(realm)
            .prototype(realm.intrinsics().constructors().iterator().prototype())
            .static_method(Self::next, js_string!("next"), 0)
            .static_property(
                JsSymbol::to_string_tag(),
                js_string!("Set Iterator"),
                Attribute::CONFIGURABLE,
            )
            .build();
    }

    fn get(intrinsics: &Intrinsics) -> JsObject {
        intrinsics.objects().iterator_prototypes().set()
    }
}

impl SetIterator {
    /// Abstract operation `CreateSetIterator( set, kind )`
    ///
    /// Creates a new iterator over the given set.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-createsetiterator
    pub(crate) fn create_set_iterator(
        set: JsObject<OrderedSet>,
        kind: PropertyNameKind,
        context: &Context,
    ) -> JsValue {
        let set_iterator = JsObject::from_proto_and_data_with_shared_shape(
            context.root_shape(),
            context.intrinsics().objects().iterator_prototypes().set(),
            Self {
                iterated_set: Some(SetIteratorLock::new(set)),
                next_index: 0,
                iteration_kind: kind,
            },
        );
        set_iterator.into()
    }

    /// %SetIteratorPrototype%.next( )
    ///
    /// Advances the iterator and gets the next result in the set.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-%setiteratorprototype%.next
    pub(crate) fn next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
        let object = this
            .as_object()
            .ok_or_else(|| JsNativeError::typ().with_message("`this` is not an SetIterator"))?;
        let step = Self::advance(&object, context)?;
        Ok(step_to_result_object(step, context))
    }

    /// The body of `next` without the result object: the next item, or `None` once the set is
    /// exhausted.
    pub(crate) fn advance(object: &JsObject, context: &mut Context) -> JsResult<Option<JsValue>> {
        let mut set_iterator = object
            .downcast_mut::<Self>()
            .ok_or_else(|| JsNativeError::typ().with_message("`this` is not an SetIterator"))?;

        let item_kind = set_iterator.iteration_kind;

        if let Some(obj) = set_iterator.iterated_set.take() {
            let e = {
                let entries = obj.0.as_ref().expect("a taken lock is still held here");
                let mut entries = entries.borrow_mut();
                let entries = entries.data_mut();
                let len = entries.full_len();
                loop {
                    let element = entries.get_index(set_iterator.next_index);
                    set_iterator.next_index += 1;
                    if element.is_some() || set_iterator.next_index >= len {
                        break element.cloned();
                    }
                }
            };
            if let Some(element) = e {
                let item = match item_kind {
                    PropertyNameKind::KeyAndValue => {
                        Array::create_array_from_list([element.clone(), element], context).into()
                    }
                    _ => element,
                };
                set_iterator.iterated_set = Some(obj);
                return Ok(Some(item));
            }
            // The end of the set: give the lock back rather than dropping it while it is taken.
            obj.release();
        }

        Ok(None)
    }
}
