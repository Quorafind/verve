//! Where compiled code finds the VM stack's elements, length and capacity (a `Vec<JsValue>`,
//! whose fields Rust does not place, so they are found by looking at a real one), and the
//! `CONSTRUCT` bit of a call frame's flags. They are checked on each thread, the first time
//! compiled code asks; if anything does not match, the stack instructions are compiled as calls
//! to helpers.

use std::{cell::OnceCell, mem::size_of, ptr};

use crate::{JsValue, vm::call_frame::CallFrameFlags};

/// Byte offsets from the start of the stack's `Vec<JsValue>`, and the construct bit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StackLayout {
    pub(crate) ptr: i32,
    pub(crate) len: i32,
    pub(crate) cap: i32,
    /// The bit of `CallFrameFlags::CONSTRUCT` in the byte of the frame's flags.
    pub(crate) construct: i64,
}

thread_local! {
    static LAYOUT: OnceCell<Option<StackLayout>> = const { OnceCell::new() };
}

/// The stack layout, if it checked out on this thread.
pub(crate) fn stack_layout() -> Option<StackLayout> {
    LAYOUT.with(|layout| *layout.get_or_init(compute))
}

fn compute() -> Option<StackLayout> {
    const WORDS: usize = size_of::<Vec<JsValue>>() / size_of::<usize>();
    if size_of::<Vec<JsValue>>() != WORDS * size_of::<usize>() || size_of::<CallFrameFlags>() != 1 {
        return None;
    }
    let mut vec: Vec<JsValue> = Vec::with_capacity(7);
    vec.extend([JsValue::undefined(), JsValue::null(), JsValue::from(3)]);
    // SAFETY: a `Vec` is `WORDS` initialized words.
    let words = unsafe { ptr::from_ref(&vec).cast::<[usize; WORDS]>().read() };
    let find = |value: usize| {
        let mut found = words.iter().enumerate().filter(|&(_, &w)| w == value);
        match (found.next(), found.next()) {
            (Some((index, _)), None) => i32::try_from(index * size_of::<usize>()).ok(),
            _ => None,
        }
    };
    let (ptr, len, cap) = (find(vec.as_ptr() as usize)?, find(3)?, find(7)?);
    let flags = CallFrameFlags::CONSTRUCT;
    // SAFETY: the flags are one initialized byte.
    let byte = unsafe { ptr::from_ref(&flags).cast::<u8>().read() };
    (byte == flags.bits() && byte.is_power_of_two()).then_some(StackLayout {
        ptr,
        len,
        cap,
        construct: i64::from(byte),
    })
}
