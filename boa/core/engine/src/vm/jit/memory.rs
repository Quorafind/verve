//! Executable memory for compiled functions.
//!
//! Each compiled function gets pages of its own. They are mapped read-write, the machine code
//! is copied in, and then they are made read-execute before anything can run them: no page is
//! ever writable and executable at once (W^X), and nothing writes to a page once it is
//! executable. The pages are unmapped when the function's code is dropped.

use std::fmt;

/// Machine code for one function, in pages that are read-execute only.
pub(crate) struct ExecutableCode {
    allocation: region::Allocation,
    len: usize,
}

impl fmt::Debug for ExecutableCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutableCode")
            .field("ptr", &self.allocation.as_ptr::<u8>())
            .field("len", &self.len)
            .finish()
    }
}

impl ExecutableCode {
    /// Copies `code` into fresh pages and makes them executable.
    pub(crate) fn new(code: &[u8]) -> Result<Self, String> {
        if code.is_empty() {
            return Err("no code".into());
        }
        let mut allocation = region::alloc(code.len(), region::Protection::READ_WRITE)
            .map_err(|error| format!("cannot map code pages: {error}"))?;
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
        .map_err(|error| format!("cannot make code pages executable: {error}"))?;

        #[cfg(not(target_arch = "x86_64"))]
        {
            // Other architectures do not keep the instruction cache coherent with data writes.
            // SAFETY: the range is the code just written.
            unsafe { wasmtime_internal_jit_icache_coherence::clear_cache(ptr.cast(), code.len()) }
                .map_err(|error| format!("cannot clear the instruction cache: {error}"))?;
            wasmtime_internal_jit_icache_coherence::pipeline_flush_mt()
                .map_err(|error| format!("cannot flush the pipeline: {error}"))?;
        }

        Ok(Self {
            allocation,
            len: code.len(),
        })
    }

    /// The first byte of the code.
    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.allocation.as_ptr::<u8>()
    }

    /// The size of the code, without the padding to a page.
    pub(crate) fn len(&self) -> usize {
        self.len
    }
}
