//! A module's compiled code, written down and read back: Boa's half of a code cache.
//!
//! [`Module::code_cache`](crate::Module::code_cache) compiles a module with every function it
//! declares and writes down what linking it needs: its import and export entries, its scopes
//! and its code. [`Module::from_code_cache`](crate::Module::from_code_cache) reads that back
//! into a module that links and runs as the parsed one would, without parsing or compiling.
//! It checks every code block at once but makes only the module's own: a function's code block
//! is made from its bytes when the function is first called, as a lazily compiled function's is
//! compiled then.
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

use std::{path::Path, rc::Rc};

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
        CodeBlock, CodeBlockFlags, Constant, GlobalFunctionBinding, Handler, InlineCache, LazyCode,
        opcode::{Address, Bytecode, Opcode, Operand},
        source_info::{Entry, SourceInfo, SourceMap, SourcePath},
    },
};

const MAGIC: [u8; 4] = *b"BOAC";
/// The layout of what is written down; changed whenever it changes.
const FORMAT: u32 = 2;
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
    // The bindings code can find: not those of a script parsed but not run.
    let visible = parts.bindings.iter().filter(|binding| {
        global
            .get_binding(&binding.name)
            .is_some_and(|found| found.binding_index() == binding.index)
    });
    for binding in visible {
        write_digest_string(&mut hash, &binding.name);
        hash.write(&binding.index.to_le_bytes());
        hash.write(&[binding.flags]);
    }
    hash.finish()
}

/// A digest of the global scope of `context`'s realm, as it is now: the bindings code compiled
/// now finds by index. Code written down against one digest is only read back against the
/// same; an embedder can mix it into its own key for a cache entry, so realms whose global
/// scopes differ keep entries of their own.
#[must_use]
pub fn global_scope_fingerprint(context: &crate::Context) -> u64 {
    global_digest(context.realm().scope())
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

/// A digest of a source text, four code units at a time: it tells texts apart, it does not
/// authenticate them, and it has to keep up with texts of megabytes.
fn source_digest(units: &[u16]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut hash = units.len() as u64;
    let mut chunks = units.chunks_exact(4);
    for chunk in &mut chunks {
        let word = u64::from(chunk[0])
            | (u64::from(chunk[1]) << 16)
            | (u64::from(chunk[2]) << 32)
            | (u64::from(chunk[3]) << 48);
        hash = (hash.rotate_left(5) ^ word).wrapping_mul(K);
    }
    for unit in chunks.remainder() {
        hash = (hash.rotate_left(5) ^ u64::from(*unit)).wrapping_mul(K);
    }
    hash
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
        let code = &CodeBlock::read_back(code)?;
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
                    let function = CodeBlock::read_back(function)?;
                    let address = std::ptr::from_ref::<CodeBlock>(&*function) as usize;
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
        // Linking looks every import and re-export up among the modules it requests.
        let requested = |module_request: Sym, attributes: &[AstImportAttribute]| {
            let request = ModuleRequest::from_ast(
                module_request.to_js_string(interner),
                attributes,
                interner,
            );
            requested_modules.contains(&request)
        };
        if !import_entries
            .iter()
            .all(|entry| requested(entry.module_request(), entry.attributes()))
            || !indirect_export_entries
                .iter()
                .all(|entry| requested(entry.module_request(), entry.attributes()))
            || !star_export_entries
                .iter()
                .all(|request| requested_modules.contains(request))
        {
            return Err("an import or export of a module the module does not request");
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

/// A module's code as read back: the bytes of its code blocks, checked whole when they were read
/// and each made into a code block when its function is first called (see `LazyCode`).
pub(crate) struct CachedModule {
    bytes: Box<[u8]>,
    /// Each code block: where its record is in `bytes`, and what its stub is made with.
    codes: Box<[CodeMeta]>,
    scopes: Box<[Scope]>,
    global: Scope,
    source_text: SourceText,
    path: SourcePath,
}

impl std::fmt::Debug for CachedModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedModule")
            .field("bytes", &self.bytes.len())
            .field("codes", &self.codes.len())
            .finish_non_exhaustive()
    }
}

/// What a code block's stub is made with, and where its record is.
struct CodeMeta {
    start: usize,
    end: usize,
    flags: CodeBlockFlags,
    length: u32,
    parameter_length: u32,
    this_mode: ThisMode,
    name: JsString,
    span: Option<LinearSpan>,
}

impl CachedModule {
    fn scope(&self, id: u32) -> Read<Scope> {
        match id as usize {
            0 => Ok(self.global.clone()),
            n => self.scopes.get(n - 1).cloned().ok_or("no such scope"),
        }
    }

    /// The stub of the code block `index`: what is read of a function before it runs, with its
    /// record to read when it is first called.
    fn stub(self: &Rc<Self>, index: usize) -> Read<CodeBlock> {
        let meta = self.codes.get(index).ok_or("no such code")?;
        let mut code = CodeBlock::new(meta.name.clone(), meta.length, false);
        code.flags.set(meta.flags);
        code.this_mode = meta.this_mode.clone();
        code.parameter_length = meta.parameter_length;
        code.source_info = SourceInfo::new(
            SourceMap::new(Box::default(), self.path.clone()),
            meta.name.clone(),
            SpannedSourceText::new(self.source_text.clone(), meta.span),
        );
        code.lazy = Some(Box::new(LazyCode::cached(self.clone(), index)));
        Ok(code)
    }

    /// Makes the code block `index` from its record, with stubs for the code blocks it nests.
    /// Its record was checked when the module was read, so this only fails if that check let
    /// something through.
    pub(crate) fn code(self: &Rc<Self>, index: usize) -> Read<CodeBlock> {
        let meta = self.codes.get(index).ok_or("no such code")?;
        let bytes = self.bytes.get(meta.start..meta.end).ok_or("no such code")?;
        let mut reader = Reader { bytes, pos: 0 };
        let raw = read_code(&mut reader, index, self.codes.len(), &|id| self.scope(id))?;
        let mut constants = ThinVec::with_capacity(raw.constants.len());
        for constant in raw.constants {
            constants.push(match constant {
                RawConstant::String(string) => Constant::String(string),
                RawConstant::Function(index) => Constant::Function(Gc::new(self.stub(index)?)),
                RawConstant::BigInt(bigint) => Constant::BigInt(bigint),
                RawConstant::Scope(scope) => Constant::Scope(scope),
            });
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
                SourceMap::new(raw.entries, self.path.clone()),
                raw.name,
                SpannedSourceText::new(self.source_text.clone(), raw.span),
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
}

/// Reads back a module [`write_module`] wrote down from `source_text`, into `realm`. Every code
/// block is checked now; only the module's own is made now, and the rest when their functions
/// are first called.
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
    let module_scope = match reader.u32()? as usize {
        0 => return Err("a module scope must be the module's own"),
        n => scopes.get(n - 1).cloned().ok_or("no such scope")?,
    };

    let count = reader.count(16)?;
    if count == 0 {
        return Err("no code");
    }
    let base = reader.pos;
    let mut codes = Vec::with_capacity(count);
    let mut nested_in = vec![false; count];
    let mut root_functions = Vec::new();
    for index in 0..count {
        let (meta, functions) = skim_code(
            &mut reader,
            index,
            count,
            scopes.len(),
            &global,
            units.len(),
        )?;
        // Every code block but the first is nested in exactly one other, which comes before it.
        for function in &functions {
            if std::mem::replace(&mut nested_in[*function], true) {
                return Err("code nested in two places");
            }
        }
        if index == 0 {
            root_functions = functions;
        }
        codes.push(CodeMeta {
            start: meta.start - base,
            end: meta.end - base,
            ..meta
        });
    }
    if nested_in.iter().skip(1).any(|nested| !nested) {
        return Err("code nested nowhere");
    }
    let code_bytes = bytes.get(base..reader.pos).ok_or("truncated")?;

    let count_functions = reader.count(4)?;
    let mut functions = Vec::with_capacity(count_functions);
    for _ in 0..count_functions {
        let index = reader.u32()?;
        let locator = reader.locator()?;
        check_global_binding(&locator, &global)?;
        functions.push((index, locator));
    }
    if reader.remaining() != 0 {
        return Err("bytes after the end");
    }

    let module = Rc::new(CachedModule {
        bytes: code_bytes.into(),
        codes: codes.into_boxed_slice(),
        scopes: scopes.into_boxed_slice(),
        global,
        source_text: source_text.clone(),
        path: path.map(Path::to_path_buf).into(),
    });
    let codeblock = Gc::new(module.code(0)?);
    for (index, _) in &functions {
        match codeblock.constants.get(*index as usize) {
            Some(Constant::Function(_)) => {}
            _ => return Err("a declared function that is not a function constant"),
        }
    }
    debug_assert!(
        root_functions
            .iter()
            .all(|index| *index < module.codes.len())
    );

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

/// A string as written down, not made into a `JsString` unless it is needed.
enum RawString<'a> {
    Latin1(&'a [u8]),
    Utf16(&'a [u8]),
}

impl RawString<'_> {
    fn to_js_string(&self) -> JsString {
        match self {
            Self::Latin1(bytes) => JsString::from(crate::string::JsStr::latin1(bytes)),
            Self::Utf16(bytes) => {
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                JsString::from(&units[..])
            }
        }
    }
}

impl<'a> Reader<'a> {
    fn raw_string(&mut self) -> Read<RawString<'a>> {
        match self.u8()? {
            0 => Ok(RawString::Latin1(self.bytes()?)),
            1 => {
                let len = self.count(2)?;
                Ok(RawString::Utf16(self.take(len * 2)?))
            }
            _ => Err("unknown string encoding"),
        }
    }
}

/// Checks the record of a code block, as reading it and running it need, without making
/// anything but its stub's name: what [`read_code`] would read, every instruction of its
/// bytecode (whole, its registers within the frame, its jumps at instructions, ending with a
/// return), its exception handlers, its constants and the tables that point at them, its source
/// map and span, and the bindings it finds in the global scope. Answers what its stub is made
/// with, and the code blocks it nests.
fn skim_code(
    reader: &mut Reader<'_>,
    index: usize,
    count: usize,
    scopes: usize,
    global: &Scope,
    source_len: usize,
) -> Read<(CodeMeta, Vec<usize>)> {
    let start = reader.pos;
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
    if register_count > MAX_REGISTERS {
        return Err("too many registers");
    }
    // The registers a frame of async code uses without naming them.
    let is_async = flags.contains(CodeBlockFlags::IS_ASYNC);
    let implicit = match (is_async, flags.contains(CodeBlockFlags::IS_GENERATOR)) {
        (true, true) => 5,
        (true, false) => 4,
        _ => 1,
    };
    if register_count < implicit {
        return Err("fewer registers than the frame uses");
    }
    for _ in 0..reader.count(1)? {
        reader.u32()?;
    }
    let (starts, indices) = verify_bytecode(reader.bytes()?, register_count)?;

    // What kind each constant is: 0 a string, 1 a function, 2 a bigint, 3 a scope.
    let n = reader.count(2)?;
    let mut kinds = Vec::with_capacity(n);
    let mut functions = Vec::new();
    for _ in 0..n {
        let kind = reader.u8()?;
        match kind {
            0 => {
                reader.raw_string()?;
            }
            1 => {
                let function = reader.u32()? as usize;
                if function <= index || function >= count {
                    return Err("a function constant that is not nested after its code");
                }
                functions.push(function);
            }
            2 => {
                let digits = std::str::from_utf8(reader.bytes()?).map_err(|_| "bad bigint")?;
                JsBigInt::from_string_radix(digits, 16).ok_or("bad bigint")?;
            }
            3 => {
                if reader.u32()? as usize > scopes {
                    return Err("no such scope");
                }
            }
            _ => return Err("unknown constant"),
        }
        kinds.push(kind);
    }
    let bindings = reader.count(4)?;
    for _ in 0..bindings {
        let name = reader.raw_string()?;
        let scope = reader.u32()?;
        let binding_index = reader.u32()?;
        let unique = reader.u32()?;
        // What the code finds in the global scope must be there, where it was.
        if scope <= 1 {
            let locator =
                BindingLocator::from_parts(name.to_js_string(), scope, binding_index, unique);
            check_global_binding(&locator, global)?;
        }
    }
    let len = starts.len();
    let is_start = |address: u32| starts.get(address as usize).copied().unwrap_or(false);
    for _ in 0..reader.count(3)? {
        let start = reader.u32()?;
        let end = reader.u32()?;
        reader.u32()?;
        if start > end || !is_start(end) || (start as usize) > len {
            return Err("an exception handler outside the code");
        }
    }
    let ics = reader.count(2)?;
    for _ in 0..ics {
        reader.raw_string()?;
    }
    check_indices(&indices, &kinds, bindings, ics)?;
    let mut last_pc = 0;
    for _ in 0..reader.count(2)? {
        let pc = reader.u32()?;
        if pc < last_pc {
            return Err("a source map out of order");
        }
        last_pc = pc;
        if reader.bool()? {
            reader.position()?;
        }
    }
    let name = reader.string()?;
    let span = if reader.bool()? {
        let start = usize::try_from(reader.u64()?).map_err(|_| "span out of range")?;
        let end = usize::try_from(reader.u64()?).map_err(|_| "span out of range")?;
        if start > end || end > source_len {
            return Err("a span outside the source text");
        }
        Some(LinearSpan::new(
            LinearPosition::new(start),
            LinearPosition::new(end),
        ))
    } else {
        None
    };
    let kind = |index: u32| kinds.get(index as usize).copied();
    for _ in 0..reader.count(1)? {
        if kind(reader.u32()?) != Some(0) {
            return Err("a global name that is not a string constant");
        }
    }
    for _ in 0..reader.count(2)? {
        let name = reader.u32()?;
        let function = reader.u32()?;
        if kind(name) != Some(0) || kind(function) != Some(1) {
            return Err("a global function that is not a function constant");
        }
    }
    for _ in 0..reader.count(1)? {
        if kind(reader.u32()?) != Some(0) {
            return Err("a global name that is not a string constant");
        }
    }
    Ok((
        CodeMeta {
            start,
            end: reader.pos,
            flags,
            length,
            parameter_length,
            this_mode,
            name,
            span,
        },
        functions,
    ))
}

/// Checks each index the instructions hold against the table their instruction reads it from:
/// `kinds` the kinds of the constants (0 a string, 1 a function, 2 a bigint, 3 a scope), and how
/// many bindings and inline caches there are.
fn check_indices(indices: &[(Table, u32)], kinds: &[u8], bindings: usize, ics: usize) -> Read<()> {
    for (table, value) in indices {
        let value = *value as usize;
        let fits = match table {
            Table::Binding => value < bindings,
            Table::InlineCache => value < ics,
            Table::String => kinds.get(value) == Some(&0),
            Table::Function => kinds.get(value) == Some(&1),
            Table::StringOrBigInt => matches!(kinds.get(value), Some(0 | 2)),
            Table::Scope => kinds.get(value) == Some(&3),
        };
        if !fits {
            return Err("an instruction's index outside its table, or of the wrong kind");
        }
    }
    Ok(())
}

/// What an index operand of an instruction indexes: a table of its code block, or a kind of
/// constant.
#[derive(Debug, Clone, Copy)]
enum Table {
    Binding,
    InlineCache,
    String,
    StringOrBigInt,
    Function,
    Scope,
}

/// What an index operand (or plain `u32`) of an instruction is, as the handlers in `vm/opcode`
/// read it.
enum Use {
    /// An index into one of the code block's tables.
    Index(Table),
    /// A register, held as a plain number.
    Register,
    /// A count or a flag: nothing it could reach outside.
    Plain,
}

/// What the operand `field` of `opcode` is; `None` for one this verifier does not know, which
/// refuses the code rather than run what it cannot check.
fn operand_use(opcode: Opcode, field: &str) -> Option<Use> {
    Some(match (opcode, field) {
        (_, "binding_index") | (Opcode::ThisForObjectEnvironmentName, "index") => {
            Use::Index(Table::Binding)
        }
        (_, "ic_index") => Use::Index(Table::InlineCache),
        (_, "scope_index") => Use::Index(Table::Scope),
        (_, "name_index" | "pattern_index" | "flags_index" | "message" | "name_indices")
        | (Opcode::InPrivate | Opcode::ThrowMutateImmutable, "index") => Use::Index(Table::String),
        (Opcode::StoreLiteral, "index") => Use::Index(Table::StringOrBigInt),
        (Opcode::GetFunction, "index") => Use::Index(Table::Function),
        (Opcode::JumpTable, "index") | (Opcode::TemplateCreate, "values") => Use::Register,
        (_, "argument_count" | "done" | "is_anonymous_function" | "phase" | "prefix")
        | (Opcode::GetArgument, "index") => Use::Plain,
        _ => return None,
    })
}

/// Checks that bytecode can run without reading outside what it has: every instruction whole
/// and not a reserved one, each register below `register_count`, each jump to the start of an
/// instruction, and the last instruction a return, so it does not run past the end. Answers
/// where instructions start, and the indices into the code block's tables its instructions
/// hold, to check once the tables are read.
/// Where instructions start, and the table indices they hold.
type Verified = (Vec<bool>, Vec<(Table, u32)>);

fn verify_bytecode(bytes: &[u8], register_count: u32) -> Read<Verified> {
    let len = bytes.len();
    if len == 0 || len > u32::MAX as usize {
        return Err("no bytecode, or too much");
    }
    let mut starts = vec![false; len];
    let mut targets = Vec::new();
    let mut indices = Vec::new();
    let mut operands = Vec::new();
    let mut pc = 0;
    let mut last = None;
    while pc < len {
        starts[pc] = true;
        operands.clear();
        let (opcode, next) =
            Bytecode::check_instruction_in(bytes, pc, &mut |operand| operands.push(operand))
                .ok_or("an instruction cut short")?;
        if opcode.as_str() == "Reserved" {
            return Err("a reserved opcode");
        }
        for operand in &operands {
            match *operand {
                Operand::Register(register) => {
                    if register >= register_count {
                        return Err("a register outside the frame");
                    }
                }
                Operand::Address(address) => targets.push(address),
                Operand::Index { field, value } => match operand_use(opcode, field) {
                    Some(Use::Index(table)) => indices.push((table, value)),
                    Some(Use::Register) if value >= register_count => {
                        return Err("a register outside the frame");
                    }
                    Some(Use::Plain | Use::Register) => {}
                    None => return Err("an operand the verifier does not know"),
                },
            }
        }
        last = Some(opcode);
        pc = next;
    }
    if !matches!(last, Some(Opcode::Return)) {
        return Err("code that does not end with a return");
    }
    if !targets
        .iter()
        .all(|target| starts.get(*target as usize).copied().unwrap_or(false))
    {
        return Err("a jump to the middle of an instruction or past the end");
    }
    Ok((starts, indices))
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

#[cfg(test)]
mod tests {
    use super::{Table, check_indices, verify_bytecode};
    use crate::vm::opcode::{BytecodeEmitter, IndexOperand, RegisterOperand};
    use thin_vec::thin_vec;

    fn bytes(emit: impl FnOnce(&mut BytecodeEmitter)) -> Box<[u8]> {
        let mut emitter = BytecodeEmitter::new();
        emit(&mut emitter);
        emitter.emit_return();
        emitter.into_bytecode().bytes
    }

    #[test]
    fn index_operands_are_checked_against_their_tables() {
        let code = bytes(|e| {
            e.emit_get_name(RegisterOperand::new(0), IndexOperand::new(1));
            e.emit_store_literal(RegisterOperand::new(0), IndexOperand::new(0));
            e.emit_get_function(RegisterOperand::new(0), IndexOperand::new(1));
        });
        let (_, indices) = verify_bytecode(&code, 1).unwrap();
        assert!(matches!(
            indices.as_slice(),
            [
                (Table::Binding, 1),
                (Table::StringOrBigInt, 0),
                (Table::Function, 1)
            ]
        ));
        // A string, then a function; two bindings.
        assert!(check_indices(&indices, &[0, 1], 2, 0).is_ok());
        assert!(
            check_indices(&indices, &[0, 1], 1, 0).is_err(),
            "binding 1 of 1"
        );
        assert!(
            check_indices(&indices, &[1, 1], 2, 0).is_err(),
            "a literal that is a function"
        );
        assert!(
            check_indices(&indices, &[0, 0], 2, 0).is_err(),
            "a function that is a string"
        );
    }

    #[test]
    fn registers_held_as_numbers_are_checked() {
        let jump = bytes(|e| e.emit_jump_table(5, thin_vec![]));
        assert!(verify_bytecode(&jump, 6).is_ok());
        assert!(verify_bytecode(&jump, 5).is_err());
        let template =
            bytes(|e| e.emit_template_create(0, RegisterOperand::new(0), thin_vec![0, 9]));
        assert!(verify_bytecode(&template, 10).is_ok());
        assert!(verify_bytecode(&template, 9).is_err());
    }

    #[test]
    fn reserved_opcodes_are_refused() {
        let reserved = (0..=u8::MAX)
            .find(|op| crate::vm::opcode::Opcode::decode(*op).as_str() == "Reserved")
            .expect("the opcode table has reserved slots");
        let code = [reserved, crate::vm::opcode::Opcode::Return as u8];
        assert_eq!(verify_bytecode(&code, 1).unwrap_err(), "a reserved opcode");
    }
}
