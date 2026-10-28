use crate::{
    JsString, SpannedSourceText,
    builtins::function::ThisMode,
    bytecompiler::ByteCompiler,
    js_string,
    vm::{
        CodeBlock, CodeBlockFlags, LazyCode,
        source_info::{SourceInfo, SourceMap, SourcePath},
    },
};
use boa_ast::{
    function::{FormalParameterList, FunctionBody},
    scope::{FunctionScopes, Scope},
};
use boa_gc::Gc;
use boa_interner::Interner;

/// `FunctionCompiler` is used to compile AST functions to bytecode.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct FunctionCompiler {
    name: JsString,
    generator: bool,
    r#async: bool,
    strict: bool,
    arrow: bool,
    method: bool,
    in_with: bool,
    force_function_scope: bool,
    name_scope: Option<Scope>,
    spanned_source_text: SpannedSourceText,
    source_path: SourcePath,
    lazy: bool,
}

/// What compiling a function's body needs once it is first called: the function as the
/// [`FunctionCompiler`] had it, its parameters and body, and the scopes it was declared in.
///
/// The body is shared with the tree it was parsed into ([`FunctionBody`] holds it behind an
/// `Rc`), so keeping it costs nothing until the rest of that tree is dropped.
#[derive(Debug, Clone)]
pub(crate) struct PendingFunction {
    compiler: FunctionCompiler,
    parameters: FormalParameterList,
    body: FunctionBody,
    variable_environment: Scope,
    lexical_environment: Scope,
    scopes: FunctionScopes,
    contains_direct_eval: bool,
    /// The interner the function was parsed with: its symbols mean something only there.
    interner: usize,
}

impl PendingFunction {
    /// Whether this function's symbols are `interner`'s, so it can compile with it.
    pub(crate) const fn parsed_with(&self, interner: &Interner) -> bool {
        self.interner == interner.id()
    }

    /// Compiles the body, as [`FunctionCompiler::compile`] does for a function it compiles at
    /// once. The functions nested in it are compiled lazily too, unless `eager`.
    pub(crate) fn compile(self, eager: bool, interner: &mut Interner) -> Gc<CodeBlock> {
        let Self {
            compiler,
            parameters,
            body,
            variable_environment,
            lexical_environment,
            scopes,
            contains_direct_eval,
            interner: _,
        } = self;
        compiler.compile_now(
            &parameters,
            &body,
            variable_environment,
            lexical_environment,
            &scopes,
            contains_direct_eval,
            !eager,
            interner,
        )
    }
}

impl FunctionCompiler {
    /// Create a new `FunctionCompiler`.
    pub(crate) fn new(spanned_source_text: SpannedSourceText) -> Self {
        Self {
            name: js_string!(),
            generator: false,
            r#async: false,
            strict: false,
            arrow: false,
            method: false,
            in_with: false,
            force_function_scope: false,
            name_scope: None,
            spanned_source_text,
            source_path: SourcePath::None,
            lazy: false,
        }
    }

    /// Compile the function's body only when it is first called (see [`LazyCode`]), and the
    /// functions nested in it the same way.
    pub(crate) const fn lazy(mut self, lazy: bool) -> Self {
        self.lazy = lazy;
        self
    }

    /// Set the name of the function.
    pub(crate) fn name<N>(mut self, name: N) -> Self
    where
        N: Into<Option<JsString>>,
    {
        let name = name.into();
        if let Some(name) = name {
            self.name = name;
        }
        self
    }

    /// Indicate if the function is an arrow function.
    pub(crate) const fn arrow(mut self, arrow: bool) -> Self {
        self.arrow = arrow;
        self
    }
    /// Indicate if the function is a method function.
    pub(crate) const fn method(mut self, method: bool) -> Self {
        self.method = method;
        self
    }
    /// Indicate if the function is a generator function.
    pub(crate) const fn generator(mut self, generator: bool) -> Self {
        self.generator = generator;
        self
    }

    /// Indicate if the function is an async function.
    pub(crate) const fn r#async(mut self, r#async: bool) -> Self {
        self.r#async = r#async;
        self
    }

    /// Indicate if the function is in a strict context.
    pub(crate) const fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Provide the name scope of the function.
    pub(crate) fn name_scope(mut self, name_scope: Option<Scope>) -> Self {
        self.name_scope = name_scope;
        self
    }

    /// Indicate if the function is in a `with` statement.
    pub(crate) const fn in_with(mut self, in_with: bool) -> Self {
        self.in_with = in_with;
        self
    }

    /// Indicate if the function is in a `with` statement.
    pub(crate) const fn force_function_scope(mut self, force_function_scope: bool) -> Self {
        self.force_function_scope = force_function_scope;
        self
    }

    /// Set source map file path.
    pub(crate) fn source_path(mut self, source_path: SourcePath) -> Self {
        self.source_path = source_path;
        self
    }

    /// Compile a function statement list and it's parameters into bytecode.
    ///
    /// A lazy function gets a code block with no bytecode yet, but everything that is read of
    /// it before it runs (its name, `length`, flags and source text); its body is compiled when
    /// it is first called, by [`CodeBlock::compiled`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compile(
        self,
        parameters: &FormalParameterList,
        body: &FunctionBody,
        variable_environment: Scope,
        lexical_environment: Scope,
        scopes: &FunctionScopes,
        contains_direct_eval: bool,
        interner: &mut Interner,
    ) -> Gc<CodeBlock> {
        if self.lazy {
            return Gc::new(self.stub(
                parameters,
                body,
                variable_environment,
                lexical_environment,
                scopes,
                contains_direct_eval,
                interner,
            ));
        }
        self.compile_now(
            parameters,
            body,
            variable_environment,
            lexical_environment,
            scopes,
            contains_direct_eval,
            false,
            interner,
        )
    }

    /// The code block of a function whose body is compiled when it is first called.
    #[allow(clippy::too_many_arguments)]
    fn stub(
        self,
        parameters: &FormalParameterList,
        body: &FunctionBody,
        variable_environment: Scope,
        lexical_environment: Scope,
        scopes: &FunctionScopes,
        contains_direct_eval: bool,
        interner: &Interner,
    ) -> CodeBlock {
        let strict = self.strict || body.strict();
        let mut code = CodeBlock::new(self.name.clone(), parameters.length(), strict);
        let mut flags = code.flags.get();
        flags.set(CodeBlockFlags::IS_ASYNC, self.r#async);
        flags.set(CodeBlockFlags::IS_GENERATOR, self.generator);
        flags.set(
            CodeBlockFlags::HAS_PROTOTYPE_PROPERTY,
            !self.arrow && !self.method && !self.r#async && !self.generator,
        );
        code.flags.set(flags);
        if self.arrow {
            code.this_mode = ThisMode::Lexical;
        }
        code.parameter_length = parameters.as_ref().len() as u32;
        code.source_info = SourceInfo::new(
            SourceMap::new(Box::default(), self.source_path.clone()),
            self.name.clone(),
            self.spanned_source_text.clone(),
        );
        code.lazy = Some(Box::new(LazyCode::new(PendingFunction {
            compiler: self,
            parameters: parameters.clone(),
            body: body.clone(),
            variable_environment,
            lexical_environment,
            scopes: scopes.clone(),
            contains_direct_eval,
            interner: interner.id(),
        })));
        code
    }

    /// Compiles the function now. Functions nested in it are compiled lazily if `lazy_nested`,
    /// unless it contains a direct `eval`: that may declare `var`s in this function's scope
    /// when it runs, and a nested function compiled after that would see them as bindings of
    /// every call of it, not only the one that ran the `eval`.
    #[allow(clippy::too_many_arguments)]
    fn compile_now(
        mut self,
        parameters: &FormalParameterList,
        body: &FunctionBody,
        variable_environment: Scope,
        lexical_environment: Scope,
        scopes: &FunctionScopes,
        contains_direct_eval: bool,
        lazy_nested: bool,
        interner: &mut Interner,
    ) -> Gc<CodeBlock> {
        self.strict = self.strict || body.strict();

        let length = parameters.length();

        let mut compiler = ByteCompiler::new(
            self.name,
            self.strict,
            false,
            variable_environment,
            lexical_environment,
            self.r#async,
            self.generator,
            interner,
            self.in_with,
            self.spanned_source_text,
            self.source_path,
        );

        compiler.length = length;
        compiler.lazy_functions = lazy_nested && !contains_direct_eval;
        compiler.code_block_flags.set(
            CodeBlockFlags::HAS_PROTOTYPE_PROPERTY,
            !self.arrow && !self.method && !self.r#async && !self.generator,
        );

        if self.arrow {
            compiler.this_mode = ThisMode::Lexical;
        }

        if let Some(scope) = self.name_scope
            && !scope.all_bindings_local()
        {
            compiler.code_block_flags |= CodeBlockFlags::HAS_BINDING_IDENTIFIER;
            let _ = compiler.push_scope(&scope);
        }

        if contains_direct_eval || !scopes.function_scope().all_bindings_local() {
            compiler.code_block_flags |= CodeBlockFlags::HAS_FUNCTION_SCOPE;
        } else if !self.arrow {
            compiler.code_block_flags.set(
                CodeBlockFlags::HAS_FUNCTION_SCOPE,
                self.force_function_scope || scopes.requires_function_scope(),
            );
        }

        if compiler.code_block_flags.has_function_scope() {
            let _ = compiler.push_scope(scopes.function_scope());
        } else {
            compiler.variable_scope = scopes.function_scope().clone();
            compiler.lexical_scope = scopes.function_scope().clone();
        }

        // Taken from:
        //  - 15.9.3 Runtime Semantics: EvaluateAsyncConciseBody: <https://tc39.es/ecma262/#sec-runtime-semantics-evaluateasyncconcisebody>
        //  - 15.8.4 Runtime Semantics: EvaluateAsyncFunctionBody: <https://tc39.es/ecma262/#sec-runtime-semantics-evaluateasyncfunctionbody>
        //
        // Note: In `EvaluateAsyncGeneratorBody` unlike the async non-generator functions we don't handle exceptions thrown by
        // `FunctionDeclarationInstantiation` (so they are propagated).
        //
        // See: 15.6.2 Runtime Semantics: EvaluateAsyncGeneratorBody: https://tc39.es/ecma262/#sec-runtime-semantics-evaluateasyncgeneratorbody
        if compiler.is_async() && !compiler.is_generator() {
            // 1. Let promiseCapability be ! NewPromiseCapability(%Promise%).
            compiler.bytecode.emit_create_promise_capability();

            // 2. Let declResult be Completion(FunctionDeclarationInstantiation(functionObject, argumentsList)).
            //
            // Note: We push an exception handler so we catch exceptions that are thrown by the
            // `FunctionDeclarationInstantiation` abstract function.
            //
            // Patched in `ByteCompiler::finish()`.
            compiler.async_handler = Some(compiler.push_handler());
        }

        compiler.function_declaration_instantiation(
            body,
            parameters,
            self.arrow,
            self.strict,
            self.generator,
            scopes,
        );

        // Taken from:
        // - 27.6.3.2 AsyncGeneratorStart ( generator, generatorBody ): <https://tc39.es/ecma262/#sec-asyncgeneratorstart>
        //
        // Note: We do handle exceptions thrown by generator body in `AsyncGeneratorStart`.
        if compiler.is_generator() {
            assert!(compiler.async_handler.is_none());

            if compiler.is_async() {
                // Patched in `ByteCompiler::finish()`.
                compiler.async_handler = Some(compiler.push_handler());
            }
        }

        {
            let mut compiler = compiler.position_guard(body);
            compiler.compile_statement_list(body.statement_list(), false, false);
        }

        compiler.params = parameters.clone();
        compiler.parameter_scope = scopes.parameter_scope();

        let code = compiler.finish();

        Gc::new(code)
    }
}
