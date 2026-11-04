use crate::JsExpect;
use crate::JsValue;
use crate::value::JsVariant;
use crate::vm::opcode::{IndexOperand, RegisterOperand, nullish_base_error};
use crate::{
    Context, JsNativeError, JsObject, JsResult,
    builtins::{
        function::set_function_name, typed_array::object::typed_array_element_set_shortcut,
    },
    object::{
        internal_methods::{
            InternalMethodPropertyContext, InternalObjectMethods, ORDINARY_INTERNAL_METHODS,
        },
        shape::slot::SlotAttributes,
    },
    property::{PropertyDescriptor, PropertyKey},
    vm::opcode::Operation,
};
use boa_macros::js_str;

/// Whether `vtable` runs the ordinary object's property internal methods.
///
/// A pointer comparison against `ORDINARY_INTERNAL_METHODS` would compare the address of a `const`
/// that the compiler is free to materialise more than once, so the methods are compared by the
/// function they call instead. The three property methods the shortcut replaces are checked, and so
/// is `[[Set]]`: the object's own `[[Set]]` is what runs first, and `OrdinarySet` on a missing key
/// calls `[[Set]]` on the prototype, so an override of it on the object or on any prototype changes
/// the outcome.
fn has_ordinary_property_methods(vtable: &InternalObjectMethods) -> bool {
    std::ptr::fn_addr_eq(
        vtable.__get_own_property__,
        ORDINARY_INTERNAL_METHODS.__get_own_property__,
    ) && std::ptr::fn_addr_eq(
        vtable.__get_prototype_of__,
        ORDINARY_INTERNAL_METHODS.__get_prototype_of__,
    ) && std::ptr::fn_addr_eq(
        vtable.__define_own_property__,
        ORDINARY_INTERNAL_METHODS.__define_own_property__,
    ) && std::ptr::fn_addr_eq(vtable.__set__, ORDINARY_INTERNAL_METHODS.__set__)
}

/// Sets `key` on `object` when no part of `[[Set]]` can be observed.
///
/// `OrdinarySet` on a receiver without an own property walks the prototype chain, where a setter or
/// a non-writable data property makes the assignment fail; anything exotic can also intercept it.
/// The answer is therefore only known up front when the object is ordinary and extensible, every
/// prototype is ordinary and lacks the key, and the receiver is the object itself. That is exactly
/// when `CreateDataProperty` inserts the value with the default attributes, so the walk is replaced
/// by one insert.
fn set_new_own_property(object: &JsObject, key: &PropertyKey, value: JsValue) -> bool {
    if !has_ordinary_property_methods(object.vtable()) {
        return false;
    }
    {
        let borrowed = object.borrow();
        if !borrowed.extensible || borrowed.properties().get(key).is_some() {
            return false;
        }
    }

    let mut current = object.prototype();
    while let Some(prototype) = current {
        if !has_ordinary_property_methods(prototype.vtable()) {
            return false;
        }
        let borrowed = prototype.borrow();
        if borrowed.properties().get(key).is_some() {
            return false;
        }
        current = borrowed.shape().prototype();
    }

    object.borrow_mut().properties.insert(
        key,
        PropertyDescriptor::builder()
            .value(value)
            .writable(true)
            .enumerable(true)
            .configurable(true)
            .build(),
    );
    true
}

fn set_by_name(
    value: RegisterOperand,
    value_object: &JsValue,
    receiver: &JsValue,
    index: IndexOperand,
    context: &mut Context,
) -> JsResult<()> {
    let value = context.vm.get_register(value.into()).clone();

    if value_object.is_null_or_undefined() {
        let name = context.vm.frame().code_block().ic[usize::from(index)]
            .name
            .clone();
        return Err(nullish_base_error(value_object, Some(&name.into()), true));
    }
    let object = value_object.to_object(context)?;

    let ic = &context.vm.frame().code_block().ic[usize::from(index)];

    let object_borrowed = object.borrow();
    if let Some((shape, slot)) = ic.get(object_borrowed.shape()) {
        let slot_index = slot.index as usize;

        if slot.attributes.is_accessor_descriptor() {
            let result = if slot.attributes.contains(SlotAttributes::PROTOTYPE) {
                let prototype = shape.prototype().expect("prototype should have value");
                let prototype = prototype.borrow();

                prototype.properties().storage[slot_index + 1].clone()
            } else {
                object_borrowed.properties().storage[slot_index + 1].clone()
            };

            drop(object_borrowed);
            if slot.attributes.has_set() && result.is_object() {
                result.as_object().expect("should contain getter").call(
                    receiver,
                    std::slice::from_ref(&value),
                    context,
                )?;
            }
        } else if slot.attributes.contains(SlotAttributes::PROTOTYPE) {
            let prototype = shape.prototype().expect("prototype should have value");
            let mut prototype = prototype.borrow_mut();

            prototype.properties_mut().storage[slot_index] = value.clone();
        } else {
            drop(object_borrowed);
            let mut object_borrowed = object.borrow_mut();
            object_borrowed.properties_mut().storage[slot_index] = value.clone();
        }
        return Ok(());
    }
    drop(object_borrowed);

    let name: PropertyKey = ic.name.clone().into();

    // `[[Set]]` inserts on the receiver, so the shortcut only applies when the receiver is the
    // object being written through.
    if let Some(receiver_object) = receiver.as_object()
        && JsObject::equals(&object, &receiver_object)
        && set_new_own_property(&object, &name, value.clone())
    {
        return Ok(());
    }

    let context = &mut InternalMethodPropertyContext::new(context);
    let succeeded = object.__set__(name.clone(), value.clone(), receiver.clone(), context)?;
    if !succeeded && context.vm.frame().code_block.strict() {
        return Err(JsNativeError::typ()
            .with_message(format!("cannot set non-writable property: {name}"))
            .into());
    }

    // Cache the property. A set of `length` is never cached: on an array it has to run
    // ArraySetLength (convert, validate, truncate), not overwrite the slot, and an
    // ordinary object can share an array's shape, so the shape alone can't tell them apart.
    let slot = *context.slot();
    if succeeded
        && slot.is_cacheable()
        && !matches!(&name, PropertyKey::String(s) if *s == js_str!("length"))
    {
        let ic = &context.vm.frame().code_block.ic[usize::from(index)];
        let object_borrowed = object.borrow();
        let shape = object_borrowed.shape();
        ic.set(shape, slot);
    }

    Ok(())
}

/// `SetPropertyByName` implements the Opcode Operation for `Opcode::SetPropertyByName`
///
/// Operation:
///  - Sets a property by name of an object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetPropertyByName;

impl SetPropertyByName {
    #[inline(always)]
    pub(crate) fn operation(
        (value, object, index): (RegisterOperand, RegisterOperand, IndexOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let object = context.vm.get_register(object.into()).clone();
        set_by_name(value, &object, &object, index, context)
    }
}

impl Operation for SetPropertyByNameWithThis {
    const NAME: &'static str = "SetPropertyByNameWithThis";
    const INSTRUCTION: &'static str = "INST - SetPropertyByNameWithThis";
    const COST: u8 = 4;
}

/// `SetPropertyByNameWithThis` implements the Opcode Operation for `Opcode::SetPropertyByNameWithThis`
///
/// Operation:
///  - Sets a property by name of an object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetPropertyByNameWithThis;

impl SetPropertyByNameWithThis {
    #[inline(always)]
    pub(crate) fn operation(
        (value, receiver, object, index): (
            RegisterOperand,
            RegisterOperand,
            RegisterOperand,
            IndexOperand,
        ),
        context: &mut Context,
    ) -> JsResult<()> {
        let value_object = context.vm.get_register(object.into()).clone();
        let receiver = context.vm.get_register(receiver.into()).clone();
        set_by_name(value, &value_object, &receiver, index, context)
    }
}

impl Operation for SetPropertyByName {
    const NAME: &'static str = "SetPropertyByName";
    const INSTRUCTION: &'static str = "INST - SetPropertyByName";
    const COST: u8 = 4;
}

/// `SetPropertyByValue` implements the Opcode Operation for `Opcode::SetPropertyByValue`
///
/// Operation:
///  - Sets a property by value of an object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetPropertyByValue;

impl SetPropertyByValue {
    #[inline(always)]
    pub(crate) fn operation(
        (value, key, receiver, object): (
            RegisterOperand,
            RegisterOperand,
            RegisterOperand,
            RegisterOperand,
        ),
        context: &mut Context,
    ) -> JsResult<()> {
        let value = context.vm.get_register(value.into()).clone();
        let key = context.vm.get_register(key.into()).clone();
        let receiver = context.vm.get_register(receiver.into()).clone();
        let object = context.vm.get_register(object.into()).clone();
        if object.is_null_or_undefined() {
            return Err(nullish_base_error(&object, Some(&key), true));
        }
        let object = object.to_object(context)?;

        let key = key.to_property_key(context)?;

        // Fast Path: both shortcuts store on `object`, which is where `[[Set]]` stores only when
        // the receiver is that object; `super[key] = value` has the prototype as `object`.
        'fast_path: {
            if let PropertyKey::Index(index) = &key
                && receiver
                    .as_object()
                    .is_some_and(|receiver| JsObject::equals(&object, &receiver))
            {
                if object.is_array() {
                    let mut object_borrowed = object.borrow_mut();

                    // Cannot modify if not extensible.
                    if !object_borrowed.extensible {
                        break 'fast_path;
                    }

                    if object_borrowed
                        .properties_mut()
                        .set_dense_property(index.get(), &value)
                    {
                        return Ok(());
                    }
                }

                // The typed array shortcut also refuses a conversion that could run user code.
                if let Some(result) =
                    typed_array_element_set_shortcut(&object, index.get(), &value, context)
                {
                    return result;
                }
            }
        }

        // Slow path:
        let succeeded = object.__set__(
            key.clone(),
            value.clone(),
            receiver.clone(),
            &mut context.into(),
        )?;
        if !succeeded && context.vm.frame().code_block.strict() {
            return Err(JsNativeError::typ()
                .with_message(format!("cannot set non-writable property: {key}"))
                .into());
        }

        Ok(())
    }
}

impl Operation for SetPropertyByValue {
    const NAME: &'static str = "SetPropertyByValue";
    const INSTRUCTION: &'static str = "INST - SetPropertyByValue";
    const COST: u8 = 4;
}

/// `SetPropertyGetterByName` implements the Opcode Operation for `Opcode::SetPropertyGetterByName`
///
/// Operation:
///  - Sets a getter property by name of an object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetPropertyGetterByName;

impl SetPropertyGetterByName {
    #[inline(always)]
    pub(crate) fn operation(
        (object, value, index): (RegisterOperand, RegisterOperand, IndexOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let object = context.vm.get_register(object.into()).clone();
        let value = context.vm.get_register(value.into()).clone();
        let name = context
            .vm
            .frame()
            .code_block()
            .constant_string(index.into())
            .into();

        let object = object.to_object(context)?;
        let set = object
            .__get_own_property__(&name, &mut InternalMethodPropertyContext::new(context))?
            .as_ref()
            .and_then(PropertyDescriptor::set)
            .cloned();
        object.__define_own_property__(
            &name,
            PropertyDescriptor::builder()
                .maybe_get(Some(value.clone()))
                .maybe_set(set)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        Ok(())
    }
}

impl Operation for SetPropertyGetterByName {
    const NAME: &'static str = "SetPropertyGetterByName";
    const INSTRUCTION: &'static str = "INST - SetPropertyGetterByName";
    const COST: u8 = 4;
}

/// `SetPropertyGetterByValue` implements the Opcode Operation for `Opcode::SetPropertyGetterByValue`
///
/// Operation:
///  - Sets a getter property by value of an object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetPropertyGetterByValue;

impl SetPropertyGetterByValue {
    #[inline(always)]
    pub(crate) fn operation(
        (value, key, object): (RegisterOperand, RegisterOperand, RegisterOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let value = context.vm.get_register(value.into()).clone();
        let key = context.vm.get_register(key.into()).clone();
        let object = context.vm.get_register(object.into()).clone();
        let object = object.to_object(context)?;
        let name = key.to_property_key(context)?;

        let set = object
            .__get_own_property__(&name, &mut InternalMethodPropertyContext::new(context))?
            .as_ref()
            .and_then(PropertyDescriptor::set)
            .cloned();
        object.__define_own_property__(
            &name,
            PropertyDescriptor::builder()
                .maybe_get(Some(value.clone()))
                .maybe_set(set)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        Ok(())
    }
}

impl Operation for SetPropertyGetterByValue {
    const NAME: &'static str = "SetPropertyGetterByValue";
    const INSTRUCTION: &'static str = "INST - SetPropertyGetterByValue";
    const COST: u8 = 4;
}

/// `SetPropertySetterByName` implements the Opcode Operation for `Opcode::SetPropertySetterByName`
///
/// Operation:
///  - Sets a setter property by name of an object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetPropertySetterByName;

impl SetPropertySetterByName {
    #[inline(always)]
    pub(crate) fn operation(
        (object, value, index): (RegisterOperand, RegisterOperand, IndexOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let object = context.vm.get_register(object.into()).clone();
        let value = context.vm.get_register(value.into()).clone();
        let name = context
            .vm
            .frame()
            .code_block()
            .constant_string(index.into())
            .into();

        let object = object.to_object(context)?;

        let get = object
            .__get_own_property__(&name, &mut InternalMethodPropertyContext::new(context))?
            .as_ref()
            .and_then(PropertyDescriptor::get)
            .cloned();
        object.__define_own_property__(
            &name,
            PropertyDescriptor::builder()
                .maybe_set(Some(value.clone()))
                .maybe_get(get)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        Ok(())
    }
}

impl Operation for SetPropertySetterByName {
    const NAME: &'static str = "SetPropertySetterByName";
    const INSTRUCTION: &'static str = "INST - SetPropertySetterByName";
    const COST: u8 = 4;
}

/// `SetPropertySetterByValue` implements the Opcode Operation for `Opcode::SetPropertySetterByValue`
///
/// Operation:
///  - Sets a setter property by value of an object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetPropertySetterByValue;

impl SetPropertySetterByValue {
    #[inline(always)]
    pub(crate) fn operation(
        (value, key, object): (RegisterOperand, RegisterOperand, RegisterOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let value = context.vm.get_register(value.into()).clone();
        let key = context.vm.get_register(key.into()).clone();
        let object = context.vm.get_register(object.into()).clone();

        let object = object.to_object(context)?;
        let name = key.to_property_key(context)?;

        let get = object
            .__get_own_property__(&name, &mut InternalMethodPropertyContext::new(context))?
            .as_ref()
            .and_then(PropertyDescriptor::get)
            .cloned();
        object.__define_own_property__(
            &name,
            PropertyDescriptor::builder()
                .maybe_set(Some(value.clone()))
                .maybe_get(get)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        Ok(())
    }
}

impl Operation for SetPropertySetterByValue {
    const NAME: &'static str = "SetPropertySetterByValue";
    const INSTRUCTION: &'static str = "INST - SetPropertySetterByValue";
    const COST: u8 = 4;
}

/// `SetFunctionName` implements the Opcode Operation for `Opcode::SetFunctionName`
///
/// Operation:
///  - Sets the name of a function object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetFunctionName;

impl SetFunctionName {
    #[inline(always)]
    pub(crate) fn operation(
        (function, name, prefix): (RegisterOperand, RegisterOperand, IndexOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let function = context.vm.get_register(function.into()).clone();
        let name = context.vm.get_register(name.into()).clone();
        let name = match name.variant() {
            JsVariant::String(name) => PropertyKey::from(name.clone()),
            JsVariant::Symbol(name) => PropertyKey::from(name.clone()),
            _ => unreachable!(),
        };

        let prefix = match u32::from(prefix) {
            1 => Some(js_str!("get")),
            2 => Some(js_str!("set")),
            _ => None,
        };

        set_function_name(
            &function
                .as_object()
                .js_expect("function is not an object")?,
            &name,
            prefix,
            context,
        )?;
        Ok(())
    }
}

impl Operation for SetFunctionName {
    const NAME: &'static str = "SetFunctionName";
    const INSTRUCTION: &'static str = "INST - SetFunctionName";
    const COST: u8 = 4;
}
