//! Executable memory for compiled functions.
//!
//! Each compiled function gets pages of its own. They are mapped read-write, the machine code
//! is copied in, and then they are made read-execute before anything can run them: no page is
//! ever writable and executable at once (W^X), and nothing writes to a page once it is
//! executable. The pages are unmapped when the function's code is dropped.
//!
//! On macOS the pages are mapped with `MAP_JIT` first, which is what a process under the
//! hardened runtime may map with only the `com.apple.security.cs.allow-jit` entitlement. On
//! Apple silicon such pages are writable or executable per thread, switched with
//! `pthread_jit_write_protect_np`: this thread writes the code with its writes on and turns them
//! off again before anything runs, and no other thread ever has them on. On Intel they are
//! made read-execute with `mprotect`, as elsewhere. Where `MAP_JIT` is refused, the ordinary
//! mapping is tried, which a process that is not hardened (or holds
//! `allow-unsigned-executable-memory`) may have.
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

/// Where a function's pages came from, which decides how they are given back.
enum Pages {
    /// Mapped read-write and protected read-execute through `region`.
    Region(region::Allocation),
    /// Mapped with `MAP_JIT`, given back with `munmap`.
    #[cfg(target_os = "macos")]
    MapJit(macos::JitPages),
}

impl Pages {
    fn as_ptr(&self) -> *const u8 {
        match self {
            Self::Region(allocation) => allocation.as_ptr::<u8>(),
            #[cfg(target_os = "macos")]
            Self::MapJit(pages) => pages.ptr.cast_const(),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Region(allocation) => allocation.len(),
            #[cfg(target_os = "macos")]
            Self::MapJit(pages) => pages.len,
        }
    }
}

/// Machine code for one function, in pages that are read-execute only.
pub(crate) struct ExecutableCode {
    pages: Pages,
    live: Rc<LiveCode>,
}

impl fmt::Debug for ExecutableCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutableCode")
            .field("ptr", &self.pages.as_ptr())
            .field("len", &self.pages.len())
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
        #[cfg(target_os = "macos")]
        let pages = match macos::JitPages::new(code) {
            Ok(pages) => Pages::MapJit(pages),
            // A process without `allow-jit` may still map ordinary executable pages.
            Err(_) => Pages::Region(region_pages(code)?),
        };
        #[cfg(not(target_os = "macos"))]
        let pages = Pages::Region(region_pages(code)?);

        #[cfg(not(target_arch = "x86_64"))]
        {
            // Other architectures do not keep the instruction cache coherent with data writes.
            // SAFETY: the range is the code just written.
            unsafe {
                wasmtime_internal_jit_icache_coherence::clear_cache(
                    pages.as_ptr().cast(),
                    code.len(),
                )
            }
            .map_err(|error| MemoryError(format!("cannot clear the instruction cache: {error}")))?;
            wasmtime_internal_jit_icache_coherence::pipeline_flush_mt()
                .map_err(|error| MemoryError(format!("cannot flush the pipeline: {error}")))?;
        }

        live.bytes.set(live.bytes.get() + pages.len());
        live.functions.set(live.functions.get() + 1);
        Ok(Self {
            pages,
            live: Rc::clone(live),
        })
    }

    /// The first byte of the code.
    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.pages.as_ptr()
    }

    /// The size of the mapping: the code rounded up to pages.
    pub(crate) fn mapped_len(&self) -> usize {
        self.pages.len()
    }
}

/// `code` in pages mapped read-write, copied, and then made read-execute.
fn region_pages(code: &[u8]) -> Result<region::Allocation, MemoryError> {
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
    Ok(allocation)
}

impl Drop for ExecutableCode {
    fn drop(&mut self) {
        self.live
            .bytes
            .set(self.live.bytes.get().saturating_sub(self.pages.len()));
        self.live
            .functions
            .set(self.live.functions.get().saturating_sub(1));
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ptr;

    /// Pages mapped with `MAP_JIT`, holding one function's code.
    pub(super) struct JitPages {
        pub(super) ptr: *mut u8,
        pub(super) len: usize,
    }

    impl JitPages {
        /// Maps `MAP_JIT` pages, copies `code` in, and leaves them executable and not writable
        /// on every thread.
        pub(super) fn new(code: &[u8]) -> Result<Self, String> {
            // SAFETY: `sysconf` has no preconditions.
            let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
                .ok()
                .filter(|page| page.is_power_of_two())
                .ok_or("no page size")?;
            let len = code
                .len()
                .checked_next_multiple_of(page)
                .ok_or("code too large")?;
            // Apple silicon maps `MAP_JIT` pages with every permission and switches writes per
            // thread; Intel maps them writable and protects them afterwards.
            let protection = if cfg!(target_arch = "aarch64") {
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC
            } else {
                libc::PROT_READ | libc::PROT_WRITE
            };
            // SAFETY: an anonymous private mapping of `len` bytes at an address the kernel picks.
            let mapped = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    len,
                    protection,
                    libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT,
                    -1,
                    0,
                )
            };
            if mapped == libc::MAP_FAILED {
                return Err(format!(
                    "MAP_JIT refused: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // Owned from here, so an error below unmaps it.
            let pages = Self {
                ptr: mapped.cast(),
                len,
            };
            pages.write(code)?;
            Ok(pages)
        }

        #[cfg(target_arch = "aarch64")]
        fn write(&self, code: &[u8]) -> Result<(), String> {
            // SAFETY: turning this thread's `MAP_JIT` writes on has no other precondition. Nothing
            // runs compiled code between here and turning them off: this thread is compiling.
            unsafe { libc::pthread_jit_write_protect_np(0) };
            // SAFETY: the mapping is `len >= code.len()` bytes, new, and writable on this thread.
            unsafe { ptr::copy_nonoverlapping(code.as_ptr(), self.ptr, code.len()) };
            // SAFETY: as above; the pages are executable again, and never writable, everywhere.
            unsafe { libc::pthread_jit_write_protect_np(1) };
            Ok(())
        }

        #[cfg(not(target_arch = "aarch64"))]
        fn write(&self, code: &[u8]) -> Result<(), String> {
            // SAFETY: the mapping is `len >= code.len()` bytes, new, and read-write.
            unsafe { ptr::copy_nonoverlapping(code.as_ptr(), self.ptr, code.len()) };
            // SAFETY: the range is exactly the mapping, which nothing else refers to.
            let protected = unsafe {
                libc::mprotect(self.ptr.cast(), self.len, libc::PROT_READ | libc::PROT_EXEC)
            };
            if protected != 0 {
                return Err(format!(
                    "cannot make MAP_JIT pages executable: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        }
    }

    impl Drop for JitPages {
        fn drop(&mut self) {
            // SAFETY: the range is exactly a mapping this value owns; no code in it is running
            // (the code block's `Rc` outlives every frame that entered it).
            unsafe { libc::munmap(self.ptr.cast(), self.len) };
        }
    }
}
