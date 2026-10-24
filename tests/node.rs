//! Node's own modules as a desktop plugin `require`s them: `path`, `util`, `events`, `os`,
//! `url`, `fs` and `fs/promises`, and `child_process` refusing to start anything.

use verve::Runtime;

/// Runs `body` as a CommonJS module; it sets `exports.result` to an array of strings.
fn run(body: &str) -> Vec<String> {
    let mut rt = Runtime::new();
    let dir = std::env::temp_dir().join(format!("verve-node-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.js");
    let exports = rt
        .load_module(&main, body)
        .unwrap_or_else(|e| panic!("{e}"));
    let json = rt.eval("JSON.stringify").unwrap();
    let text = rt
        .call(&json, &rt_undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&verve::print(&text)).unwrap();
    parsed["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or(&v.to_string()).to_owned())
        .collect()
}

fn rt_undefined() -> boa_engine::JsValue {
    boa_engine::JsValue::undefined()
}

#[test]
fn path_util_events_os_and_url() {
    let result = run(r#"
        const path = require('path');
        const util = require('node:util');
        const EventEmitter = require('events');
        const os = require('os');
        const { fileURLToPath, URL } = require('url');
        const heard = [];
        class Bus extends EventEmitter {}
        const bus = new Bus();
        bus.on('x', (a, b) => heard.push('on ' + a + b));
        bus.once('x', () => heard.push('once'));
        bus.emit('x', 1, 2);
        bus.emit('x', 3, 4);
        const f = () => heard.push('off');
        bus.on('y', f); bus.off('y', f);
        let threw = false;
        try { bus.emit('error', new Error('boom')) } catch (e) { threw = e.message === 'boom' }
        exports.result = [
            path.join('a', 'b/../c', 'd.md'),
            path.resolve('/vault', 'notes', '../x.md'),
            path.dirname('/a/b/c.md'),
            path.basename('/a/b/c.md', '.md'),
            path.extname('note.tar.gz'),
            path.relative('/a/b', '/a/c/d'),
            String(path.isAbsolute('C:\\x')),
            path.parse('/a/b.txt').name,
            util.format('%s is %d, %j', 'x', 42, { a: 1 }),
            heard.join(','),
            String(bus.listenerCount('x')),
            String(bus.listenerCount('y')),
            String(threw),
            String(bus instanceof EventEmitter),
            typeof os.platform(),
            typeof os.homedir(),
            fileURLToPath(require('url').pathToFileURL(__filename)) === __filename ? 'ok' : 'bad',
            String(new URL('https://e.com').host),
        ];
    "#);
    assert_eq!(
        result,
        [
            "a/c/d.md",
            "/vault/x.md",
            "/a/b",
            "c",
            ".gz",
            "../c/d",
            "true",
            "b",
            "x is 42, {\"a\":1}",
            "on 12,once,on 34",
            "1",
            "0",
            "true",
            "true",
            "string",
            "string",
            "ok",
            "e.com",
        ]
    );
}

#[test]
fn fs_reads_and_writes_and_child_process_refuses() {
    let dir = std::env::temp_dir().join(format!("verve-fs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dir_js = dir.to_string_lossy().replace('\\', "/");
    let result = run(&format!(
        r#"
        const fs = require('fs');
        const fsp = require('fs/promises');
        const {{ promisify }} = require('util');
        const {{ exec }} = require('child_process');
        const dir = '{dir_js}';
        fs.mkdirSync(dir + '/sub', {{ recursive: true }});
        fs.writeFileSync(dir + '/sub/a.txt', 'héllo');
        const out = [
            fs.readFileSync(dir + '/sub/a.txt', 'utf8'),
            String(fs.readFileSync(dir + '/sub/a.txt').length),
            String(fs.existsSync(dir + '/sub/a.txt')),
            String(fs.statSync(dir + '/sub').isDirectory()),
            fs.readdirSync(dir + '/sub').join(),
        ];
        let missing = '';
        try {{ fs.readFileSync(dir + '/nope') }} catch (e) {{ missing = e.message.split(':')[0] }}
        out.push(missing);
        exports.result = out;
        exports.pending = (async () => {{
            await fsp.writeFile(dir + '/b.txt', 'two');
            out.push(await fsp.readFile(dir + '/b.txt', 'utf8'));
            try {{ await promisify(exec)('echo hi') }} catch (e) {{ out.push(e.code) }}
        }})();
        "#
    ));
    // The synchronous half; the asynchronous one has settled by now too — the runtime runs
    // every microtask before it answers.
    assert_eq!(
        result,
        [
            "héllo", "6", "true", "true", "a.txt", "ENOENT", "two", "ENOSYS"
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_refuses_to_serve_and_console_keeps_time() {
    let result = run(r#"
        const http = require('http');
        const out = [String(typeof require('https').createServer), http.STATUS_CODES[404]];
        const server = http.createServer(() => {});
        server.on('error', (e) => out.push('server ' + e.code));
        server.listen(51361, '127.0.0.1');
        http.get('http://example.com').on('error', (e) => out.push('get ' + e.code));
        console.time('t');
        console.count(); console.count();
        console.timeEnd('t');
        console.assert(1 === 1, 'not shown');
        exports.result = out;
    "#);
    assert_eq!(
        result,
        ["function", "Not Found", "server EACCES", "get ENOSYS"]
    );
}
