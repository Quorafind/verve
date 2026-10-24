//! Node's smaller modules: each loadable by either name, and each doing what libraries bundled
//! into plugins ask of it — or refusing as Node does when the system says no.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use runtime_x::RuntimeX;

fn folder(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runtime-x-misc-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A runtime on a clock the test moves.
fn fake_clock() -> (Rc<Cell<f64>>, RuntimeX) {
    let time = Rc::new(Cell::new(0.));
    let read = Rc::clone(&time);
    (time, RuntimeX::with_clock(move || read.get()))
}

/// Runs `body` as a module, then the host's loop until `time` reaches `until`; answers the
/// global `log` joined by `|`.
fn run(test: &str, body: &str, until: f64) -> (String, Vec<runtime_x::ConsoleLine>) {
    let (time, mut rt) = fake_clock();
    let dir = folder(test);
    rt.eval("globalThis.log = []").unwrap();
    rt.load_module(&dir.join("main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    let mut now = 0.;
    while now <= until {
        time.set(now);
        rt.run_due();
        rt.run_microtasks();
        now += 10.;
    }
    let log = runtime_x::print(&rt.eval("log.join('|')").unwrap());
    (log, rt.take_console())
}

fn expect(log: &str, expected: &[&str]) {
    let parts: Vec<&str> = log.split('|').collect();
    assert_eq!(parts, expected, "\nwhole log: {log}");
}

#[test]
fn every_module_loads_by_both_names() {
    let mine = [
        "async_hooks",
        "child_process",
        "cluster",
        "console",
        "constants",
        "dgram",
        "diagnostics_channel",
        "dns",
        "dns/promises",
        "fs",
        "fs/promises",
        "http",
        "http2",
        "https",
        "inspector",
        "net",
        "os",
        "path",
        "path/posix",
        "path/win32",
        "perf_hooks",
        "punycode",
        "querystring",
        "readline",
        "readline/promises",
        "repl",
        "timers",
        "timers/promises",
        "tls",
        "trace_events",
        "tty",
        "url",
        "v8",
        "vm",
        "wasi",
        "worker_threads",
    ];
    let list = mine
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(",");
    let (log, _) = run(
        "load",
        &format!(
            "for (const name of [{list}]) {{
                const bare = require(name), prefixed = require('node:' + name);
                log.push(bare === prefixed && (typeof bare === 'object' || typeof bare === 'function') ? 'ok' : 'bad ' + name);
            }}"
        ),
        0.,
    );
    assert!(log.split('|').all(|l| l == "ok"), "{log}");
}

#[test]
fn querystring_punycode_url() {
    let (log, _) = run(
        "qs",
        r#"
        const qs = require('querystring');
        const punycode = require('punycode');
        const url = require('url');
        const parsed = qs.parse('a=1&b=2&b=3&c=x+y%20z&d&e=%E4%B8%AD');
        log.push(JSON.stringify(parsed), Object.getPrototypeOf(parsed) === null);
        log.push(qs.stringify({ a: [1, 2], b: 'x y', c: true, d: null, e: { no: 1 } }));
        log.push(qs.parse('a:1;b:2', ';', ':').b, qs.escape('ü&'), qs.unescape('%zz%41'));
        log.push(punycode.toASCII('mañana.com'), punycode.toUnicode('xn--maana-pta.com'), punycode.ucs2.decode('a😀').join(','));
        const u = url.parse('https://u:p@Example.com:8080/a/b?x=1&y=2#h', true);
        log.push(u.host, u.pathname, u.query.y, u.hash, u.href);
        log.push(url.format({ protocol: 'https', hostname: 'e.com', pathname: '/x', query: { a: 1 } }));
        log.push(url.resolve('http://e.com/a/b', '../c'), url.resolve('/one/two', 'three'));
        log.push(url.format(new URL('https://a:b@e.com/p?q#f'), { auth: false, fragment: false }));
        log.push(url.urlToHttpOptions(new URL('http://a:b@e.com:81/p?q')).port, url.domainToASCII('español.com'));
        log.push(new url.Url() instanceof url.Url, url.parse('/x?y').search);
        "#,
        0.,
    );
    expect(
        &log,
        &[
            r#"{"a":"1","b":["2","3"],"c":"x y z","d":"","e":"中"}"#,
            "true",
            "a=1&a=2&b=x%20y&c=true&d=&e=",
            "2",
            "%C3%BC%26",
            "%zzA",
            "xn--maana-pta.com",
            "mañana.com",
            "97,128512",
            "example.com:8080",
            "/a/b",
            "2",
            "#h",
            "https://u:p@example.com:8080/a/b?x=1&y=2#h",
            "https://e.com/x?a=1",
            "http://e.com/c",
            "/one/three",
            "https://e.com/p?q",
            "81",
            "xn--espaol-zwa.com",
            "true",
            "?y",
        ],
    );
}

#[test]
fn os_and_constants() {
    let (log, _) = run(
        "os",
        r#"
        const os = require('os');
        const constants = require('constants');
        log.push(typeof os.release(), os.cpus().length > 0, os.totalmem() > 0, os.freemem() > 0, os.endianness());
        log.push(os.constants.signals.SIGTERM, os.constants.errno.ENOENT, constants.O_RDONLY, constants.ENOENT, constants.S_IFDIR);
        log.push(typeof os.userInfo().username, os.loadavg().length, typeof os.networkInterfaces(), os.availableParallelism() > 0);
        "#,
        0.,
    );
    expect(
        &log,
        &[
            "string", "true", "true", "true", "LE", "15", "2", "0", "2", "16384", "string", "3",
            "object", "true",
        ],
    );
}

#[test]
fn timers_and_timer_promises() {
    let (log, _) = run(
        "timers",
        r#"
        const timers = require('timers');
        const { setTimeout: sleep, setImmediate: soon, setInterval: every, scheduler } = require('timers/promises');
        const t = timers.setTimeout(() => log.push('timeout'), 20);
        log.push(typeof t.unref, t.hasRef(), t.unref() === t, t.hasRef(), typeof +t);
        const u = timers.setTimeout(() => log.push('never'), 30);
        clearTimeout(u);
        sleep(50, 'slept').then(v => log.push(v));
        soon('soon').then(v => log.push(v));
        const ac = new AbortController();
        sleep(100, 'x', { signal: ac.signal }).catch(e => log.push(e.name + ':' + e.code));
        timers.setTimeout(() => ac.abort(), 60);
        (async () => {
            let n = 0;
            for await (const v of every(25, 'tick')) { log.push(v + ++n); if (n === 2) break; }
            await scheduler.wait(10);
            log.push('waited');
        })();
        "#,
        200.,
    );
    expect(
        &log,
        &[
            "function",
            "true",
            "true",
            "false",
            "number",
            "soon",
            "timeout",
            "tick1",
            "slept",
            "tick2",
            "AbortError:ABORT_ERR",
            "waited",
        ],
    );
}

#[test]
fn async_local_storage_carries_its_store() {
    let (log, _) = run(
        "als",
        r#"
        const { AsyncLocalStorage, AsyncResource, executionAsyncId } = require('async_hooks');
        const als = new AsyncLocalStorage();
        const other = new AsyncLocalStorage();
        log.push(String(als.getStore()));
        const out = als.run({ id: 1 }, (a) => {
            log.push(als.getStore().id + a);
            other.run('o', () => log.push(als.getStore().id + other.getStore()));
            als.run({ id: 2 }, () => log.push(als.getStore().id));
            log.push(als.getStore().id);
            setTimeout(() => log.push('timer ' + als.getStore().id), 10);
            queueMicrotask(() => log.push('micro ' + als.getStore().id));
            Promise.resolve().then(() => log.push('then ' + als.getStore().id));
            return 'out';
        }, 'a');
        log.push(out, String(als.getStore()));
        const resource = als.run('captured', () => new AsyncResource('X'));
        resource.runInAsyncScope(() => log.push('resource ' + als.getStore()));
        const bound = als.run('bound', () => AsyncLocalStorage.bind(() => als.getStore()));
        log.push(bound(), typeof executionAsyncId());
        als.enterWith('entered');
        log.push(als.getStore());
        als.disable();
        log.push(String(als.getStore()));
        "#,
        30.,
    );
    expect(
        &log,
        &[
            "undefined",
            "1a",
            "1o",
            "2",
            "1",
            "out",
            "undefined",
            "resource captured",
            "bound",
            "number",
            "entered",
            "undefined",
            "micro 1",
            "then 1",
            "timer 1",
        ],
    );
}

#[test]
fn diagnostics_perf_inspector() {
    let (log, _) = run(
        "diag",
        r#"
        const dc = require('diagnostics_channel');
        const ch = dc.channel('my');
        log.push(ch === dc.channel('my'), ch.hasSubscribers, dc.hasSubscribers('my'));
        const f = (m, name) => log.push(name + ':' + m.v);
        dc.subscribe('my', f);
        ch.publish({ v: 1 });
        log.push(ch.hasSubscribers, ch.unsubscribe(f), ch.hasSubscribers);
        const tc = dc.tracingChannel('op');
        tc.subscribe({ start: m => log.push('start'), end: m => log.push('end ' + m.result) });
        log.push(tc.traceSync(() => 7, {}));
        const { performance, PerformanceObserver, monitorEventLoopDelay } = require('perf_hooks');
        log.push(performance === globalThis.performance, typeof new PerformanceObserver(() => {}).observe);
        const h = monitorEventLoopDelay(); h.enable(); log.push(h.disable());
        const inspector = require('inspector');
        log.push(String(inspector.url()));
        new inspector.Session().post('x', (e) => log.push(e.code));
        "#,
        0.,
    );
    expect(
        &log,
        &[
            "true",
            "false",
            "false",
            "my:1",
            "true",
            "true",
            "false",
            "start",
            "end 7",
            "7",
            "true",
            "function",
            "true",
            "undefined",
            "ERR_INSPECTOR_NOT_AVAILABLE",
        ],
    );
}

#[test]
fn vm_v8_and_worker_threads() {
    let (log, _) = run(
        "vm",
        r#"
        const vm = require('vm');
        log.push(vm.runInThisContext('1 + 2'));
        globalThis.shared = 'global';
        const sandbox = { x: 2 };
        log.push(vm.runInNewContext('var y = x * 21; z = shared + "!"; y', sandbox));
        log.push(sandbox.y, sandbox.z, typeof globalThis.y, vm.isContext(sandbox));
        const ctx = vm.createContext({ n: 1 });
        new vm.Script('n += 1').runInContext(ctx);
        log.push(ctx.n, vm.runInContext('this === globalThis ? "global" : typeof this.n', ctx));
        try { new vm.Script('let = = 1') } catch (e) { log.push(e.name) }
        log.push(vm.compileFunction('return a + b', ['a', 'b'])(2, 3));
        const v8 = require('v8');
        log.push(JSON.stringify(v8.deserialize(v8.serialize({ a: [1, 'b'] }))), v8.getHeapStatistics().heap_size_limit > 0);
        const wt = require('worker_threads');
        log.push(wt.isMainThread, String(wt.parentPort), wt.threadId);
        wt.setEnvironmentData('k', 'v');
        log.push(wt.getEnvironmentData('k'));
        const w = new wt.Worker('x.js');
        w.on('error', e => log.push(e.code));
        w.on('exit', c => log.push('exit ' + c));
        "#,
        0.,
    );
    expect(
        &log,
        &[
            "3",
            "42",
            "42",
            "global!",
            "undefined",
            "true",
            "2",
            "number",
            "SyntaxError",
            "5",
            r#"{"a":[1,"b"]}"#,
            "true",
            "true",
            "null",
            "0",
            "v",
            "ERR_WORKER_INIT_FAILED",
            "exit 1",
        ],
    );
}

#[test]
fn readline_tty_console() {
    let (log, _) = run(
        "readline",
        r#"
        const readline = require('readline');
        const EventEmitter = require('events');
        const input = new EventEmitter();
        const written = [];
        const output = { write: t => written.push(t) };
        const rl = readline.createInterface({ input, output });
        rl.on('line', l => log.push('line ' + l));
        rl.on('close', () => log.push('closed'));
        rl.question('name? ', a => log.push('answer ' + a));
        input.emit('data', Buffer.from('Ada\nsec'));
        input.emit('data', 'ond\r\nthird');
        input.emit('end');
        log.push(written.join(''));
        const rp = require('readline/promises').createInterface({ input: new EventEmitter() });
        rp.write('typed\n');
        (async () => { for await (const l of rp) { log.push('iter ' + l); break; } })();
        const tty = require('tty');
        log.push(tty.isatty(1), new tty.WriteStream(1).getColorDepth());
        const { Console } = require('console');
        const out = [];
        const c = new Console({ stdout: { write: t => out.push(t) } });
        c.log('%s=%d', 'n', 5);
        c.error('bad');
        log.push(JSON.stringify(out), require('console') === console);
        "#,
        0.,
    );
    expect(
        &log,
        &[
            "answer Ada",
            "line second",
            "line third",
            "closed",
            "name? ",
            "false",
            "1",
            r#"["n=5\n","bad\n"]"#,
            "true",
            "iter typed",
        ],
    );
}

#[test]
fn network_and_processes_refuse() {
    let (log, _) = run(
        "net",
        r#"
        const net = require('net');
        log.push(net.isIP('1.2.3.4'), net.isIP('::1'), net.isIP('fe80::1%eth0'), net.isIP('x'), net.isIPv4('1.2.3.4'));
        const s = net.connect(80, 'example.com');
        s.on('error', e => log.push('socket ' + e.code));
        s.on('close', () => log.push('socket closed'));
        log.push(s instanceof net.Socket, s instanceof require('events'));
        const dns = require('dns');
        dns.lookup('localhost', (e, address, family) => log.push('lookup ' + address + ' ' + family));
        dns.lookup('example.com', e => log.push('lookup ' + e.code));
        require('dns').promises.lookup('127.0.0.1').then(r => log.push('promise ' + r.address));
        const http = require('http');
        const req = http.request('http://example.com');
        req.on('error', e => log.push('http ' + e.code));
        log.push(req instanceof http.ClientRequest, http.STATUS_CODES[418], http.METHODS.includes('PATCH'), typeof http.globalAgent);
        try { http.validateHeaderName('bad header') } catch (e) { log.push(e.code) }
        const https = require('https');
        log.push(new https.Agent() instanceof http.Agent);
        const cp = require('child_process');
        cp.exec('ls', (e, out, err) => log.push('exec ' + e.code + JSON.stringify(out)));
        try { cp.execSync('ls') } catch (e) { log.push('execSync ' + e.code) }
        log.push('spawnSync ' + cp.spawnSync('ls').error.code);
        const child = cp.spawn('ls');
        child.on('error', e => log.push('child ' + e.code));
        log.push(typeof child.stdout.on, require('cluster').isPrimary);
        const srv = net.createServer();
        srv.on('error', e => log.push('listen ' + e.code));
        srv.listen(3000);
        "#,
        0.,
    );
    expect(
        &log,
        &[
            "4",
            "6",
            "6",
            "0",
            "true",
            "true",
            "true",
            "true",
            "I'm a Teapot",
            "true",
            "object",
            "ERR_INVALID_HTTP_TOKEN",
            "true",
            "execSync ENOSYS",
            "spawnSync ENOSYS",
            "function",
            "true",
            "socket ECONNREFUSED",
            "socket closed",
            "lookup 127.0.0.1 4",
            "lookup ENOTFOUND",
            "promise 127.0.0.1",
            "http ENOSYS",
            "exec ENOSYS\"\"",
            "child ENOSYS",
            "listen EACCES",
        ],
    );
}
