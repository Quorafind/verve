//! A module's compiled code, written down and read back: Boa's half of a code cache.
//!
//! [`Module::code_cache`](crate::Module::code_cache) compiles a module with every function it
//! declares and writes down what linking it needs: its import and export entries, its scopes
//! and its code. [`Module::from_code_cache`](crate::Module::from_code_cache) reads that back
//! into a module that links and runs as the parsed one would, without parsing or compiling.
//!
//! # What is checked, and what is trusted
//!
//! Reading checks that the bytes are well formed, so that bytes that were damaged or written by
//! another build are refused rather than run:
//!
//! - the header: this format, this build's [`fingerprint`], the length and digest of the source
//!   text the code was compiled from, and the global scope it was compiled against;
//! - every length, count, index and tag, against what is left to read and what it indexes;
//! - every scope: binding flags, names bound once, binding indices within the scope;
//! - every code block: its flags; its bytecode, instruction by instruction, with each register
//!   within the frame and each jump to the start of an instruction; its exception handlers; its
//!   constants and source map; every span within the source text;
//! - every binding of the global scope it names, against the realm it is read into.
//!
//! Nothing read is trusted to be in bounds, and reading never panics. Well-formed code is run as
//! written, though: what its instructions do with constant, binding and inline cache indices is
//! not proved here, and code written by someone else can do whatever bytecode can. So the bytes
//! must come from a writer the embedder trusts, as it trusts the source text itself; an embedder
//! that keeps them on disk must authenticate them (Verve signs them with a key of its own).

use std::path::Path;

use boa_ast::{
    LinearPosition, LinearSpan, Position, Span,
    declaration::{
        ImportAttribute as AstImportAttribute, ImportEntry, ImportName, IndirectExportEntry,
        LocalExportEntry, ReExportImportName,
    },
    expression::Identifier,
    scope::{BindingLocator, BindingParts, Scope, ScopeParts},
};
use boa_gc::Gc;
use boa_interner::{Interner, Sym};
use rustc_hash::FxHashMap;
use thin_vec::ThinVec;

use crate::{
    JsBigInt, JsString, SpannedSourceText,
    builtins::function::ThisMode,
    bytecompiler::ToJsString,
    module::{CompiledModule, ImportAttribute, ModuleCode, ModuleRequest},
    realm::Realm,
    spanned_source_text::SourceText,
    string::JsStrVariant,
    vm::{
        CodeBlock, CodeBlockFlags, Constant, GlobalFunctionBinding, Handler, InlineCache,
        opcode::{Address, Bytecode, Opcode, Operand},
        source_info::{Entry, SourceInfo, SourceMap, SourcePath},
    },
};

const MAGIC: [u8; 4] = *b"BOAC";
/// The layout of what is written down; changed whenever it changes.
const FORMAT: u32 = 1;
/// More registers than a frame of real code has, so a count beyond it is refused.
const MAX_REGISTERS: u32 = 1 << 20;

/// What code written down by this build can be read by: this format, Boa's version, this build's
/// opcodes and the features that change what code compiles to. Code written by a build with
/// another fingerprint is refused; an embedder can mix the fingerprint into its own key for a
/// cache entry, with what identifies its own build.
#[must_use]
pub fn fingerprint() -> u64 {
    let mut hash = Fnv::new();
    hash.write(&FORMAT.to_le_bytes());
    hash.write(env!("CARGO_PKG_VERSION").as_bytes());
    for opcode in 0..=u8::MAX {
        hash.write(Opcode::decode(opcode).as_str().as_bytes());
        hash.write(&[0]);
    }
    hash.write(&[u8::from(cfg!(feature = "annex-b"))]);
    hash.write(&(size_of::<CodeBlock>() as u64).to_le_bytes());
    hash.finish()
}

/// Whether every module parsed is written down and read back before it is used: the
/// `BOA_CODE_CACHE_ROUNDTRIP` test switch (`1` to turn it on), read once.
pub(crate) fn round_trip() -> bool {
    static ROUND_TRIP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ROUND_TRIP.get_or_init(|| {
        std::env::var("BOA_CODE_CACHE_ROUNDTRIP").is_ok_and(|value| {
            matches!(
                cow_utils::CowUtils::cow_to_ascii_lowercase(value.trim()).as_ref(),
                "1" | "true" | "on" | "yes"
            )
        })
    })
}

/// FNV-1a: a digest to tell apart, not to authenticate.
struct Fnv(u64);

impl Fnv {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn write_u16s(&mut self, units: &[u16]) {
        for unit in units {
            self.write(&unit.to_le_bytes());
        }
    }

    const fn finish(&self) -> u64 {
        self.0
    }
}

/// The digest of the global scope's bindings: code is compiled against them (a name bound
/// there is found by its index, any other on the global object), so it must be read into a
/// realm whose global scope binds the same names.
fn global_digest(global: &Scope) -> u64 {
    let parts = global.parts();
    let mut hash = Fnv::new();
    hash.write(&parts.index.to_le_bytes());
    for binding in &parts.bindings {
        write_digest_string(&mut hash, &binding.name);
        hash.write(&binding.index.to_le_bytes());
        hash.write(&[binding.flags]);
    }
    hash.finish()
}

fn write_digest_string(hash: &mut Fnv, string: &JsString) {
    match string.as_str().variant() {
        JsStrVariant::Latin1(bytes) => {
            hash.write(&[0]);
            hash.write(&(bytes.len() as u64).to_le_bytes());
            hash.write(bytes);
        }
        JsStrVariant::Utf16(units) => {
            hash.write(&[1]);
            hash.write(&(units.len() as u64).to_le_bytes());
            hash.write_u16s(units);
        }
    }
}

fn source_digest(units: &[u16]) -> u64 {
    let mut hash = Fnv::new();
    hash.write_u16s(units);
    hash.finish()
}

// ---- Writing ----

/// Writes down a compiled module, or answers `None` if something in it cannot be: a function
/// not compiled yet, code shared between two places, or a scope outside the realm's.
pub(crate) fn write_module(
    code: &ModuleCode,
    compiled: &CompiledModule,
    source_text: &[u16],
    realm: &Realm,
    interner: &Interner,
) -> Option<Vec<u8>> {
    let global = realm.scope().clone();
    let mut writer = Writer {
        out: Vec::with_capacity(source_text.len() * 2),
        interner,
        global: global.clone(),
        scope_ids: FxHashMap::default(),
        scopes: Vec::new(),
        code_ids: FxHashMap::default(),
        codes: Vec::new(),
    };
    writer.collect_code(&compiled.codeblock)?;
    let module_scope = writer.scope_ref(&compiled.scope)?;
    for index in 0..writer.codes.len() {
        let code = writer.codes[index].clone();
        for constant in &code.constants {
            if let Constant::Scope(scope) = constant {
                writer.scope_ref(scope)?;
            }
        }
    }

    writer.out.extend_from_slice(&MAGIC);
    writer.u32(FORMAT);
    writer.u64(fingerprint());
    writer.u64(source_text.len() as u64);
    writer.u64(source_digest(source_text));
    writer.u64(global_digest(&global));

    writer.module_code(code);

    writer.u32(writer.scopes.len() as u32);
    for index in 0..writer.scopes.len() {
        let scope = writer.scopes[index].clone();
        let outer = scope.outer()?;
        let outer = writer.scope_ref(outer)?;
        writer.u32(outer);
        let parts = scope.parts();
        writer.u32(parts.index);
        writer.u8(u8::from(parts.function)
            | (u8::from(parts.arrow) << 1)
            | (u8::from(parts.this_escaped) << 2));
        writer.u32(parts.bindings.len() as u32);
        for binding in &parts.bindings {
            writer.string(&binding.name);
            writer.u32(binding.index);
            writer.u8(binding.flags);
        }
    }
    writer.u32(module_scope);

    writer.u32(writer.codes.len() as u32);
    for index in 0..writer.codes.len() {
        let code = writer.codes[index].clone();
        writer.code(&code)?;
    }

    writer.u32(compiled.functions.len() as u32);
    for (index, locator) in &compiled.functions {
        writer.u32(*index);
        writer.locator(locator);
    }
    Some(writer.out)
}

struct Writer<'a> {
    out: Vec<u8>,
    interner: &'a Interner,
    global: Scope,
    scope_ids: FxHashMap<usize, u32>,
    scopes: Vec<Scope>,
    code_ids: FxHashMap<usize, u32>,
    codes: Vec<Gc<CodeBlock>>,
}

impl Writer<'_> {
    fn u8(&mut self, value: u8) {
        self.out.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.u64(u64::from(value));
    }

    fn u64(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.out.push(byte);
                return;
            }
            self.out.push(byte | 0x80);
        }
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.u64(bytes.len() as u64);
        self.out.extend_from_slice(bytes);
    }

    fn string(&mut self, string: &JsString) {
        match string.as_str().variant() {
            JsStrVariant::Latin1(bytes) => {
                self.u8(0);
                self.bytes(bytes);
            }
            JsStrVariant::Utf16(units) => {
                self.u8(1);
                self.u64(units.len() as u64);
                for unit in units {
                    self.out.extend_from_slice(&unit.to_le_bytes());
                }
            }
        }
    }

    fn sym(&mut self, sym: Sym) {
        let string = sym.to_js_string(self.interner);
        self.string(&string);
    }

    fn position(&mut self, position: Position) {
        self.u32(position.line_number());
        self.u32(position.column_number());
    }

    fn attributes(&mut self, attributes: &[AstImportAttribute]) {
        self.u32(attributes.len() as u32);
        for attribute in attributes {
            self.sym(attribute.key());
            self.sym(attribute.value());
        }
    }

    fn request(&mut self, request: &ModuleRequest) {
        self.string(request.specifier());
        self.u32(request.attributes().len() as u32);
        for attribute in request.attributes() {
            self.string(attribute.key());
            self.string(attribute.value());
        }
    }

    fn locator(&mut self, locator: &BindingLocator) {
        let (name, scope, index, unique) = locator.parts();
        self.string(&name);
        self.u32(scope);
        self.u32(index);
        self.u32(unique);
    }

    fn module_code(&mut self, code: &ModuleCode) {
        self.u8(u8::from(code.has_tla));
        self.u32(code.requested_modules.len() as u32);
        for request in &code.requested_modules {
            self.request(request);
        }
        self.u32(code.import_entries.len() as u32);
        for entry in &code.import_entries {
            self.sym(entry.module_request());
            match entry.import_name() {
                ImportName::Namespace => self.u8(0),
                ImportName::Name(name) => {
                    self.u8(1);
                    self.sym(name);
                }
            }
            let local = entry.local_name();
            self.sym(local.sym());
            self.position(boa_ast::Spanned::span(&local).start());
            self.position(boa_ast::Spanned::span(&local).end());
            self.attributes(entry.attributes());
        }
        self.u32(code.local_export_entries.len() as u32);
        for entry in &code.local_export_entries {
            self.sym(entry.local_name());
            self.sym(entry.export_name());
        }
        self.u32(code.indirect_export_entries.len() as u32);
        for entry in &code.indirect_export_entries {
            self.sym(entry.module_request());
            match entry.import_name() {
                ReExportImportName::Star => self.u8(0),
                ReExportImportName::Name(name) => {
                    self.u8(1);
                    self.sym(name);
                }
            }
            self.sym(entry.export_name());
            self.attributes(entry.attributes());
        }
        self.u32(code.star_export_entries.len() as u32);
        for request in &code.star_export_entries {
            self.request(request);
        }
    }

    /// Numbers a code block and those nested in it, parents first.
    fn collect_code(&mut self, code: &Gc<CodeBlock>) -> Option<()> {
        if code.is_lazy() {
            return None;
        }
        let address = std::ptr::from_ref::<CodeBlock>(code) as usize;
        if self.code_ids.contains_key(&address) {
            return None;
        }
        self.code_ids.insert(address, self.codes.len() as u32);
        self.codes.push(code.clone());
        for constant in &code.constants {
            if let Constant::Function(function) = constant {
                self.collect_code(function)?;
            }
        }
        Some(())
    }

    /// The number a scope is written down as: 0 for the realm's global scope, `n` for the
    /// `n`th of the module's, its outer scopes numbered first.
    fn scope_ref(&mut self, scope: &Scope) -> Option<u32> {
        if scope.ptr_eq(&self.global) {
            return Some(0);
        }
        if let Some(id) = self.scope_ids.get(&scope.addr()) {
            return Some(*id);
        }
        self.scope_ref(scope.outer()?)?;
        self.scopes.push(scope.clone());
        let id = self.scopes.len() as u32;
        self.scope_ids.insert(scope.addr(), id);
        Some(id)
    }

    fn code(&mut self, code: &CodeBlock) -> Option<()> {
        self.u32(u32::from(code.flags.get().bits()));
        self.u32(code.length);
        self.u32(code.parameter_length);
        self.u32(code.register_count);
        self.u8(match code.this_mode {
            ThisMode::Lexical => 0,
            ThisMode::Strict => 1,
            ThisMode::Global => 2,
        });
        self.u32(code.mapped_arguments_binding_indices.len() as u32);
        for index in &code.mapped_arguments_binding_indices {
            self.u32(index.map_or(0, |index| index + 1));
        }
        self.bytes(&code.bytecode.bytes);
        self.u32(code.constants.len() as u32);
        for constant in &code.constants {
            match constant {
                Constant::String(string) => {
                    self.u8(0);
                    self.string(string);
                }
                Constant::Function(function) => {
                    self.u8(1);
                    let address = std::ptr::from_ref::<CodeBlock>(function) as usize;
                    let id = *self.code_ids.get(&address)?;
                    self.u32(id);
                }
                Constant::BigInt(bigint) => {
                    self.u8(2);
                    self.bytes(bigint.to_string_radix(16).as_bytes());
                }
                Constant::Scope(scope) => {
                    self.u8(3);
                    let id = self.scope_ref(scope)?;
                    self.u32(id);
                }
            }
        }
        self.u32(code.bindings.len() as u32);
        for locator in &code.bindings {
            self.locator(locator);
        }
        self.u32(code.handlers.len() as u32);
        for handler in &code.handlers {
            self.u32(handler.start.as_u32());
            self.u32(handler.end.as_u32());
            self.u32(handler.environment_count);
        }
        self.u32(code.ic.len() as u32);
        for ic in &code.ic {
            self.string(&ic.name);
        }
        let map = code.source_info.map();
        self.u32(map.entries().len() as u32);
        for entry in map.entries() {
            self.u32(entry.pc);
            match entry.position {
                None => self.u8(0),
                Some(position) => {
                    self.u8(1);
                    self.position(position);
                }
            }
        }
        self.string(code.name());
        match code.source_info.text_spanned().span() {
            None => self.u8(0),
            Some(span) => {
                self.u8(1);
                self.u64(span.start().pos() as u64);
                self.u64(span.end().pos() as u64);
            }
        }
        self.u32(code.global_lexs.len() as u32);
        for index in &code.global_lexs {
            self.u32(*index);
        }
        self.u32(code.global_fns.len() as u32);
        for binding in &code.global_fns {
            self.u32(binding.name_index);
            self.u32(binding.function_index);
        }
        self.u32(code.global_vars.len() as u32);
        for index in &code.global_vars {
            self.u32(*index);
        }
        Some(())
    }
}

// ---- Reading ----

/// Why written-down code was refused.
pub(crate) type Refused = &'static str;

type Read<T> = Result<T, Refused>;

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn u8(&mut self) -> Read<u8> {
        let byte = *self.bytes.get(self.pos).ok_or("truncated")?;
        self.pos += 1;
        Ok(byte)
    }

    fn u64(&mut self) -> Read<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.u8()?;
            let bits = u64::from(byte & 0x7f);
            if shift == 63 && bits > 1 {
                return Err("varint overflows");
            }
            value |= bits << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("varint too long")
    }

    fn u32(&mut self) -> Read<u32> {
        u32::try_from(self.u64()?).map_err(|_| "number out of range")
    }

    fn bool(&mut self) -> Read<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("not a boolean"),
        }
    }

    /// A count of things that take at least `size` bytes each, no more than are left.
    fn count(&mut self, size: usize) -> Read<usize> {
        let count = usize::try_from(self.u64()?).map_err(|_| "count out of range")?;
        if count
            .checked_mul(size)
            .is_none_or(|bytes| bytes > self.remaining())
        {
            return Err("count beyond the end");
        }
        Ok(count)
    }

    fn take(&mut self, len: usize) -> Read<&'a [u8]> {
        let end = self.pos.checked_add(len).ok_or("length out of range")?;
        let bytes = self.bytes.get(self.pos..end).ok_or("truncated")?;
        self.pos = end;
        Ok(bytes)
    }

    fn bytes(&mut self) -> Read<&'a [u8]> {
        let len = self.count(1)?;
        self.take(len)
    }

    fn string(&mut self) -> Read<JsString> {
        match self.u8()? {
            0 => {
                let bytes = self.bytes()?;
                Ok(JsString::from(crate::string::JsStr::latin1(bytes)))
            }
            1 => {
                let len = self.count(2)?;
                let bytes = self.take(len * 2)?;
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                Ok(JsString::from(&units[..]))
            }
            _ => Err("unknown string encoding"),
        }
    }

    fn sym(&mut self, interner: &mut Interner) -> Read<Sym> {
        let string = self.string()?;
        let units = string.to_vec();
        Ok(interner.get_or_intern(&units[..]))
    }

    fn position(&mut self) -> Read<Position> {
        let line = self.u32()?;
        let column = self.u32()?;
        if line == 0 || column == 0 {
            return Err("position at line or column 0");
        }
        Ok(Position::new(line, column))
    }

    fn attributes(&mut self, interner: &mut Interner) -> Read<Box<[AstImportAttribute]>> {
        let count = self.count(4)?;
        let mut attributes = Vec::with_capacity(count);
        for _ in 0..count {
            let key = self.sym(interner)?;
            let value = self.sym(interner)?;
            attributes.push(AstImportAttribute::new(key, value));
        }
        Ok(attributes.into_boxed_slice())
    }

    fn request(&mut self) -> Read<ModuleRequest> {
        let specifier = self.string()?;
        let count = self.count(4)?;
        let mut attributes = Vec::with_capacity(count);
        for _ in 0..count {
            let key = self.string()?;
            let value = self.string()?;
            attributes.push(ImportAttribute::new(key, value));
        }
        Ok(ModuleRequest::new(specifier, attributes.into_boxed_slice()))
    }

    fn locator(&mut self) -> Read<BindingLocator> {
        let name = self.string()?;
        let scope = self.u32()?;
        let index = self.u32()?;
        let unique = self.u32()?;
        Ok(BindingLocator::from_parts(name, scope, index, unique))
    }

    fn module_code(&mut self, path: Option<&Path>, interner: &mut Interner) -> Read<ModuleCode> {
        let has_tla = self.bool()?;
        let count = self.count(2)?;
        let mut requested_modules = indexmap::IndexSet::default();
        for _ in 0..count {
            if !requested_modules.insert(self.request()?) {
                return Err("a module requested twice");
            }
        }
        let count = self.count(4)?;
        let mut import_entries = Vec::with_capacity(count);
        for _ in 0..count {
            let module_request = self.sym(interner)?;
            let import_name = match self.u8()? {
                0 => ImportName::Namespace,
                1 => ImportName::Name(self.sym(interner)?),
                _ => return Err("unknown import name"),
            };
            let local = self.sym(interner)?;
            let start = self.position()?;
            let end = self.position()?;
            if start > end {
                return Err("span ends before it starts");
            }
            let attributes = self.attributes(interner)?;
            import_entries.push(ImportEntry::new(
                module_request,
                import_name,
                Identifier::new(local, Span::new(start, end)),
                attributes,
            ));
        }
        let count = self.count(2)?;
        let mut local_export_entries = Vec::with_capacity(count);
        for _ in 0..count {
            let local = self.sym(interner)?;
            let export = self.sym(interner)?;
            local_export_entries.push(LocalExportEntry::new(local, export));
        }
        let count = self.count(4)?;
        let mut indirect_export_entries = Vec::with_capacity(count);
        for _ in 0..count {
            let module_request = self.sym(interner)?;
            let import_name = match self.u8()? {
                0 => ReExportImportName::Star,
                1 => ReExportImportName::Name(self.sym(interner)?),
                _ => return Err("unknown re-export name"),
            };
            let export_name = self.sym(interner)?;
            let attributes = self.attributes(interner)?;
            indirect_export_entries.push(IndirectExportEntry::new(
                module_request,
                import_name,
                export_name,
                attributes,
            ));
        }
        let count = self.count(2)?;
        let mut star_export_entries = Vec::with_capacity(count);
        for _ in 0..count {
            star_export_entries.push(self.request()?);
        }
        Ok(ModuleCode {
            has_tla,
            requested_modules,
            path: path.map(Path::to_path_buf),
            import_entries,
            local_export_entries,
            indirect_export_entries,
            star_export_entries,
        })
    }
}

/// A code block as read, before the code blocks it nests are made.
struct RawCode {
    flags: CodeBlockFlags,
    length: u32,
    parameter_length: u32,
    register_count: u32,
    this_mode: ThisMode,
    mapped_arguments_binding_indices: ThinVec<Option<u32>>,
    bytecode: Box<[u8]>,
    constants: Vec<RawConstant>,
    bindings: Box<[BindingLocator]>,
    handlers: ThinVec<Handler>,
    ic: Box<[InlineCache]>,
    entries: Box<[Entry]>,
    name: JsString,
    span: Option<LinearSpan>,
    global_lexs: Box<[u32]>,
    global_fns: Box<[GlobalFunctionBinding]>,
    global_vars: Box<[u32]>,
}

enum RawConstant {
    String(JsString),
    Function(usize),
    BigInt(JsBigInt),
    Scope(Scope),
}

/// Reads back a module [`write_module`] wrote down from `source_text`, into `realm`.
pub(crate) fn read_module(
    bytes: &[u8],
    source_text: &SourceText,
    path: Option<&Path>,
    realm: &Realm,
    interner: &mut Interner,
) -> Read<(ModuleCode, CompiledModule)> {
    let mut reader = Reader { bytes, pos: 0 };
    if reader.take(4)? != MAGIC {
        return Err("not written-down code");
    }
    if reader.u32()? != FORMAT {
        return Err("another format");
    }
    if reader.u64()? != fingerprint() {
        return Err("written by another build");
    }
    let units = source_text.code_points().unwrap_or(&[]);
    if reader.u64()? != units.len() as u64 || reader.u64()? != source_digest(units) {
        return Err("compiled from another source text");
    }
    let global = realm.scope().clone();
    if reader.u64()? != global_digest(&global) {
        return Err("compiled against another global scope");
    }

    let code = reader.module_code(path, interner)?;

    let count = reader.count(4)?;
    let mut scopes = Vec::with_capacity(count);
    for _ in 0..count {
        let outer = reader.u32()? as usize;
        let outer = match outer {
            0 => global.clone(),
            n => scopes
                .get(n - 1)
                .cloned()
                .ok_or("scope inside one not read yet")?,
        };
        let index = reader.u32()?;
        let flags = reader.u8()?;
        if flags & !0b111 != 0 {
            return Err("unknown scope flags");
        }
        let count = reader.count(4)?;
        let mut bindings = Vec::with_capacity(count);
        for _ in 0..count {
            let name = reader.string()?;
            let index = reader.u32()?;
            let flags = reader.u8()?;
            bindings.push(BindingParts { name, index, flags });
        }
        let parts = ScopeParts {
            index,
            function: flags & 1 != 0,
            arrow: flags & 2 != 0,
            this_escaped: flags & 4 != 0,
            bindings,
        };
        scopes.push(Scope::from_parts(outer, parts)?);
    }
    let scope_ref = |id: u32| -> Read<Scope> {
        match id as usize {
            0 => Ok(global.clone()),
            n => scopes.get(n - 1).cloned().ok_or("no such scope"),
        }
    };
    let module_scope = scope_ref(reader.u32()?)?;
    if module_scope.ptr_eq(&global) {
        return Err("a module scope must be the module's own");
    }

    let count = reader.count(16)?;
    if count == 0 {
        return Err("no code");
    }
    let mut raw = Vec::with_capacity(count);
    for index in 0..count {
        raw.push(read_code(&mut reader, index, count, &scope_ref)?);
    }

    let count_functions = reader.count(4)?;
    let mut functions = Vec::with_capacity(count_functions);
    for _ in 0..count_functions {
        let index = reader.u32()?;
        let locator = reader.locator()?;
        functions.push((index, locator));
    }
    if reader.remaining() != 0 {
        return Err("bytes after the end");
    }

    // Every code block but the first is nested in exactly one other, which comes before it.
    let mut nested_in = vec![false; raw.len()];
    for code in &raw {
        for constant in &code.constants {
            if let RawConstant::Function(index) = constant
                && std::mem::replace(&mut nested_in[*index], true)
            {
                return Err("code nested in two places");
            }
        }
    }
    if nested_in.iter().skip(1).any(|nested| !nested) {
        return Err("code nested nowhere");
    }

    let path: SourcePath = path.map(Path::to_path_buf).into();
    let source_len = units.len();
    let mut built: Vec<Option<Gc<CodeBlock>>> = vec![None; raw.len()];
    for (index, code) in raw.into_iter().enumerate().rev() {
        let code = build_code(code, &mut built, source_text, &path, source_len)?;
        verify_code(&code)?;
        built[index] = Some(Gc::new(code));
    }
    let codeblock = built[0].take().ok_or("no code")?;

    for (index, _) in &functions {
        match codeblock.constants.get(*index as usize) {
            Some(Constant::Function(_)) => {}
            _ => return Err("a declared function that is not a function constant"),
        }
    }

    // What the code finds in the global scope must be there, where it was.
    let mut pending = vec![codeblock.clone()];
    while let Some(code) = pending.pop() {
        check_global_bindings(&code, &global)?;
        for constant in &code.constants {
            if let Constant::Function(function) = constant {
                pending.push(function.clone());
            }
        }
    }
    for (_, locator) in &functions {
        check_global_binding(locator, &global)?;
    }

    Ok((
        code,
        CompiledModule {
            scope: module_scope,
            codeblock,
            functions,
            eager: true,
        },
    ))
}

fn read_code(
    reader: &mut Reader<'_>,
    index: usize,
    count: usize,
    scope_ref: &dyn Fn(u32) -> Read<Scope>,
) -> Read<RawCode> {
    let flags = u16::try_from(reader.u32()?).map_err(|_| "unknown code flags")?;
    let flags = CodeBlockFlags::from_bits(flags).ok_or("unknown code flags")?;
    let length = reader.u32()?;
    let parameter_length = reader.u32()?;
    let register_count = reader.u32()?;
    let this_mode = match reader.u8()? {
        0 => ThisMode::Lexical,
        1 => ThisMode::Strict,
        2 => ThisMode::Global,
        _ => return Err("unknown this mode"),
    };
    let n = reader.count(1)?;
    let mut mapped = ThinVec::with_capacity(n);
    for _ in 0..n {
        mapped.push(reader.u32()?.checked_sub(1));
    }
    let bytecode = reader.bytes()?.to_vec().into_boxed_slice();
    let n = reader.count(2)?;
    let mut constants = Vec::with_capacity(n);
    for _ in 0..n {
        constants.push(match reader.u8()? {
            0 => RawConstant::String(reader.string()?),
            1 => {
                let function = reader.u32()? as usize;
                if function <= index || function >= count {
                    return Err("a function constant that is not nested after its code");
                }
                RawConstant::Function(function)
            }
            2 => {
                let digits = std::str::from_utf8(reader.bytes()?).map_err(|_| "bad bigint")?;
                RawConstant::BigInt(JsBigInt::from_string_radix(digits, 16).ok_or("bad bigint")?)
            }
            3 => RawConstant::Scope(scope_ref(reader.u32()?)?),
            _ => return Err("unknown constant"),
        });
    }
    let n = reader.count(4)?;
    let mut bindings = Vec::with_capacity(n);
    for _ in 0..n {
        bindings.push(reader.locator()?);
    }
    let n = reader.count(3)?;
    let mut handlers = ThinVec::with_capacity(n);
    for _ in 0..n {
        let start = reader.u32()?;
        let end = reader.u32()?;
        let environment_count = reader.u32()?;
        handlers.push(Handler {
            start: Address::new(start),
            end: Address::new(end),
            environment_count,
        });
    }
    let n = reader.count(2)?;
    let mut ic = Vec::with_capacity(n);
    for _ in 0..n {
        ic.push(InlineCache::new(reader.string()?));
    }
    let n = reader.count(2)?;
    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        let pc = reader.u32()?;
        let position = if reader.bool()? {
            Some(reader.position()?)
        } else {
            None
        };
        entries.push(Entry { pc, position });
    }
    let name = reader.string()?;
    let span = if reader.bool()? {
        let start = usize::try_from(reader.u64()?).map_err(|_| "span out of range")?;
        let end = usize::try_from(reader.u64()?).map_err(|_| "span out of range")?;
        if start > end {
            return Err("span ends before it starts");
        }
        Some(LinearSpan::new(
            LinearPosition::new(start),
            LinearPosition::new(end),
        ))
    } else {
        None
    };
    let n = reader.count(1)?;
    let mut global_lexs = Vec::with_capacity(n);
    for _ in 0..n {
        global_lexs.push(reader.u32()?);
    }
    let n = reader.count(2)?;
    let mut global_fns = Vec::with_capacity(n);
    for _ in 0..n {
        let name_index = reader.u32()?;
        let function_index = reader.u32()?;
        global_fns.push(GlobalFunctionBinding {
            name_index,
            function_index,
        });
    }
    let n = reader.count(1)?;
    let mut global_vars = Vec::with_capacity(n);
    for _ in 0..n {
        global_vars.push(reader.u32()?);
    }
    Ok(RawCode {
        flags,
        length,
        parameter_length,
        register_count,
        this_mode,
        mapped_arguments_binding_indices: mapped,
        bytecode,
        constants,
        bindings: bindings.into_boxed_slice(),
        handlers,
        ic: ic.into_boxed_slice(),
        entries: entries.into_boxed_slice(),
        name,
        span,
        global_lexs: global_lexs.into_boxed_slice(),
        global_fns: global_fns.into_boxed_slice(),
        global_vars: global_vars.into_boxed_slice(),
    })
}

fn build_code(
    raw: RawCode,
    built: &mut [Option<Gc<CodeBlock>>],
    source_text: &SourceText,
    path: &SourcePath,
    source_len: usize,
) -> Read<CodeBlock> {
    let mut constants = ThinVec::with_capacity(raw.constants.len());
    for constant in raw.constants {
        constants.push(match constant {
            RawConstant::String(string) => Constant::String(string),
            RawConstant::Function(index) => Constant::Function(
                built
                    .get_mut(index)
                    .and_then(Option::take)
                    .ok_or("a function constant read twice")?,
            ),
            RawConstant::BigInt(bigint) => Constant::BigInt(bigint),
            RawConstant::Scope(scope) => Constant::Scope(scope),
        });
    }
    if let Some(span) = raw.span
        && span.end().pos() > source_len
    {
        return Err("span beyond the source text");
    }
    Ok(CodeBlock {
        flags: std::cell::Cell::new(raw.flags),
        length: raw.length,
        parameter_length: raw.parameter_length,
        register_count: raw.register_count,
        this_mode: raw.this_mode,
        mapped_arguments_binding_indices: raw.mapped_arguments_binding_indices,
        bytecode: Bytecode {
            bytes: raw.bytecode,
        },
        constants,
        bindings: raw.bindings,
        handlers: raw.handlers,
        ic: raw.ic,
        source_info: SourceInfo::new(
            SourceMap::new(raw.entries, path.clone()),
            raw.name,
            SpannedSourceText::new(source_text.clone(), raw.span),
        ),
        global_lexs: raw.global_lexs,
        global_fns: raw.global_fns,
        global_vars: raw.global_vars,
        debug_id: CodeBlock::get_next_codeblock_id(),
        #[cfg(feature = "trace")]
        traced: std::cell::Cell::new(false),
        #[cfg(feature = "jit")]
        jit: crate::vm::jit::JitSlot::default(),
        lazy: None,
    })
}

/// Checks that a code block can run without reading outside what it has: every instruction
/// whole, its registers within the frame, its jumps and handlers at instructions, its tables of
/// global names and functions pointing at constants of the right kind, and its source map in
/// order.
fn verify_code(code: &CodeBlock) -> Read<()> {
    if code.register_count > MAX_REGISTERS {
        return Err("too many registers");
    }
    // The registers a frame of async code uses without naming them.
    let implicit = if code.is_async_generator() {
        5
    } else if code.is_async() {
        4
    } else {
        1
    };
    if code.register_count < implicit {
        return Err("fewer registers than the frame uses");
    }

    let bytes = &code.bytecode;
    let len = bytes.bytes.len();
    if len == 0 || len > u32::MAX as usize {
        return Err("no bytecode, or too much");
    }
    let mut starts = vec![false; len];
    let mut targets = Vec::new();
    let mut bad_register = false;
    let mut pc = 0;
    let mut last = None;
    while pc < len {
        starts[pc] = true;
        let (opcode, next) = bytes
            .check_instruction(pc, &mut |operand| match operand {
                Operand::Register(register) => {
                    bad_register |= register >= code.register_count;
                }
                Operand::Address(address) => targets.push(address),
            })
            .ok_or("an instruction cut short")?;
        if bad_register {
            return Err("a register outside the frame");
        }
        last = Some(opcode);
        pc = next;
    }
    // Running on past the last instruction would read past the end.
    if !matches!(last, Some(Opcode::Return)) {
        return Err("code that does not end with a return");
    }
    let is_start = |address: u32| starts.get(address as usize).copied().unwrap_or(false);
    if !targets.iter().all(|target| is_start(*target)) {
        return Err("a jump to the middle of an instruction or past the end");
    }
    for handler in &code.handlers {
        let (start, end) = (handler.start.as_u32(), handler.end.as_u32());
        if start > end || !is_start(end) || (start as usize) > len {
            return Err("an exception handler outside the code");
        }
    }

    let is_string = |index: u32| {
        matches!(
            code.constants.get(index as usize),
            Some(Constant::String(_))
        )
    };
    if !code.global_lexs.iter().all(|index| is_string(*index))
        || !code.global_vars.iter().all(|index| is_string(*index))
    {
        return Err("a global name that is not a string constant");
    }
    for binding in &code.global_fns {
        if !is_string(binding.name_index)
            || !matches!(
                code.constants.get(binding.function_index as usize),
                Some(Constant::Function(_))
            )
        {
            return Err("a global function that is not a function constant");
        }
    }

    let entries = code.source_info.map().entries();
    if entries.windows(2).any(|pair| pair[0].pc > pair[1].pc) {
        return Err("a source map out of order");
    }
    Ok(())
}

/// Checks every binding the code finds in the global scope against the global scope it runs
/// in.
fn check_global_bindings(code: &CodeBlock, global: &Scope) -> Read<()> {
    code.bindings
        .iter()
        .try_for_each(|locator| check_global_binding(locator, global))
}

fn check_global_binding(locator: &BindingLocator, global: &Scope) -> Read<()> {
    let (name, scope, index, _) = locator.parts();
    match scope {
        // On the global object: the global scope must not bind the name, or the code compiled
        // now would find it there.
        0 if global.has_binding(&name) => {
            return Err("a global the global scope now binds");
        }
        // In the global scope: it must be bound there still, at the same index.
        1 => match global.get_binding(&name) {
            Some(binding)
                if binding.binding_index() == index
                    && binding.scope()
                        == boa_ast::scope::BindingLocatorScope::GlobalDeclarative => {}
            _ => return Err("a global binding that is not where it was"),
        },
        _ => {}
    }
    Ok(())
}
