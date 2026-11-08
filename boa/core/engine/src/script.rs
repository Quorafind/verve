//! Boa's implementation of ECMAScript's Scripts.
//!
//! This module contains the [`Script`] type, which represents a [**Script Record**][script].
//!
//! More information:
//!  - [ECMAScript reference][spec]
//!
//! [spec]: https://tc39.es/ecma262/#sec-scripts
//! [script]: https://tc39.es/ecma262/#sec-script-records

use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

use boa_gc::{Finalize, Gc, GcRefCell, Trace};
use boa_parser::{Parser, Source, source::ReadChar};

use crate::{
    Context, HostDefined, JsResult, JsString, JsValue, Module, SpannedSourceText,
    bytecompiler::{ByteCompiler, global_declaration_instantiation_context},
    environments::EnvironmentStack,
    js_string,
    realm::Realm,
    spanned_source_text::SourceText,
    vm::{ActiveRunnable, CallFrame, CallFrameFlags, CodeBlock},
};

/// ECMAScript's [**Script Record**][spec].
///
/// [spec]: https://tc39.es/ecma262/#sec-script-records
#[derive(Clone, Trace, Finalize)]
pub struct Script {
    inner: Gc<Inner>,
}

impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Script")
            .field("realm", &self.inner.realm.addr())
            .field("phase", &self.inner.phase.borrow())
            .field("loaded_modules", &self.inner.loaded_modules)
            .finish()
    }
}

#[derive(Trace, Debug, Finalize)]
enum ScriptPhase {
    Ast(#[unsafe_ignore_trace] boa_ast::Script),
    Codeblock(Gc<CodeBlock>),
}

#[derive(Trace, Finalize)]
struct Inner {
    realm: Realm,
    phase: GcRefCell<ScriptPhase>,
    /// The global scope the script was parsed against, and the positions in it of its global
    /// lexical declarations. Parsing makes them, so the script's code can find them, but no
    /// other code may until the script's declarations are instantiated: they are hidden until
    /// then, and again if that fails.
    #[unsafe_ignore_trace]
    global_scope: boa_ast::scope::Scope,
    #[unsafe_ignore_trace]
    global_lexicals: std::ops::Range<u32>,
    /// Whether the script's global lexical declarations are instantiated.
    #[unsafe_ignore_trace]
    declared: std::cell::Cell<bool>,
    /// Whether compiling it made a global var binding for a block-level function declaration
    /// (Annex B.3.2.2), which code read back would not make.
    #[unsafe_ignore_trace]
    annex_b_binding: std::cell::Cell<bool>,
    source_text: SourceText,
    loaded_modules: GcRefCell<FxHashMap<JsString, Module>>,
    host_defined: HostDefined,
    path: Option<PathBuf>,
}

impl Script {
    /// Gets the realm of this script.
    #[must_use]
    pub fn realm(&self) -> &Realm {
        &self.inner.realm
    }

    /// Returns the [`ECMAScript specification`][spec] defined [`\[\[HostDefined\]\]`][`HostDefined`] field of the [`Module`].
    ///
    /// [spec]: https://tc39.es/ecma262/#script-record
    #[must_use]
    pub fn host_defined(&self) -> &HostDefined {
        &self.inner.host_defined
    }

    /// Gets the loaded modules of this script.
    pub(crate) fn loaded_modules(&self) -> &GcRefCell<FxHashMap<JsString, Module>> {
        &self.inner.loaded_modules
    }

    /// Abstract operation [`ParseScript ( sourceText, realm, hostDefined )`][spec].
    ///
    /// Parses the provided `src` as an ECMAScript script, returning an error if parsing fails.
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-parse-script
    pub fn parse<R: ReadChar>(
        src: Source<'_, R>,
        realm: Option<Realm>,
        context: &mut Context,
    ) -> JsResult<Self> {
        let path = src.path().map(Path::to_path_buf);
        let mut parser = Parser::new(src);
        parser.set_identifier(context.next_parser_identifier());
        if context.is_strict() {
            parser.set_strict();
        }
        let scope = context.realm().scope().clone();
        let before = scope.num_bindings();
        let parsed = parser.parse_script_with_source(&scope, context.interner_mut());
        let global_lexicals = before..scope.num_bindings();
        scope.withdraw_bindings(global_lexicals.clone());
        let (mut code, source) = parsed?;
        if !context.optimizer_options().is_empty() {
            context.optimize_statement_list(code.statements_mut());
        }

        let source_text = SourceText::new(source);

        let script = Self {
            inner: Gc::new(Inner {
                realm: realm.unwrap_or_else(|| context.realm().clone()),
                phase: GcRefCell::new(ScriptPhase::Ast(code)),
                global_scope: scope,
                global_lexicals,
                declared: std::cell::Cell::new(false),
                annex_b_binding: std::cell::Cell::new(false),
                source_text,
                loaded_modules: GcRefCell::default(),
                host_defined: HostDefined::default(),
                path,
            }),
        };
        // A script from a file is the one a cache keeps: one evaluated from a string, with no
        // path, is not, and is used as parsed.
        if script.inner.path.is_some() && crate::code_cache::round_trip() {
            return script.through_code_cache(context);
        }
        Ok(script)
    }

    /// The script written down and read back, as [`Script::code_cache`] and
    /// [`Script::from_code_cache`] would across runs: `BOA_CODE_CACHE_ROUNDTRIP=1` makes every
    /// parse that can be do it, so a whole suite runs code read back from a cache.
    fn through_code_cache(self, context: &mut Context) -> JsResult<Self> {
        // A script that cannot be written down is used as parsed: writing it compiles it, and
        // every function it declares, here rather than when they are first called.
        if !self.can_be_written_down() {
            return Ok(self);
        }
        // Compiling here, while the script is parsed rather than when it runs, moves what
        // compiling it does — the global var bindings of Annex B.3.2.2 — to the parse. Only this
        // test switch compiles so early; a run compiles as it evaluates.
        let Ok(Some(bytes)) = self.code_cache(context) else {
            return Ok(self);
        };
        let units = self.inner.source_text.code_points().unwrap_or(&[]);
        let source = String::from_utf16_lossy(units);
        let path = self.inner.path.clone();
        let realm = self.inner.realm.clone();
        Self::from_code_cache(&bytes, &source, path.as_deref(), Some(realm), context).or(Ok(self))
    }

    /// Whether [`Script::code_cache`] can write this script down: not one that declares
    /// anything in the global scope, which parsing makes and code read back cannot. Asked of the
    /// syntax tree, as [`crate::code_cache::write_script`] asks it of the compiled code, so that
    /// asking costs no compilation.
    fn can_be_written_down(&self) -> bool {
        use boa_ast::operations::{lexically_declared_names, var_scoped_declarations};
        let phase = self.inner.phase.borrow();
        let ScriptPhase::Ast(source) = &*phase else {
            return true;
        };
        if !lexically_declared_names(source).is_empty()
            || !var_scoped_declarations(source).is_empty()
        {
            return false;
        }
        #[cfg(feature = "annex-b")]
        if !source.strict()
            && !boa_ast::operations::annex_b_function_declarations_names(source).is_empty()
        {
            return false;
        }
        true
    }

    /// Compiles this script, with every function it declares, and answers its code written
    /// down, for [`Script::from_code_cache`] to read back in a later run instead of parsing and
    /// compiling the same source text again. `None` for a script that cannot be written down:
    /// one that declares anything in the global scope, because parsing a script is what makes
    /// its global declarations and code read back would have to make them again.
    ///
    /// The bytes say what the script's code does, so they must be kept where nothing that
    /// should not run code can change them; see [`code_cache`](crate::code_cache).
    ///
    /// # Errors
    ///
    /// If compiling the script throws.
    pub fn code_cache(&self, context: &mut Context) -> JsResult<Option<Vec<u8>>> {
        let codeblock = self.codeblock_eager(context)?;
        // A block-level function declaration in a non-strict script makes a global var binding
        // while the script compiles (Annex B.3.2.2), as its other global declarations are made
        // while it parses: code read back makes neither.
        if self.inner.annex_b_binding.get() {
            return Ok(None);
        }
        let source = self.get_source();
        Ok(crate::code_cache::write_script(
            &codeblock,
            source.code_points().unwrap_or(&[]),
            &self.inner.realm,
            context.interner(),
        ))
    }

    /// The script whose code [`Script::code_cache`] wrote down, compiled from `source` (the same
    /// text, as given to [`Script::parse`]), read back without parsing or compiling it. It runs
    /// as the script parsed from `source` at `path` would.
    ///
    /// # Errors
    ///
    /// If the bytes are refused: not written down from this source text, by this build, against
    /// this realm's global scope, or not well formed (see [`code_cache`](crate::code_cache)). The
    /// message says why; parse the source instead.
    pub fn from_code_cache(
        bytes: &[u8],
        source: &str,
        path: Option<&Path>,
        realm: Option<Realm>,
        context: &mut Context,
    ) -> JsResult<Self> {
        let realm = realm.unwrap_or_else(|| context.realm().clone());
        let text = boa_ast::SourceText::from_code_units(source.encode_utf16().collect());
        let source_text = SourceText::new(text);
        let codeblock = crate::code_cache::read_script(bytes, &source_text, path, &realm).map_err(
            |reason| {
                crate::JsNativeError::error().with_message(format!("code cache refused: {reason}"))
            },
        )?;
        Ok(Self {
            inner: Gc::new(Inner {
                global_scope: realm.scope().clone(),
                realm,
                phase: GcRefCell::new(ScriptPhase::Codeblock(codeblock)),
                // A script that declares anything in the global scope is not written down, so
                // the one read back has nothing to hide and nothing to instantiate.
                global_lexicals: 0..0,
                declared: std::cell::Cell::new(true),
                annex_b_binding: std::cell::Cell::new(false),
                source_text,
                loaded_modules: GcRefCell::default(),
                host_defined: HostDefined::default(),
                path: path.map(Path::to_path_buf),
            }),
        })
    }

    /// Compiles the codeblock of this script.
    ///
    /// This is a no-op if this has been called previously.
    pub fn codeblock(&self, context: &mut Context) -> JsResult<Gc<CodeBlock>> {
        self.compiled(context, context.lazy_compile())
    }

    /// Compiles the codeblock of this script, with every function it declares, so that it can
    /// be written down: a function compiled when first called is compiled later.
    fn codeblock_eager(&self, context: &mut Context) -> JsResult<Gc<CodeBlock>> {
        self.compiled(context, false)
    }

    fn compiled(&self, context: &mut Context, lazy: bool) -> JsResult<Gc<CodeBlock>> {
        // The script's own code finds its global lexical declarations while it compiles; other
        // code only once they are instantiated.
        let declared = self.inner.declared.get();
        if !declared {
            self.declare_global_lexicals()?;
        }
        let compiled = self.compile(context, lazy);
        if !declared {
            self.inner
                .global_scope
                .withdraw_bindings(self.inner.global_lexicals.clone());
        }
        compiled
    }

    /// Makes the script's global lexical declarations found by other code, as instantiating
    /// them does; a `SyntaxError` if one of them is declared already.
    fn declare_global_lexicals(&self) -> JsResult<()> {
        self.inner
            .global_scope
            .declare_withdrawn(self.inner.global_lexicals.clone())
            .map_err(|name| {
                crate::JsNativeError::syntax()
                    .with_message(format!(
                        "Identifier '{}' has already been declared",
                        name.to_std_string_escaped()
                    ))
                    .into()
            })
    }

    fn compile(&self, context: &mut Context, lazy: bool) -> JsResult<Gc<CodeBlock>> {
        let cb = {
            let phase = self.inner.phase.borrow();
            let source = match &*phase {
                ScriptPhase::Codeblock(codeblock) => return Ok(codeblock.clone()),
                ScriptPhase::Ast(source) => source,
            };

            let mut annex_b_function_names = Vec::new();

            global_declaration_instantiation_context(
                &mut annex_b_function_names,
                source,
                self.inner.realm.scope(),
                context,
            )?;
            // Each of these made a global var binding as it was found.
            if !annex_b_function_names.is_empty() {
                self.inner.annex_b_binding.set(true);
            }

            let spanned_source_text = SpannedSourceText::new_source_only(self.get_source());

            let mut compiler = ByteCompiler::new(
                js_string!("<main>"),
                source.strict(),
                false,
                self.inner.realm.scope().clone(),
                self.inner.realm.scope().clone(),
                false,
                false,
                context.interner_mut(),
                false,
                spanned_source_text,
                self.path().map(Path::to_owned).into(),
            );

            compiler.lazy_functions = lazy;
            #[cfg(feature = "annex-b")]
            {
                compiler.annex_b_function_names = annex_b_function_names;
            }

            compiler.global_declaration_instantiation(source);
            compiler.compile_statement_list(source.statements(), true, false);

            Gc::new(compiler.finish())
        };

        *self.inner.phase.borrow_mut() = ScriptPhase::Codeblock(cb.clone());

        Ok(cb)
    }

    /// Evaluates this script and returns its result.
    ///
    /// Note that this won't run any scheduled promise jobs; you need to call [`Context::run_jobs`]
    /// on the context or [`JobExecutor::run_jobs`] on the provided queue to run them.
    ///
    /// [`JobExecutor::run_jobs`]: crate::job::JobExecutor::run_jobs
    pub fn evaluate(&self, context: &mut Context) -> JsResult<JsValue> {
        self.prepare_run(context)?;
        let record = context.run();

        context.vm.pop_frame();

        record.consume()
    }

    /// Evaluates this script and returns its result, periodically yielding to the executor
    /// in order to avoid blocking the current thread.
    ///
    /// This uses an implementation defined amount of "clock cycles" that need to pass before
    /// execution is suspended. See [`Script::evaluate_async_with_budget`] if you want to also
    /// customize this parameter.
    #[allow(clippy::future_not_send)]
    pub async fn evaluate_async(&self, context: &mut Context) -> JsResult<JsValue> {
        self.evaluate_async_with_budget(context, 256).await
    }

    /// Evaluates this script and returns its result, yielding to the executor each time `budget`
    /// number of "clock cycles" pass.
    ///
    /// Note that "clock cycle" is in quotation marks because we can't determine exactly how many
    /// CPU clock cycles a VM instruction will take, but all instructions have a "cost" associated
    /// with them that depends on their individual complexity. We'd recommend benchmarking with
    /// different budget sizes in order to find the ideal yielding time for your application.
    #[allow(clippy::future_not_send)]
    pub async fn evaluate_async_with_budget(
        &self,
        context: &mut Context,
        budget: u32,
    ) -> JsResult<JsValue> {
        self.prepare_run(context)?;

        let record = context.run_async_with_budget(budget).await;

        context.vm.pop_frame();

        record.consume()
    }

    fn prepare_run(&self, context: &mut Context) -> JsResult<()> {
        // GlobalDeclarationInstantiation: the script's global lexical declarations are declared
        // now, and hidden again if instantiating them fails.
        let declared = self.inner.declared.get();
        if !declared {
            self.declare_global_lexicals()?;
            self.inner.declared.set(true);
        }
        let undeclare = |script: &Self| {
            if !declared {
                script.inner.declared.set(false);
                script
                    .inner
                    .global_scope
                    .withdraw_bindings(script.inner.global_lexicals.clone());
            }
        };
        let codeblock = self.codeblock(context).inspect_err(|_| undeclare(self))?;

        let global_env = EnvironmentStack::new();
        context.vm.push_frame_with_stack(
            CallFrame::new(
                codeblock.clone(),
                Some(ActiveRunnable::Script(self.clone())),
                global_env,
                self.inner.realm.clone(),
            )
            .with_env_fp(0)
            .with_flags(CallFrameFlags::EXIT_EARLY),
            JsValue::undefined(),
            JsValue::null(),
        );

        self.realm().resize_global_env();

        context
            .global_declaration_instantiation(&codeblock)
            .inspect_err(|_| {
                context.vm.pop_frame();
                undeclare(self);
            })?;

        Ok(())
    }

    pub(super) fn path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    pub(super) fn get_source(&self) -> SourceText {
        self.inner.source_text.clone()
    }
}
