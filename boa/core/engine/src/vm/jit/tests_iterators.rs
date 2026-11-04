//! Tests of `for…of`, spread and destructuring over the built-in iterators, which step without
//! making a result object when `next` is the intrinsic one: every case that must keep making
//! one (a replaced `next`, a user iterator, a generator) and every read the step makes of the
//! iterated object must give the interpreter's result.

use super::tests::same;

/// The six kinds of iterator, each over every shape of its iterated object.
#[test]
fn the_six_iterators_match_the_interpreter() {
    same(
        "const out = [];
         const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name + ':' + e.message); } };
         function* gen(n) { for (let i = 0; i < n; i++) yield i * 2; return 'r'; }
         const user = (n) => ({ [Symbol.iterator]() { let i = 0; return { next() { return i < n ? { value: i++, done: false } : { value: 'end', done: true }; } }; } });
         const sum = (src) => { let s = 0; for (const x of src) s += typeof x === 'object' ? x[0] + x[1] : x; return s; };
         for (let r = 0; r < 12; r++) {
           t(() => sum([1, 2, 3, 4, 5]));
           t(() => sum(new Set([1, 2, 3, 3, 4])));
           t(() => sum(new Map([[1, 2], [3, 4]])));
           t(() => sum(new Map([[1, 2], [3, 4]]).keys()));
           t(() => sum(new Map([[1, 2], [3, 4]]).values()));
           t(() => sum(new Set([5, 6]).entries()));
           t(() => sum([7, 8].entries()));
           t(() => sum([7, 8].keys()));
           t(() => { let s = ''; for (const c of 'a\\u{1F600}b\\uD800c') s += c.length; return s; });
           t(() => sum(gen(10)));
           t(() => sum(user(10)));
           t(() => sum(new Uint8Array([1, 2, 3])));
           t(() => sum(Object.assign([1, 2], { length: 5 })));
           t(() => sum([1, , 3, , 5]));
           t(() => { const a = [1, 2, 3]; a.length = 1e5; let n = 0; for (const x of a) n += x === undefined ? 1 : 0; return n; });
         }
         out.map(show).join(' ')",
    );
}

/// The array changes while it is iterated: appended to, cut short, resized, emptied.
#[test]
fn arrays_changing_during_iteration_match_the_interpreter() {
    same(
        "const out = [];
         const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
         for (let r = 0; r < 12; r++) {
           t(() => { const a = [1, 2, 3], seen = []; for (const x of a) { seen.push(x); if (a.length < 6) a.push(x * 10); } return seen; });
           t(() => { const a = [1, 2, 3, 4, 5], seen = []; for (const x of a) { seen.push(x); if (x === 2) a.length = 3; } return seen; });
           t(() => { const a = [1, 2, 3, 4, 5], seen = []; for (const x of a) { seen.push(x); if (x === 2) a.length = 0; } return seen; });
           t(() => { const a = [1, 2, 3], seen = []; for (const x of a) { seen.push(x); if (x === 3) a.length = 8; } return seen.length; });
           t(() => { const a = [1, 2, 3, 4], seen = []; for (const x of a) { seen.push(x); a.shift(); } return seen; });
           t(() => { const s = new Set([1, 2, 3]), seen = []; for (const x of s) { seen.push(x); if (x < 6) s.add(x + 3); if (x === 2) s.delete(3); } return seen; });
           t(() => { const m = new Map([[1, 1]]), seen = []; for (const [k] of m) { seen.push(k); if (k < 5) m.set(k + 1, 0); } return seen; });
           t(() => { const a = [1, 2, 3], it = a[Symbol.iterator](), seen = []; for (const x of it) { seen.push(x); } return [seen, it.next()]; });
           t(() => { const it = [1, 2][Symbol.iterator](); const seen = []; for (const x of it) { seen.push(x); break; } for (const x of it) seen.push(x); return seen; });
         }
         out.map(show).join(' ')",
    );
}

/// `Array.prototype[Symbol.iterator]` and `%ArrayIteratorPrototype%.next` replaced before the
/// loop starts, and `%ArrayIteratorPrototype%.next` replaced inside it, where the record keeps
/// the `next` it read first.
#[test]
fn rewritten_iterator_protocol_matches_the_interpreter() {
    same(
        "const out = [];
         const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name + ':' + e.message); } };
         const AIP = Object.getPrototypeOf([][Symbol.iterator]());
         const realNext = AIP.next, realIter = Array.prototype[Symbol.iterator];
         const restore = () => { AIP.next = realNext; Array.prototype[Symbol.iterator] = realIter; };
         for (let r = 0; r < 12; r++) {
           t(() => { Array.prototype[Symbol.iterator] = function* () { yield 'g1'; yield 'g2'; }; try { const s = []; for (const x of [1, 2, 3]) s.push(x); return s; } finally { restore(); } });
           t(() => { AIP.next = function () { const n = (this.n = (this.n || 0) + 1); return { done: n > 3, value: 'n' + n }; }; try { const s = []; for (const x of [1, 2, 3, 4, 5]) s.push(x); return s; } finally { restore(); } });
           t(() => { const s = []; for (const x of [1, 2, 3, 4, 5]) { s.push(x); if (x === 2) AIP.next = function () { return { done: true }; }; } restore(); return s; });
           t(() => { const s = []; const it = [1, 2, 3][Symbol.iterator](); AIP.next = function () { return { done: true }; }; for (const x of it) s.push(x); restore(); return s; });
           t(() => { const it = [1, 2, 3][Symbol.iterator](); it.next = function () { const done = !!this.k; this.k = true; return { done, value: 'own' }; }; const s = []; for (const x of { [Symbol.iterator]: () => it }) s.push(x); return s; });
           t(() => { AIP.next = 5; try { for (const x of [1]) ; } finally { restore(); } });
           t(() => { delete AIP.next; try { for (const x of [1]) ; } finally { restore(); } });
           t(() => { const log = []; const it = [1, 2, 3][Symbol.iterator](); AIP.return = function () { log.push('ret'); return {}; }; try { for (const x of it) { log.push(x); break; } } finally { delete AIP.return; } return log; });
           t(() => { const it = [1, 2][Symbol.iterator](); return [it.next(), it.next(), it.next(), it.next()]; });
           t(() => { const next = new Map().keys().next; for (const x of { next, [Symbol.iterator]() { return this; } }) ; });
           t(() => { const next = 'ab'[Symbol.iterator]().next; for (const x of { next, [Symbol.iterator]() { return this; } }) ; });
           t(() => { for (const x of { next: AIP.next, [Symbol.iterator]() { return this; } }) ; });
           t(() => { const it = Object.create([1, 2][Symbol.iterator]()); for (const x of { [Symbol.iterator]: () => it }) ; });
           t(() => { const it = [1, 2, 3].values(); const s = []; for (const x of { [Symbol.iterator]: () => it }) { s.push(x); if (x === 1) it.next(); } return s; });
           t(() => AIP.next.call({}));
           t(() => AIP.next.call(new Set().values()));
         }
         out.map(show).join(' ')",
    );
}

/// Iterated objects whose reads run user code or fail: proxies, accessors, holes, typed arrays
/// including a detached or shrinking buffer.
#[test]
fn observable_reads_of_the_iterated_object_match_the_interpreter() {
    same(
        "const out = [];
         const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
         for (let r = 0; r < 12; r++) {
           t(() => { const log = []; const p = new Proxy([10, 20, 30], { get(o, k, rc) { log.push(String(k)); return Reflect.get(o, k, rc); } }); const s = []; for (const x of p) s.push(x); return [s, log]; });
           t(() => { const log = []; const p = new Proxy({ length: 2, 0: 'a', 1: 'b' }, { get(o, k) { log.push(String(k)); return o[k]; } }); const s = []; for (const x of Array.prototype[Symbol.iterator].call(p)) s.push(x); return [s, log]; });
           t(() => { const o = { get length() { out.push('len'); return 2; }, get 0() { out.push('g0'); return 'x'; }, 1: 'y' }; const s = []; for (const x of Array.prototype.values.call(o)) s.push(x); return s; });
           t(() => { const a = [1, , 3]; Array.prototype[1] = 'proto'; try { const s = []; for (const x of a) s.push(x); return s; } finally { delete Array.prototype[1]; } });
           t(() => { const a = [1, 2, 3]; Object.defineProperty(a, 1, { get() { throw new RangeError('boom'); } }); const s = []; for (const x of a) s.push(x); return s; });
           t(() => { const a = new Float64Array([1.5, 2.5, 3.5]); const s = []; for (const x of a) s.push(x); return s; });
           t(() => { const b = new ArrayBuffer(8); const a = new Uint8Array(b); const s = []; for (const x of a) { s.push(x); if (s.length === 2) structuredClone(b, { transfer: [b] }); } return s; });
           t(() => { const b = new ArrayBuffer(8, { maxByteLength: 16 }); const a = new Uint8Array(b); const s = []; for (const x of a) { s.push(x); if (s.length === 2) b.resize(1); } return s; });
           t(() => { const b = new ArrayBuffer(8); const a = new Uint8Array(b); const it = a[Symbol.iterator](); structuredClone(b, { transfer: [b] }); for (const x of it) ; });
           t(() => { const a = [1, 2, 3]; const s = []; for (const [i, x] of a.entries()) s.push(i + ':' + x); return s; });
           t(() => { for (const x of undefined) ; });
           t(() => { for (const x of 5) ; });
           t(() => { for (const x of { [Symbol.iterator]() { return 1; } }) ; });
           t(() => { for (const x of { [Symbol.iterator]() { return { next() { return 1; } }; } }) ; });
         }
         out.map(show).join(' ')",
    );
}

/// `break`, `return`, `throw` and `continue` out of a loop close the iterator exactly when the
/// spec says, and a `return` method that throws or answers a non-object is reported.
#[test]
fn closing_iterators_matches_the_interpreter() {
    same(
        "const out = [];
         const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name + ':' + e.message); } };
         const closable = (n, ret) => { const log = []; const it = { i: 0, next() { return this.i < n ? { value: this.i++, done: false } : { value: undefined, done: true }; }, return: ret ? function () { log.push('return'); return ret(); } : undefined, [Symbol.iterator]() { return this; } }; return [it, log]; };
         const AIP = Object.getPrototypeOf([][Symbol.iterator]());
         for (let r = 0; r < 12; r++) {
           t(() => { const [it, log] = closable(5, () => ({})); for (const x of it) { if (x === 2) break; } return log; });
           t(() => { const [it, log] = closable(5, () => ({})); (() => { for (const x of it) { if (x === 2) return; } })(); return log; });
           t(() => { const [it, log] = closable(5, () => ({})); try { for (const x of it) { if (x === 2) throw new Error('inner'); } } catch (e) { log.push(e.message); } return log; });
           t(() => { const [it, log] = closable(5, () => ({})); outer: for (let i = 0; i < 2; i++) for (const x of it) { if (x === 1) continue outer; } return log; });
           t(() => { const [it, log] = closable(3, () => ({})); for (const x of it) ; return log; });
           t(() => { const [it] = closable(5, () => { throw new RangeError('ret'); }); for (const x of it) { if (x === 1) break; } });
           t(() => { const [it] = closable(5, () => 1); for (const x of it) { if (x === 1) break; } });
           t(() => { const [it] = closable(5, () => { throw new RangeError('ret'); }); for (const x of it) { if (x === 1) throw new TypeError('body'); } });
           t(() => { const log = []; const m = new Map([[1, 2], [3, 4]]); AIP.return = undefined; for (const e of m) break; return log; });
           t(() => { const log = []; function* g() { try { yield 1; yield 2; } finally { log.push('fin'); } } for (const x of g()) break; return log; });
           t(() => { const log = []; function* g() { try { yield 1; yield 2; } finally { log.push('fin'); } } const f = () => { for (const x of g()) return x; }; return [f(), log]; });
           t(() => { const log = []; function* inner() { try { yield 1; yield 2; } finally { log.push('inner'); } } function* outer() { yield* inner(); } for (const x of outer()) break; return log; });
           t(() => { const [a, b] = new Set([1, 2, 3]); return [a, b]; });
           t(() => { const log = []; const it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { log.push('r'); return {}; } }; } }; const [a, b] = it; return [a, b, log]; });
         }
         out.map(show).join(' ')",
    );
}

/// Spread, destructuring and the builtins that take an iterable step the same iterators.
#[test]
fn spread_and_destructuring_match_the_interpreter() {
    same(
        "const out = [];
         const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name + ':' + e.message); } };
         function* gen() { yield 1; yield 2; yield 3; }
         for (let r = 0; r < 12; r++) {
           t(() => [...[1, 2, 3], ...'héy', ...new Set([4, 4, 5]), ...new Map([[6, 7]]), ...gen(), ...[8, 9].entries()]);
           t(() => { const [a, , b = 5, ...rest] = [1, 2, undefined, 4, 5]; return [a, b, rest]; });
           t(() => { const [a, b] = 'xyz'; const [[c, d]] = new Map([[1, 2]]); const [e, f, g] = new Set([1]); return [a, b, c, d, e, f, g]; });
           t(() => { let a, b; [a, b] = [b, a] = [1, 2]; return [a, b]; });
           t(() => Math.max(...new Set([3, 9, 2])));
           t(() => Array.from(new Map([[1, 2]]).keys()).concat(Array.from('ab'), Array.from(new Set([7]))));
           t(() => [new Map([[1, 2], [3, 4]]).size, new Set('hello').size, new WeakSet([{}]) instanceof WeakSet, Object.fromEntries(new Map([['a', 1]]))]);
           t(() => { const f = (...args) => args.length; return f(...gen(), ...[1, 2]); });
           t(() => { const [a] = { [Symbol.iterator]() { return { next() { throw new SyntaxError('step'); } }; } }; });
           t(() => { const [a] = {}; });
           t(() => [...{ [Symbol.iterator]: null }]);
         }
         out.map(show).join(' ')",
    );
}

/// A generator driving a `for…of` and one driven by `yield*` over built-in iterators; the
/// outer generator is resumed, thrown into and returned from while the inner one is stepped.
#[test]
fn generators_over_builtin_iterators_match_the_interpreter() {
    same(
        "const out = [];
         const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name + ':' + e.message); } };
         function* over(src) { for (const x of src) { const got = yield x; if (got) out.push('got' + got); } return 'done'; }
         function* delegate(src) { const r = yield* src; yield 'after' + r; }
         for (let r = 0; r < 12; r++) {
           t(() => [...over([1, 2, 3])]);
           t(() => [...over(new Set('abc'))]);
           t(() => { const g = over(new Map([[1, 2], [3, 4]])); return [g.next(), g.next('a'), g.return('x'), g.next()]; });
           t(() => { const g = over([1, 2, 3]); g.next(); try { g.throw(new RangeError('t')); } catch (e) { return e.name; } });
           t(() => [...delegate([1, 2])]);
           t(() => [...delegate('hi')]);
           t(() => [...delegate(new Set([1]).values())]);
           t(() => { const g = delegate([1, 2, 3]); return [g.next(), g.return(9), g.next()]; });
           t(() => { const g = delegate([1, 2, 3]); g.next(); try { g.throw(new Error('d')); } catch (e) { return e.message; } });
           t(() => { const it = [1, 2, 3][Symbol.iterator](); function* d() { yield* it; } const g = d(); return [g.next(), it.next(), g.next(), g.next(), g.next()]; });
         }
         out.map(show).join(' ')",
    );
}
