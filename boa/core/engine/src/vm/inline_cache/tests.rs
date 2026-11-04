use boa_gc::Gc;
use boa_parser::Source;

use crate::{
    Context, JsObject, JsResult, JsValue,
    builtins::{OrdinaryObject, function::OrdinaryFunction},
    js_string,
    object::{
        ObjectInitializer, internal_methods::InternalMethodPropertyContext,
        shape::slot::SlotAttributes,
    },
    property::{Attribute, PropertyDescriptor, PropertyKey},
    vm::CodeBlock,
};

#[test]
fn get_own_property_internal_method() {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);

    let o = context
        .intrinsics()
        .templates()
        .ordinary_object()
        .create(OrdinaryObject, Vec::default());

    let property: PropertyKey = js_string!("prop").into();
    let value = 100;

    o.set(property.clone(), value, true, context)
        .expect("should not fail");

    let context = &mut InternalMethodPropertyContext::new(context);

    assert_eq!(context.slot().index, 0);
    assert_eq!(context.slot().attributes, SlotAttributes::empty());

    o.__get_own_property__(&property, context)
        .expect("should not fail");

    assert!(
        !context.slot().in_prototype(),
        "Since it's an owned property, the prototype bit should not be set"
    );

    assert!(
        context.slot().is_cacheable(),
        "Since it's an owned property, this should be cacheable"
    );

    let shape = o.borrow().shape().clone();

    let slot = shape.lookup(&property);

    assert!(slot.is_some(), "the property should be found in the object");

    let slot = slot.expect("the property should be found in the object");

    assert_eq!(context.slot().index, slot.index);
}

#[test]
fn get_internal_method() {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);

    let o = context
        .intrinsics()
        .templates()
        .ordinary_object()
        .create(OrdinaryObject, Vec::default());

    let property: PropertyKey = js_string!("prop").into();
    let value = 100;

    o.set(property.clone(), value, true, context)
        .expect("should not fail");

    let context = &mut InternalMethodPropertyContext::new(context);

    assert_eq!(context.slot().index, 0);
    assert_eq!(context.slot().attributes, SlotAttributes::empty());

    o.__get__(&property, o.clone().into(), context)
        .expect("should not fail");

    assert!(
        !context.slot().in_prototype(),
        "Since it's an owned property, the prototype bit should not be set"
    );

    assert!(
        context.slot().is_cacheable(),
        "Since it's an owned property, this should be cacheable"
    );

    let shape = o.borrow().shape().clone();

    let slot = shape.lookup(&property);

    assert!(slot.is_some(), "the property should be found in the object");

    let slot = slot.expect("the property should be found in the object");

    assert_eq!(context.slot().index, slot.index);
}

#[test]
fn get_internal_method_in_prototype() {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);

    let o = context
        .intrinsics()
        .templates()
        .ordinary_object()
        .create(OrdinaryObject, Vec::default());

    let property: PropertyKey = js_string!("prop").into();
    let value = 100;

    let prototype = context.intrinsics().constructors().object().prototype();

    prototype
        .set(property.clone(), value, true, context)
        .expect("should not fail");

    let context = &mut InternalMethodPropertyContext::new(context);

    assert_eq!(context.slot().index, 0);
    assert_eq!(context.slot().attributes, SlotAttributes::empty());

    o.__get__(&property, o.clone().into(), context)
        .expect("should not fail");

    assert!(
        context.slot().in_prototype(),
        "Since it's an prototype property, the prototype bit should not be set"
    );

    assert!(
        context.slot().is_cacheable(),
        "Since it's an prototype property, this should be cacheable"
    );

    let shape = prototype.borrow().shape().clone();

    let slot = shape.lookup(&property);

    assert!(slot.is_some(), "the property should be found in the object");

    let slot = slot.expect("the property should be found in the object");

    assert_eq!(context.slot().index, slot.index);
}

#[test]
fn define_own_property_internal_method_non_existent_property() {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);

    let o = context
        .intrinsics()
        .templates()
        .ordinary_object()
        .create(OrdinaryObject, Vec::default());

    let property: PropertyKey = js_string!("prop").into();
    let value = 100;

    o.set(property.clone(), value, true, context)
        .expect("should not fail");

    let context = &mut InternalMethodPropertyContext::new(context);

    assert_eq!(context.slot().index, 0);
    assert_eq!(context.slot().attributes, SlotAttributes::empty());

    o.__define_own_property__(
        &property,
        PropertyDescriptor::builder()
            .value(value)
            .writable(true)
            .configurable(true)
            .enumerable(true)
            .build(),
        context,
    )
    .expect("should not fail");

    assert!(
        !context.slot().in_prototype(),
        "Since it's an owned property, the prototype bit should not be set"
    );

    assert!(
        context.slot().is_cacheable(),
        "Since it's an owned property, this should be cacheable"
    );

    let shape = o.borrow().shape().clone();

    let slot = shape.lookup(&property);

    assert!(slot.is_some(), "the property should be found in the object");

    let slot = slot.expect("the property should be found in the object");

    assert_eq!(context.slot().index, slot.index);
}

#[test]
fn define_own_property_internal_method_existing_property_property() {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);

    let o = context
        .intrinsics()
        .templates()
        .ordinary_object()
        .create(OrdinaryObject, Vec::default());

    let property: PropertyKey = js_string!("prop").into();
    let value = 100;

    o.set(property.clone(), value, true, context)
        .expect("should not fail");

    o.__define_own_property__(
        &property,
        PropertyDescriptor::builder()
            .value(value)
            .writable(true)
            .configurable(true)
            .enumerable(true)
            .build(),
        &mut context.into(),
    )
    .expect("should not fail");

    let context = &mut InternalMethodPropertyContext::new(context);

    assert_eq!(context.slot().index, 0);
    assert_eq!(context.slot().attributes, SlotAttributes::empty());

    o.__define_own_property__(
        &property,
        PropertyDescriptor::builder()
            .value(value + 100)
            .writable(true)
            .configurable(true)
            .enumerable(true)
            .build(),
        context,
    )
    .expect("should not fail");

    assert!(
        !context.slot().in_prototype(),
        "Since it's an owned property, the prototype bit should not be set"
    );

    assert!(
        context.slot().is_cacheable(),
        "Since it's an owned property, this should be cacheable"
    );

    let shape = o.borrow().shape().clone();

    let slot = shape.lookup(&property);

    assert!(slot.is_some(), "the property should be found in the object");

    let slot = slot.expect("the property should be found in the object");

    assert_eq!(context.slot().index, slot.index);
}

#[test]
fn set_internal_method() {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);

    let o = context
        .intrinsics()
        .templates()
        .ordinary_object()
        .create(OrdinaryObject, Vec::default());

    let property: PropertyKey = js_string!("prop").into();
    let value = 100;

    o.set(property.clone(), value, true, context)
        .expect("should not fail");

    let context = &mut InternalMethodPropertyContext::new(context);

    assert_eq!(context.slot().index, 0);
    assert_eq!(context.slot().attributes, SlotAttributes::empty());

    o.__set__(property.clone(), value.into(), o.clone().into(), context)
        .expect("should not fail");

    assert!(
        !context.slot().in_prototype(),
        "Since it's an owned property, the prototype bit should not be set"
    );

    assert!(
        context.slot().is_cacheable(),
        "Since it's an owned property, this should be cacheable"
    );

    let shape = o.borrow().shape().clone();

    let slot = shape.lookup(&property);

    assert!(slot.is_some(), "the property should be found in the object");

    let slot = slot.expect("the property should be found in the object");

    assert_eq!(context.slot().index, slot.index);
}

fn get_codeblock(value: &JsValue) -> Option<(JsObject, Gc<CodeBlock>)> {
    let object = value.as_object()?.clone();
    let code = object.downcast_ref::<OrdinaryFunction>()?.code.clone();

    Some((object, code))
}

#[test]
fn set_property_by_name_set_inline_cache_on_property_load() -> JsResult<()> {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);
    let function = context.eval(Source::from_bytes("(function (o) { return o.test; })"))?;
    let (function, code) = get_codeblock(&function).unwrap();

    assert_eq!(code.ic.len(), 1);
    assert_eq!(code.ic[0].entries.borrow().len(), 0);

    let o = ObjectInitializer::new(context)
        .property(js_string!("test"), 0, Attribute::all())
        .build();
    let o_shape = o.borrow().shape().clone();

    function.call(&JsValue::undefined(), &[o.clone().into()], context)?;

    assert_eq!(code.ic[0].entries.borrow().len(), 1);
    assert_eq!(
        code.ic[0].entries.borrow()[0]
            .shape
            .upgrade()
            .unwrap()
            .to_addr_usize(),
        o_shape.to_addr_usize()
    );

    Ok(())
}

#[test]
fn get_property_by_name_set_inline_cache_on_property_load() -> JsResult<()> {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);
    let function = context.eval(Source::from_bytes("(function (o) { o.test = 30; })"))?;
    let (function, code) = get_codeblock(&function).unwrap();

    assert_eq!(code.ic.len(), 1);
    assert_eq!(code.ic[0].entries.borrow().len(), 0);

    let o = ObjectInitializer::new(context)
        .property(js_string!("test"), 0, Attribute::all())
        .build();
    let o_shape = o.borrow().shape().clone();

    function.call(&JsValue::undefined(), &[o.clone().into()], context)?;

    assert_eq!(code.ic[0].entries.borrow().len(), 1);
    assert_eq!(
        code.ic[0].entries.borrow()[0]
            .shape
            .upgrade()
            .unwrap()
            .to_addr_usize(),
        o_shape.to_addr_usize()
    );

    Ok(())
}

#[test]
fn test_polymorphic_inline_cache() -> JsResult<()> {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);
    let function = context.eval(Source::from_bytes("(function (o) { return o.test; })"))?;
    let (function, code) = get_codeblock(&function).unwrap();

    assert_eq!(code.ic.len(), 1);
    assert_eq!(code.ic[0].entries.borrow().len(), 0);
    assert!(!code.ic[0].megamorphic.get());

    let shapes = vec![
        ObjectInitializer::new(context)
            .property(js_string!("test"), 1, Attribute::all())
            .build(),
        ObjectInitializer::new(context)
            .property(js_string!("a"), 2, Attribute::all())
            .property(js_string!("test"), 3, Attribute::all())
            .build(),
        ObjectInitializer::new(context)
            .property(js_string!("b"), 4, Attribute::all())
            .property(js_string!("test"), 5, Attribute::all())
            .build(),
        ObjectInitializer::new(context)
            .property(js_string!("c"), 6, Attribute::all())
            .property(js_string!("test"), 7, Attribute::all())
            .build(),
    ];

    for o in &shapes {
        function.call(&JsValue::undefined(), &[o.clone().into()], context)?;
    }

    assert_eq!(code.ic[0].entries.borrow().len(), 4);
    assert!(!code.ic[0].megamorphic.get());

    Ok(())
}

#[test]
fn test_megamorphic_inline_cache() -> JsResult<()> {
    let context = &mut Context::default();
    // The bytecode is read before the function first runs.
    context.set_lazy_compile(false);
    let function = context.eval(Source::from_bytes("(function (o) { return o.test; })"))?;
    let (function, code) = get_codeblock(&function).unwrap();

    let shapes = vec![
        ObjectInitializer::new(context)
            .property(js_string!("test"), 1, Attribute::all())
            .build(),
        ObjectInitializer::new(context)
            .property(js_string!("a"), 1, Attribute::all())
            .property(js_string!("test"), 1, Attribute::all())
            .build(),
        ObjectInitializer::new(context)
            .property(js_string!("b"), 1, Attribute::all())
            .property(js_string!("test"), 1, Attribute::all())
            .build(),
        ObjectInitializer::new(context)
            .property(js_string!("c"), 1, Attribute::all())
            .property(js_string!("test"), 1, Attribute::all())
            .build(),
        ObjectInitializer::new(context)
            .property(js_string!("d"), 1, Attribute::all())
            .property(js_string!("test"), 1, Attribute::all())
            .build(),
    ];

    for o in &shapes {
        function.call(&JsValue::undefined(), &[o.clone().into()], context)?;
    }

    assert_eq!(code.ic[0].entries.borrow().len(), 0);
    assert!(code.ic[0].megamorphic.get());

    // Regression check: repeated miss should remain empty
    let o6 = ObjectInitializer::new(context)
        .property(js_string!("e"), 1, Attribute::all())
        .property(js_string!("test"), 1, Attribute::all())
        .build();
    function.call(&JsValue::undefined(), &[o6.clone().into()], context)?;
    assert_eq!(code.ic[0].entries.borrow().len(), 0);
    assert!(code.ic[0].megamorphic.get());

    Ok(())
}

#[test]
fn a_cached_length_set_still_runs_array_set_length() {
    let context = &mut Context::default();
    let result = context
        .eval(Source::from_bytes(
            r#"
            function setLen(o, v) { o.length = v; }
            for (let i = 0; i < 10; i++) setLen([1, 2, 3, 4, 5], 2);
            const fake = Object.create(Array.prototype);
            Object.defineProperty(fake, 'length', { value: 0, writable: true });
            setLen(fake, 1);
            const a = [1, 2, 3, 4, 5];
            setLen(a, 2);
            let threw = '';
            try { setLen([1, 2], -1); } catch (e) { threw = e.name; }
            const b = [1, 2];
            try { setLen(b, {}); } catch (e) { threw += e.name; }
            `${a.length}:${a[3]}:${threw}:${b.length}`
            "#,
        ))
        .expect("the script runs");
    assert_eq!(
        result
            .to_string(context)
            .expect("a string")
            .to_std_string_escaped(),
        "2:undefined:RangeErrorRangeError:2"
    );
}

/// Runs `source` and returns its string result.
fn eval_string(source: &str) -> String {
    let context = &mut Context::default();
    context
        .eval(Source::from_bytes(source))
        .expect("the script runs")
        .to_string(context)
        .expect("a string")
        .to_std_string_escaped()
}

#[test]
fn a_prototype_property_replaced_by_an_accessor_is_not_read_from_the_old_slot() {
    assert_eq!(
        eval_string(
            r#"
            function A() {} A.prototype.x = 'd';
            const a = new A(); const f = (o) => o.x;
            for (let i = 0; i < 10; i++) f(a);
            delete A.prototype.x;
            Object.defineProperty(A.prototype, 'x', { get() { return 'g'; }, configurable: true });
            f(a)
            "#
        ),
        "g"
    );
}

#[test]
fn a_prototype_that_lost_properties_is_not_read_past_its_storage() {
    assert_eq!(
        eval_string(
            r#"
            function B() {} B.prototype.p = 1; B.prototype.q = 2; B.prototype.x = 'd';
            const b = new B(); const g = (o) => o.x;
            for (let i = 0; i < 10; i++) g(b);
            delete B.prototype.p; delete B.prototype.q; delete B.prototype.x;
            String(g(b))
            "#
        ),
        "undefined"
    );
}

#[test]
fn a_changed_prototype_replaces_its_cache_entry() -> JsResult<()> {
    let context = &mut Context::default();
    context.set_lazy_compile(false);
    let function = context.eval(Source::from_bytes("(function (o) { return o.x; })"))?;
    let (function, code) = get_codeblock(&function).unwrap();
    let prototype = context.eval(Source::from_bytes("globalThis.p = { p: 1, x: 'd' }"))?;
    let object = context.eval(Source::from_bytes("Object.create(p)"))?;
    let object = object.as_object().unwrap();
    let call = |context: &mut Context| {
        function.call(&JsValue::undefined(), &[object.clone().into()], context)
    };

    assert_eq!(call(context)?.to_string(context)?, js_string!("d"));
    assert_eq!(code.ic[0].entries.borrow().len(), 1);
    assert!(code.ic[0].entries.borrow()[0].prototype_shape.is_some());

    // The prototype changes shape: the entry is dropped on the next access and recorded again,
    // not added next to the stale one, so the cache never fills up with entries of one receiver.
    for i in 0..10 {
        let name = js_string!(format!("k{i}"));
        let prototype = prototype.as_object().unwrap();
        prototype.delete_property_or_throw(js_string!("p"), context)?;
        prototype.set(name, i, true, context)?;
        assert_eq!(call(context)?.to_string(context)?, js_string!("d"));
        assert_eq!(code.ic[0].entries.borrow().len(), 1);
        assert!(!code.ic[0].megamorphic.get());
        prototype.set(js_string!("p"), 1, true, context)?;
    }
    Ok(())
}

#[test]
fn a_prototype_slot_is_not_cached_for_a_unique_receiver() {
    assert_eq!(
        eval_string(
            r#"
            const p = { x: 'p' }; const a = Object.create(p);
            for (let i = 0; i < 1100; i++) a['k' + i] = i;
            const f = (o) => o.x;
            for (let i = 0; i < 10; i++) f(a);
            a.x = 'own';
            f(a)
            "#
        ),
        "own"
    );
}

#[test]
fn an_added_property_is_cached_per_site_and_a_changed_prototype_replaces_the_entry() -> JsResult<()>
{
    let context = &mut Context::default();
    context.set_lazy_compile(false);
    let function = context.eval(Source::from_bytes("(function (o) { o.x = 1; })"))?;
    let (function, code) = get_codeblock(&function).unwrap();
    let prototype = context.eval(Source::from_bytes("globalThis.p = { p: 1 }"))?;
    let call = |context: &mut Context| -> JsResult<JsValue> {
        let object = context.eval(Source::from_bytes("Object.create(p)"))?;
        function.call(
            &JsValue::undefined(),
            std::slice::from_ref(&object),
            context,
        )?;
        Ok(object)
    };

    // The first add is made the slow way and recorded; the second takes the cache.
    call(context)?;
    assert_eq!(code.ic[0].adds.len(), 1);
    let object = call(context)?;
    assert_eq!(code.ic[0].adds.len(), 1);
    assert_eq!(
        object
            .as_object()
            .unwrap()
            .borrow()
            .properties()
            .storage
            .len(),
        1
    );

    // The prototype changes shape: the entry is replaced, not added to, and the cache does not
    // fill up with the entries of one kind of receiver.
    for i in 0..10 {
        let prototype = prototype.as_object().unwrap();
        prototype.set(js_string!(format!("k{i}")), i, true, context)?;
        call(context)?;
        call(context)?;
        assert_eq!(code.ic[0].adds.len(), 1);
        assert!(!code.ic[0].adds.is_full());
    }
    Ok(())
}

#[test]
fn an_added_property_is_not_cached_past_a_setter_the_chain_gains() {
    assert_eq!(
        eval_string(
            r#"
            const log = [];
            const f = (o) => { o.x = 1; };
            const proto = { p: 1 };
            const make = () => Object.create(proto);
            for (let i = 0; i < 10; i++) f(make());
            Object.defineProperty(Object.prototype, 'x', { set(v) { log.push(v); }, configurable: true });
            const o = make(); f(o);
            const own = Object.keys(o).length;
            delete Object.prototype.x;
            const p = make(); f(p);
            log.length + ':' + own + ':' + Object.keys(p).length
            "#
        ),
        "1:0:1"
    );
}

#[test]
fn a_site_with_many_shapes_stops_recording() -> JsResult<()> {
    let context = &mut Context::default();
    context.set_lazy_compile(false);
    let function = context.eval(Source::from_bytes("(function (o) { o.x = 1; })"))?;
    let (function, code) = get_codeblock(&function).unwrap();
    for i in 0..6 {
        let object = context.eval(Source::from_bytes(&format!(
            "Object.assign(Object.create(null), {{ k{i}: 0 }})"
        )))?;
        function.call(&JsValue::undefined(), &[object], context)?;
    }
    assert!(code.ic[0].adds.is_full());
    assert_eq!(code.ic[0].adds.len(), 0);
    Ok(())
}
