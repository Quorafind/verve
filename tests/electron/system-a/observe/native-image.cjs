// What Electron 39's nativeImage answers for a small PNG with a transparent, a half-transparent and
// an opaque pixel. Prints JSON.
'use strict';
const zlib = require('node:zlib');
const fs = require('node:fs');
const path = require('node:path');
const { app, nativeImage } = require('electron');

function crc32(buf) {
  let crc = ~0;
  for (const byte of buf) {
    let c = (crc ^ byte) & 0xff;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    crc = (crc >>> 8) ^ c;
  }
  return ~crc >>> 0;
}
function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, 'latin1');
  data.copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}
/** A PNG of the RGBA pixels, row by row. */
function png(width, height, rgba) {
  const stride = 1 + width * 4;
  const raw = Buffer.alloc(height * stride);
  for (let y = 0; y < height; y++) {
    Buffer.from(rgba.slice(y * width * 4, (y + 1) * width * 4)).copy(raw, y * stride + 1);
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 6;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', header),
    chunk('IDAT', zlib.deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}
const hex = (b) => Buffer.from(b).toString('hex');

app.whenReady().then(() => {
  const out = {};
  const scratch = process.env.VERVE_PROBE_SCRATCH;
  // 4 x 2: transparent, red at 50%, opaque green, opaque blue / opaque white, black, grey, blue at 25%.
  const pixels = [
    0, 0, 0, 0, 255, 0, 0, 128, 0, 255, 0, 255, 0, 0, 255, 255,
    255, 255, 255, 255, 0, 0, 0, 255, 100, 100, 100, 255, 0, 0, 255, 64,
  ];
  const bytes = png(4, 2, pixels);
  fs.writeFileSync(path.join(scratch, 'sample.png'), bytes);
  const image = nativeImage.createFromBuffer(bytes);
  out.size = image.getSize();
  out.empty = image.isEmpty();
  out.aspect = image.getAspectRatio();
  out.scaleFactors = image.getScaleFactors();
  out.bitmap = hex(image.toBitmap());
  out.bitmapGet = hex(image.getBitmap());
  out.dataURLPrefix = image.toDataURL().slice(0, 30);
  out.pngRoundTripBitmap = hex(nativeImage.createFromBuffer(image.toPNG()).toBitmap());
  out.jpegSize = nativeImage.createFromBuffer(image.toJPEG(90)).getSize();
  out.fromDataURL = hex(nativeImage.createFromDataURL(image.toDataURL()).toBitmap());
  const rebuilt = nativeImage.createFromBitmap(image.toBitmap(), { width: 4, height: 2 });
  out.fromBitmap = { size: rebuilt.getSize(), bitmap: hex(rebuilt.toBitmap()) };
  const scaled = nativeImage.createFromBitmap(image.toBitmap(), { width: 4, height: 2, scaleFactor: 2 });
  out.fromBitmapScaled = { size: scaled.getSize(), at2: scaled.getSize(2), factors: scaled.getScaleFactors() };
  const cropped = image.crop({ x: 1, y: 0, width: 2, height: 2 });
  out.crop = { size: cropped.getSize(), bitmap: hex(cropped.toBitmap()) };
  out.cropPartlyOutside = image.crop({ x: 3, y: 1, width: 5, height: 5 }).getSize();
  out.cropAllOutside = image.crop({ x: 10, y: 10, width: 5, height: 5 }).getSize();
  const resized = image.resize({ width: 8, height: 4, quality: 'best' });
  out.resize = { size: resized.getSize(), len: resized.toBitmap().length };
  out.resizeWidthOnly = image.resize({ width: 8 }).getSize();
  out.resizeNone = image.resize({}).getSize();
  const empty = nativeImage.createEmpty();
  out.emptyImage = {
    empty: empty.isEmpty(),
    size: empty.getSize(),
    png: hex(empty.toPNG()),
    jpeg: hex(empty.toJPEG(80)),
    bitmap: hex(empty.toBitmap()),
    dataURL: empty.toDataURL(),
    aspect: empty.getAspectRatio(),
    factors: empty.getScaleFactors(),
  };
  out.badBuffer = nativeImage.createFromBuffer(Buffer.from('not an image')).isEmpty();
  out.missingPath = nativeImage.createFromPath(path.join(scratch, 'nope.png')).isEmpty();
  out.badDataURL = nativeImage.createFromDataURL('data:image/png;base64,AAAA').isEmpty();
  const copy = nativeImage.createFromBuffer(bytes);
  const before = copy.isTemplateImage();
  copy.setTemplateImage(true);
  out.template = { before, after: copy.isTemplateImage(), mac: copy.isMacTemplateImage };
  fs.writeFileSync(path.join(scratch, 'a.png'), bytes);
  fs.writeFileSync(path.join(scratch, 'a@2x.png'), png(8, 4, new Array(8 * 4 * 4).fill(200)));
  fs.writeFileSync(path.join(scratch, 'solo@2x.png'), png(8, 4, new Array(8 * 4 * 4).fill(10)));
  const multi = nativeImage.createFromPath(path.join(scratch, 'a.png'));
  out.multi = {
    size: multi.getSize(),
    factors: multi.getScaleFactors(),
    at2: multi.getSize(2),
    bitmap1: hex(multi.toBitmap()).slice(0, 16),
    bitmap2: hex(multi.toBitmap({ scaleFactor: 2 })).slice(0, 16),
    len2: multi.toBitmap({ scaleFactor: 2 }).length,
  };
  const solo = nativeImage.createFromPath(path.join(scratch, 'solo@2x.png'));
  out.solo = { size: solo.getSize(), factors: solo.getScaleFactors(), len1: solo.toBitmap().length };
  const added = nativeImage.createEmpty();
  added.addRepresentation({ scaleFactor: 2, width: 8, height: 4, buffer: fs.readFileSync(path.join(scratch, 'a@2x.png')) });
  out.added = { empty: added.isEmpty(), size: added.getSize(), factors: added.getScaleFactors() };
  out.types = {
    ctor: nativeImage.createEmpty().constructor.name,
    keys: Object.keys(nativeImage).sort(),
    protoKeys: Object.getOwnPropertyNames(Object.getPrototypeOf(image)).sort(),
    str: String(image),
  };
  const calls = {
    createFromBuffer: () => nativeImage.createFromBuffer('x'),
    createFromBufferNoArg: () => nativeImage.createFromBuffer(),
    createFromPath: () => nativeImage.createFromPath(1),
    createFromDataURL: () => nativeImage.createFromDataURL(1),
    createFromBitmapShort: () => nativeImage.createFromBitmap(Buffer.alloc(4), { width: 2, height: 2 }),
    createFromBitmapNoOpts: () => nativeImage.createFromBitmap(Buffer.alloc(4)),
    resizeNoArg: () => image.resize(),
    cropNoArg: () => image.crop(),
    toJPEGNoArg: () => image.toJPEG(),
  };
  for (const [name, call] of Object.entries(calls)) {
    try {
      const r = call();
      out[`call_${name}`] = { returned: r && r.constructor && r.constructor.name, empty: r && r.isEmpty && r.isEmpty() };
    } catch (error) {
      out[`call_${name}`] = { name: error.name, message: error.message };
    }
  }
  console.log(JSON.stringify(out, null, 1));
  app.quit();
});
