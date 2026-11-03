// The scaling probes of docs/electron/perf-js.md: each probe has a size n and a body that does
// its work at that size, and the driver times it at n and at 4n. A ratio far above 4 is a
// super-linear cost — the class of bug that made `split`, `Map.prototype.delete` and regex scans
// quadratic in perf-baseline.md.
//
// String probes come twice, over Latin-1 text and over text with characters outside Latin-1, as
// Boa keeps a string in one of two encodings and the two may not share a code path.
//
//   node scaling.js            every probe, one JSON line each
//   node scaling.js --list     the probe names, as a JSON array
//   node scaling.js <probe>    one probe, one JSON line
//
// A probe answers {"probe","n","t1","t4","ratio"}; with "t4":null and a "skipped" reason when
// t(n) is too large to time at 4n; and with "superLinear":true when a run passed the 20 s limit.
'use strict';

const now = () => performance.now();

// The package's rule: a run past the 20 s limit at either size is super-linear, and 4n is then
// not attempted. Between SLOW_N_MS and the limit t(n) is too slow to time at 4n — a quadratic 4n
// would cost 16 times as much and could pass the limit — so the probe is reported as skipped,
// with no verdict: a linear operation at an oversized n would otherwise be called a bug.
const LIMIT_MS = 20000;
const SLOW_N_MS = LIMIT_MS / 16;

const LATIN1 = 'abcdefgh';
const UNICODE = '日本語かな漢字';

const probes = {};

function probe(name, n, make, run) {
    probes[name] = { n, make, run };
}

function stringProbe(name, n, make, run) {
    probe(name + '-latin1', n, (count) => make(LATIN1, count), run);
    probe(name + '-unicode', n, (count) => make(UNICODE, count), run);
}

function parts(count, alphabet, separator) {
    const list = new Array(count);
    for (let i = 0; i < count; i++) list[i] = alphabet + i;
    return list.join(separator);
}

function filler(alphabet, length) {
    return alphabet.repeat(Math.ceil(length / alphabet.length)).slice(0, length);
}

function numbers(count) {
    const list = new Array(count);
    for (let i = 0; i < count; i++) list[i] = i;
    return list;
}

function keyedMap(count) {
    const map = new Map();
    for (let i = 0; i < count; i++) map.set('k' + i, i);
    return map;
}

function numberSet(count) {
    const set = new Set();
    for (let i = 0; i < count; i++) set.add(i);
    return set;
}

// An object with n keys and every other one deleted, which is what leaves a Boa object in its
// dictionary mode; both probes below then read and write that object.
function deletedKeys(count) {
    const object = {};
    for (let i = 0; i < count; i++) object['k' + i] = i;
    for (let i = 0; i < count; i += 2) delete object['k' + i];
    return object;
}

// --- strings ------------------------------------------------------------------------------

stringProbe(
    'split-char',
    4000,
    (alphabet, count) => parts(count, alphabet, '-'),
    (text) => text.split('-').length,
);
stringProbe(
    'split-multi',
    4000,
    (alphabet, count) => parts(count, alphabet, '||'),
    (text) => text.split('||').length,
);
stringProbe(
    'indexOf',
    25000000,
    (alphabet, count) => filler(alphabet, count) + 'needle',
    (text) => text.indexOf('needle'),
);
stringProbe(
    'includes',
    25000000,
    (alphabet, count) => filler(alphabet, count) + 'needle',
    (text) => text.includes('needle'),
);
stringProbe(
    'lastIndexOf',
    5000000,
    // The needle first, so the backwards scan is the whole string.
    (alphabet, count) => 'needle' + filler(alphabet, count),
    (text) => text.lastIndexOf('needle'),
);
stringProbe(
    'replace-string',
    10000000,
    (alphabet, count) => filler(alphabet, count) + 'needle' + filler(alphabet, count),
    (text) => text.replace('needle', 'X').length,
);
stringProbe(
    'replaceAll-string',
    4000,
    (alphabet, count) => parts(count, alphabet, '--'),
    (text) => text.replaceAll('--', '|').length,
);
stringProbe(
    'replace-regex',
    5000,
    (alphabet, count) => parts(count, alphabet, '--'),
    (text) => text.replace(/--/g, '|').length,
);
stringProbe(
    'replaceAll-regex',
    5000,
    (alphabet, count) => parts(count, alphabet, '--'),
    (text) => text.replaceAll(/--/g, '|').length,
);
stringProbe(
    'matchAll',
    4000,
    (alphabet, count) => {
        const lines = new Array(count);
        for (let i = 0; i < count; i++) lines[i] = alphabet + i + '@example.com baz\n';
        return lines.join('');
    },
    (text) => {
        let count = 0;
        for (const _ of text.matchAll(/(\S+)@(\S+)\.com/g)) count++;
        return count;
    },
);
stringProbe(
    'exec-loop',
    6000,
    (alphabet, count) => parts(count, alphabet, ' '),
    (text) => {
        const pattern = /[^ ]+/g;
        let count = 0;
        while (pattern.exec(text) !== null) count++;
        return count;
    },
);
stringProbe(
    'repeat',
    1000000,
    (alphabet) => alphabet,
    (text, count) => text.repeat(count).length,
);
stringProbe(
    'padStart',
    5000000,
    (alphabet) => alphabet,
    (text, count) => text.padStart(count, '0').length,
);

probe(
    'concat',
    20000,
    (count) => count,
    (count) => {
        let text = '';
        for (let i = 0; i < count; i++) text += 'ab' + i;
        return text.length;
    },
);

// --- arrays -------------------------------------------------------------------------------

// `unshift` and `splice` move the rest of the array, so one call costs O(n) whatever the engine;
// each probe makes a fixed number of calls over an array of n, so the ratio is 4 while a call is
// O(n) and 16 only if the engine made each call more than O(n) as the array grows. Those two
// bodies mutate the array they are given, which the two timed runs of one probe share: the calls
// made are fixed, so it moves by 2% of n at most (100 of 5,000). Copies are avoided on purpose:
// `slice` of n elements is a per-element cost of its own, larger than the moves a copy would have
// isolated. `shift` rebuilds its array every round and is explained at its probe.

probe(
    'array-indexOf',
    400000,
    (count) => numbers(count),
    (list) => list.indexOf(-1),
);
probe(
    'array-includes',
    400000,
    (count) => numbers(count),
    (list) => list.includes(-1),
);
// `shift` is not a quadratic-algorithm probe: Boa's dense `shift` is a `Vec::remove(0)`, one
// memmove of the rest of the array, so a call is O(n) and the ratio is 4 while the moved bytes
// stay in cache. Three rounds over a fresh array keep both sizes' working set inside the 1 MB L2
// (n=50,000 is 200 KB of i32 elements, 4n is 800 KB) and each round shifts only 8% of n, so the
// body has no state to share between the two timed runs. At n=100,000 the 4n run leaves the cache
// and the ratio jumps to 12.5 with the algorithm unchanged: that is a bandwidth cliff, which is
// why this row measures the memmove's cost and is not handed to P65 as a scaling defect.
probe(
    'array-shift',
    50000,
    (count) => count,
    (count) => {
        let sum = 0;
        for (let round = 0; round < 3; round++) {
            const list = numbers(count);
            for (let i = 0; i < 4000; i++) sum += list.shift();
        }
        return sum;
    },
);
probe(
    'array-unshift',
    5000,
    (count) => numbers(count),
    (list) => {
        for (let i = 0; i < 100; i++) list.unshift(-i);
        return list.length;
    },
);
probe(
    'array-splice',
    5000,
    (count) => numbers(count),
    (list) => {
        for (let i = 0; i < 100; i++) list.splice(0, 1);
        return list.length;
    },
);

// --- maps and sets ------------------------------------------------------------------------

probe(
    'map-set',
    40000,
    (count) => count,
    (count) => keyedMap(count).size,
);
probe(
    'map-iterate',
    20000,
    (count) => keyedMap(count),
    (map) => {
        let sum = 0;
        for (const entry of map) sum += entry[1];
        return sum;
    },
);
probe(
    'map-delete',
    20000,
    (count) => count,
    (count) => {
        const map = keyedMap(count);
        for (let i = 0; i < count; i += 2) map.delete('k' + i);
        return map.size;
    },
);
probe(
    'set-add',
    80000,
    (count) => count,
    (count) => numberSet(count).size,
);
probe(
    'set-iterate',
    40000,
    (count) => numberSet(count),
    (set) => {
        let sum = 0;
        for (const value of set) sum += value;
        return sum;
    },
);
probe(
    'set-delete',
    20000,
    (count) => count,
    (count) => {
        const set = numberSet(count);
        for (let i = 0; i < count; i += 2) set.delete(i);
        return set.size;
    },
);
probe(
    'set-delete-while-iterating',
    20000,
    (count) => count,
    (count) => {
        const set = numberSet(count);
        let seen = 0;
        for (const value of set) {
            seen++;
            if ((value & 1) === 0) set.delete(value);
        }
        return seen + set.size;
    },
);

// --- objects ------------------------------------------------------------------------------

probe(
    'dict-delete-read',
    4000,
    (count) => count,
    (count) => {
        const object = deletedKeys(count);
        let sum = 0;
        for (let i = 0; i < count; i++) sum += object['k' + i] === undefined ? 0 : 1;
        return sum;
    },
);
probe(
    'dict-delete-write',
    4000,
    (count) => count,
    (count) => {
        const object = deletedKeys(count);
        for (let i = 0; i < count; i += 2) object['w' + i] = i;
        return Object.keys(object).length;
    },
);

// --- JSON ---------------------------------------------------------------------------------

function records(count) {
    const list = new Array(count);
    for (let i = 0; i < count; i++) list[i] = { id: i, name: 'item' + i, tags: ['a', 'b'] };
    return list;
}

// One character in eleven is a quote, a backslash, a newline or a tab, so stringifying it is
// mostly the escaping step rather than the copy.
const ESCAPED = 'abcdefg"\\\n\t';

function escapedText(length) {
    return ESCAPED.repeat(Math.ceil(length / ESCAPED.length)).slice(0, length);
}

probe(
    'json-stringify',
    20000,
    (count) => records(count),
    (list) => JSON.stringify(list).length,
);
probe(
    'json-stringify-longstring',
    3000000,
    // A long string value whose characters mostly need escaping, which is the serializer path a
    // large text field takes (a note body, a source file, a log line).
    (count) => escapedText(count),
    (text) => JSON.stringify(text).length,
);
probe(
    'json-parse',
    20000,
    (count) => JSON.stringify(records(count)),
    (text) => JSON.parse(text).length,
);

// --- the driver ---------------------------------------------------------------------------

function timed(entry, data, size) {
    const start = now();
    entry.run(data, size);
    return now() - start;
}

// The faster of two runs, so that a collection or a scheduling hiccup does not decide.
function fastest(entry, data, size) {
    return Math.min(timed(entry, data, size), timed(entry, data, size));
}

// One probe at both sizes: n is the size the probe chose, so that t(n) lands in 10-200 ms
// interpreted; 4n tells linear from super-linear. A run past the 20 s limit at either size is
// super-linear, by the package's rule; t(n) merely too large to time at 4n is skipped instead.
function runProbe(name, entry) {
    const small = entry.make(entry.n);
    const t1 = fastest(entry, small, entry.n);
    if (t1 > LIMIT_MS) {
        return {
            ...skipped(name, entry, t1, null, 't(n) is over the ' + LIMIT_MS + ' ms limit'),
            superLinear: true,
        };
    }
    if (t1 > SLOW_N_MS) {
        return skipped(name, entry, t1, null, 't(n) is over ' + SLOW_N_MS + ' ms: 4n could pass the limit');
    }
    const large = entry.make(entry.n * 4);
    const t4 = fastest(entry, large, entry.n * 4);
    if (t4 > LIMIT_MS) {
        return {
            ...skipped(name, entry, t1, t4, 't(4n) is over the ' + LIMIT_MS + ' ms limit'),
            superLinear: true,
        };
    }
    return { probe: name, n: entry.n, t1: round(t1), t4: round(t4), ratio: round(t4 / t1) };
}

// A probe with no ratio, and why: the same shape for both ways of not timing 4n.
function skipped(name, entry, t1, t4, reason) {
    return {
        probe: name,
        n: entry.n,
        t1: round(t1),
        t4: t4 === null ? null : round(t4),
        ratio: null,
        skipped: reason,
    };
}

function round(value) {
    return +value.toFixed(1);
}

const asked = process.argv[2];
if (asked === '--list') {
    console.log(JSON.stringify(Object.keys(probes)));
} else if (asked !== undefined) {
    if (probes[asked] === undefined) {
        console.error('unknown probe: ' + asked);
        process.exit(2);
    }
    console.log(JSON.stringify(runProbe(asked, probes[asked])));
} else {
    for (const name of Object.keys(probes)) console.log(JSON.stringify(runProbe(name, probes[name])));
}
