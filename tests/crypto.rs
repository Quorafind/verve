//! Cryptography as plugins use it: Node's `crypto` (hashes, HMAC, key derivation, AES) and the
//! web's `crypto.subtle` (the AES-GCM with a PBKDF2 key that sync and encryption plugins use),
//! checked against the published test vectors.

use verve::Runtime;

/// Runs `body` as a CommonJS module; it sets `exports.result` to an array of strings, perhaps
/// once its promises settle.
fn run(body: &str) -> Vec<String> {
    let mut rt = Runtime::new();
    let dir = std::env::temp_dir().join(format!("verve-crypto-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exports = rt
        .load_module(&dir.join("main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    let json = rt.eval("JSON.stringify").unwrap();
    let text = rt
        .call(&json, &boa_engine::JsValue::undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&verve::print(&text)).unwrap();
    let Some(result) = parsed["result"].as_array() else {
        panic!("no result: {parsed} {:?}", rt.take_console());
    };
    result
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

#[test]
fn hashes_and_hmacs_match_the_published_vectors() {
    let result = run(r#"
        const crypto = require('node:crypto');
        const h = (alg, data, enc) => crypto.createHash(alg).update(data).digest(enc || 'hex');
        const partial = crypto.createHash('sha256').update('a').update(Buffer.from('b')).update('63', 'hex');
        const copy = partial.copy();
        exports.result = [
            h('md5', ''),
            h('md5', 'abc'),
            h('sha1', 'abc'),
            h('sha224', 'abc'),
            h('sha256', 'abc'),
            h('SHA-256', new Uint8Array([97, 98, 99])),
            h('RSA-SHA256', 'abc', 'base64'),
            h('sha384', 'abc'),
            h('sha512', 'abc').slice(0, 32),
            partial.digest('hex'),
            copy.digest().length,
            crypto.hash('sha1', 'abc'),
            crypto.createHmac('sha256', Buffer.alloc(20, 0x0b)).update('Hi There').digest('hex'),
            crypto.createHmac('sha256', 'Jefe').update('what do ya want for nothing?').digest('hex'),
            crypto.createHmac('sha512', 'Jefe').update('what do ya want for nothing?').digest('hex').slice(0, 32),
            (() => { try { partial.update('x'); return 'updated' } catch (e) { return e.code } })(),
            (() => { try { crypto.createHash('nope'); return 'made' } catch (e) { return e.code } })(),
            String(crypto.getHashes().includes('sha256')),
            typeof crypto.createHash('md5').on,
        ];
    "#);
    assert_eq!(
        result,
        [
            "d41d8cd98f00b204e9800998ecf8427e",
            "900150983cd24fb0d6963f7d28e17f72",
            "a9993e364706816aba3e25717850c26c9cd0d89d",
            "23097d223405d8228642a477bda255b32aadbce4bda0b3f7e36c9da7",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=",
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7",
            "ddaf35a193617abacc417349ae204131",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "32",
            "a9993e364706816aba3e25717850c26c9cd0d89d",
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            "164b7a7bfcf819e2e395fbe73b56e0a3",
            "ERR_CRYPTO_HASH_FINALIZED",
            "ERR_CRYPTO_INVALID_DIGEST",
            "true",
            "function",
        ]
    );
}

#[test]
fn key_derivation_and_randomness() {
    let result = run(r#"
        const crypto = require('crypto');
        const out = [];
        out.push(crypto.pbkdf2Sync('password', 'salt', 1, 20, 'sha1').toString('hex'));
        out.push(crypto.pbkdf2Sync('password', 'salt', 4096, 20, 'sha1').toString('hex'));
        out.push(crypto.pbkdf2Sync('password', 'salt', 1, 32, 'sha256').toString('hex'));
        out.push(crypto.scryptSync('', '', 64, { N: 16, r: 1, p: 1 }).toString('hex').slice(0, 32));
        out.push(Buffer.from(crypto.hkdfSync('sha256', Buffer.alloc(22, 0x0b),
            Buffer.from('000102030405060708090a0b0c', 'hex'), Buffer.from('f0f1f2f3f4f5f6f7f8f9', 'hex'), 42)).toString('hex'));
        out.push(String(crypto.randomBytes(16).length));
        out.push(String(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID())));
        let inRange = true;
        for (let i = 0; i < 200; i++) { const n = crypto.randomInt(3, 7); if (n < 3 || n >= 7) inRange = false; }
        out.push(String(inRange));
        out.push(String(crypto.timingSafeEqual(Buffer.from('ab'), Buffer.from('ab'))));
        try { crypto.timingSafeEqual(Buffer.from('a'), Buffer.from('ab')) } catch (e) { out.push(e.code) }
        const filled = crypto.randomFillSync(new Uint8Array(32));
        out.push(String(filled.some(b => b !== 0)));
        out.push(String(crypto.webcrypto === globalThis.crypto));
        const key = crypto.createSecretKey(Buffer.from('k3y'));
        out.push(key.type + ' ' + key.symmetricKeySize + ' ' + key.export().toString());
        crypto.pbkdf2('password', 'salt', 2, 20, 'sha1', (err, derived) => {
            out.push(String(err) + ' ' + derived.toString('hex'));
            crypto.randomBytes(8, (err, bytes) => {
                out.push(String(bytes.length));
                exports.result = out;
            });
        });
    "#);
    assert_eq!(
        result,
        [
            "0c60c80f961f0e71f3a9b524af6012062fe037a6",
            "4b007901b765489abead49d926f721d065a429c1",
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b",
            "77d6576238657b203b19ca42c18a0497",
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
            "16",
            "true",
            "true",
            "true",
            "ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH",
            "true",
            "true",
            "secret 3 k3y",
            "null ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957",
            "8",
        ]
    );
}

#[test]
fn node_ciphers_round_trip_and_match_nist() {
    let result = run(r#"
        const crypto = require('crypto');
        const out = [];
        // SP 800-38A F.2.5, CBC-AES256, one block, no padding.
        const key256 = Buffer.from('603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4', 'hex');
        const iv = Buffer.from('000102030405060708090a0b0c0d0e0f', 'hex');
        const c = crypto.createCipheriv('aes-256-cbc', key256, iv).setAutoPadding(false);
        out.push(c.update('6bc1bee22e409f96e93d7e117393172a', 'hex', 'hex') + c.final('hex'));
        // F.5.1, CTR-AES128.
        const ctr = crypto.createCipheriv('aes-128-ctr', Buffer.from('2b7e151628aed2a6abf7158809cf4f3c', 'hex'),
            Buffer.from('f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff', 'hex'));
        out.push(ctr.update(Buffer.from('6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51', 'hex')).toString('hex'));
        // CBC with padding, fed in pieces.
        const enc = crypto.createCipheriv('aes-256-cbc', key256, iv);
        const sealed = Buffer.concat([enc.update('hello, '), enc.update('encrypted world'), enc.final()]);
        const dec = crypto.createDecipheriv('aes-256-cbc', key256, iv);
        out.push(dec.update(sealed, undefined, 'utf8') + dec.final('utf8'));
        // GCM: AAD, a tag, and a tampered tag refused.
        const gkey = crypto.randomBytes(32), giv = crypto.randomBytes(12);
        const g = crypto.createCipheriv('aes-256-gcm', gkey, giv);
        g.setAAD(Buffer.from('header'));
        const gcmText = Buffer.concat([g.update('secret note', 'utf8'), g.final()]);
        const tag = g.getAuthTag();
        out.push(String(tag.length));
        const gd = crypto.createDecipheriv('aes-256-gcm', gkey, giv);
        gd.setAAD(Buffer.from('header')); gd.setAuthTag(tag);
        out.push(gd.update(gcmText, null, 'utf8') + gd.final('utf8'));
        const bad = crypto.createDecipheriv('aes-256-gcm', gkey, giv);
        bad.setAuthTag(Buffer.alloc(16));
        bad.update(gcmText);
        try { bad.final(); out.push('accepted') } catch (e) { out.push(e.message) }
        try { crypto.createDecipheriv('aes-256-cbc', key256, iv).final(); } catch (e) { out.push(e.message) }
        try { crypto.createCipheriv('aes-256-cbc', Buffer.alloc(5), iv) } catch (e) { out.push(e.code) }
        // As a stream: write, end, read.
        const s = crypto.createCipheriv('aes-128-ecb', Buffer.alloc(16), null);
        s.write(Buffer.alloc(16)); s.end();
        out.push(s.read().toString('hex'));
        exports.result = out;
    "#);
    assert_eq!(
        result,
        [
            "f58c4c04d6e5f1ba779eabfb5f7bfbd6",
            "874d6191b620e3261bef6864990db6ce9806f66b7970fdff8617187bb9fffdff",
            "hello, encrypted world",
            "16",
            "secret note",
            "Unsupported state or unable to authenticate data",
            "wrong final block length",
            "ERR_CRYPTO_INVALID_KEYLEN",
            // AES-128 of a zero block under a zero key, then its padding block.
            "66e94bd4ef8a2c3b884cfa59ca342b2e0143db63ee66b0cdff9f69917680151e",
        ]
    );
}

#[test]
fn subtle_encrypts_with_a_passphrase_as_sync_plugins_do() {
    let result = run(r#"
        const subtle = crypto.subtle;
        const enc = new TextEncoder();
        const hex = buf => Array.from(new Uint8Array(buf), b => b.toString(16).padStart(2, '0')).join('');
        const out = [];
        (async () => {
            out.push(hex(await subtle.digest('SHA-256', enc.encode('abc'))));
            out.push(hex(await subtle.digest({ name: 'SHA-1' }, enc.encode('abc'))));
            // Self-hosted LiveSync's shape: a passphrase digest as PBKDF2 material, AES-GCM.
            const digest = await subtle.digest('SHA-256', enc.encode('passphrase'));
            const material = await subtle.importKey('raw', digest, { name: 'PBKDF2' }, false, ['deriveKey']);
            const salt = enc.encode('saltsaltsaltsalt');
            const key = await subtle.deriveKey({ name: 'PBKDF2', salt, iterations: 1000, hash: 'SHA-256' },
                material, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
            out.push(key.algorithm.name + ' ' + key.algorithm.length + ' ' + key.type + ' ' + key.extractable);
            out.push(String(key instanceof CryptoKey));
            const iv = crypto.getRandomValues(new Uint8Array(12));
            const sealed = await subtle.encrypt({ name: 'AES-GCM', iv }, key, enc.encode('a secret note'));
            out.push(String(sealed.byteLength));
            out.push(new TextDecoder().decode(await subtle.decrypt({ name: 'AES-GCM', iv }, key, sealed)));
            const tampered = new Uint8Array(sealed); tampered[0] ^= 1;
            try { await subtle.decrypt({ name: 'AES-GCM', iv }, key, tampered); out.push('opened') }
            catch (e) { out.push(e.name) }
            // PBKDF2 bits against RFC 6070.
            const pw = await subtle.importKey('raw', enc.encode('password'), 'PBKDF2', false, ['deriveBits']);
            out.push(hex(await subtle.deriveBits({ name: 'PBKDF2', salt: enc.encode('salt'), iterations: 2, hash: 'SHA-1' }, pw, 160)));
            // HMAC: sign and verify, RFC 4231 case 2.
            const mac = await subtle.importKey('raw', enc.encode('Jefe'), { name: 'HMAC', hash: 'SHA-256' }, true, ['sign', 'verify']);
            const sig = await subtle.sign('HMAC', mac, enc.encode('what do ya want for nothing?'));
            out.push(hex(sig));
            out.push(String(await subtle.verify('HMAC', mac, sig, enc.encode('what do ya want for nothing?'))));
            out.push(String(await subtle.verify('HMAC', mac, sig, enc.encode('something else'))));
            // Generated keys, exported raw and as JWK, imported back.
            const aes = await subtle.generateKey({ name: 'AES-CBC', length: 128 }, true, ['encrypt', 'decrypt']);
            const jwk = await subtle.exportKey('jwk', aes);
            out.push(jwk.kty + ' ' + jwk.alg);
            const again = await subtle.importKey('jwk', jwk, 'AES-CBC', false, ['encrypt', 'decrypt']);
            const cbcIv = new Uint8Array(16);
            const cbc = await subtle.encrypt({ name: 'AES-CBC', iv: cbcIv }, aes, enc.encode('sixteen bytes!!!'));
            out.push(String(cbc.byteLength));
            out.push(new TextDecoder().decode(await subtle.decrypt({ name: 'AES-CBC', iv: cbcIv }, again, cbc)));
            try { await subtle.exportKey('raw', again); out.push('exported') } catch (e) { out.push(e.name) }
            try { await subtle.encrypt({ name: 'AES-GCM', iv }, mac, enc.encode('x')); out.push('encrypted') } catch (e) { out.push(e.name) }
            // AES-CTR against SP 800-38A F.5.1.
            const raw = new Uint8Array('2b7e151628aed2a6abf7158809cf4f3c'.match(/../g).map(h => parseInt(h, 16)));
            const ctrKey = await subtle.importKey('raw', raw, 'AES-CTR', false, ['encrypt']);
            const counter = new Uint8Array('f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff'.match(/../g).map(h => parseInt(h, 16)));
            const block = new Uint8Array('6bc1bee22e409f96e93d7e117393172a'.match(/../g).map(h => parseInt(h, 16)));
            out.push(hex(await subtle.encrypt({ name: 'AES-CTR', counter, length: 64 }, ctrKey, block)));
            // AES-KW wrapping an AES key.
            const kek = await subtle.generateKey({ name: 'AES-KW', length: 256 }, false, ['wrapKey', 'unwrapKey']);
            const wrapped = await subtle.wrapKey('raw', aes, kek, 'AES-KW');
            out.push(String(wrapped.byteLength));
            const unwrapped = await subtle.unwrapKey('raw', wrapped, kek, 'AES-KW', 'AES-CBC', true, ['decrypt']);
            out.push(hex(await subtle.exportKey('raw', unwrapped)) === hex(await subtle.exportKey('raw', aes)) ? 'same key' : 'different');
            try { await subtle.generateKey({ name: 'RSA-OAEP' }, true, ['encrypt']); } catch (e) { out.push(e.name) }
            exports.result = out;
        })().catch(e => { exports.result = ['failed: ' + e + ' ' + e.stack]; });
    "#);
    assert_eq!(
        result,
        [
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "a9993e364706816aba3e25717850c26c9cd0d89d",
            "AES-GCM 256 secret false",
            "true",
            "29",
            "a secret note",
            "OperationError",
            "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957",
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            "true",
            "false",
            "oct A128CBC",
            "32",
            "sixteen bytes!!!",
            "InvalidAccessError",
            "InvalidAccessError",
            "874d6191b620e3261bef6864990db6ce",
            "24",
            "same key",
            "NotSupportedError",
        ]
    );
}
