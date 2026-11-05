//! [`Lent`], a handle the frame of a call can hold without having counted it.

use std::{fmt, mem::ManuallyDrop, ops::Deref, ptr};

use boa_gc::{Finalize, Trace, Tracer};

/// A `T` that is either the frame's own, or lent to it by something that outlives the frame.
///
/// A call used to clone the handles its frame reaches through the callee (the code block, the
/// realm, the script or module) and drop them at return: a count up and a count down, three
/// times, for what the callee object, which stays on the VM stack for as long as the frame lives,
/// already keeps alive. A lent handle is a bitwise copy of one that something else owns: it adds
/// no count and drops nothing, and reads exactly as the owned one does.
///
/// Whoever lends a handle promises that the owner, and the value it holds, outlives the frame.
/// Cloning a frame, or replacing a handle, makes an owned copy, so nothing outside the VM's live
/// frames ever holds a lent one.
pub(crate) struct Lent<T: Trace> {
    value: ManuallyDrop<T>,
    owned: bool,
}

impl<T: Trace> Lent<T> {
    /// A handle the frame owns.
    #[inline]
    pub(crate) const fn owned(value: T) -> Self {
        Self {
            value: ManuallyDrop::new(value),
            owned: true,
        }
    }

    /// A handle the frame borrows from `owner`, without counting it.
    ///
    /// # Safety
    ///
    /// `owner` must be kept alive, and must not be changed, for as long as the returned value
    /// (and the frame holding it) lives.
    #[inline]
    pub(crate) unsafe fn lend(owner: &T) -> Self {
        Self {
            // SAFETY: a bitwise copy that is never dropped (`owned` is false), and is read only
            // while `owner` lives, as the caller promises.
            value: ManuallyDrop::new(unsafe { ptr::read(owner) }),
            owned: false,
        }
    }

    /// Makes the handle owned, counting it if it was lent.
    pub(crate) fn make_owned(&mut self)
    where
        T: Clone,
    {
        if !self.owned {
            let owned = T::clone(&self.value);
            // The lent copy is overwritten without being dropped.
            self.value = ManuallyDrop::new(owned);
            self.owned = true;
        }
    }

    /// Replaces the handle with the owned `value`, and returns the old one, owned.
    pub(crate) fn replace(&mut self, value: T) -> T
    where
        T: Clone,
    {
        let old = if self.owned {
            // SAFETY: `self.value` is overwritten right below, not dropped or used again.
            unsafe { ManuallyDrop::take(&mut self.value) }
        } else {
            T::clone(&self.value)
        };
        self.value = ManuallyDrop::new(value);
        self.owned = true;
        old
    }

    /// An owned copy of the handle, as `Clone` would make it.
    pub(crate) fn duplicate(&self) -> Self
    where
        T: Clone,
    {
        Self::owned(T::clone(&self.value))
    }
}

impl<T: Trace> Deref for Lent<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: Trace> Drop for Lent<T> {
    #[inline]
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: dropped once, here, and not used again.
            unsafe { ManuallyDrop::drop(&mut self.value) };
        }
    }
}

impl<T: Trace + fmt::Debug> fmt::Debug for Lent<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (*self.value).fmt(f)
    }
}

impl<T: Trace> Finalize for Lent<T> {
    fn finalize(&self) {
        if self.owned {
            self.value.finalize();
        }
    }
}

// SAFETY: it traces what it holds, owned or lent (a lent one is reachable from its owner, so
// marking it again is harmless), and takes part in the counts of roots only when it counted.
unsafe impl<T: Trace> Trace for Lent<T> {
    #[inline]
    unsafe fn trace(&self, tracer: &mut Tracer) {
        // SAFETY: as `T::trace`.
        unsafe { self.value.trace(tracer) };
    }

    #[inline]
    unsafe fn trace_non_roots(&self) {
        if self.owned {
            // SAFETY: as `T::trace_non_roots`.
            unsafe { self.value.trace_non_roots() };
        }
    }

    #[inline]
    fn run_finalizer(&self) {
        if self.owned {
            self.value.run_finalizer();
        }
    }
}
