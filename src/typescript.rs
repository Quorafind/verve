//! `module.stripTypeScriptTypes`: TypeScript to JavaScript, as Node does it.
//!
//! Hosts load TypeScript through module hooks that call this on a file's source — Rebon's mods
//! runtime does, for every `.ts` and `.tsx` file of a mod — so the answer has to be Node's, not
//! a bundler's. Node has two modes, and so does this:
//!
//! - `strip` (the default) erases the types and nothing else. Every erased character becomes
//!   whitespace of the same width — a space, or for a character outside ASCII a space of the
//!   same UTF-8 length — and line breaks stay, so every line and column of the JavaScript left
//!   is where it was in the TypeScript and no source map is needed. Syntax that only means
//!   something once compiled to other JavaScript (an `enum`, a namespace with values in it, a
//!   parameter property, `import x = require()`, `export =`, a `<T>x` assertion) cannot be
//!   erased, and is refused with Node's `ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX`.
//! - `transform` compiles those too, and prints the program anew: positions move, which is
//!   what the optional source map is for.
//!
//! Erasing has one hazard, which Node's stripper (amaro, built on swc) guards against too.
//! TypeScript ends a statement where the next one begins with a type-only keyword; once that
//! statement is blank, JavaScript can read the two lines around it as one, when the second
//! begins with `(`, `[`, a template or an operator. Where that could happen a `;` takes the
//! first erased place between them. And an arrow function whose return type spans lines would
//! leave a line break between `)` and `=>`, which JavaScript forbids, so its `)` moves to the
//! type's last line.
//!
//! The parser is oxc's. TypeScript is read as `.ts`, as Node reads it; source Node would refuse
//! because it holds JSX is read again as `.tsx`, and its JSX is left as written for a JSX
//! transform of the host's own to compile.

use std::path::{Path, PathBuf};

use boa_engine::{Context, JsError, JsNativeError, JsResult, JsString, JsValue, js_string};
use oxc_allocator::{Allocator, ArenaVec};
use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};
use oxc_transformer::{JsxOptions, TransformOptions, Transformer, TypeScriptOptions};

use crate::native::{arg, closure, get, invoke, js_text, object};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Strip,
    Transform,
}

/// Why TypeScript could not be turned into JavaScript.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// Not TypeScript: `ERR_INVALID_TYPESCRIPT_SYNTAX`.
    Invalid(String),
    /// TypeScript that erasing cannot turn into JavaScript: `ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX`.
    Unsupported(String),
}

/// The JavaScript `source` holds, by `mode`; with a source map in a `data:` URL when asked for
/// (transform only), else a `sourceURL` comment when `url` is given — as Node appends them.
pub(crate) fn strip_types(
    source: &str,
    mode: Mode,
    source_map: bool,
    url: &str,
) -> Result<String, Refusal> {
    let allocator = Allocator::default();
    let ts = SourceType::ts().with_unambiguous(true);
    let mut parsed = Parser::new(&allocator, source, ts).parse();
    if !parsed.diagnostics.is_empty() || parsed.panicked {
        let tsx = Parser::new(&allocator, source, ts.with_jsx(true)).parse();
        if tsx.diagnostics.is_empty() && !tsx.panicked {
            parsed = tsx;
        } else {
            return Err(Refusal::Invalid(diagnostic(
                source,
                url,
                parsed.diagnostics,
            )));
        }
    }
    let (code, map) = match mode {
        Mode::Strip => (strip(source, &parsed.program, url)?, None),
        Mode::Transform => {
            let program = parsed.program;
            let run = || transform(&allocator, program, source, url, source_map);
            // A transform oxc cannot finish panics; the runtime that asked goes on.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).map_err(|_| {
                Refusal::Unsupported("TypeScript the transform could not compile".into())
            })??
        }
    };
    Ok(match map {
        Some(map) => format!(
            "{code}\n\n//# sourceMappingURL=data:application/json;base64,{}",
            base64(map.as_bytes())
        ),
        None if !url.is_empty() => format!("{code}\n\n//# sourceURL={url}"),
        None => code,
    })
}

/// `program` compiled to JavaScript and printed, with its source map when asked for.
fn transform<'a>(
    allocator: &'a Allocator,
    mut program: Program<'a>,
    source: &str,
    url: &str,
    source_map: bool,
) -> Result<(String, Option<String>), Refusal> {
    let scoping = SemanticBuilder::new()
        .with_enum_eval(true)
        .build(&program)
        .semantic
        .into_scoping();
    let options = TransformOptions {
        // Node compiles with `verbatimModuleSyntax`: an import goes only when it says `type`.
        typescript: TypeScriptOptions {
            only_remove_type_imports: true,
            ..TypeScriptOptions::default()
        },
        jsx: JsxOptions::disable(),
        ..TransformOptions::default()
    };
    let name = if url.is_empty() { "input.ts" } else { url };
    let done = Transformer::new(allocator, Path::new(name), &options)
        .build_with_scoping(scoping, &mut program);
    if !done.diagnostics.is_empty() {
        return Err(Refusal::Unsupported(diagnostic(
            source,
            url,
            done.diagnostics,
        )));
    }
    let printed = Codegen::new()
        .with_options(CodegenOptions {
            source_map_path: source_map.then(|| PathBuf::from(url)),
            ..CodegenOptions::default()
        })
        .build(&program);
    Ok((printed.code, printed.map.map(|map| map.to_json_string())))
}

/// The first of oxc's diagnostics, where it happened.
fn diagnostic(
    source: &str,
    url: &str,
    diagnostics: impl IntoIterator<Item = oxc_diagnostics::OxcDiagnostic>,
) -> String {
    let Some(first) = diagnostics.into_iter().next() else {
        return "Invalid TypeScript".into();
    };
    match first.labels.first() {
        Some(label) => format!("{} ({})", first.message, place(source, url, label.offset())),
        None => first.message.to_string(),
    }
}

/// `url:line:column` of a byte offset, the column counted in UTF-16 units from 1.
fn place(source: &str, url: &str, offset: u32) -> String {
    let before = &source[..(offset as usize).min(source.len())];
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[start..].encode_utf16().count() + 1;
    let url = if url.is_empty() {
        String::new()
    } else {
        format!("{url}:")
    };
    format!("{url}{line}:{column}")
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ─── Strip mode ──────────────────────────────────────────────────────────────────────────────

fn strip(source: &str, program: &Program, url: &str) -> Result<String, Refusal> {
    let mut eraser = Eraser {
        src: source.as_bytes(),
        erase: Vec::new(),
        lists: Vec::new(),
        leading: Vec::new(),
        arrows: Vec::new(),
        refused: None,
    };
    eraser.visit_program(program);
    if let Some((message, at)) = eraser.refused {
        return Err(Refusal::Unsupported(format!(
            "{message} ({})",
            place(source, url, at)
        )));
    }
    let mut out = Output::new(source);
    for &(start, end) in &eraser.erase {
        out.blank(start as usize, end as usize);
    }
    for &(close, start, end) in &eraser.arrows {
        out.move_paren(close as usize, start as usize, end as usize)?;
    }
    for list in &eraser.lists {
        out.separate(list);
    }
    Ok(out.finish())
}

/// What becomes of a statement in strip mode.
enum Verdict {
    Keep,
    Erase,
    Refuse(&'static str),
}

const ENUM: &str = "TypeScript enum is not supported in strip-only mode";
const NAMESPACE: &str = "TypeScript namespace declaration is not supported in strip-only mode";
const MODULE: &str = "`module` keyword is not supported. Use `namespace` instead";
const IMPORT_EQUALS: &str =
    "TypeScript import equals declaration is not supported in strip-only mode";
const EXPORT_ASSIGNMENT: &str = "TypeScript export assignment is not supported in strip-only mode";
const PARAMETER_PROPERTY: &str =
    "TypeScript parameter property is not supported in strip-only mode";
const ASSERTION: &str = "The angle-bracket syntax for type assertions, `<T>expr`, is not supported in type strip mode. Instead, use the 'as' syntax: `expr as T`";

fn declaration_verdict(declaration: &Declaration) -> Verdict {
    match declaration {
        Declaration::VariableDeclaration(it) if it.declare => Verdict::Erase,
        Declaration::FunctionDeclaration(it) if it.declare || it.body.is_none() => Verdict::Erase,
        Declaration::ClassDeclaration(it) if it.declare => Verdict::Erase,
        Declaration::TSTypeAliasDeclaration(_)
        | Declaration::TSInterfaceDeclaration(_)
        | Declaration::TSExternalModuleDeclaration(_)
        | Declaration::TSGlobalDeclaration(_) => Verdict::Erase,
        Declaration::TSEnumDeclaration(it) if it.declare => Verdict::Erase,
        Declaration::TSEnumDeclaration(_) => Verdict::Refuse(ENUM),
        Declaration::TSNamespaceDeclaration(it) if it.declare => Verdict::Erase,
        Declaration::TSNamespaceDeclaration(it)
            if it.kind == TSNamespaceDeclarationKind::Module =>
        {
            Verdict::Refuse(MODULE)
        }
        Declaration::TSNamespaceDeclaration(it) if namespace_is_types(it) => Verdict::Erase,
        Declaration::TSNamespaceDeclaration(_) => Verdict::Refuse(NAMESPACE),
        Declaration::TSImportEqualsDeclaration(it) if it.import_kind.is_type() => Verdict::Erase,
        Declaration::TSImportEqualsDeclaration(_) => Verdict::Refuse(IMPORT_EQUALS),
        _ => Verdict::Keep,
    }
}

fn statement_verdict(statement: &Statement) -> Verdict {
    match statement {
        Statement::ImportDeclaration(it) if it.import_kind.is_type() => Verdict::Erase,
        Statement::ExportNamedDeclaration(it) if it.export_kind.is_type() => Verdict::Erase,
        Statement::ExportFromDeclaration(it) if it.export_kind.is_type() => Verdict::Erase,
        Statement::ExportAllDeclaration(it) if it.export_kind.is_type() => Verdict::Erase,
        Statement::ExportDeclaration(it) => declaration_verdict(&it.declaration),
        Statement::ExportDefaultDeclaration(it) => match &it.declaration {
            ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => Verdict::Erase,
            ExportDefaultDeclarationKind::FunctionDeclaration(f)
                if f.declare || f.body.is_none() =>
            {
                Verdict::Erase
            }
            _ => Verdict::Keep,
        },
        Statement::TSExportAssignment(_) => Verdict::Refuse(EXPORT_ASSIGNMENT),
        Statement::TSNamespaceExportDeclaration(_) => Verdict::Erase,
        _ => match statement.as_declaration() {
            Some(declaration) => declaration_verdict(declaration),
            None => Verdict::Keep,
        },
    }
}

/// A namespace TypeScript compiles to nothing: types, and namespaces of types, only.
fn namespace_is_types(namespace: &TSNamespaceDeclaration) -> bool {
    match &namespace.body {
        TSNamespaceDeclarationBody::TSNamespaceDeclaration(inner) => {
            inner.declare || namespace_is_types(inner)
        }
        TSNamespaceDeclarationBody::TSModuleBlock(block) => block.body.iter().all(|s| {
            matches!(s, Statement::EmptyStatement(_))
                || matches!(statement_verdict(s), Verdict::Erase)
        }),
    }
}

/// Walks the program and notes what to erase: whole statements and members, and the types,
/// modifiers and assertions inside the code that stays.
struct Eraser<'s> {
    src: &'s [u8],
    erase: Vec<(u32, u32)>,
    /// Every list of statements or class members, by their spans, for the `;` pass.
    lists: Vec<Vec<Span>>,
    /// The directives of the body whose statements come next: the head of that list.
    leading: Vec<Span>,
    /// Arrow functions whose return type spans lines: the `)` and the type's span.
    arrows: Vec<(u32, u32, u32)>,
    refused: Option<(&'static str, u32)>,
}

const MODIFIERS: [&str; 7] = [
    "public",
    "private",
    "protected",
    "readonly",
    "override",
    "declare",
    "abstract",
];

impl Eraser<'_> {
    fn blank(&mut self, start: u32, end: u32) {
        if start < end {
            self.erase.push((start, end));
        }
    }

    fn refuse(&mut self, message: &'static str, at: u32) {
        self.refused.get_or_insert((message, at));
    }

    /// The first byte at or after `at` that is not whitespace or a comment.
    fn skip_trivia(&self, mut at: usize) -> usize {
        let src = self.src;
        while at < src.len() {
            match src[at] {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c => at += 1,
                b'/' if src.get(at + 1) == Some(&b'/') => {
                    while at < src.len() && src[at] != b'\n' {
                        at += 1;
                    }
                }
                b'/' if src.get(at + 1) == Some(&b'*') => {
                    at = find(src, at + 2, b"*/").map_or(src.len(), |end| end + 2);
                }
                _ => break,
            }
        }
        at
    }

    /// Erases `span`, and the `,` after it when there is one: a type-only specifier or a
    /// `this` parameter out of a list.
    fn blank_item(&mut self, span: Span) {
        let after = self.skip_trivia(span.end as usize);
        let end = if self.src.get(after) == Some(&b',') {
            after + 1
        } else {
            span.end as usize
        };
        self.blank(span.start, end as u32);
    }

    /// Erases the `?` or `!` that follows `after` (past a computed key's `]`).
    fn blank_mark(&mut self, after: u32, mark: u8) {
        let mut at = self.skip_trivia(after as usize);
        if self.src.get(at) == Some(&b']') {
            at = self.skip_trivia(at + 1);
        }
        if self.src.get(at) == Some(&mark) {
            self.blank(at as u32, at as u32 + 1);
        }
    }

    /// Erases TypeScript's modifiers among the words from `from` to `to`, before a member's
    /// name: `static`, `async`, `get`, `set` and `accessor` stay.
    fn blank_modifiers(&mut self, from: u32, to: u32) {
        let mut at = from as usize;
        loop {
            at = self.skip_trivia(at);
            if at >= to as usize {
                break;
            }
            let end = at + self.src[at..].iter().take_while(|b| is_word(**b)).count();
            if end == at {
                break;
            }
            if MODIFIERS.iter().any(|m| m.as_bytes() == &self.src[at..end]) {
                self.blank(at as u32, end as u32);
            }
            at = end;
        }
    }

    /// Erases `element` if TypeScript alone has it, else its modifiers and marks. Answers
    /// whether it stays.
    fn member(&mut self, element: &ClassElement) -> bool {
        let erase = match element {
            ClassElement::TSIndexSignature(_) => true,
            ClassElement::PropertyDefinition(it) => {
                it.declare || it.r#type == PropertyDefinitionType::TSAbstractPropertyDefinition
            }
            ClassElement::MethodDefinition(it) => {
                it.r#type == MethodDefinitionType::TSAbstractMethodDefinition
                    || it.value.body.is_none()
            }
            ClassElement::AccessorProperty(it) => {
                it.r#type == AccessorPropertyType::TSAbstractAccessorProperty
            }
            ClassElement::StaticBlock(_) => false,
        };
        if erase {
            let span = element.span();
            self.blank(span.start, span.end);
            return false;
        }
        let (decorators, key, optional, definite) = match element {
            ClassElement::PropertyDefinition(it) => {
                (&it.decorators, &it.key, it.optional, it.definite)
            }
            ClassElement::MethodDefinition(it) => (&it.decorators, &it.key, it.optional, false),
            ClassElement::AccessorProperty(it) => (&it.decorators, &it.key, false, it.definite),
            _ => return true,
        };
        let from = decorators
            .last()
            .map_or(element.span().start, |d| d.span.end);
        let key_span = key.span();
        self.blank_modifiers(from, key_span.start);
        if optional {
            self.blank_mark(key_span.end, b'?');
        }
        if definite {
            self.blank_mark(key_span.end, b'!');
        }
        true
    }
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

impl<'a> Visit<'a> for Eraser<'_> {
    fn visit_program(&mut self, it: &Program<'a>) {
        self.leading = it.directives.iter().map(|d| d.span).collect();
        walk::walk_program(self, it);
    }

    fn visit_function_body(&mut self, it: &FunctionBody<'a>) {
        self.leading = it.directives.iter().map(|d| d.span).collect();
        walk::walk_function_body(self, it);
    }

    fn visit_statements(&mut self, it: &ArenaVec<'a, Statement<'a>>) {
        let mut list = std::mem::take(&mut self.leading);
        list.extend(it.iter().map(GetSpan::span));
        self.lists.push(list);
        walk::walk_statements(self, it);
    }

    fn visit_statement(&mut self, it: &Statement<'a>) {
        match statement_verdict(it) {
            Verdict::Keep => walk::walk_statement(self, it),
            Verdict::Erase => {
                let span = it.span();
                self.blank(span.start, span.end);
            }
            Verdict::Refuse(message) => self.refuse(message, it.span().start),
        }
    }

    fn visit_class_elements(&mut self, it: &ArenaVec<'a, ClassElement<'a>>) {
        self.lists.push(it.iter().map(GetSpan::span).collect());
        walk::walk_class_elements(self, it);
    }

    fn visit_class_element(&mut self, it: &ClassElement<'a>) {
        if self.member(it) {
            walk::walk_class_element(self, it);
        }
    }

    fn visit_class(&mut self, it: &Class<'a>) {
        if it.r#abstract {
            let from = it.decorators.last().map_or(it.span.start, |d| d.span.end);
            let at = self.skip_trivia(from as usize);
            if self.src[at..].starts_with(b"abstract") {
                self.blank(at as u32, at as u32 + 8);
            }
        }
        if let (Some(first), Some(last)) = (it.implements.first(), it.implements.last()) {
            let head = &self.src[..first.span.start as usize];
            let start = head
                .windows(10)
                .rposition(|w| w == b"implements")
                .map_or(first.span.start, |i| i as u32);
            self.blank(start, last.span.end);
        }
        walk::walk_class(self, it);
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: oxc_semantic::ScopeFlags) {
        if let Some(this) = &it.this_param {
            self.blank_item(this.span);
        }
        walk::walk_function(self, it, flags);
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        if let Some(returns) = &it.return_type {
            let close = it.params.span.end.saturating_sub(1);
            let between = &self.src[close as usize..returns.span.end as usize];
            if self.src[close as usize] == b')' && between.contains(&b'\n') {
                self.arrows
                    .push((close, returns.span.start, returns.span.end));
            }
        }
        walk::walk_arrow_function_expression(self, it);
    }

    fn visit_formal_parameter(&mut self, it: &FormalParameter<'a>) {
        if it.accessibility.is_some() || it.readonly || it.r#override {
            self.refuse(PARAMETER_PROPERTY, it.span.start);
        }
        if it.optional {
            self.blank_mark(it.pattern.span().end, b'?');
        }
        walk::walk_formal_parameter(self, it);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if it.definite {
            self.blank_mark(it.id.span().end, b'!');
        }
        walk::walk_variable_declarator(self, it);
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        for specifier in it.specifiers.iter().flatten() {
            if let ImportDeclarationSpecifier::ImportSpecifier(s) = specifier
                && s.import_kind.is_type()
            {
                self.blank_item(s.span);
            }
        }
    }

    fn visit_export_named_declaration(&mut self, it: &ExportNamedDeclaration<'a>) {
        for s in &it.specifiers {
            if s.export_kind.is_type() {
                self.blank_item(s.span);
            }
        }
    }

    fn visit_export_from_declaration(&mut self, it: &ExportFromDeclaration<'a>) {
        for s in &it.specifiers {
            if s.export_kind.is_type() {
                self.blank_item(s.span);
            }
        }
    }

    fn visit_ts_this_parameter(&mut self, _: &TSThisParameter<'a>) {}

    fn visit_ts_type_annotation(&mut self, it: &TSTypeAnnotation<'a>) {
        self.blank(it.span.start, it.span.end);
    }

    fn visit_ts_type_parameter_declaration(&mut self, it: &TSTypeParameterDeclaration<'a>) {
        self.blank(it.span.start, it.span.end);
    }

    fn visit_ts_type_parameter_instantiation(&mut self, it: &TSTypeParameterInstantiation<'a>) {
        self.blank(it.span.start, it.span.end);
    }

    fn visit_ts_as_expression(&mut self, it: &TSAsExpression<'a>) {
        self.blank(it.expression.span().end, it.span.end);
        self.visit_expression(&it.expression);
    }

    fn visit_ts_satisfies_expression(&mut self, it: &TSSatisfiesExpression<'a>) {
        self.blank(it.expression.span().end, it.span.end);
        self.visit_expression(&it.expression);
    }

    fn visit_ts_non_null_expression(&mut self, it: &TSNonNullExpression<'a>) {
        self.blank(it.expression.span().end, it.span.end);
        self.visit_expression(&it.expression);
    }

    fn visit_ts_instantiation_expression(&mut self, it: &TSInstantiationExpression<'a>) {
        self.visit_expression(&it.expression);
        self.visit_ts_type_parameter_instantiation(&it.type_arguments);
    }

    fn visit_ts_type_assertion(&mut self, it: &TSTypeAssertion<'a>) {
        self.refuse(ASSERTION, it.span.start);
    }
}

/// The source as it is erased: its bytes, which of them were erased, and which of those were
/// erased to a plain space and so may take a `;` or a `)`.
struct Output {
    bytes: Vec<u8>,
    erased: Vec<bool>,
    free: Vec<bool>,
}

impl Output {
    fn new(source: &str) -> Self {
        let len = source.len();
        Self {
            bytes: source.as_bytes().to_vec(),
            erased: vec![false; len],
            free: vec![false; len],
        }
    }

    /// Whitespace of the same UTF-8 length and UTF-16 width for each character, keeping line
    /// breaks: U+00A0 for two bytes, U+2002 for three, two U+00A0 for four.
    fn blank(&mut self, start: usize, end: usize) {
        let mut at = start;
        while at < end {
            let len = utf8_len(self.bytes[at]);
            let char = &self.bytes[at..at + len];
            let line_break = char == b"\n"
                || char == b"\r"
                || char == "\u{2028}".as_bytes()
                || char == "\u{2029}".as_bytes();
            if !line_break {
                let space: &[u8] = match len {
                    1 => b" ",
                    2 => "\u{a0}".as_bytes(),
                    3 => "\u{2002}".as_bytes(),
                    _ => "\u{a0}\u{a0}".as_bytes(),
                };
                self.bytes[at..at + len].copy_from_slice(space);
                self.erased[at..at + len].fill(true);
                self.free[at] = len == 1;
            }
            at += len;
        }
    }

    fn put(&mut self, at: usize, byte: u8) {
        self.bytes[at] = byte;
        self.erased[at] = false;
        self.free[at] = false;
    }

    fn is_blank(&self, at: usize) -> bool {
        self.erased[at] || self.bytes[at].is_ascii_whitespace()
    }

    /// Moves an arrow function's `)` from `close` to the last free place on the last line of
    /// its return type, `start..end`, so no line break comes before `=>`.
    fn move_paren(&mut self, close: usize, start: usize, end: usize) -> Result<(), Refusal> {
        let line = self.bytes[start..end]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(start, |i| start + i + 1);
        let Some(slot) = (line..end).rev().find(|&i| self.free[i]) else {
            return Err(Refusal::Unsupported(
                "An arrow function's return type ends on a line with no room for its `)`".into(),
            ));
        };
        self.blank(close, close + 1);
        self.put(slot, b')');
        Ok(())
    }

    /// Puts a `;` between two statements (or members) of `list` where erasing what came between
    /// them would let the second continue the first.
    fn separate(&mut self, list: &[Span]) {
        let mut last: Option<usize> = None;
        for span in list {
            let (start, end) = (span.start as usize, span.end as usize);
            let Some(first) = (start..end).find(|&i| !self.is_blank(i)) else {
                continue;
            };
            if let Some(last) = last
                && self.bytes[last] != b';'
                && b"([`+-/*%<>=,.?:&|^!".contains(&self.bytes[first])
                && let Some(slot) = (last + 1..first).find(|&i| self.free[i])
            {
                self.put(slot, b';');
            }
            last = (start..end).rev().find(|&i| !self.is_blank(i));
        }
    }

    fn finish(self) -> String {
        String::from_utf8(self.bytes).expect("erasing keeps whole characters")
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xf0.. => 4,
        0xe0.. => 3,
        0xc0.. => 2,
        _ => 1,
    }
}

// ─── `module.stripTypeScriptTypes` ───────────────────────────────────────────────────────────

/// `stripTypeScriptTypes(code[, options])`, as a function of its own: it warns once, as Node's
/// does, that it is experimental.
pub(crate) fn function(context: &mut Context) -> JsResult<JsValue> {
    let state = object(context);
    Ok(closure(context, "stripTypeScriptTypes", 1, call, state).into())
}

fn call(
    _: &JsValue,
    args: &[JsValue],
    state: &boa_engine::JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    if !state.get(js_string!("warned"), context)?.to_boolean() {
        state.set(js_string!("warned"), true, false, context)?;
        let process = crate::native::global(context, "process")?;
        if get(&process, "emitWarning", context)?.is_callable() {
            let message = js_text(
                "stripTypeScriptTypes is an experimental feature and might change at any time",
            );
            invoke(
                &process,
                "emitWarning",
                &[message, js_text("ExperimentalWarning")],
                context,
            )?;
        }
    }
    let code = arg(args, 0);
    let Some(code) = code.as_string().map(|s| s.to_std_string_escaped()) else {
        return Err(type_error(context, "code", "string", &code));
    };
    let options = arg(args, 1);
    if !options.is_undefined() && !options.is_object() {
        return Err(type_error(context, "options", "object", &options));
    }
    let mode = match get(&options, "mode", context)? {
        m if m.is_undefined() => Mode::Strip,
        m if m.as_string().is_some_and(|s| s == js_string!("strip")) => Mode::Strip,
        m if m.as_string().is_some_and(|s| s == js_string!("transform")) => Mode::Transform,
        m => {
            let message = format!(
                "The property 'options.mode' must be one of: 'strip', 'transform'. Received {}",
                shown(&m)
            );
            return Err(coded(
                context,
                JsNativeError::typ(),
                &message,
                "ERR_INVALID_ARG_VALUE",
            ));
        }
    };
    let source_map = match get(&options, "sourceMap", context)? {
        m if m.is_undefined() => false,
        m => match m.as_boolean() {
            Some(b) => b,
            None => return Err(type_error(context, "options.sourceMap", "boolean", &m)),
        },
    };
    if source_map && mode == Mode::Strip {
        let message =
            "The property 'options.sourceMap' must be one of: false, undefined. Received true";
        return Err(coded(
            context,
            JsNativeError::typ(),
            message,
            "ERR_INVALID_ARG_VALUE",
        ));
    }
    let url = match get(&options, "sourceUrl", context)? {
        u if u.is_undefined() => String::new(),
        u => match u.as_string() {
            Some(s) => s.to_std_string_escaped(),
            None => return Err(type_error(context, "options.sourceUrl", "string", &u)),
        },
    };
    match strip_types(&code, mode, source_map, &url) {
        Ok(js) => Ok(js_text(&js)),
        Err(Refusal::Invalid(message)) => Err(coded(
            context,
            JsNativeError::syntax(),
            &message,
            "ERR_INVALID_TYPESCRIPT_SYNTAX",
        )),
        Err(Refusal::Unsupported(message)) => Err(coded(
            context,
            JsNativeError::syntax(),
            &message,
            "ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX",
        )),
    }
}

fn coded(context: &mut Context, kind: JsNativeError, message: &str, code: &str) -> JsError {
    let error = kind.with_message(message.to_owned()).into_opaque(context);
    let _ = error.set(js_string!("code"), JsString::from(code), false, context);
    JsError::from_opaque(error.into())
}

fn type_error(context: &mut Context, name: &str, expected: &str, value: &JsValue) -> JsError {
    let kind = if name.contains('.') {
        "property"
    } else {
        "argument"
    };
    let message = format!(
        "The \"{name}\" {kind} must be of type {expected}. Received {}",
        shown(value)
    );
    coded(
        context,
        JsNativeError::typ(),
        &message,
        "ERR_INVALID_ARG_TYPE",
    )
}

fn shown(value: &JsValue) -> String {
    if value.is_undefined() || value.is_null() {
        crate::print(value)
    } else if value.is_object() {
        "an instance of Object".into()
    } else if value.is_string() {
        format!("type string ('{}')", crate::print(value))
    } else {
        format!("type {} ({})", value.type_of(), crate::print(value))
    }
}
