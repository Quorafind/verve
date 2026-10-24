//! `zlib`: deflate and inflate — zlib's format, raw, and gzip — by `flate2` on `miniz_oxide`,
//! written in Rust, and brotli by the `brotli` crate.
//!
//! Everything here is one [`Codec`]: a compressor or decompressor fed a piece at a time,
//! answering what it can at once. The synchronous functions (`gzipSync`) feed it everything and
//! finish it; the callback ones (`gzip(buf, cb)`) do the same and answer once this turn is over;
//! the streams (`createGzip()`) are Node's `Transform`s whose `transform` and `flush` feed it. The
//! web's `CompressionStream` is built on the same codec.
//!
//! miniz_oxide has no gzip of its own and no window sizes, so the gzip header and trailer are
//! written and read here, and `windowBits`, `memLevel` and `strategy` are accepted and do not
//! change the output — it is valid deflate either way.

use std::cell::RefCell;
use std::io::Write;

use boa_engine::{
    Context, Finalize, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue,
    Trace, js_string, native_function::NativeFunctionPointer,
};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use crate::native::{arg, closure, function, get, hidden, js_text, object, text_of, type_error};

/// What a codec does: which format, and which way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Deflate,
    Inflate,
    DeflateRaw,
    InflateRaw,
    Gzip,
    Gunzip,
    /// Inflates gzip or zlib, whichever the data turns out to be.
    Unzip,
    BrotliCompress,
    BrotliDecompress,
}

impl Kind {
    /// The web's `CompressionStream` format (`gzip`, `deflate`, `deflate-raw`) one way or the
    /// other.
    pub(crate) fn for_format(format: &str, compress: bool) -> Option<Kind> {
        Some(match (format, compress) {
            ("gzip", true) => Kind::Gzip,
            ("gzip", false) => Kind::Gunzip,
            ("deflate", true) => Kind::Deflate,
            ("deflate", false) => Kind::Inflate,
            ("deflate-raw", true) => Kind::DeflateRaw,
            ("deflate-raw", false) => Kind::InflateRaw,
            _ => return None,
        })
    }

    /// Node's mode number for it, as `zlib.constants` has them.
    fn mode(self) -> u32 {
        match self {
            Kind::Deflate => 1,
            Kind::Inflate => 2,
            Kind::Gzip => 3,
            Kind::Gunzip => 4,
            Kind::DeflateRaw => 5,
            Kind::InflateRaw => 6,
            Kind::Unzip => 7,
            Kind::BrotliDecompress => 8,
            Kind::BrotliCompress => 9,
        }
    }
}

/// What a codec is made with: Node's options, as far as they mean anything here.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Options {
    /// Deflate's level, 0 to 9, or -1 for the default (6).
    pub level: i32,
    /// Brotli's quality (0 to 11) and window (10 to 24).
    pub quality: u32,
    pub window: u32,
    /// How much a synchronous call may answer before it is an error; `usize::MAX` for no limit.
    pub max_output: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            level: -1,
            quality: 11,
            window: 22,
            max_output: usize::MAX,
        }
    }
}

/// A failure, as Node's zlib reports one: its message, and the `code` and `errno` it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZlibError {
    pub message: String,
    pub code: &'static str,
    pub errno: i32,
}

impl ZlibError {
    fn data(message: &str) -> ZlibError {
        ZlibError {
            message: message.to_owned(),
            code: "Z_DATA_ERROR",
            errno: -3,
        }
    }

    fn truncated() -> ZlibError {
        ZlibError {
            message: "unexpected end of file".to_owned(),
            code: "Z_BUF_ERROR",
            errno: -5,
        }
    }

    /// As a JavaScript `Error`, with `code` and `errno`.
    pub(crate) fn to_js(&self, context: &mut Context) -> JsValue {
        let error = JsNativeError::error()
            .with_message(self.message.clone())
            .into_opaque(context);
        let _ = error.set(js_string!("code"), js_text(self.code), false, context);
        let _ = error.set(js_string!("errno"), self.errno, false, context);
        error.into()
    }
}

/// How much room to make in the output for each step: the input's size or so, at least 16 KiB.
fn room(input: usize) -> usize {
    input.saturating_mul(2).clamp(16 * 1024, 4 * 1024 * 1024)
}

/// The gzip header written before a member: no name, no time, made "on Unix", as zlib does.
const GZIP_HEADER: [u8; 10] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];

/// Deflate, in any of its three wrappings.
pub(crate) struct Deflater {
    compress: Compress,
    /// Gzip: the CRC and length of what was compressed, for the trailer; whether the header
    /// has been written.
    gzip: Option<(crc32fast::Hasher, u32, bool)>,
    finished: bool,
}

impl Deflater {
    fn new(kind: Kind, level: i32) -> Deflater {
        let level = if (0..=9).contains(&level) {
            level as u32
        } else {
            6
        };
        Deflater {
            compress: Compress::new(Compression::new(level), kind == Kind::Deflate),
            gzip: (kind == Kind::Gzip).then(|| (crc32fast::Hasher::new(), 0, false)),
            finished: false,
        }
    }

    fn run(&mut self, input: &[u8], flush: FlushCompress) -> Result<Vec<u8>, ZlibError> {
        let mut out = Vec::new();
        if let Some((crc, size, header)) = &mut self.gzip {
            if !*header {
                out.extend_from_slice(&GZIP_HEADER);
                *header = true;
            }
            crc.update(input);
            *size = size.wrapping_add(input.len() as u32);
        }
        let mut at = 0;
        loop {
            out.reserve(room(input.len() - at));
            let before = self.compress.total_in();
            let status = self
                .compress
                .compress_vec(&input[at..], &mut out, flush)
                .map_err(|_| ZlibError::data("deflate failed"))?;
            at += (self.compress.total_in() - before) as usize;
            let full = out.len() == out.capacity();
            if status == Status::StreamEnd {
                break;
            }
            if at == input.len() && !full {
                break;
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<u8>, ZlibError> {
        if self.finished {
            return Ok(Vec::new());
        }
        let mut out = self.run(&[], FlushCompress::Finish)?;
        if let Some((crc, size, _)) = &self.gzip {
            out.extend_from_slice(&crc.clone().finalize().to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
        }
        self.finished = true;
        Ok(out)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Unzip, before it has seen which format it has.
    Detect,
    GzipHeader,
    Body,
    GzipTrailer,
    Done,
}

/// Inflate, in any of its three wrappings, gzip's header and trailer read here — and, as zlib's
/// `gunzip` does, one gzip member after another.
pub(crate) struct Inflater {
    gzip: bool,
    zlib_header: bool,
    stage: Stage,
    decompress: Decompress,
    /// Input not yet used: a header or trailer only partly come, or what followed the end.
    input: Vec<u8>,
    crc: crc32fast::Hasher,
    size: u32,
}

/// How long a gzip header at the start of `data` is, once all of it has come.
fn gzip_header_length(data: &[u8]) -> Result<Option<usize>, ZlibError> {
    if data.len() < 10 {
        if data.first().is_some_and(|b| *b != 0x1f) || data.get(1).is_some_and(|b| *b != 0x8b) {
            return Err(ZlibError::data("incorrect header check"));
        }
        return Ok(None);
    }
    if data[0] != 0x1f || data[1] != 0x8b {
        return Err(ZlibError::data("incorrect header check"));
    }
    if data[2] != 8 {
        return Err(ZlibError::data("unknown compression method"));
    }
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        let Some(extra) = data.get(at..at + 2) else {
            return Ok(None);
        };
        at += 2 + u16::from_le_bytes([extra[0], extra[1]]) as usize;
    }
    for bit in [8, 16] {
        if flags & bit != 0 {
            match data
                .get(at..)
                .and_then(|rest| rest.iter().position(|b| *b == 0))
            {
                Some(end) => at += end + 1,
                None => return Ok(None),
            }
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    Ok((at <= data.len()).then_some(at))
}

impl Inflater {
    fn new(kind: Kind) -> Inflater {
        let (gzip, zlib_header, stage) = match kind {
            Kind::Gunzip => (true, false, Stage::GzipHeader),
            Kind::Unzip => (false, true, Stage::Detect),
            Kind::Inflate => (false, true, Stage::Body),
            _ => (false, false, Stage::Body),
        };
        Inflater {
            gzip,
            zlib_header,
            stage,
            decompress: Decompress::new(zlib_header && !gzip),
            input: Vec::new(),
            crc: crc32fast::Hasher::new(),
            size: 0,
        }
    }

    fn push(&mut self, data: &[u8]) -> Result<Vec<u8>, ZlibError> {
        self.input.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            match self.stage {
                Stage::Detect => {
                    if self.input.len() < 2 {
                        break;
                    }
                    if self.input[0] == 0x1f && self.input[1] == 0x8b {
                        self.gzip = true;
                        self.stage = Stage::GzipHeader;
                    } else {
                        self.decompress = Decompress::new(true);
                        self.stage = Stage::Body;
                    }
                }
                Stage::GzipHeader => match gzip_header_length(&self.input)? {
                    None => break,
                    Some(length) => {
                        self.input.drain(..length);
                        self.decompress = Decompress::new(false);
                        self.crc = crc32fast::Hasher::new();
                        self.size = 0;
                        self.stage = Stage::Body;
                    }
                },
                Stage::Body => {
                    if self.zlib_header && !self.gzip && self.decompress.total_in() == 0 {
                        // zlib's two header bytes, checked as zlib checks them.
                        if self.input.len() < 2 {
                            break;
                        }
                        let (cmf, flg) = (self.input[0], self.input[1]);
                        if cmf & 0x0f != 8 || (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 {
                            return Err(ZlibError::data("incorrect header check"));
                        }
                    }
                    let ended = self.inflate(&mut out)?;
                    if !ended {
                        break;
                    }
                    self.stage = if self.gzip {
                        Stage::GzipTrailer
                    } else {
                        Stage::Done
                    };
                }
                Stage::GzipTrailer => {
                    if self.input.len() < 8 {
                        break;
                    }
                    let crc = u32::from_le_bytes(self.input[..4].try_into().expect("four bytes"));
                    let size = u32::from_le_bytes(self.input[4..8].try_into().expect("four bytes"));
                    if crc != self.crc.clone().finalize() {
                        return Err(ZlibError::data("incorrect data check"));
                    }
                    if size != self.size {
                        return Err(ZlibError::data("incorrect length check"));
                    }
                    self.input.drain(..8);
                    // Another member, or the end: what follows that is not gzip is ignored.
                    match self.input.first() {
                        Some(0x1f) => self.stage = Stage::GzipHeader,
                        _ => {
                            self.input.clear();
                            self.stage = Stage::Done;
                        }
                    }
                    if self.stage == Stage::Done {
                        break;
                    }
                }
                Stage::Done => {
                    self.input.clear();
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Inflates what input there is into `out`; answers whether the deflate stream ended.
    fn inflate(&mut self, out: &mut Vec<u8>) -> Result<bool, ZlibError> {
        let mut at = 0;
        let mut ended = false;
        loop {
            out.reserve(room(self.input.len() - at));
            let (before_in, before_out) = (self.decompress.total_in(), out.len());
            let status = self
                .decompress
                .decompress_vec(&self.input[at..], out, FlushDecompress::None)
                .map_err(|error| ZlibError::data(&inflate_message(&error)))?;
            let used = (self.decompress.total_in() - before_in) as usize;
            at += used;
            if self.gzip {
                self.crc.update(&out[before_out..]);
                self.size = self.size.wrapping_add((out.len() - before_out) as u32);
            }
            let full = out.len() == out.capacity();
            if status == Status::StreamEnd {
                ended = true;
                break;
            }
            if (at == self.input.len() && !full) || (used == 0 && out.len() == before_out) {
                break;
            }
        }
        self.input.drain(..at);
        Ok(ended)
    }

    fn finish(&mut self) -> Result<Vec<u8>, ZlibError> {
        let out = self.push(&[])?;
        if self.stage != Stage::Done {
            return Err(ZlibError::truncated());
        }
        Ok(out)
    }
}

fn inflate_message(error: &flate2::DecompressError) -> String {
    let text = error.to_string();
    if text.is_empty() || text.contains("deflate decompression error") {
        "invalid block type".to_owned()
    } else {
        text
    }
}

/// A compressor or decompressor, fed a piece at a time.
pub(crate) enum Codec {
    Deflate(Deflater),
    Inflate(Inflater),
    BrotliCompress(Option<Box<brotli::CompressorWriter<Vec<u8>>>>),
    BrotliDecompress(Option<Box<brotli::DecompressorWriter<Vec<u8>>>>),
}

fn brotli_failed() -> ZlibError {
    ZlibError {
        message: "Decompression failed".to_owned(),
        code: "ERR_BROTLI_DECOMPRESSION_FAILED",
        errno: -1,
    }
}

impl Codec {
    pub(crate) fn new(kind: Kind, options: Options) -> Codec {
        match kind {
            Kind::Deflate | Kind::DeflateRaw | Kind::Gzip => {
                Codec::Deflate(Deflater::new(kind, options.level))
            }
            Kind::Inflate | Kind::InflateRaw | Kind::Gunzip | Kind::Unzip => {
                Codec::Inflate(Inflater::new(kind))
            }
            Kind::BrotliCompress => {
                Codec::BrotliCompress(Some(Box::new(brotli::CompressorWriter::new(
                    Vec::new(),
                    16 * 1024,
                    options.quality.min(11),
                    options.window.clamp(10, 24),
                ))))
            }
            Kind::BrotliDecompress => Codec::BrotliDecompress(Some(Box::new(
                brotli::DecompressorWriter::new(Vec::new(), 16 * 1024),
            ))),
        }
    }

    /// Feeds it `data`; answers what came out of it.
    pub(crate) fn push(&mut self, data: &[u8]) -> Result<Vec<u8>, ZlibError> {
        match self {
            Codec::Deflate(deflater) => deflater.run(data, FlushCompress::None),
            Codec::Inflate(inflater) => inflater.push(data),
            Codec::BrotliCompress(writer) => {
                let writer = writer
                    .as_mut()
                    .ok_or_else(|| ZlibError::data("write after end"))?;
                writer
                    .write_all(data)
                    .map_err(|_| ZlibError::data("Compression failed"))?;
                Ok(std::mem::take(writer.get_mut()))
            }
            Codec::BrotliDecompress(writer) => {
                let writer = writer
                    .as_mut()
                    .ok_or_else(|| ZlibError::data("write after end"))?;
                writer.write_all(data).map_err(|_| brotli_failed())?;
                Ok(std::mem::take(writer.get_mut()))
            }
        }
    }

    /// Makes it give up everything it has so far, as `Z_SYNC_FLUSH` does.
    pub(crate) fn flush(&mut self) -> Result<Vec<u8>, ZlibError> {
        match self {
            Codec::Deflate(deflater) => deflater.run(&[], FlushCompress::Sync),
            Codec::Inflate(inflater) => inflater.push(&[]),
            Codec::BrotliCompress(writer) => {
                let writer = writer
                    .as_mut()
                    .ok_or_else(|| ZlibError::data("write after end"))?;
                writer
                    .flush()
                    .map_err(|_| ZlibError::data("Compression failed"))?;
                Ok(std::mem::take(writer.get_mut()))
            }
            Codec::BrotliDecompress(writer) => {
                let writer = writer
                    .as_mut()
                    .ok_or_else(|| ZlibError::data("write after end"))?;
                Ok(std::mem::take(writer.get_mut()))
            }
        }
    }

    /// Ends it: what was left in it, the trailer — or, decompressing, an error if the data
    /// stopped short.
    pub(crate) fn finish(&mut self) -> Result<Vec<u8>, ZlibError> {
        match self {
            Codec::Deflate(deflater) => deflater.finish(),
            Codec::Inflate(inflater) => inflater.finish(),
            Codec::BrotliCompress(writer) => {
                Ok(writer.take().map(|w| w.into_inner()).unwrap_or_default())
            }
            Codec::BrotliDecompress(writer) => {
                let Some(mut writer) = writer.take() else {
                    return Ok(Vec::new());
                };
                writer.close().map_err(|_| ZlibError::truncated())?;
                Ok(std::mem::take(writer.get_mut()))
            }
        }
    }
}

/// Runs `data` through a codec of `kind` all at once.
pub(crate) fn run_all(kind: Kind, options: Options, data: &[u8]) -> Result<Vec<u8>, ZlibError> {
    let mut codec = Codec::new(kind, options);
    let mut out = codec.push(data)?;
    out.extend(codec.finish()?);
    if out.len() > options.max_output {
        return Err(ZlibError {
            message: format!(
                "Cannot create a Buffer larger than {} bytes",
                options.max_output
            ),
            code: "ERR_BUFFER_TOO_LARGE",
            errno: -1,
        });
    }
    Ok(out)
}

// ─── From JavaScript ────────────────────────────────────────────────────────────────────────

/// Node's options object, read: `level`, brotli's `params`, `maxOutputLength`. The rest
/// (`windowBits`, `memLevel`, `strategy`, `chunkSize`, `finishFlush`) are accepted as they are.
pub(crate) fn options_of(value: &JsValue, context: &mut Context) -> JsResult<Options> {
    let mut options = Options::default();
    if !value.is_object() {
        return Ok(options);
    }
    let level = get(value, "level", context)?;
    if !level.is_undefined() {
        let level = level.to_number(context)?;
        if !(-1. ..=9.).contains(&level) {
            return Err(crate::native::range_error(format!(
                "The value of \"options.level\" is out of range. It must be >= -1 and <= 9. Received {level}"
            )));
        }
        options.level = level as i32;
    }
    let params = get(value, "params", context)?;
    if params.is_object() {
        let quality = get(&params, "1", context)?;
        if !quality.is_undefined() {
            options.quality = quality.to_number(context)? as u32;
        }
        let window = get(&params, "2", context)?;
        if !window.is_undefined() {
            options.window = window.to_number(context)? as u32;
        }
    }
    let max = get(value, "maxOutputLength", context)?;
    if !max.is_undefined() {
        options.max_output = max.to_number(context)?.max(1.) as usize;
    }
    Ok(options)
}

/// The bytes a zlib function is given: a buffer, a typed array, a `DataView`, an `ArrayBuffer`
/// or a string (as UTF-8).
fn input_bytes(value: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    if value.is_string() {
        return Ok(text_of(value, context)?.into_bytes());
    }
    crate::encoding::bytes_of(value, context)?.ok_or_else(|| {
        let error = JsNativeError::typ()
            .with_message(format!(
                "The \"buffer\" argument must be of type string or an instance of Buffer, TypedArray, DataView, or ArrayBuffer. Received {}",
                value.type_of()
            ))
            .into_opaque(context);
        let _ = error.set(js_string!("code"), js_text("ERR_INVALID_ARG_TYPE"), false, context);
        JsError::from_opaque(error.into())
    })
}

/// A codec as JavaScript holds it.
#[derive(Trace, Finalize, JsData)]
struct CodecData {
    #[unsafe_ignore_trace]
    codec: RefCell<Codec>,
}

fn with_codec(
    this: &JsValue,
    f: impl FnOnce(&mut Codec) -> Result<Vec<u8>, ZlibError>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| type_error("not a zlib codec"))?;
    let outcome = {
        let data = object
            .downcast_ref::<CodecData>()
            .ok_or_else(|| type_error("not a zlib codec"))?;
        f(&mut data.codec.borrow_mut())
    };
    match outcome {
        Ok(bytes) => crate::encoding::make_buffer(context, bytes),
        Err(error) => Err(JsError::from_opaque(error.to_js(context))),
    }
}

const CODEC_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("push", 1, |this, args, cx| {
        let data = input_bytes(&arg(args, 0), cx)?;
        with_codec(this, |codec| codec.push(&data), cx)
    }),
    ("flush", 0, |this, _, cx| with_codec(this, Codec::flush, cx)),
    ("finish", 0, |this, _, cx| {
        with_codec(this, Codec::finish, cx)
    }),
];

/// A codec of `kind` that JavaScript (or native code holding it) can feed: `push(chunk)`,
/// `flush()` and `finish()` each answer a `Buffer` of what came out, and throw Node's zlib
/// errors (with `code` and `errno`).
pub(crate) fn codec_object(
    context: &mut Context,
    kind: Kind,
    options: Options,
) -> JsResult<JsObject> {
    let codec = JsObject::from_proto_and_data(
        Some(context.intrinsics().constructors().object().prototype()),
        CodecData {
            codec: RefCell::new(Codec::new(kind, options)),
        },
    );
    crate::native::methods(context, &codec, CODEC_METHODS)?;
    Ok(codec)
}

/// `gzipSync(buffer, options)` and the rest.
fn sync(kind: Kind, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let data = input_bytes(&arg(args, 0), context)?;
    let options = options_of(&arg(args, 1), context)?;
    match run_all(kind, options, &data) {
        Ok(bytes) => crate::encoding::make_buffer(context, bytes),
        Err(error) => {
            if error.code == "ERR_BUFFER_TOO_LARGE" {
                let thrown =
                    crate::native::range_error(error.message.clone()).into_opaque(context)?;
                if let Some(object) = thrown.as_object() {
                    object.set(js_string!("code"), js_text(error.code), false, context)?;
                }
                return Err(JsError::from_opaque(thrown));
            }
            Err(JsError::from_opaque(error.to_js(context)))
        }
    }
}

/// `gzip(buffer, options, callback)` and the rest: done now, answered once this turn is over.
fn with_callback(kind: Kind, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let callback = args
        .iter()
        .rev()
        .find(|a| a.is_callable())
        .cloned()
        .ok_or_else(|| {
            let error = JsNativeError::typ()
                .with_message("The \"callback\" argument must be of type function")
                .into_opaque(context);
            let _ = error.set(
                js_string!("code"),
                js_text("ERR_INVALID_ARG_TYPE"),
                false,
                context,
            );
            JsError::from_opaque(error.into())
        })?;
    let given: Vec<JsValue> = args.iter().filter(|a| !a.is_callable()).cloned().collect();
    let answer = match sync(kind, &given, context) {
        Ok(buffer) => vec![JsValue::null(), buffer],
        Err(error) => vec![error.into_opaque(context)?],
    };
    crate::microtask(context, callback, answer);
    Ok(JsValue::undefined())
}

/// The `transform` of a zlib stream: the chunk through the codec, what came out pushed on.
fn stream_transform(
    this: &JsValue,
    args: &[JsValue],
    codec: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let callback = arg(args, 2);
    let outcome = input_bytes(&arg(args, 0), context).and_then(|data| {
        if let Some(stream) = this.as_object() {
            let written = get(this, "bytesWritten", context)?
                .to_number(context)
                .unwrap_or(0.);
            stream.set(
                js_string!("bytesWritten"),
                written + data.len() as f64,
                false,
                context,
            )?;
        }
        crate::native::invoke(
            &codec.clone().into(),
            "push",
            &[crate::encoding::make_buffer(context, data)?],
            context,
        )
    });
    answer_stream(&callback, outcome, context)
}

/// The `flush` of a zlib stream, at its end: what was left, and the trailer.
fn stream_flush(
    _: &JsValue,
    args: &[JsValue],
    codec: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let outcome = crate::native::invoke(&codec.clone().into(), "finish", &[], context);
    answer_stream(&arg(args, 0), outcome, context)
}

fn answer_stream(
    callback: &JsValue,
    outcome: JsResult<JsValue>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let args = match outcome {
        Ok(buffer) => {
            let empty = get(&buffer, "length", context)?.to_number(context)? == 0.;
            if empty {
                vec![]
            } else {
                vec![JsValue::null(), buffer]
            }
        }
        Err(error) => vec![error.into_opaque(context)?],
    };
    crate::call(context, callback, &JsValue::undefined(), &args)?;
    Ok(JsValue::undefined())
}

/// `createGzip(options)` and the rest: a `Transform` from Node's `stream` module, its
/// `transform` and `flush` feeding a codec.
fn create_stream(kind: Kind, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let options_value = arg(args, 0);
    let options = options_of(&options_value, context)?;
    let Some(stream) = crate::modules::builtin(context, "stream")? else {
        return Err(crate::native::coded_error(
            context,
            "zlib streams need Node's stream module, which this runtime does not have",
            "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
        ));
    };
    let transform_class = get(&stream, "Transform", context)?;
    let transform_class = transform_class
        .as_object()
        .filter(|c| c.is_constructor())
        .ok_or_else(|| type_error("stream.Transform is not a class"))?;
    let codec = codec_object(context, kind, options)?;
    let config = object(context);
    let transform = closure(context, "transform", 3, stream_transform, codec.clone());
    config.set(js_string!("transform"), transform, false, context)?;
    let flush = closure(context, "flush", 1, stream_flush, codec.clone());
    config.set(js_string!("flush"), flush, false, context)?;
    let stream = transform_class.construct(&[config.into()], None, context)?;
    stream.set(js_string!("bytesWritten"), 0, false, context)?;
    hidden(&stream, js_string!("_handle"), codec.clone(), context)?;
    hidden(&stream, js_string!("_mode"), kind.mode(), context)?;
    let flush_now = closure(
        context,
        "flush",
        2,
        |this, args, codec: &JsObject, cx| {
            // `stream.flush(kind?, callback?)`: what the codec holds, pushed on now.
            let out = crate::native::invoke(&codec.clone().into(), "flush", &[], cx)?;
            if get(&out, "length", cx)?.to_number(cx)? > 0. {
                crate::native::invoke(this, "push", &[out], cx)?;
            }
            if let Some(callback) = args.iter().find(|a| a.is_callable()) {
                crate::microtask(cx, callback.clone(), Vec::new());
            }
            Ok(JsValue::undefined())
        },
        codec,
    );
    hidden(&stream, js_string!("flush"), flush_now, context)?;
    let calls_back: [(&str, NativeFunctionPointer); 3] = [
        ("params", |_, args, cx| {
            if let Some(callback) = args.iter().find(|a| a.is_callable()) {
                crate::microtask(cx, callback.clone(), Vec::new());
            }
            Ok(JsValue::undefined())
        }),
        ("close", |this, args, cx| {
            if let Some(callback) = args.iter().find(|a| a.is_callable()) {
                crate::microtask(cx, callback.clone(), Vec::new());
            }
            let destroy = get(this, "destroy", cx)?;
            if destroy.is_callable() {
                crate::call(cx, &destroy, this, &[])?;
            }
            Ok(JsValue::undefined())
        }),
        ("reset", |_, _, _| Ok(JsValue::undefined())),
    ];
    for (name, f) in calls_back {
        let method = function(context, name, 1, f);
        hidden(&stream, JsString::from(name), method, context)?;
    }
    Ok(stream.into())
}

/// Each kind's functions: `xSync`, `x` with a callback, `createX`, and the class `X`.
const KINDS: &[(Kind, &str, &str, &str)] = &[
    (Kind::Deflate, "deflate", "createDeflate", "Deflate"),
    (Kind::Inflate, "inflate", "createInflate", "Inflate"),
    (
        Kind::DeflateRaw,
        "deflateRaw",
        "createDeflateRaw",
        "DeflateRaw",
    ),
    (
        Kind::InflateRaw,
        "inflateRaw",
        "createInflateRaw",
        "InflateRaw",
    ),
    (Kind::Gzip, "gzip", "createGzip", "Gzip"),
    (Kind::Gunzip, "gunzip", "createGunzip", "Gunzip"),
    (Kind::Unzip, "unzip", "createUnzip", "Unzip"),
    (
        Kind::BrotliCompress,
        "brotliCompress",
        "createBrotliCompress",
        "BrotliCompress",
    ),
    (
        Kind::BrotliDecompress,
        "brotliDecompress",
        "createBrotliDecompress",
        "BrotliDecompress",
    ),
];

#[derive(Clone, Copy)]
struct KindBox(Kind);

impl Finalize for KindBox {}
// SAFETY: a `Kind` holds no garbage-collected value.
unsafe impl boa_gc::Trace for KindBox {
    boa_gc::empty_trace!();
}

/// zlib's numbers, as `zlib.constants` has them.
const CONSTANTS: &[(&str, f64)] = &[
    ("Z_NO_FLUSH", 0.),
    ("Z_PARTIAL_FLUSH", 1.),
    ("Z_SYNC_FLUSH", 2.),
    ("Z_FULL_FLUSH", 3.),
    ("Z_FINISH", 4.),
    ("Z_BLOCK", 5.),
    ("Z_OK", 0.),
    ("Z_STREAM_END", 1.),
    ("Z_NEED_DICT", 2.),
    ("Z_ERRNO", -1.),
    ("Z_STREAM_ERROR", -2.),
    ("Z_DATA_ERROR", -3.),
    ("Z_MEM_ERROR", -4.),
    ("Z_BUF_ERROR", -5.),
    ("Z_VERSION_ERROR", -6.),
    ("Z_NO_COMPRESSION", 0.),
    ("Z_BEST_SPEED", 1.),
    ("Z_BEST_COMPRESSION", 9.),
    ("Z_DEFAULT_COMPRESSION", -1.),
    ("Z_FILTERED", 1.),
    ("Z_HUFFMAN_ONLY", 2.),
    ("Z_RLE", 3.),
    ("Z_FIXED", 4.),
    ("Z_DEFAULT_STRATEGY", 0.),
    ("ZLIB_VERNUM", 4865.),
    ("DEFLATE", 1.),
    ("INFLATE", 2.),
    ("GZIP", 3.),
    ("GUNZIP", 4.),
    ("DEFLATERAW", 5.),
    ("INFLATERAW", 6.),
    ("UNZIP", 7.),
    ("BROTLI_DECODE", 8.),
    ("BROTLI_ENCODE", 9.),
    ("Z_MIN_WINDOWBITS", 8.),
    ("Z_MAX_WINDOWBITS", 15.),
    ("Z_DEFAULT_WINDOWBITS", 15.),
    ("Z_MIN_CHUNK", 64.),
    ("Z_MAX_CHUNK", f64::INFINITY),
    ("Z_DEFAULT_CHUNK", 16384.),
    ("Z_MIN_MEMLEVEL", 1.),
    ("Z_MAX_MEMLEVEL", 9.),
    ("Z_DEFAULT_MEMLEVEL", 8.),
    ("Z_MIN_LEVEL", -1.),
    ("Z_MAX_LEVEL", 9.),
    ("Z_DEFAULT_LEVEL", -1.),
    ("BROTLI_OPERATION_PROCESS", 0.),
    ("BROTLI_OPERATION_FLUSH", 1.),
    ("BROTLI_OPERATION_FINISH", 2.),
    ("BROTLI_OPERATION_EMIT_METADATA", 3.),
    ("BROTLI_PARAM_MODE", 0.),
    ("BROTLI_MODE_GENERIC", 0.),
    ("BROTLI_MODE_TEXT", 1.),
    ("BROTLI_MODE_FONT", 2.),
    ("BROTLI_DEFAULT_MODE", 0.),
    ("BROTLI_PARAM_QUALITY", 1.),
    ("BROTLI_MIN_QUALITY", 0.),
    ("BROTLI_MAX_QUALITY", 11.),
    ("BROTLI_DEFAULT_QUALITY", 11.),
    ("BROTLI_PARAM_LGWIN", 2.),
    ("BROTLI_MIN_WINDOW_BITS", 10.),
    ("BROTLI_MAX_WINDOW_BITS", 24.),
    ("BROTLI_LARGE_MAX_WINDOW_BITS", 30.),
    ("BROTLI_DEFAULT_WINDOW", 22.),
    ("BROTLI_PARAM_LGBLOCK", 3.),
    ("BROTLI_MIN_INPUT_BLOCK_BITS", 16.),
    ("BROTLI_MAX_INPUT_BLOCK_BITS", 24.),
    ("BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING", 4.),
    ("BROTLI_PARAM_SIZE_HINT", 5.),
    ("BROTLI_PARAM_LARGE_WINDOW", 6.),
    ("BROTLI_PARAM_NPOSTFIX", 7.),
    ("BROTLI_PARAM_NDIRECT", 8.),
    ("BROTLI_DECODER_RESULT_ERROR", 0.),
    ("BROTLI_DECODER_RESULT_SUCCESS", 1.),
    ("BROTLI_DECODER_RESULT_NEEDS_MORE_INPUT", 2.),
    ("BROTLI_DECODER_RESULT_NEEDS_MORE_OUTPUT", 3.),
    ("BROTLI_DECODER_PARAM_DISABLE_RING_BUFFER_REALLOCATION", 0.),
    ("BROTLI_DECODER_PARAM_LARGE_WINDOW", 1.),
    ("BROTLI_DECODER_NO_ERROR", 0.),
    ("BROTLI_DECODER_SUCCESS", 1.),
    ("BROTLI_DECODER_NEEDS_MORE_INPUT", 2.),
    ("BROTLI_DECODER_NEEDS_MORE_OUTPUT", 3.),
    ("BROTLI_DECODER_ERROR_FORMAT_EXUBERANT_NIBBLE", -1.),
    ("BROTLI_DECODER_ERROR_FORMAT_RESERVED", -2.),
    ("BROTLI_DECODER_ERROR_FORMAT_EXUBERANT_META_NIBBLE", -3.),
    ("BROTLI_DECODER_ERROR_FORMAT_SIMPLE_HUFFMAN_ALPHABET", -4.),
    ("BROTLI_DECODER_ERROR_FORMAT_SIMPLE_HUFFMAN_SAME", -5.),
    ("BROTLI_DECODER_ERROR_FORMAT_CL_SPACE", -6.),
    ("BROTLI_DECODER_ERROR_FORMAT_HUFFMAN_SPACE", -7.),
    ("BROTLI_DECODER_ERROR_FORMAT_CONTEXT_MAP_REPEAT", -8.),
    ("BROTLI_DECODER_ERROR_FORMAT_BLOCK_LENGTH_1", -9.),
    ("BROTLI_DECODER_ERROR_FORMAT_BLOCK_LENGTH_2", -10.),
    ("BROTLI_DECODER_ERROR_FORMAT_TRANSFORM", -11.),
    ("BROTLI_DECODER_ERROR_FORMAT_DICTIONARY", -12.),
    ("BROTLI_DECODER_ERROR_FORMAT_WINDOW_BITS", -13.),
    ("BROTLI_DECODER_ERROR_FORMAT_PADDING_1", -14.),
    ("BROTLI_DECODER_ERROR_FORMAT_PADDING_2", -15.),
    ("BROTLI_DECODER_ERROR_FORMAT_DISTANCE", -16.),
    ("BROTLI_DECODER_ERROR_DICTIONARY_NOT_SET", -19.),
    ("BROTLI_DECODER_ERROR_INVALID_ARGUMENTS", -20.),
    ("BROTLI_DECODER_ERROR_ALLOC_CONTEXT_MODES", -21.),
    ("BROTLI_DECODER_ERROR_ALLOC_TREE_GROUPS", -22.),
    ("BROTLI_DECODER_ERROR_ALLOC_CONTEXT_MAP", -25.),
    ("BROTLI_DECODER_ERROR_ALLOC_RING_BUFFER_1", -26.),
    ("BROTLI_DECODER_ERROR_ALLOC_RING_BUFFER_2", -27.),
    ("BROTLI_DECODER_ERROR_ALLOC_BLOCK_TYPE_TREES", -30.),
    ("BROTLI_DECODER_ERROR_UNREACHABLE", -31.),
];

/// `require('zlib')`.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    for (kind, name, create, class) in KINDS {
        let kind = KindBox(*kind);
        let sync_fn = closure(
            context,
            &format!("{name}Sync"),
            2,
            |_, args, kind: &KindBox, cx| sync(kind.0, args, cx),
            kind,
        );
        module.set(
            JsString::from(format!("{name}Sync")),
            sync_fn,
            false,
            context,
        )?;
        let callback_fn = closure(
            context,
            name,
            3,
            |_, args, kind: &KindBox, cx| with_callback(kind.0, args, cx),
            kind,
        );
        module.set(JsString::from(*name), callback_fn, false, context)?;
        let create_fn = closure(
            context,
            create,
            1,
            |_, args, kind: &KindBox, cx| create_stream(kind.0, args, cx),
            kind,
        );
        module.set(JsString::from(*create), create_fn, false, context)?;
        // `new zlib.Gzip(options)`, as old code writes it, makes the same stream.
        let class_fn: JsObject = boa_engine::object::FunctionObjectBuilder::new(
            context.realm(),
            boa_engine::NativeFunction::from_copy_closure_with_captures(
                |_, args, kind: &KindBox, cx| create_stream(kind.0, args, cx),
                kind,
            ),
        )
        .name(JsString::from(*class))
        .length(1)
        .constructor(true)
        .build()
        .into();
        module.set(JsString::from(*class), class_fn, false, context)?;
    }
    let crc32 = function(context, "crc32", 2, |_, args, cx| {
        let data = input_bytes(&arg(args, 0), cx)?;
        let start = match arg(args, 1) {
            v if v.is_undefined() => 0,
            v => v.to_number(cx)? as u32,
        };
        let mut hasher = crc32fast::Hasher::new_with_initial(start);
        hasher.update(&data);
        Ok(hasher.finalize().into())
    });
    module.set(js_string!("crc32"), crc32, false, context)?;
    let constants = object(context);
    for (name, value) in CONSTANTS {
        constants.set(JsString::from(*name), *value, false, context)?;
        // The old top-level names: `zlib.Z_BEST_COMPRESSION`.
        if name.starts_with("Z_") {
            module.set(JsString::from(*name), *value, false, context)?;
        }
    }
    module.set(js_string!("constants"), constants, false, context)?;
    let codes = object(context);
    for (name, value) in CONSTANTS.iter().filter(|(n, _)| {
        matches!(
            *n,
            "Z_OK"
                | "Z_STREAM_END"
                | "Z_NEED_DICT"
                | "Z_ERRNO"
                | "Z_STREAM_ERROR"
                | "Z_DATA_ERROR"
                | "Z_MEM_ERROR"
                | "Z_BUF_ERROR"
                | "Z_VERSION_ERROR"
        )
    }) {
        codes.set(JsString::from(*name), *value, false, context)?;
        codes.set(
            JsString::from(value.to_string()),
            js_text(name),
            false,
            context,
        )?;
    }
    module.set(js_string!("codes"), codes, false, context)?;
    Ok(module.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(kind: Kind, data: &[u8]) -> Result<Vec<u8>, ZlibError> {
        run_all(kind, Options::default(), data)
    }

    #[test]
    fn every_format_round_trips_fed_a_byte_at_a_time() {
        let text: Vec<u8> = (0..20_000)
            .map(|i| b"the quick brown fox "[i % 20])
            .collect();
        for (pack, unpack) in [
            (Kind::Gzip, Kind::Gunzip),
            (Kind::Deflate, Kind::Inflate),
            (Kind::DeflateRaw, Kind::InflateRaw),
            (Kind::Gzip, Kind::Unzip),
            (Kind::Deflate, Kind::Unzip),
            (Kind::BrotliCompress, Kind::BrotliDecompress),
        ] {
            let packed = all(pack, &text).unwrap();
            assert!(packed.len() < text.len() / 10, "{pack:?} compresses");
            let mut codec = Codec::new(unpack, Options::default());
            let mut out = Vec::new();
            for byte in &packed {
                out.extend(codec.push(std::slice::from_ref(byte)).unwrap());
            }
            out.extend(codec.finish().unwrap());
            assert_eq!(out, text, "{pack:?} → {unpack:?}");
        }
    }

    #[test]
    fn gzip_members_follow_each_other_and_bad_data_says_why() {
        let mut two = all(Kind::Gzip, b"one ").unwrap();
        two.extend(all(Kind::Gzip, b"two").unwrap());
        assert_eq!(all(Kind::Gunzip, &two).unwrap(), b"one two");
        assert_eq!(
            all(Kind::Inflate, b"not zlib").unwrap_err().message,
            "incorrect header check"
        );
        assert_eq!(
            all(Kind::Gunzip, b"not gzip at all").unwrap_err().message,
            "incorrect header check"
        );
        let packed = all(Kind::Deflate, b"hello hello hello").unwrap();
        assert_eq!(
            all(Kind::Inflate, &packed[..packed.len() - 3]).unwrap_err(),
            ZlibError::truncated()
        );
        let mut corrupt = all(Kind::Gzip, b"hello").unwrap();
        let at = corrupt.len() - 6;
        corrupt[at] ^= 0xff;
        assert_eq!(
            all(Kind::Gunzip, &corrupt).unwrap_err().code,
            "Z_DATA_ERROR"
        );
        assert_eq!(all(Kind::Inflate, &[]).unwrap_err(), ZlibError::truncated());
    }

    #[test]
    fn a_codec_handle_is_fed_from_javascript() {
        let mut rt = crate::RuntimeX::new();
        let pack = codec_object(
            rt.context(),
            Kind::for_format("deflate-raw", true).unwrap(),
            Options::default(),
        )
        .unwrap();
        let unpack = codec_object(
            rt.context(),
            Kind::for_format("deflate-raw", false).unwrap(),
            Options::default(),
        )
        .unwrap();
        let global = rt.context().global_object();
        global
            .set(js_string!("pack"), pack, false, rt.context())
            .unwrap();
        global
            .set(js_string!("unpack"), unpack, false, rt.context())
            .unwrap();
        let out = rt
            .eval(
                "const packed = Buffer.concat([pack.push('handle '), pack.push(new Uint8Array([111, 107])), pack.finish()]);
                 let threw = '';
                 try { unpack.push(packed.subarray(0, 2)); unpack.finish(); } catch (e) { threw = e.code; }
                 threw",
            )
            .unwrap();
        assert_eq!(crate::print(&out), "Z_BUF_ERROR");
        let unpack = codec_object(rt.context(), Kind::InflateRaw, Options::default()).unwrap();
        global
            .set(js_string!("unpack"), unpack, false, rt.context())
            .unwrap();
        let out = rt
            .eval("Buffer.concat([unpack.push(packed), unpack.finish()]).toString()")
            .unwrap();
        assert_eq!(crate::print(&out), "handle ok");
    }
}

// ─── CompressionStream and DecompressionStream ──────────────────────────────────────────────

/// A web compression stream's codec, held by its transformer.
#[derive(Trace, Finalize, JsData)]
struct WebCodec {
    #[unsafe_ignore_trace]
    codec: RefCell<Codec>,
}

/// Feeds the codec `f`, and enqueues what comes out as a `Uint8Array` — the chunks the web's
/// streams answer, where Node's answer `Buffer`s.
fn web_codec_step(
    state: &JsObject,
    controller: &JsValue,
    f: impl FnOnce(&mut Codec) -> Result<Vec<u8>, ZlibError>,
    context: &mut Context,
) -> JsResult<()> {
    let outcome = {
        let data = state.downcast_ref::<WebCodec>().expect("a web codec");
        f(&mut data.codec.borrow_mut())
    };
    let bytes =
        outcome.map_err(|error| type_error(format!("{}: {}", error.code, error.message)))?;
    if !bytes.is_empty() {
        let chunk = boa_engine::object::builtins::JsUint8Array::from_iter(bytes, context)?;
        crate::streams::transform_enqueue(context, controller, chunk.into())?;
    }
    Ok(())
}

/// `new CompressionStream(format)` / `new DecompressionStream(format)`: a readable and a
/// writable side, with a codec of `format` — `gzip`, `deflate` or `deflate-raw` — between them.
/// Chunks written must be `BufferSource`s; what is read is `Uint8Array`s. A decompression stream
/// whose input stops short of its end errors when it is closed, as the standard says.
fn construct_web_codec(
    this: &JsValue,
    args: &[JsValue],
    compress: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = if compress {
        "CompressionStream"
    } else {
        "DecompressionStream"
    };
    let fallback = get(this, "prototype", context)?
        .as_object()
        .ok_or_else(|| {
            type_error(format!(
                "Class constructor {name} cannot be invoked without 'new'"
            ))
        })?;
    let format = text_of(&arg(args, 0), context)?;
    let kind = Kind::for_format(&format, compress).ok_or_else(|| {
        type_error(format!(
            "Failed to construct '{name}': Unsupported compression format: '{format}'"
        ))
    })?;
    let state = JsObject::from_proto_and_data(
        None,
        WebCodec {
            codec: RefCell::new(Codec::new(kind, Options::default())),
        },
    );
    let transform = crate::streams::native_transform(
        context,
        state,
        |state, chunk, controller, cx| {
            let bytes = crate::encoding::bytes_of(&chunk, cx)?.ok_or_else(|| {
                type_error("The provided value is not of type '(ArrayBuffer or ArrayBufferView)'")
            })?;
            web_codec_step(state, controller, |codec| codec.push(&bytes), cx)
        },
        |state, controller, cx| web_codec_step(state, controller, Codec::finish, cx),
    )?;
    Ok(crate::streams::pair_instance(
        context,
        this,
        name,
        fallback,
        transform,
        JsValue::undefined(),
    )?
    .into())
}

/// The web's two compression classes, as globals made the first time they are read.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    crate::native::lazy_global(context, "CompressionStream", |cx| {
        let (class, _) = crate::streams::pair_class(
            cx,
            "CompressionStream",
            1,
            |this, args, cx| construct_web_codec(this, args, true, cx),
            &[],
        )?;
        Ok(class.into())
    })?;
    crate::native::lazy_global(context, "DecompressionStream", |cx| {
        let (class, _) = crate::streams::pair_class(
            cx,
            "DecompressionStream",
            1,
            |this, args, cx| construct_web_codec(this, args, false, cx),
            &[],
        )?;
        Ok(class.into())
    })?;
    Ok(())
}
