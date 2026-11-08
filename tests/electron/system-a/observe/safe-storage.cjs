// Encrypts a few texts with Electron 39's safeStorage in the scratch profile `run.cjs` gives it,
// and writes `samples.json` into the scratch folder, the plain text and the ciphertext (hex) of each;
// Electron writes `Local State` into the profile (`<scratch>/profile`) as it exits. Both are what
// Verve's DPAPI test reads. The key in `Local State` is
// wrapped by DPAPI for the Windows user who ran this, so only that user can read the samples;
// the key is a throwaway made for the profile.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { app, safeStorage } = require('electron');

const attempt = (call) => {
  try {
    return { value: call() };
  } catch (error) {
    return { name: error.name, message: error.message };
  }
};

app.whenReady().then(() => {
  const scratch = process.env.VERVE_PROBE_SCRATCH;
  const texts = ['hello', 'héllo wörld ✓', '', 'x'.repeat(1000)];
  const samples = texts.map((text) => ({ text, cipher: safeStorage.encryptString(text).toString('hex') }));
  const out = {
    available: safeStorage.isEncryptionAvailable(),
    roundTrip: samples.every((s) => safeStorage.decryptString(Buffer.from(s.cipher, 'hex')) === s.text),
    prefix: samples[0].cipher.slice(0, 6),
    lengths: samples.map((s) => s.cipher.length / 2),
    twice: safeStorage.encryptString('hello').toString('hex') !== samples[0].cipher,
    backend: attempt(() => safeStorage.getSelectedStorageBackend()),
    plain: attempt(() => safeStorage.setUsePlainTextEncryption(true)),
    decryptGarbage: attempt(() => safeStorage.decryptString(Buffer.from('not encrypted at all, not at all'))),
    decryptBadTag: attempt(() => {
      const bytes = Buffer.from(samples[0].cipher, 'hex');
      bytes[bytes.length - 1] ^= 1;
      return safeStorage.decryptString(bytes);
    }),
    encryptBad: attempt(() => safeStorage.encryptString(1)),
    decryptBad: attempt(() => safeStorage.decryptString('x')),
    keys: Object.getOwnPropertyNames(Object.getPrototypeOf(safeStorage)).sort(),
  };
  fs.writeFileSync(path.join(scratch, 'samples.json'), JSON.stringify(samples, null, 1));
  console.log(JSON.stringify(out, null, 1));
  app.quit();
});
