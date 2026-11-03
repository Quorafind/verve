// The JS throughput suite of docs/electron/perf-js.md: one measurement per process, one JSON
// line per run, so a driver repeats it and takes the median. The first ten tests are
// docs/electron/perf-baseline.md's, with its names and its sizes, so their numbers compare with
// that table directly; the last four are shaped like application code — a virtual DOM diff, a
// tokenizer, HTML built by templates, and a map/filter/reduce chain.
//
//   node bench.js            all tests, one JSON object
//   node bench.js <test>     one test, one JSON object
//   node bench.js --list     the test names, as a JSON array
'use strict';

const TARGET = process.argv[2];
const now = () => performance.now();

let sink;

function fib(n) {
    return n < 2 ? n : fib(n - 1) + fib(n - 2);
}

class A {
    constructor(x) {
        this.x = x;
    }
    get() {
        return this.x;
    }
    add(o) {
        return new A(this.x + o.get());
    }
}

class B extends A {
    get() {
        return this.x + 1;
    }
}

// A tree of about 2,000 nodes as plain objects, keys as strings. Every round builds a fresh
// copy with a tenth of its leaves changed and diffs it against the previous one.
const VDOM_SECTIONS = 20;
const VDOM_LEAVES = 99;
const VDOM_ROUNDS = 5;

function vnode(tag, key, text) {
    return { tag, key, text, props: {}, children: [] };
}

function vdomTree(round) {
    const root = vnode('div', 'root', '');
    for (let s = 0; s < VDOM_SECTIONS; s++) {
        const section = vnode('section', 's' + s, '');
        for (let i = 0; i < VDOM_LEAVES; i++) {
            const index = s * VDOM_LEAVES + i;
            // One leaf in ten carries a new text.
            const changed = index % 10 === 0 ? round : 0;
            section.children.push(vnode('span', 's' + s + '-' + i, 'item ' + index + '-' + changed));
        }
        root.children.push(section);
    }
    return root;
}

function vdomDiff(oldNode, newNode, path, patches) {
    if (oldNode.tag !== newNode.tag || oldNode.key !== newNode.key) {
        patches.push({ op: 'replace', path });
        return;
    }
    if (oldNode.text !== newNode.text) {
        patches.push({ op: 'text', path, value: newNode.text });
    }
    const oldByKey = new Map();
    for (const child of oldNode.children) {
        oldByKey.set(child.key, child);
    }
    for (const child of newNode.children) {
        const old = oldByKey.get(child.key);
        if (old === undefined) {
            patches.push({ op: 'insert', path: path + '/' + child.key });
        } else {
            vdomDiff(old, child, path + '/' + child.key, patches);
        }
    }
}

// About 200 KB of source for the tokenizer: function declarations with numbers, comments and
// string literals, so that every branch of the pattern is taken.
const TOKEN_BYTES = 200 * 1024;
const TOKEN_PATTERN =
    /\/\/[^\n]*|\s+|[A-Za-z_$][\w$]*|\d+|'(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*"|===|!==|=>|&&|\|\||[+\-*/%<>=!?:;,.(){}\[\]]/y;

let tokenSource;

function tokenText() {
    if (tokenSource === undefined) {
        const lines = [];
        let size = 0;
        for (let i = 0; size < TOKEN_BYTES; i++) {
            const line =
                'function name' + i + "(a, b) { const x = a + " + i + '; // note ' + i +
                "\n  return x * b + '" + i + "'; }\n";
            lines.push(line);
            size += line.length;
        }
        tokenSource = lines.join('');
    }
    return tokenSource;
}

function tokenize(text) {
    let count = 0;
    let pos = 0;
    while (pos < text.length) {
        TOKEN_PATTERN.lastIndex = pos;
        const match = TOKEN_PATTERN.exec(text);
        if (match === null) {
            pos++;
            continue;
        }
        pos = TOKEN_PATTERN.lastIndex;
        count++;
    }
    return count;
}

const tests = {
    fib: () => fib(30),
    alloc: () => {
        let a = [];
        for (let i = 0; i < 300000; i++) {
            a.push({ i, s: 'k' + i, n: { v: i } });
            if (a.length > 1000) a = [];
        }
        sink = a;
    },
    concat: () => {
        let s = '';
        for (let i = 0; i < 20000; i++) s += 'ab' + i;
        sink = s.length;
    },
    splitjoin: () => {
        const parts = [];
        for (let i = 0; i < 20000; i++) parts.push('ab' + i);
        const s = parts.join('a');
        const p = s.split('a');
        sink = p.join('-').length;
    },
    regex: () => {
        let t = '';
        for (let i = 0; i < 5000; i++) t += 'foo' + i + ' bar@example.com baz 2024-01-' + (i % 28 + 1) + '\n';
        let c = 0;
        for (const m of t.matchAll(/(\w+)@(\w+)\.com/g)) c++;
        c += t.replace(/\d{4}-\d\d-\d+/g, 'D').length;
        sink = c;
    },
    json: () => {
        const o = [];
        for (let i = 0; i < 70000; i++) {
            o.push({ id: i, name: 'item' + i, tags: ['a', 'b', 'c'], nested: { x: i * 1.5, y: 'yy' + i, z: [1, 2, 3] } });
        }
        const s = JSON.stringify(o);
        if (s.length < 5e6) throw 1;
        const p = JSON.parse(s);
        sink = p.length + JSON.stringify(p).length;
    },
    sort: () => {
        const a = new Array(1000000);
        let x = 12345;
        for (let i = 0; i < a.length; i++) {
            x = (x * 1103515245 + 12345) & 0x7fffffff;
            a[i] = x;
        }
        a.sort((p, q) => p - q);
        sink = a[0];
    },
    mapset: () => {
        const m = new Map();
        const s = new Set();
        for (let i = 0; i < 20000; i++) {
            m.set('k' + i, i);
            s.add(i);
        }
        for (let i = 0; i < 20000; i += 2) {
            m.delete('k' + i);
            s.delete(i);
        }
        let c = 0;
        for (const [k, v] of m) c += v;
        sink = c + s.size;
    },
    oo: () => {
        let a = new A(0);
        const b = new B(1);
        for (let i = 0; i < 1000000; i++) {
            a = a.add(b);
        }
        sink = a.x;
    },
    typed: () => {
        const f = new Float64Array(1000000);
        for (let i = 0; i < f.length; i++) f[i] = i * 0.5;
        let s = 0;
        for (let r = 0; r < 5; r++) {
            for (let i = 0; i < f.length; i++) {
                f[i] = f[i] * 1.0001 + 1;
                s += f[i];
            }
        }
        sink = s;
    },
    vdom: () => {
        let old = vdomTree(0);
        let patches = 0;
        for (let round = 1; round <= VDOM_ROUNDS; round++) {
            const next = vdomTree(round);
            const list = [];
            vdomDiff(old, next, '', list);
            patches += list.length;
            old = next;
        }
        sink = patches;
    },
    tokenize: () => {
        sink = tokenize(tokenText());
    },
    template: () => {
        const rows = new Array(1000);
        for (let i = 0; i < rows.length; i++) rows[i] = i;
        const html = '<table>' +
            rows.map((i) => `<tr><td>${i}</td><td>${i * 2}</td><td>item ${i}</td></tr>`).join('') +
            '</table>';
        sink = html.length;
    },
    closure: () => {
        const values = new Array(1000000);
        for (let i = 0; i < values.length; i++) values[i] = i;
        const scale = 3;
        sink = values
            .map((v) => v * scale + 1)
            .filter((v) => (v & 3) === 0)
            .reduce((sum, v) => sum + v, 0);
    },
};

if (TARGET === '--list') {
    console.log(JSON.stringify(Object.keys(tests)));
} else {
    const names = TARGET ? [TARGET] : Object.keys(tests);
    const out = {};
    for (const name of names) {
        const test = tests[name];
        if (test === undefined) {
            console.error('unknown test: ' + name);
            process.exit(2);
        }
        test();
        const t0 = now();
        test();
        const t1 = now();
        out[name] = +(t1 - t0).toFixed(1);
    }
    console.log(JSON.stringify(out));
}
