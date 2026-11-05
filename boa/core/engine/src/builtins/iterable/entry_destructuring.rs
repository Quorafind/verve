//! When `const [key, value] = entry` can skip making `entry`.
//!
//! An entry that a built-in iterator hands to a `[key, value]` pattern is a fresh array nobody
//! else holds, so destructuring it is observable only through the iteration protocol of arrays,
//! and the guard below answers whether any of that protocol has been touched.

use crate::{
    Context, JsValue,
    builtins::iterable::Stepper,
    js_string,
    object::{JsObject, PropertyMap},
    property::PropertyKey,
    symbol::JsSymbol,
};

/// Whether array destructuring of a fresh two-element array into two names runs no user code and
/// can therefore be done by binding the two elements.
///
/// The spec reads `entry[Symbol.iterator]`, calls it, reads `next` of the array iterator it made,
/// calls `next` twice (the third `next` never happens, so the array's `length` is not the end
/// of it), and then closes the iterator by reading `return`. For a fresh array every one of those
/// lands on a built-in unless a program replaced it, and where each lookup ends is known:
///
/// - `Symbol.iterator` is found on `%Array.prototype%`: the array is fresh, so it has no own
///   property of its own, and `Array.prototype` is the realm's intrinsic whatever the program
///   does to the `Array` constructor. It must be a data property holding `%Array.prototype.values%`.
/// - `next` is found on `%ArrayIteratorPrototype%`, an own data property holding that realm's
///   built-in `next`, whose reads of the array (its `length`, its own elements `0` and `1`) are
///   plain.
/// - `return` is absent. The chain from the array iterator is `%ArrayIteratorPrototype%`,
///   `%IteratorPrototype%`, `%Object.prototype%`, then null (the last link cannot be changed), so
///   the first two links are checked to still be those objects and none of the three may have
///   a `return`. All are ordinary objects, never proxies.
///
/// Each of these is read afresh, because a program can change any of them between two elements.
pub(crate) fn entry_destructuring_is_unobservable(context: &Context) -> bool {
    let intrinsics = context.intrinsics();
    let array_prototype = intrinsics.constructors().array().prototype();
    let array_iterator_prototype = intrinsics.objects().iterator_prototypes().array();
    let iterator_prototype = intrinsics.constructors().iterator().prototype();
    let object_prototype = intrinsics.constructors().object().prototype();

    let values: JsObject = intrinsics.objects().array_prototype_values().into();
    let iterator_key = PropertyKey::from(JsSymbol::iterator());
    let next_key = PropertyKey::from(js_string!("next"));
    let return_key = PropertyKey::from(js_string!("return"));

    holds(&array_prototype, &iterator_key, |method| {
        method
            .as_object()
            .is_some_and(|method| JsObject::equals(&method, &values))
    }) && holds(&array_iterator_prototype, &next_key, |next| {
        matches!(Stepper::of(next, context), Some(Stepper::Array))
    }) && lacks_and_inherits(
        &array_iterator_prototype,
        &return_key,
        Some(&iterator_prototype),
    ) && lacks_and_inherits(&iterator_prototype, &return_key, Some(&object_prototype))
        && lacks_and_inherits(&object_prototype, &return_key, None)
}

/// What an ordinary object holds under a string or symbol key of its own.
enum Own<'a> {
    Absent,
    Data(&'a JsValue),
    Accessor,
}

fn own<'a>(properties: &'a PropertyMap, key: &PropertyKey) -> Own<'a> {
    match properties.shape.lookup(key) {
        None => Own::Absent,
        Some(slot) if slot.attributes.is_accessor_descriptor() => Own::Accessor,
        Some(slot) => Own::Data(&properties.storage[slot.index as usize]),
    }
}

/// Whether `object` has an own data property `key` holding `expected`.
fn holds(object: &JsObject, key: &PropertyKey, expected: impl Fn(&JsValue) -> bool) -> bool {
    object.try_borrow().is_ok_and(
        |object| matches!(own(object.properties(), key), Own::Data(value) if expected(value)),
    )
}

/// Whether `object` has no own property `key`, and its prototype is `prototype` (`None` for
/// the end of the chain).
fn lacks_and_inherits(object: &JsObject, key: &PropertyKey, prototype: Option<&JsObject>) -> bool {
    object.try_borrow().is_ok_and(|object| {
        matches!(own(object.properties(), key), Own::Absent)
            && object.properties().shape.prototype().as_ref() == prototype
    })
}
