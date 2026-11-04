//! Boa's implementation of ECMAScript's `IteratorRecord` and iterator prototype objects.

use crate::{
    Context, JsResult, JsValue,
    builtins::{
        BuiltInBuilder, IntrinsicObject, array::array_iterator::ArrayIterator,
        map::map_iterator::MapIterator, set::set_iterator::SetIterator,
        string::string_iterator::StringIterator,
    },
    context::intrinsics::Intrinsics,
    error::JsNativeError,
    js_string,
    native_function::{
        CoroutineBranch, CoroutineState, NativeFunctionObject, NativeFunctionPointer,
    },
    object::JsObject,
    realm::Realm,
    symbol::JsSymbol,
    vm::CompletionRecord,
};
use boa_gc::{Finalize, Trace};

mod async_from_sync_iterator;
pub(crate) mod iterator_constructor;
pub(crate) mod iterator_helper;
mod iterator_prototype;
pub(crate) mod wrap_for_valid_iterator;

#[cfg(test)]
mod tests;

pub(crate) use async_from_sync_iterator::AsyncFromSyncIterator;
pub(crate) use iterator_prototype::Iterator;

/// `IfAbruptCloseIterator ( value, iteratorRecord )`
///
/// `IfAbruptCloseIterator` is a shorthand for a sequence of algorithm steps that use an `Iterator`
/// Record.
///
/// More information:
///  - [ECMA reference][spec]
///
///  [spec]: https://tc39.es/ecma262/#sec-ifabruptcloseiterator
macro_rules! if_abrupt_close_iterator {
    ($value:expr, $iterator_record:expr, $context:expr) => {
        match $value {
            // 1. If value is an abrupt completion, return ? IteratorClose(iteratorRecord, value).
            Err(err) => return $iterator_record.close(Err(err), $context),
            // 2. Else if value is a Completion Record, set value to value.
            Ok(value) => value,
        }
    };
}

// Export macro to crate level
pub(crate) use if_abrupt_close_iterator;

use super::OrdinaryObject;

/// The built-in iterator prototypes.
#[derive(Debug, Trace, Finalize)]
pub struct IteratorPrototypes {
    /// The `IteratorPrototype` object.
    iterator: JsObject,

    /// The `AsyncIteratorPrototype` object.
    async_iterator: JsObject,

    /// The `AsyncFromSyncIteratorPrototype` prototype object.
    async_from_sync_iterator: JsObject,

    /// The `ArrayIteratorPrototype` prototype object.
    array: JsObject,

    /// The `SetIteratorPrototype` prototype object.
    set: JsObject,

    /// The `StringIteratorPrototype` prototype object.
    string: JsObject,

    /// The `RegExpStringIteratorPrototype` prototype object.
    regexp_string: JsObject,

    /// The `MapIteratorPrototype` prototype object.
    map: JsObject,

    /// The `%SegmentIteratorPrototype%` prototype object.
    #[cfg(feature = "intl")]
    segment: JsObject,

    /// The `%IteratorHelperPrototype%` prototype object.
    iterator_helper: JsObject,

    /// The `%WrapForValidIteratorPrototype%` prototype object.
    wrap_for_valid_iterator: JsObject,
}

impl Default for IteratorPrototypes {
    fn default() -> Self {
        Self {
            iterator: JsObject::with_null_proto(),
            async_iterator: JsObject::with_null_proto(),
            async_from_sync_iterator: JsObject::with_null_proto(),
            array: JsObject::with_null_proto(),
            set: JsObject::with_null_proto(),
            string: JsObject::with_null_proto(),
            regexp_string: JsObject::with_null_proto(),
            map: JsObject::with_null_proto(),
            #[cfg(feature = "intl")]
            segment: JsObject::with_null_proto(),
            iterator_helper: JsObject::with_null_proto(),
            wrap_for_valid_iterator: JsObject::with_null_proto(),
        }
    }
}

impl IteratorPrototypes {
    /// Returns the `ArrayIteratorPrototype` object.
    #[inline]
    #[must_use]
    pub fn array(&self) -> JsObject {
        self.array.clone()
    }

    /// Returns the `AsyncIteratorPrototype` object.
    #[inline]
    #[must_use]
    pub fn async_iterator(&self) -> JsObject {
        self.async_iterator.clone()
    }

    /// Returns the `AsyncFromSyncIteratorPrototype` object.
    #[inline]
    #[must_use]
    pub fn async_from_sync_iterator(&self) -> JsObject {
        self.async_from_sync_iterator.clone()
    }

    /// Returns the `SetIteratorPrototype` object.
    #[inline]
    #[must_use]
    pub fn set(&self) -> JsObject {
        self.set.clone()
    }

    /// Returns the `StringIteratorPrototype` object.
    #[inline]
    #[must_use]
    pub fn string(&self) -> JsObject {
        self.string.clone()
    }

    /// Returns the `RegExpStringIteratorPrototype` object.
    #[inline]
    #[must_use]
    pub fn regexp_string(&self) -> JsObject {
        self.regexp_string.clone()
    }

    /// Returns the `MapIteratorPrototype` object.
    #[inline]
    #[must_use]
    pub fn map(&self) -> JsObject {
        self.map.clone()
    }

    /// Returns the `%SegmentIteratorPrototype%` object.
    #[inline]
    #[must_use]
    #[cfg(feature = "intl")]
    pub fn segment(&self) -> JsObject {
        self.segment.clone()
    }

    /// Returns the `%IteratorHelperPrototype%` object.
    #[inline]
    #[must_use]
    pub fn iterator_helper(&self) -> JsObject {
        self.iterator_helper.clone()
    }

    /// Returns the `%WrapForValidIteratorPrototype%` object.
    #[inline]
    #[must_use]
    pub fn wrap_for_valid_iterator(&self) -> JsObject {
        self.wrap_for_valid_iterator.clone()
    }
}

/// `%AsyncIteratorPrototype%` object
///
/// More information:
///  - [ECMA reference][spec]
///
/// [spec]: https://tc39.es/ecma262/#sec-asynciteratorprototype
pub(crate) struct AsyncIterator;

impl IntrinsicObject for AsyncIterator {
    fn init(realm: &Realm) {
        BuiltInBuilder::with_intrinsic::<Self>(realm)
            .static_method(|v, _, _| Ok(v.clone()), JsSymbol::async_iterator(), 0)
            .build();
    }

    fn get(intrinsics: &Intrinsics) -> JsObject {
        intrinsics.objects().iterator_prototypes().async_iterator()
    }
}

/// `CreateIterResultObject( value, done )`
///
/// Generates an object supporting the `IteratorResult` interface.
pub fn create_iter_result_object(value: JsValue, done: bool, context: &mut Context) -> JsValue {
    // 1. Assert: Type(done) is Boolean.
    // 2. Let obj be ! OrdinaryObjectCreate(%Object.prototype%).
    // 3. Perform ! CreateDataPropertyOrThrow(obj, "value", value).
    // 4. Perform ! CreateDataPropertyOrThrow(obj, "done", done).
    let obj = context
        .intrinsics()
        .templates()
        .iterator_result()
        .create(OrdinaryObject, vec![value, done.into()]);

    // 5. Return obj.
    obj.into()
}

/// The result object of a built-in iterator's `next` for one step: `None` is the final
/// `{ value: undefined, done: true }`.
pub(crate) fn step_to_result_object(step: Option<JsValue>, context: &mut Context) -> JsValue {
    match step {
        Some(value) => create_iter_result_object(value, false, context),
        None => create_iter_result_object(JsValue::undefined(), true, context),
    }
}

/// The built-in iterators whose `next` can be run without making its result object.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Stepper {
    Array,
    Map,
    Set,
    String,
}

impl Stepper {
    /// The stepper for `next_method`, if it is the untouched `next` of one of the built-in
    /// iterators of the running realm.
    ///
    /// Such a `next` makes a fresh result object that only `IteratorStep` ever reads, and reading
    /// the `done` and `value` of a fresh ordinary object runs no user code, so the object can be
    /// skipped. A function from another realm is left alone: `next` would run there, creating its
    /// arrays with that realm's prototypes.
    fn of(next_method: &JsValue, context: &Context) -> Option<Self> {
        let function = next_method.as_object()?;
        let native = function.downcast_ref::<NativeFunctionObject>()?;
        if native.realm.as_ref() != Some(context.realm()) {
            return None;
        }
        let pointer = native.f.as_fn_ptr()?;
        let is = |next: NativeFunctionPointer| pointer as usize == next as usize;
        if is(ArrayIterator::next) {
            Some(Self::Array)
        } else if is(MapIterator::next) {
            Some(Self::Map)
        } else if is(SetIterator::next) {
            Some(Self::Set)
        } else if is(StringIterator::next) {
            Some(Self::String)
        } else {
            None
        }
    }

    /// The body of the iterator's `next` on `iterator`: the next value, or `None` once the
    /// iterator is exhausted.
    pub(crate) fn advance(
        self,
        iterator: &JsObject,
        context: &mut Context,
    ) -> JsResult<Option<JsValue>> {
        match self {
            Self::Array => ArrayIterator::advance(iterator, context),
            Self::Map => MapIterator::advance(iterator, context),
            Self::Set => SetIterator::advance(iterator, context),
            Self::String => StringIterator::advance(iterator, context),
        }
    }

    /// [`advance`](Self::advance) when it is known to run no JavaScript and not to fail;
    /// [`Declined`], with nothing changed, when that cannot be known.
    #[cfg(feature = "jit")]
    fn advance_quietly(
        self,
        iterator: &JsObject,
        context: &mut Context,
    ) -> Result<Option<JsValue>, Declined> {
        let right_type = match self {
            Self::Array => return ArrayIterator::advance_dense(iterator),
            Self::Map => iterator.is::<MapIterator>(),
            Self::Set => iterator.is::<SetIterator>(),
            Self::String => iterator.is::<StringIterator>(),
        };
        // These read nothing but their own state and make arrays and substrings.
        if right_type {
            self.advance(iterator, context).map_err(|_| Declined)
        } else {
            Err(Declined)
        }
    }
}

/// A step that was not made because it might run JavaScript or fail.
#[cfg(feature = "jit")]
pub(crate) struct Declined;

/// Iterator hint for `GetIterator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IteratorHint {
    /// Hints that the iterator should be sync.
    Sync,

    /// Hints that the iterator should be async.
    Async,
}

impl JsValue {
    /// `GetIteratorFromMethod ( obj, method )`
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-getiteratorfrommethod
    pub fn get_iterator_from_method(
        &self,
        method: &JsObject,
        context: &mut Context,
    ) -> JsResult<IteratorRecord> {
        // 1. Let iterator be ? Call(method, obj).
        let iterator = method.call(self, &[], context)?;
        // 2. If iterator is not an Object, throw a TypeError exception.
        let iterator_obj = iterator.as_object().ok_or_else(|| {
            JsNativeError::typ().with_message("returned iterator is not an object")
        })?;
        // 3. Let nextMethod be ? Get(iterator, "next").
        let next_method = iterator_obj.get(js_string!("next"), context)?;
        // 4. Let iteratorRecord be the Iterator Record { [[Iterator]]: iterator, [[NextMethod]]: nextMethod, [[Done]]: false }.
        // 5. Return iteratorRecord.
        let mut record = IteratorRecord::new(iterator_obj.clone(), next_method);
        record.stepper = Stepper::of(&record.next_method, context);
        Ok(record)
    }

    /// `GetIterator ( obj, kind )`
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-getiterator
    pub fn get_iterator(
        &self,
        hint: IteratorHint,
        context: &mut Context,
    ) -> JsResult<IteratorRecord> {
        let method = match hint {
            // 1. If kind is async, then
            IteratorHint::Async => {
                // a. Let method be ? GetMethod(obj, %Symbol.asyncIterator%).
                let Some(method) = self.get_method(JsSymbol::async_iterator(), context)? else {
                    // b. If method is undefined, then
                    //     i. Let syncMethod be ? GetMethod(obj, %Symbol.iterator%).
                    let sync_method =
                        self.get_method(JsSymbol::iterator(), context)?
                            .ok_or_else(|| {
                                // ii. If syncMethod is undefined, throw a TypeError exception.
                                JsNativeError::typ().with_message(format!(
                                    "value with type `{}` is not iterable",
                                    self.type_of()
                                ))
                            })?;
                    // iii. Let syncIteratorRecord be ? GetIteratorFromMethod(obj, syncMethod).
                    let sync_iterator_record =
                        self.get_iterator_from_method(&sync_method, context)?;
                    // iv. Return CreateAsyncFromSyncIterator(syncIteratorRecord).
                    return Ok(AsyncFromSyncIterator::create(sync_iterator_record, context));
                };

                Some(method)
            }
            // 2. Else,
            IteratorHint::Sync => {
                // a. Let method be ? GetMethod(obj, %Symbol.iterator%).
                self.get_method(JsSymbol::iterator(), context)?
            }
        };

        let method = method.ok_or_else(|| {
            // 3. If method is undefined, throw a TypeError exception.
            JsNativeError::typ().with_message(format!(
                "value with type `{}` is not iterable",
                self.type_of()
            ))
        })?;

        // 4. Return ? GetIteratorFromMethod(obj, method).
        self.get_iterator_from_method(&method, context)
    }
}

/// The result of the iteration process.
#[derive(Debug, Clone, Trace, Finalize)]
pub struct IteratorResult {
    object: JsObject,
}

impl IteratorResult {
    /// Gets a new `IteratorResult` from a value. Returns `Err` if
    /// the value is not a [`JsObject`]
    pub(crate) fn from_value(value: JsValue) -> JsResult<Self> {
        if let Some(object) = value.into_object() {
            Ok(Self { object })
        } else {
            Err(JsNativeError::typ()
                .with_message("next value should be an object")
                .into())
        }
    }

    /// Gets the inner object of this `IteratorResult`.
    pub(crate) const fn object(&self) -> &JsObject {
        &self.object
    }

    /// `IteratorComplete ( iterResult )`
    ///
    /// The abstract operation `IteratorComplete` takes argument `iterResult` (an `Object`) and
    /// returns either a normal completion containing a `Boolean` or a throw completion.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-iteratorcomplete
    #[inline]
    pub fn complete(&self, context: &mut Context) -> JsResult<bool> {
        // 1. Return ToBoolean(? Get(iterResult, "done")).
        Ok(self.object.get(js_string!("done"), context)?.to_boolean())
    }

    /// `IteratorValue ( iterResult )`
    ///
    /// The abstract operation `IteratorValue` takes argument `iterResult` (an `Object`) and
    /// returns either a normal completion containing an ECMAScript language value or a throw
    /// completion.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-iteratorvalue
    #[inline]
    pub fn value(&self, context: &mut Context) -> JsResult<JsValue> {
        // 1. Return ? Get(iterResult, "value").
        self.object.get(js_string!("value"), context)
    }
}

/// Iterator Record
///
/// An Iterator Record is a Record value used to encapsulate an
/// `Iterator` or `AsyncIterator` along with the `next` method.
///
/// More information:
///  - [ECMA reference][spec]
///
/// [spec]: https://tc39.es/ecma262/#sec-iterator-records
#[derive(Clone, Debug, Finalize, Trace)]
pub struct IteratorRecord {
    /// `[[Iterator]]`
    ///
    /// An object that conforms to the `Iterator` or `AsyncIterator` interface.
    iterator: JsObject,

    /// `[[NextMethod]]`
    ///
    /// The `next` method of the `[[Iterator]]` object.
    next_method: JsValue,

    /// `[[Done]]`
    ///
    /// Whether the iterator has been closed.
    done: bool,

    /// The result of the last call to `next`.
    last_result: LastResult,

    /// How to step this record without making result objects, when `next_method` is a built-in
    /// iterator's own `next`. Decided once, when the record is made by `GetIterator`.
    #[unsafe_ignore_trace]
    stepper: Option<Stepper>,
}

/// The result of the last step of an [`IteratorRecord`].
#[derive(Clone, Debug, Finalize, Trace)]
enum LastResult {
    /// The object a `next` call returned.
    Object(IteratorResult),

    /// The value of a step made without a result object. After the last step it is `undefined`,
    /// as in the object the built-in `next` would have made.
    Value(JsValue),
}

impl IteratorRecord {
    /// Creates a new `IteratorRecord` with the given iterator object, next method and `done` flag.
    #[inline]
    #[must_use]
    pub fn new(iterator: JsObject, next_method: JsValue) -> Self {
        Self {
            iterator,
            next_method,
            done: false,
            last_result: LastResult::Value(JsValue::undefined()),
            stepper: None,
        }
    }

    /// Get the `[[Iterator]]` field of the `IteratorRecord`.
    pub(crate) const fn iterator(&self) -> &JsObject {
        &self.iterator
    }

    /// Gets the `[[NextMethod]]` field of the `IteratorRecord`.
    pub(crate) const fn next_method(&self) -> &JsValue {
        &self.next_method
    }

    /// Gets the last result object of the iterator record, making it if the last step was made
    /// without one.
    pub(crate) fn last_result_object(&self, context: &mut Context) -> JsObject {
        match &self.last_result {
            LastResult::Object(result) => result.object().clone(),
            LastResult::Value(value) => {
                create_iter_result_object(value.clone(), self.done, context)
                    .into_object()
                    .expect("an iterator result is an object")
            }
        }
    }

    /// Runs `f`, setting the `done` field of this `IteratorRecord` to `true` if `f` returns
    /// an error.
    fn set_done_on_err<R, F>(&mut self, f: F) -> JsResult<R>
    where
        F: FnOnce(&mut Self) -> JsResult<R>,
    {
        let result = f(self);
        if result.is_err() {
            self.done = true;
        }
        result
    }

    /// Gets the current value of the `IteratorRecord`.
    pub(crate) fn value(&mut self, context: &mut Context) -> JsResult<JsValue> {
        self.set_done_on_err(|iter| match &iter.last_result {
            LastResult::Object(result) => result.value(context),
            LastResult::Value(value) => Ok(value.clone()),
        })
    }

    /// Get the `[[Done]]` field of the `IteratorRecord`.
    pub(crate) const fn done(&self) -> bool {
        self.done
    }

    /// Updates the current result value of this iterator record.
    pub(crate) fn update_result(&mut self, result: JsValue, context: &mut Context) -> JsResult<()> {
        self.set_done_on_err(|iter| {
            // 3. If Type(result) is not Object, throw a TypeError exception.
            // 4. Return result.
            // `IteratorResult::from_value` does this for us.

            // `IteratorStep(iteratorRecord)`
            // https://tc39.es/ecma262/#sec-iteratorstep

            // 1. Let result be ? IteratorNext(iteratorRecord).
            let result = IteratorResult::from_value(result)?;
            // 2. Let done be ? IteratorComplete(result).
            // 3. If done is true, return false.
            iter.done = result.complete(context)?;

            iter.last_result = LastResult::Object(result);

            Ok(())
        })
    }

    /// `IteratorNext ( iteratorRecord [ , value ] )`
    ///
    /// The abstract operation `IteratorNext` takes argument `iteratorRecord` (an `Iterator`
    /// Record) and optional argument `value` (an ECMAScript language value) and returns either a
    /// normal completion containing an `Object` or a throw completion.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-iteratornext
    pub(crate) fn next(
        &mut self,
        value: Option<&JsValue>,
        context: &mut Context,
    ) -> JsResult<IteratorResult> {
        // 1. If value is not present, then
        //     a. Let result be Completion(Call(iteratorRecord.[[NextMethod]], iteratorRecord.[[Iterator]])).
        // 2. Else,
        //     a. Let result be Completion(Call(iteratorRecord.[[NextMethod]], iteratorRecord.[[Iterator]], « value »)).
        // 3. If result is a throw completion, then
        //     a. Set iteratorRecord.[[Done]] to true.
        //     b. Return ? result.
        // 4. Set result to ! result.
        // 5. If result is not an Object, then
        //     a. Set iteratorRecord.[[Done]] to true.
        //     b. Throw a TypeError exception.
        // 6. Return result.
        // NOTE: In this case, `set_done_on_err` does all the heavylifting for us, which
        // simplifies the instructions below.
        self.set_done_on_err(|iter| {
            iter.next_method
                .call(
                    &iter.iterator.clone().into(),
                    value.map_or(&[], std::slice::from_ref),
                    context,
                )
                .and_then(IteratorResult::from_value)
        })
    }

    /// How to step this record without a result object, when its `next` is a built-in
    /// iterator's own.
    pub(crate) const fn stepper(&self) -> Option<Stepper> {
        self.stepper
    }

    /// The value of the last step if it was made without a result object.
    pub(crate) const fn unboxed_value(&self) -> Option<&JsValue> {
        match &self.last_result {
            LastResult::Value(value) => Some(value),
            LastResult::Object(_) => None,
        }
    }

    /// Steps a record that has a [`Stepper`] when that runs no JavaScript and cannot fail, as
    /// fast helpers of compiled code need; [`Declined`], with nothing changed, for any other.
    #[cfg(feature = "jit")]
    pub(crate) fn step_quietly(&mut self, context: &mut Context) -> Result<(), Declined> {
        let step = self
            .stepper
            .ok_or(Declined)?
            .advance_quietly(&self.iterator, context)?;
        self.finish_unboxed_step(Ok(step))
            .map(|_| ())
            .map_err(|_| Declined)
    }

    /// Records the outcome of `advance` (the next value, `None` at the end, or an error) as the
    /// last step, and returns whether the iterator is done. The built-in `next` has no result
    /// object here for anyone to read, so the value is all there is.
    pub(crate) fn finish_unboxed_step(
        &mut self,
        step: JsResult<Option<JsValue>>,
    ) -> JsResult<bool> {
        self.set_done_on_err(|iter| {
            let step = step?;
            iter.done = step.is_none();
            iter.last_result = LastResult::Value(step.unwrap_or_default());
            Ok(iter.done)
        })
    }

    /// `IteratorStep ( iteratorRecord )`
    ///
    /// Updates the `IteratorRecord` and returns `true` if the next result record returned
    /// `done: true`, otherwise returns `false`. This differs slightly from the spec, but also
    /// simplifies some logic around iterators.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-iteratorstep
    pub(crate) fn step(&mut self, context: &mut Context) -> JsResult<bool> {
        self.set_done_on_err(|iter| {
            if let Some(stepper) = iter.stepper {
                let step = stepper.advance(&iter.iterator, context);
                return iter.finish_unboxed_step(step);
            }

            // 1. Let result be ? IteratorNext(iteratorRecord).
            let result = iter.next(None, context)?;

            // 2. Let done be Completion(IteratorComplete(result)).
            // 3. If done is a throw completion, then
            //     a. Set iteratorRecord.[[Done]] to true.
            //     b. Return ? done.
            // 4. Set done to ! done.
            // 5. If done is true, then
            //     a. Set iteratorRecord.[[Done]] to true.
            //     b. Return done.
            iter.done = result.complete(context)?;

            iter.last_result = LastResult::Object(result);

            // 6. Return result.
            Ok(iter.done)
        })
    }

    /// `IteratorStepValue ( iteratorRecord )`
    ///
    /// Updates the `IteratorRecord` and returns `Some(value)` if the next result record returned
    /// `done: true`, otherwise returns `None`.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-iteratorstepvalue
    pub(crate) fn step_value(&mut self, context: &mut Context) -> JsResult<Option<JsValue>> {
        // 1. Let result be ? IteratorStep(iteratorRecord).
        if self.step(context)? {
            // 2. If result is done, then
            //     a. Return done.
            Ok(None)
        } else {
            // 3. Let value be Completion(IteratorValue(result)).
            // 4. If value is a throw completion, then
            //     a. Set iteratorRecord.[[Done]] to true.
            // 5. Return ? value.
            self.value(context).map(Some)
        }
    }

    /// [`IfAbruptCloseIterator( value, iteratorRecord )`][spec], but
    /// adapted to be used inside `NativeCoroutine`.
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-ifabruptcloseiterator
    pub(crate) fn if_abrupt_close_iterator(
        &self,
        completion: CompletionRecord,
        context: &mut Context,
    ) -> CoroutineState {
        // 1. Assert: value is a Completion Record.
        // 2. If value is an abrupt completion, return ? IteratorClose(iteratorRecord, value).
        // 3. Set value to ! value.
        match completion {
            CompletionRecord::Return(value) => {
                self.close(Ok(value), context).branch()?;
                CoroutineState::Break(Ok(()))
            }
            CompletionRecord::Throw(err) => self.close(Err(err), context).branch(),
            CompletionRecord::Normal(value) => CoroutineState::Continue(value),
        }
    }

    /// `IteratorClose ( iteratorRecord, completion )`
    ///
    /// The abstract operation `IteratorClose` takes arguments `iteratorRecord` (an
    /// [Iterator Record][Self]) and `completion` (a `Completion` Record) and returns a
    /// `Completion` Record. It is used to notify an iterator that it should perform any actions it
    /// would normally perform when it has reached its completed state.
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    ///  [spec]: https://tc39.es/ecma262/#sec-iteratorclose
    pub(crate) fn close(
        &self,
        completion: JsResult<JsValue>,
        context: &mut Context,
    ) -> JsResult<JsValue> {
        // 1. Assert: Type(iteratorRecord.[[Iterator]]) is Object.

        // 2. Let iterator be iteratorRecord.[[Iterator]].
        let iterator = &self.iterator;

        // 3. Let innerResult be Completion(GetMethod(iterator, "return")).
        let inner_result = iterator.get_method(js_string!("return"), context);

        // 4. If innerResult.[[Type]] is normal, then
        let inner_result = match inner_result {
            Ok(inner_result) => {
                // a. Let return be innerResult.[[Value]].
                let r#return = inner_result;

                if let Some(r#return) = r#return {
                    // c. Set innerResult to Completion(Call(return, iterator)).
                    r#return.call(&iterator.clone().into(), &[], context)
                } else {
                    // b. If return is undefined, return ? completion.
                    return completion;
                }
            }
            Err(inner_result) => {
                // 5. If completion.[[Type]] is throw, return ? completion.
                completion?;

                // 6. If innerResult.[[Type]] is throw, return ? innerResult.
                return Err(inner_result);
            }
        };

        // 5. If completion.[[Type]] is throw, return ? completion.
        let completion = completion?;

        // 6. If innerResult.[[Type]] is throw, return ? innerResult.
        let inner_result = inner_result?;

        if inner_result.is_object() {
            // 8. Return ? completion.
            Ok(completion)
        } else {
            // 7. If Type(innerResult.[[Value]]) is not Object, throw a TypeError exception.
            Err(JsNativeError::typ()
                .with_message("inner result was not an object")
                .into())
        }
    }

    /// `IteratorToList ( iteratorRecord )`
    ///
    /// More information:
    ///  - [ECMA reference][spec]
    ///
    ///  [spec]: https://tc39.es/ecma262/#sec-iteratortolist
    pub(crate) fn into_list(mut self, context: &mut Context) -> JsResult<Vec<JsValue>> {
        // 1. Let values be a new empty List.
        let mut values = Vec::new();

        // 2. Repeat,
        //     a. Let next be ? IteratorStepValue(iteratorRecord).
        while let Some(value) = self.step_value(context)? {
            // c. Append next to values.
            values.push(value);
        }

        //     b. If next is done, then
        //         i. Return values.
        Ok(values)
    }
}

/// `GetIteratorDirect ( obj )`
///
/// The abstract operation `GetIteratorDirect` takes argument `obj` (an Object)
/// and returns either a normal completion containing an Iterator Record or a
/// throw completion.
///
/// More information:
///  - [ECMAScript reference][spec]
///
/// [spec]: https://tc39.es/ecma262/#sec-getiteratordirect
pub(crate) fn get_iterator_direct(
    obj: &JsObject,
    context: &mut Context,
) -> JsResult<IteratorRecord> {
    // 1. Let nextMethod be ? Get(obj, "next").
    let next_method = obj.get(js_string!("next"), context)?;
    // 2. Let iteratorRecord be the Iterator Record { [[Iterator]]: obj, [[NextMethod]]: nextMethod, [[Done]]: false }.
    // 3. Return iteratorRecord.
    Ok(IteratorRecord::new(obj.clone(), next_method))
}

/// `GetIteratorFlattenable ( obj, stringHandling )`
///
/// The abstract operation `GetIteratorFlattenable` takes arguments `obj` (an ECMAScript
/// language value) and `stringHandling` (iterate-strings or reject-strings) and returns
/// either a normal completion containing an Iterator Record or a throw completion.
///
/// More information:
///  - [ECMAScript reference][spec]
///
/// [spec]: https://tc39.es/ecma262/#sec-getiteratorflattenable
pub(crate) fn get_iterator_flattenable(
    obj: &JsValue,
    iterate_strings: bool,
    context: &mut Context,
) -> JsResult<IteratorRecord> {
    // 1. If obj is not an Object, then
    if !obj.is_object() {
        // a. If stringHandling is reject-strings or obj is not a String, throw a TypeError exception.
        if !iterate_strings || !obj.is_string() {
            return Err(JsNativeError::typ()
                .with_message("GetIteratorFlattenable: value is not an object")
                .into());
        }
    }

    // 2. Let method be ? GetMethod(obj, @@iterator).
    let method = obj.get_method(JsSymbol::iterator(), context)?;

    match method {
        // 3. If method is undefined, then
        None => {
            // a. Let iterator be obj.
            let iterator = obj.as_object().ok_or_else(|| {
                JsNativeError::typ()
                    .with_message("GetIteratorFlattenable: value is not iterable and not an object")
            })?;

            // b. Return ? GetIteratorDirect(iterator).
            get_iterator_direct(&iterator, context)
        }
        // 4. Else,
        Some(method) => {
            // a. Let iterator be ? Call(method, obj).
            let iterator = method.call(obj, &[], context)?;

            // b. If iterator is not an Object, throw a TypeError exception.
            let iterator_obj = iterator.as_object().ok_or_else(|| {
                JsNativeError::typ()
                    .with_message("GetIteratorFlattenable: iterator result is not an object")
            })?;

            // c. Return ? GetIteratorDirect(iterator).
            get_iterator_direct(&iterator_obj, context)
        }
    }
}
