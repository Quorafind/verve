//! This module implements the `MapIterator` object.
//!
//! More information:
//!  - [ECMAScript reference][spec]
//!
//! [spec]: https://tc39.es/ecma262/#sec-map-iterator-objects

use super::ordered_map::OrderedMap;
use crate::{
    Context, JsData, JsResult,
    builtins::{
        BuiltInBuilder, IntrinsicObject, JsValue,
        iterable::{Step, step_to_result_object},
    },
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
struct MapIteratorLock(Option<JsObject<OrderedMap<JsValue>>>);

impl MapIteratorLock {
    fn new(js_object: JsObject<OrderedMap<JsValue>>) -> Self {
        js_object.borrow_mut().data_mut().lock();
        Self(Some(js_object))
    }

    /// The lock is taken out of the iterator at the end of the map, so dropping it as a plain value
    /// cannot reach `Finalize`; releasing it here keeps the count right. An iterator that is
    /// abandoned half-way is finalized instead.
    fn release(mut self) {
        if let Some(js_object) = self.0.take() {
            js_object.borrow_mut().data_mut().unlock();
        }
    }
}

impl Finalize for MapIteratorLock {
    fn finalize(&self) {
        if let Some(js_object) = &self.0 {
            js_object.borrow_mut().data_mut().unlock();
        }
    }
}

/// The Map Iterator object represents an iteration over a map. It implements the iterator protocol.
///
/// More information:
///  - [ECMAScript reference][spec]
///
/// [spec]: https://tc39.es/ecma262/#sec-map-iterator-objects
#[derive(Debug, Finalize, Trace, JsData)]
pub(crate) struct MapIterator {
    iterated_map: Option<MapIteratorLock>,
    next_index: usize,
    #[unsafe_ignore_trace]
    iteration_kind: PropertyNameKind,
}

impl IntrinsicObject for MapIterator {
    fn init(realm: &Realm) {
        BuiltInBuilder::with_intrinsic::<Self>(realm)
            .prototype(realm.intrinsics().constructors().iterator().prototype())
            .static_method(Self::next, js_string!("next"), 0)
            .static_property(
                JsSymbol::to_string_tag(),
                js_string!("Map Iterator"),
                Attribute::CONFIGURABLE,
            )
            .build();
    }

    fn get(intrinsics: &Intrinsics) -> JsObject {
        intrinsics.objects().iterator_prototypes().map()
    }
}

impl MapIterator {
    /// Abstract operation `CreateMapIterator( map, kind )`
    ///
    /// Creates a new iterator over the given map.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-createmapiterator
    pub(crate) fn create_map_iterator(
        map: JsObject<OrderedMap<JsValue>>,
        kind: PropertyNameKind,
        context: &mut Context,
    ) -> JsValue {
        let iter = Self {
            iterated_map: Some(MapIteratorLock::new(map)),
            next_index: 0,
            iteration_kind: kind,
        };
        let map_iterator = JsObject::from_proto_and_data_with_shared_shape(
            context.root_shape(),
            context.intrinsics().objects().iterator_prototypes().map(),
            iter,
        );
        map_iterator.into()
    }

    /// %MapIteratorPrototype%.next( )
    ///
    /// Advances the iterator and gets the next result in the map.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-%mapiteratorprototype%.next
    pub(crate) fn next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
        let object = this
            .as_object()
            .ok_or_else(|| JsNativeError::typ().with_message("`this` is not a MapIterator"))?;
        let step = Self::advance(&object, context)?.map(|step| step.into_value(context));
        Ok(step_to_result_object(step, context))
    }

    /// The body of `next` without the result object: the next item, or `None` once the map is
    /// exhausted. An entry iterator answers the key and value, not the array of them.
    pub(crate) fn advance(object: &JsObject, _: &mut Context) -> JsResult<Option<Step>> {
        let mut map_iterator = object
            .downcast_mut::<Self>()
            .ok_or_else(|| JsNativeError::typ().with_message("`this` is not a MapIterator"))?;

        let item_kind = map_iterator.iteration_kind;

        if let Some(obj) = map_iterator.iterated_map.take() {
            let e = {
                let entries = obj.0.as_ref().expect("a taken lock is still held here");
                let mut entries = entries.borrow_mut();
                let entries = entries.data_mut();
                let len = entries.full_len();
                loop {
                    let element = entries
                        .get_index(map_iterator.next_index)
                        .map(|(v, k)| (v.clone(), k.clone()));
                    map_iterator.next_index += 1;
                    if element.is_some() || map_iterator.next_index >= len {
                        break element;
                    }
                }
            };
            if let Some((key, value)) = e {
                let item = match item_kind {
                    PropertyNameKind::Key => Step::Value(key),
                    PropertyNameKind::Value => Step::Value(value),
                    PropertyNameKind::KeyAndValue => Step::Entry(key, value),
                };
                map_iterator.iterated_map = Some(obj);
                return Ok(Some(item));
            }
            // The end of the map: give the lock back rather than dropping it while it is taken.
            obj.release();
        }

        Ok(None)
    }
}
