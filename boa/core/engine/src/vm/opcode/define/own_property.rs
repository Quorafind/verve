use crate::{
    Context, JsNativeError, JsObject, JsResult, JsValue,
    object::internal_methods::InternalMethodPropertyContext,
    property::{PropertyDescriptor, PropertyKey},
    vm::opcode::{IndexOperand, Operation, RegisterOperand},
};

/// Defines the own data property `key` of `object` when the receiver cannot observe the creation.
///
/// `ValidateAndApplyPropertyDescriptor` on an ordinary extensible object that lacks the key inserts
/// it unchanged, so the answer is `true` and no user code runs. An array, an arguments object, a
/// module namespace, a proxy or a typed array has its own `[[DefineOwnProperty]]` or shape rules,
/// and a non-extensible object or an existing key has to compare against the current descriptor, so
/// those return `false` and the caller takes the full path.
fn define_new_own_property(object: &JsObject, key: &PropertyKey, value: JsValue) -> bool {
    if !object.is_ordinary() {
        return false;
    }
    let mut borrowed = object.borrow_mut();
    if !borrowed.extensible || borrowed.properties().get(key).is_some() {
        return false;
    }
    borrowed.properties.insert(
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

/// `DefineOwnPropertyByName` implements the Opcode Operation for `Opcode::DefineOwnPropertyByName`
///
/// Operation:
///  - Defines a own property of an object by name.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DefineOwnPropertyByName;

impl DefineOwnPropertyByName {
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
            .constant_string(index.into());
        let object = object.to_object(context)?;
        let key = PropertyKey::from(name);
        if define_new_own_property(&object, &key, value.clone()) {
            return Ok(());
        }
        object.__define_own_property__(
            &key,
            PropertyDescriptor::builder()
                .value(value.clone())
                .writable(true)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        Ok(())
    }
}

impl Operation for DefineOwnPropertyByName {
    const NAME: &'static str = "DefineOwnPropertyByName";
    const INSTRUCTION: &'static str = "INST - DefineOwnPropertyByName";
    const COST: u8 = 4;
}

/// `DefineOwnPropertyByValue` implements the Opcode Operation for `Opcode::DefineOwnPropertyByValue`
///
/// Operation:
///  - Defines a own property of an object by value.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DefineOwnPropertyByValue;

impl DefineOwnPropertyByValue {
    #[inline(always)]
    pub(crate) fn operation(
        (value, key, object): (RegisterOperand, RegisterOperand, RegisterOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let value = context.vm.get_register(value.into()).clone();
        let key = context.vm.get_register(key.into()).clone();
        let object = context.vm.get_register(object.into()).clone();
        let object = object.to_object(context)?;
        let key = key.to_property_key(context)?;
        let success = object.__define_own_property__(
            &key,
            PropertyDescriptor::builder()
                .value(value.clone())
                .writable(true)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        if !success {
            return Err(JsNativeError::typ()
                .with_message("failed to defined own property")
                .into());
        }
        Ok(())
    }
}

impl Operation for DefineOwnPropertyByValue {
    const NAME: &'static str = "DefineOwnPropertyByValue";
    const INSTRUCTION: &'static str = "INST - DefineOwnPropertyByValue";
    const COST: u8 = 4;
}
