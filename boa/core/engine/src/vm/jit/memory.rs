//! Executable memory for compiled functions.
//!
//! Each compiled function gets pages of its own. They are mapped read-write, the machine code
//! is copied in, and then they are made read-execute before anything can run them: no page is
//! ever writable and executable at once (W^X), and nothing writes to a page once it is
//! executable. The pages are unmapped when the function's code is dropped.
//!
//! What is mapped is counted, page-rounded, in a [`LiveCode`] the context shares with its code,
//! so the context's limits see memory given back as well as memory taken.

use std::{cell::Cell, fmt, rc::Rc};

/// The machine code a context has mapped and not yet given back.
#[derive(Debug, Default)]
pub(crate) struct LiveCode {
    bytes: Cell<usize>,
    functions: Cell<usize>,
}

impl LiveCode {
    /// Bytes mapped, rounded up to pages.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes.get()
    }

    /// Functions whose code is mapped.
    pub(crate) fn functions(&self) -> usize {
        self.functions.get()
    }
}

/// Why executable memory could not be had.
#[derive(Debug)]
pub(crate) struct MemoryError(pub(crate) String);

/// Machine code for one function, in pages that are read-execute only.
pub(crate) struct ExecutableCode {
    allocation: region::Allocation,
    live: Rc<LiveCode>,
}

impl fmt::Debug for ExecutableCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutableCode")
            .field("ptr", &self.allocation.as_ptr::<u8>())
            .field("len", &self.allocation.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
thread_local! {
    /// Tests make mapping fail, as a host that forbids executable memory does.
    pub(crate) static FAIL_TO_MAP: Cell<bool> = const { Cell::new(false) };
}

impl ExecutableCode {
    /// Copies `code` into fresh pages and makes them executable, counting them in `live`.
    pub(crate) fn new(code: &[u8], live: &Rc<LiveCode>) -> Result<Self, MemoryError> {
        if code.is_empty() {
            return Err(MemoryError("no code".into()));
        }
        #[cfg(test)]
        if FAIL_TO_MAP.with(Cell::get) {
            return Err(MemoryError("executable memory refused (test)".into()));
        }
        let mut allocation = region::alloc(code.len(), region::Protection::READ_WRITE)
            .map_err(|error| MemoryError(format!("cannot map code pages: {error}")))?;
        let ptr = allocation.as_mut_ptr::<u8>();
        // SAFETY: the allocation is at least `code.len()` bytes, read-write, and new.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, code.len()) };

        // SAFETY: the range is exactly the allocation, which nothing else refers to.
        unsafe {
            region::protect(
                ptr.cast_const(),
                allocation.len(),
                region::Protection::READ_EXECUTE,
            )
        }
        .map_err(|error| MemoryError(format!("cannot make code pages executable: {error}")))?;

        #[cfg(not(target_arch = "x86_64"))]
        {
            // Other architectures do not keep the instruction cache coherent with data writes.
            // SAFETY: the range is the code just written.
            unsafe { wasmtime_internal_jit_icache_coherence::clear_cache(ptr.cast(), code.len()) }
                .map_err(|error| {
                    MemoryError(format!("cannot clear the instruction cache: {error}"))
                })?;
            wasmtime_internal_jit_icache_coherence::pipeline_flush_mt()
                .map_err(|error| MemoryError(format!("cannot flush the pipeline: {error}")))?;
        }

        live.bytes.set(live.bytes.get() + allocation.len());
        live.functions.set(live.functions.get() + 1);
        Ok(Self {
            allocation,
            live: Rc::clone(live),
        })
    }

    /// The first byte of the code.
    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.allocation.as_ptr::<u8>()
    }

    /// The size of the mapping: the code rounded up to pages.
    pub(crate) fn mapped_len(&self) -> usize {
        self.allocation.len()
    }
}

impl Drop for ExecutableCode {
    fn drop(&mut self) {
        self.live
            .bytes
            .set(self.live.bytes.get().saturating_sub(self.allocation.len()));
        self.live
            .functions
            .set(self.live.functions.get().saturating_sub(1));
    }
}
