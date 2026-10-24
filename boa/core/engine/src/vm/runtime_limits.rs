/// Represents the limits of different runtime operations.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeLimits {
    /// Max stack size before an error is thrown.
    stack_size: usize,

    /// Max loop iterations before an error is thrown.
    loop_iteration: u64,

    /// Max backtrace count in exception.
    backtrace_limit: usize,

    /// Max function recursion limit
    recursion: usize,

    /// Max native stack, in bytes, that nested runs of the interpreter may take.
    native_stack: usize,
}

impl Default for RuntimeLimits {
    /// Limits close to what V8 allows.
    ///
    /// A call from JavaScript to JavaScript takes no native stack — its frame lives on the heap —
    /// so the recursion limit is V8's order of magnitude, 10 000 calls, and the value stack has
    /// room for them. Only a call from native code back into JavaScript (a getter, an
    /// `Array.prototype.map` callback, `Function.prototype.apply`, a `JSON.parse` reviver, `eval`)
    /// nests the interpreter on the native stack — 1.6 to 4.5 KB a level in an optimized build,
    /// 6 to 17 KB in an unoptimized one — so those are bounded by the native stack they have
    /// taken: 1 MB, which fits a thread of Rust's default 2 MB. An embedder running scripts on
    /// a bigger stack should raise it to about half that stack
    /// ([`set_native_stack_limit`](Self::set_native_stack_limit)).
    #[inline]
    fn default() -> Self {
        Self {
            loop_iteration: u64::MAX,
            recursion: 10_000,
            backtrace_limit: 50,
            stack_size: 1024 * 1024,
            native_stack: 1024 * 1024,
        }
    }
}

impl RuntimeLimits {
    /// Return the loop iteration limit.
    ///
    /// If the limit is exceeded in a loop it will throw an error.
    ///
    /// The limit value [`u64::MAX`] means that there is no limit.
    #[inline]
    #[must_use]
    pub const fn loop_iteration_limit(&self) -> u64 {
        self.loop_iteration
    }

    /// Set the loop iteration limit.
    ///
    /// If the limit is exceeded in a loop it will throw an error.
    ///
    /// Setting the limit to [`u64::MAX`] means that there is no limit.
    #[inline]
    pub fn set_loop_iteration_limit(&mut self, value: u64) {
        self.loop_iteration = value;
    }

    /// Disable loop iteration limit.
    #[inline]
    pub fn disable_loop_iteration_limit(&mut self) {
        self.loop_iteration = u64::MAX;
    }

    /// Get max backtrace limit for an exception.
    ///
    /// Default is 50.
    #[inline]
    #[must_use]
    pub const fn backtrace_limit(&self) -> usize {
        self.backtrace_limit
    }

    /// Set max backtrace limit for an exception.
    #[inline]
    pub fn set_backtrace_limit(&mut self, value: usize) {
        self.backtrace_limit = value;
    }

    /// Get max stack size.
    #[inline]
    #[must_use]
    pub const fn stack_size_limit(&self) -> usize {
        self.stack_size
    }

    /// Set max stack size before an error is thrown.
    #[inline]
    pub fn set_stack_size_limit(&mut self, value: usize) {
        self.stack_size = value;
    }

    /// Get recursion limit.
    #[inline]
    #[must_use]
    pub const fn recursion_limit(&self) -> usize {
        self.recursion
    }

    /// Set recursion limit before an error is thrown.
    #[inline]
    pub fn set_recursion_limit(&mut self, value: usize) {
        self.recursion = value;
    }

    /// Get the native stack limit: how many bytes of native stack nested runs of the
    /// interpreter may take, counted from the outermost one, before a call throws a
    /// `RangeError`.
    ///
    /// Calls from native code back into JavaScript (accessors, callbacks of built-ins,
    /// `Function.prototype.call`, `eval`) nest the interpreter on the native stack, so this must
    /// stay below the stack of the thread running the context, with room for the embedder's
    /// own frames — about half of it. Default is 1 MB, for Rust's default thread of 2 MB.
    #[inline]
    #[must_use]
    pub const fn native_stack_limit(&self) -> usize {
        self.native_stack
    }

    /// Set the native stack limit, in bytes.
    #[inline]
    pub fn set_native_stack_limit(&mut self, value: usize) {
        self.native_stack = value;
    }
}
