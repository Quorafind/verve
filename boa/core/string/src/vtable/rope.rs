//! `VTable` implementations for [`RopeString`], a lazy concatenation of two strings.
use crate::iter::CodePointsIter;
use crate::r#type::{Latin1, Utf16};
use crate::vtable::{JsStringVTable, SequenceString};
use crate::{JsStr, JsStrVariant, JsString, JsStringKind};
use std::cell::{Cell, RefCell, UnsafeCell};
use std::process::abort;
use std::ptr::{self, NonNull};
use std::rc::Rc;

/// The longest string [`RopeString::append_short`] copies into a buffer; longer ones are cheaper
/// to keep as they are.
const MAX_BUFFERED_LEN: usize = 64;

/// The room a buffer starts with, in code units.
const BUFFER_START_CAPACITY: usize = 128;

/// The longest Latin-1 buffer tail [`RopeString::append_short`] widens to UTF-16 to take a wide
/// append. Beyond it the copy could cost more than the nodes it saves (and repeated on forks of
/// one rope, add up to a quadratic cost), so the append gets a node of its own instead.
const MAX_WIDEN_LEN: usize = 1024;

/// What a rope currently holds.
enum RopeState {
    /// The two halves, not yet copied into one buffer.
    Unflattened { left: JsString, right: JsString },
    /// `left` followed by the first `len - left.len()` bytes of a Latin-1 buffer. Ropes made by
    /// appending to each other share the buffer, each reading only up to its own length, and the
    /// one whose length is the buffer's current end may append to it in place.
    Appended {
        left: JsString,
        buffer: Rc<RefCell<Vec<u8>>>,
    },
    /// As `Appended`, for a rope that is not Latin-1: the buffer holds UTF-16 code units, whatever
    /// width the pieces appended to it had.
    AppendedWide {
        left: JsString,
        buffer: Rc<RefCell<Vec<u16>>>,
    },
    /// The concatenation, built by the first read of the content. Once written it is never
    /// changed, which is what lets [`rope_as_str`] hand out slices borrowed from it.
    Flat(JsString),
}

/// The concatenation of two strings, copied into one buffer only when its content is first read.
///
/// `s += x` in a loop builds a chain of these instead of copying the whole accumulated string on
/// every step; [`JsString::len`] and clones never read the content.
///
/// # Notes
/// Like the other kinds it is `!Sync` (`Cell` refcount, `UnsafeCell` state).
#[repr(C)]
pub(crate) struct RopeString {
    /// Embedded `VTable` - must be the first field for vtable dispatch.
    vtable: JsStringVTable,
    refcount: Cell<usize>,
    state: UnsafeCell<RopeState>,
    /// Whether the flat result is Latin-1, i.e. both halves are. Kept here so that building a
    /// rope over another rope does not have to read the inner one's content.
    latin1: bool,
}

/// Whether `string` is Latin-1, without flattening it if it is a rope.
fn is_latin1(string: &JsString) -> bool {
    if string.kind() == JsStringKind::Rope {
        // SAFETY: The kind was just checked.
        let rope: &RopeString = unsafe { string.as_inner() };
        rope.latin1
    } else {
        string.as_str().is_latin1()
    }
}

/// One piece of the content of a rope that has not been flattened.
enum Piece<'a> {
    String(&'a JsString),
    /// The first bytes of an appended-to buffer.
    Bytes(&'a RefCell<Vec<u8>>, usize),
    /// The first code units of an appended-to UTF-16 buffer.
    Units(&'a RefCell<Vec<u16>>, usize),
}

/// The two pieces of `state`, a rope of length `len`, if it has not been flattened.
fn pieces(state: &RopeState, len: usize) -> Option<[Piece<'_>; 2]> {
    match state {
        RopeState::Unflattened { left, right } => Some([Piece::String(left), Piece::String(right)]),
        RopeState::Appended { left, buffer } => {
            Some([Piece::String(left), Piece::Bytes(buffer, len - left.len())])
        }
        RopeState::AppendedWide { left, buffer } => {
            Some([Piece::String(left), Piece::Units(buffer, len - left.len())])
        }
        RopeState::Flat(_) => None,
    }
}

/// The pieces of `string` if it is a rope that has not been flattened yet.
fn unflattened(string: &JsString) -> Option<[Piece<'_>; 2]> {
    if string.kind() != JsStringKind::Rope {
        return None;
    }
    // SAFETY: The kind was just checked.
    let rope: &RopeString = unsafe { string.as_inner() };
    // SAFETY: The state is only written by `flatten` and `take_rope_children`. `flatten` does
    // not write while it holds references from here (it writes after it is done with them),
    // and `take_rope_children` runs on the last reference only. The program is single-threaded.
    pieces(unsafe { &*rope.state.get() }, rope.vtable.len)
}

impl RopeString {
    /// Creates a rope for `left` + `right`, whose total length is `len`.
    #[allow(clippy::new_ret_no_self)]
    pub(crate) fn new(left: JsString, right: JsString, len: usize) -> JsString {
        let latin1 = is_latin1(&left) && is_latin1(&right);
        Self::with_state(RopeState::Unflattened { left, right }, len, latin1)
    }

    /// Creates a rope in `state`, whose total length is `len`.
    fn with_state(state: RopeState, len: usize, latin1: bool) -> JsString {
        let node = Box::new(RopeString {
            vtable: JsStringVTable {
                clone: rope_clone,
                drop: rope_drop,
                as_str: rope_as_str,
                code_points: rope_code_points,
                refcount: rope_refcount,
                len,
                kind: JsStringKind::Rope,
            },
            refcount: Cell::new(1),
            state: UnsafeCell::new(state),
            latin1,
        });
        // SAFETY: `RopeString` starts with its vtable, as every string must.
        unsafe { JsString::from_ptr(NonNull::from(Box::leak(node)).cast()) }
    }

    /// Creates the rope for `x` + `y` (of length `len`) that keeps the short `y` in a buffer
    /// instead of a node of its own, or returns `None` if the two do not fit that.
    ///
    /// `x` must be a rope whose last half is itself short, or one that ends in a buffer. An
    /// append to a rope that ends in a buffer extends the buffer where it ends, which no rope
    /// sees past its own length, so every rope made before keeps its content. If the buffer has
    /// been extended by another rope in the meantime, `None` sends the caller to a plain node.
    ///
    /// A buffer holds bytes while everything in the rope is Latin-1 and UTF-16 code units from the
    /// first piece that is not. A byte buffer is never turned into a unit buffer in place (ropes
    /// made before would read it as bytes): the first wide append copies it into a new one.
    pub(crate) fn append_short(x: &JsString, y: &JsString, len: usize) -> Option<JsString> {
        if y.len() > MAX_BUFFERED_LEN || x.kind() != JsStringKind::Rope {
            return None;
        }
        // SAFETY: The kind was just checked.
        let rope: &RopeString = unsafe { x.as_inner() };
        let y_str = y.as_str();
        // SAFETY: See `unflattened`. Nothing writes the state while this reads it.
        let state = unsafe { &*rope.state.get() };
        let tail = |left: &JsString| rope.vtable.len - left.len();
        let state = match state {
            RopeState::Appended { left, buffer } => {
                if let Some(appended) = y_str.as_latin1() {
                    let mut bytes = buffer.borrow_mut();
                    if bytes.len() != tail(left) {
                        return None;
                    }
                    bytes.extend_from_slice(appended);
                    drop(bytes);
                    RopeState::Appended {
                        left: left.clone(),
                        buffer: Rc::clone(buffer),
                    }
                } else {
                    // Only the part this rope sees is copied, so it does not matter whether
                    // another rope has taken the end of the buffer.
                    let tail = tail(left);
                    if tail > MAX_WIDEN_LEN {
                        return None;
                    }
                    let bytes = buffer.borrow();
                    let mut units = Vec::with_capacity(BUFFER_START_CAPACITY.max(tail * 2));
                    units.extend(bytes[..tail].iter().map(|b| u16::from(*b)));
                    drop(bytes);
                    push_units(&mut units, y_str);
                    RopeState::AppendedWide {
                        left: left.clone(),
                        buffer: Rc::new(RefCell::new(units)),
                    }
                }
            }
            RopeState::AppendedWide { left, buffer } => {
                let mut units = buffer.borrow_mut();
                if units.len() != tail(left) {
                    return None;
                }
                push_units(&mut units, y_str);
                drop(units);
                RopeState::AppendedWide {
                    left: left.clone(),
                    buffer: Rc::clone(buffer),
                }
            }
            RopeState::Unflattened { left, right } if right.len() <= MAX_BUFFERED_LEN => {
                let right_str = right.as_str();
                match (right_str.as_latin1(), y_str.as_latin1()) {
                    (Some(r), Some(appended)) if rope.latin1 => {
                        let mut bytes = Vec::with_capacity(BUFFER_START_CAPACITY);
                        bytes.extend_from_slice(r);
                        bytes.extend_from_slice(appended);
                        RopeState::Appended {
                            left: left.clone(),
                            buffer: Rc::new(RefCell::new(bytes)),
                        }
                    }
                    _ => {
                        let mut units = Vec::with_capacity(BUFFER_START_CAPACITY);
                        push_units(&mut units, right_str);
                        push_units(&mut units, y_str);
                        RopeState::AppendedWide {
                            left: left.clone(),
                            buffer: Rc::new(RefCell::new(units)),
                        }
                    }
                }
            }
            _ => return None,
        };
        let latin1 = matches!(state, RopeState::Appended { .. });
        Some(Self::with_state(state, len, latin1))
    }

    /// The flat string of `string` if it is a rope that has been flattened.
    pub(crate) fn flat_of(string: &JsString) -> Option<&JsString> {
        if string.kind() != JsStringKind::Rope {
            return None;
        }
        // SAFETY: The kind was just checked.
        let rope: &RopeString = unsafe { string.as_inner() };
        // SAFETY: See `unflattened`.
        match unsafe { &*rope.state.get() } {
            RopeState::Flat(flat) => Some(flat),
            RopeState::Unflattened { .. }
            | RopeState::Appended { .. }
            | RopeState::AppendedWide { .. } => None,
        }
    }

    /// Copies the halves into one buffer, stores it, releases the halves and returns its content.
    #[cold]
    #[inline(never)]
    fn flatten(&self) -> JsStr<'static> {
        let len = self.vtable.len;
        // SAFETY: See `unflattened`.
        let Some(halves) = pieces(unsafe { &*self.state.get() }, len) else {
            unreachable!("`flatten` is only called on an unflattened rope");
        };
        let flat = if self.latin1 {
            let seq = SequenceString::<Latin1>::allocate(len);
            // SAFETY: The allocation holds `len` bytes after its header.
            let mut dst = unsafe { (&raw mut (*seq.as_ptr()).data).cast::<u8>().add(len) };
            for_each_leaf_rev(halves, |leaf| {
                let JsStrVariant::Latin1(bytes) = leaf.variant() else {
                    unreachable!("both halves of a Latin-1 rope are Latin-1");
                };
                // SAFETY: The leaves add up to `len` and are written from the end, so
                // `dst - bytes.len()` stays inside the buffer. The buffer is new, so it cannot
                // overlap a leaf.
                unsafe {
                    dst = dst.sub(bytes.len());
                    ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
                }
            });
            // SAFETY: A `SequenceString` starts with its vtable.
            unsafe { JsString::from_ptr(seq.cast()) }
        } else {
            let seq = SequenceString::<Utf16>::allocate(len);
            // SAFETY: The allocation holds `len` code units after its header.
            let mut dst = unsafe { (&raw mut (*seq.as_ptr()).data).cast::<u16>().add(len) };
            for_each_leaf_rev(halves, |leaf| {
                // SAFETY: As for Latin-1: the leaves add up to `len`, written from the end into
                // a new buffer.
                unsafe {
                    dst = dst.sub(leaf.len());
                    match leaf.variant() {
                        JsStrVariant::Utf16(units) => {
                            ptr::copy_nonoverlapping(units.as_ptr(), dst, units.len());
                        }
                        JsStrVariant::Latin1(bytes) => {
                            for (i, byte) in bytes.iter().enumerate() {
                                dst.add(i).write(u16::from(*byte));
                            }
                        }
                    }
                }
            });
            // SAFETY: A `SequenceString` starts with its vtable.
            unsafe { JsString::from_ptr(seq.cast()) }
        };

        // SAFETY: The slice lives as long as `flat`, which the state keeps until the node dies.
        let content = unsafe { flat.as_str().as_static() };
        // SAFETY: `halves` is not used past this point, and nothing else holds a
        // reference into the state (see `unflattened`).
        let old = std::mem::replace(unsafe { &mut *self.state.get() }, RopeState::Flat(flat));
        // Dropping the halves can release a very long chain; `rope_drop` does that iteratively.
        drop(old);
        content
    }

    /// Moves what this rope holds that is itself a rope into `out`, leaving the node holding
    /// nothing, so that dropping it does not recurse.
    ///
    /// The caller must own the last reference to the node.
    fn take_rope_children(&self, out: &mut Vec<JsString>) {
        // SAFETY: The caller owns the last reference to the node, so nothing else reads the state.
        let state = unsafe { &mut *self.state.get() };
        let old = std::mem::replace(state, RopeState::Flat(JsString::default()));
        let mut keep = |s: JsString| {
            if s.kind() == JsStringKind::Rope {
                out.push(s);
            }
        };
        match old {
            RopeState::Unflattened { left, right } => {
                keep(left);
                keep(right);
            }
            RopeState::Appended { left, .. } | RopeState::AppendedWide { left, .. } => keep(left),
            RopeState::Flat(flat) => keep(flat),
        }
    }
}

/// Appends the code units of `string` to `units`.
fn push_units(units: &mut Vec<u16>, string: JsStr<'_>) {
    match string.variant() {
        JsStrVariant::Latin1(bytes) => units.extend(bytes.iter().map(|b| u16::from(*b))),
        JsStrVariant::Utf16(wide) => units.extend_from_slice(wide),
    }
}

/// Calls `f` with every non-rope (or already flattened) string and every buffer under `halves`,
/// from the last one to the first, using a heap stack so that a chain of any depth is safe.
fn for_each_leaf_rev(halves: [Piece<'_>; 2], mut f: impl FnMut(JsStr<'_>)) {
    let mut stack = Vec::from(halves);
    while let Some(piece) = stack.pop() {
        match piece {
            Piece::Bytes(buffer, len) => f(JsStr::latin1(&buffer.borrow()[..len])),
            Piece::Units(buffer, len) => f(JsStr::utf16(&buffer.borrow()[..len])),
            Piece::String(string) => match unflattened(string) {
                Some(halves) => stack.extend(halves),
                None => f(string.as_str()),
            },
        }
    }
}

#[inline]
fn rope_clone(vtable: NonNull<JsStringVTable>) -> JsString {
    // SAFETY: This is part of the correct vtable which is validated on construction.
    let this: &RopeString = unsafe { vtable.cast().as_ref() };
    let Some(strong) = this.refcount.get().checked_add(1) else {
        abort();
    };
    this.refcount.set(strong);
    // SAFETY: validated the string outside this function.
    unsafe { JsString::from_ptr(vtable) }
}

fn rope_drop(vtable: NonNull<JsStringVTable>) {
    // SAFETY: This is part of the correct vtable which is validated on construction.
    let this: &RopeString = unsafe { vtable.cast().as_ref() };
    let Some(new) = this.refcount.get().checked_sub(1) else {
        abort();
    };
    this.refcount.set(new);
    if new != 0 {
        return;
    }

    // Release the chain below this node with a loop, not with the recursion that dropping the
    // fields would be: `s += x` makes it as deep as the number of appends.
    let mut pending = Vec::new();
    this.take_rope_children(&mut pending);
    while let Some(rope) = pending.pop() {
        if rope.refcount() == Some(1) {
            // SAFETY: Only ropes are pushed, and this is the last reference to it.
            let node: &RopeString = unsafe { rope.as_inner() };
            node.take_rope_children(&mut pending);
        }
        drop(rope);
    }

    // SAFETY: This was the last reference, and the node was allocated by `Box` in `new`.
    unsafe {
        drop(Box::from_raw(vtable.cast::<RopeString>().as_ptr()));
    }
}

#[inline]
fn rope_as_str(vtable: NonNull<JsStringVTable>) -> JsStr<'static> {
    // SAFETY: This is part of the correct vtable which is validated on construction.
    let this: &RopeString = unsafe { vtable.cast().as_ref() };
    // SAFETY: See `unflattened`.
    if let RopeState::Flat(flat) = unsafe { &*this.state.get() } {
        // SAFETY: The flat string is never replaced, so it lives as long as the node.
        return unsafe { flat.as_str().as_static() };
    }
    this.flatten()
}

#[inline]
fn rope_code_points(vtable: NonNull<JsStringVTable>) -> CodePointsIter<'static> {
    CodePointsIter::new(rope_as_str(vtable))
}

/// `VTable` function for refcount, need to return an `Option<usize>`.
#[inline]
#[allow(clippy::unnecessary_wraps)]
fn rope_refcount(vtable: NonNull<JsStringVTable>) -> Option<usize> {
    // SAFETY: This is part of the correct vtable which is validated on construction.
    let this: &RopeString = unsafe { vtable.cast().as_ref() };
    Some(this.refcount.get())
}
