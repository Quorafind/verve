//! Tests of the calls between compiled functions: how a call's arguments, `this` and result
//! travel through the VM stack, and what happens when a call fails, recurses too deep or goes
//! through a generator, an async function or a native function.

use super::tests::{same, same_after_jobs};

/// Arguments given, missing and extra, `arguments`, `this` in sloppy and strict functions,
/// rest parameters, defaults, constructors, getters and callbacks of natives.
#[test]
fn arguments_and_this_match_the_interpreter() {
    same(
        "function sum(a, b, c) { return (a === undefined ? 100 : a) + (b === undefined ? 10 : b) + (c === undefined ? 1 : c); }
         function count() { return arguments.length + ':' + [].slice.call(arguments).join(','); }
         function rest(a, ...r) { return a + '/' + r.length; }
         function dflt(a, b = a + 1, c = b * 2) { return [a, b, c]; }
         function sloppyThis() { return typeof this + ':' + (this === globalThis); }
         function strictThis() { 'use strict'; return typeof this + ':' + String(this); }
         function mapped(a) { arguments[0] = 9; return a; }
         function obj(o) { return o.x + o.y; }
         class P { constructor(x) { this.x = x; } get double() { return this.x * 2; } add(o) { return new P(this.x + o.x); } }
         const out = [];
         for (let i = 0; i < 30; i++) {
           out.push(sum(), sum(1), sum(1, 2), sum(1, 2, 3), sum(1, 2, 3, 4), sum(undefined, null, 'x'));
           out.push(count(), count(i), count(i, 'a', {}), rest(i), rest(i, 1, 2, 3), dflt(i), dflt(i, 5), dflt(i, undefined, 7));
           out.push(sloppyThis(), strictThis(), sloppyThis.call(5), strictThis.call(5), sloppyThis.call(null), strictThis.call('s'));
           out.push(mapped(i), obj({ x: i, y: 1 }), new P(i).add(new P(1)).double);
           out.push([1, 2, 3].map((v) => sum(v, i)).join(), [3, 1, 2].sort((a, b) => sum(a) - sum(b)).join());
         }
         out.map(show).join(' ')",
    );
}

/// Exceptions thrown in a callee reach each caller, through `finally` and `catch`, with the
/// same `stack`, whatever the depth; the recursion limit gives the same error and leaves the
/// engine usable.
#[test]
fn failing_calls_match_the_interpreter() {
    same(
        "function leaf(n) { if (n % 5 === 4) throw new TypeError('leaf ' + n); return n; }
         function a(n) { try { return leaf(n) + 1; } finally { log.push('a' + n); } }
         function b(n) { try { return a(n) * 2; } catch (e) { log.push(e.message); return -1; } }
         function c(n) { return b(n) + a(n + 1); }
         function overflow(n) { return overflow(n + 1) + 1; }
         function deep(n) { return n === 0 ? 0 : deep(n - 1) + 1; }
         function undef() { return missing + 1; }
         function notFunction() { const v = 3; return v(1); }
         const log = [], out = [];
         for (let i = 0; i < 40; i++) { try { out.push(c(i)); } catch (e) { out.push(e.name + ':' + e.message); } }
         for (const f of [undef, notFunction, overflow]) {
           for (let k = 0; k < 3; k++) { try { f(0); } catch (e) { out.push(e.name + ':' + e.message.slice(0, 30)); } }
         }
         out.push(deep(500), deep(5000));
         try { deep(1e6); } catch (e) { out.push(e.name); }
         out.push(deep(100));
         try { a(4); } catch (e) { out.push(e.stack.split('\n').length > 2); }
         out.concat(log).map(show).join(' ')",
    );
}

/// Generators, async functions and natives in the middle of a chain of calls: frames that are
/// suspended, resumed and entered from `Array.prototype.map` and promise jobs.
#[test]
fn calls_through_generators_async_and_natives_match_the_interpreter() {
    same_after_jobs(
        "function work(n) { return n * 3 + 1; }
         function* gen(n) { for (let i = 0; i < n; i++) { const r = yield work(i); if (r) i += r; } return work(n); }
         async function wait(n) { const a = await work(n); const b = await Promise.resolve(work(a)); return a + b; }
         function viaMap(n) { return [1, 2, 3].map((x) => [x, x + 1].map((y) => work(y + n)).join('+')).join('|'); }
         function viaReduce(n) { return [1, 2, 3, 4].reduce((acc, x) => acc + work(x + n), 0); }
         const log = [], it = gen(20);
         let step = it.next();
         for (let i = 0; i < 30 && !step.done; i++) step = it.next(i % 3 === 0 ? 1 : 0);
         log.push(step.value);
         for (let i = 0; i < 20; i++) { log.push(viaMap(i), viaReduce(i)); wait(i).then((v) => log.push('w' + v)); }
         log.push([...gen(5)].join());
         globalThis.result = log;
         0",
        "result.map(show).join(' ')",
    );
}

/// Constructors of every shape: base classes with and without a function environment (arrow
/// functions, `eval`, `new.target`, `super.x` in the body), derived classes with and without
/// `super()`, returns of objects and primitives, a different `newTarget`, a changed or
/// non-object `prototype`, bound functions and proxies, and an exception from a constructor.
#[test]
fn constructors_match_the_interpreter() {
    same(
        "class A { constructor(x) { this.x = x; this.nt = new.target === A; } }
         class Arrow { constructor() { this.f = () => this; this.g = () => new.target; } }
         class Ev { constructor() { this.ok = eval('this') === this; } }
         class Sup { constructor() { this.ok = super.toString === Object.prototype.toString; } }
         class Cap { constructor(a) { this.get = () => a; } }
         class Empty {}
         class R1 { constructor() { return { z: 1 }; } }
         class R2 { constructor() { return 5; } }
         class D1 extends A {}
         class D2 extends A { constructor() { super(4); } }
         class D3 extends A { constructor() { } }
         class D4 extends A { constructor() { return {}; } }
         class D5 extends A { constructor() { return 1; } }
         class D6 extends A { constructor() { super(1); super(2); } }
         class D7 extends A { constructor() { const f = () => super(9); f(); this.k = this.x; } }
         class Boom { constructor() { throw new Error('boom'); } }
         function F(x) { this.x = x; }
         function G() { this.v = new.target === G; }
         const bound = A.bind(null, 6), prox = new Proxy(A, {});
         const out = [];
         for (let i = 0; i < 40; i++) {
           const a = new A(i), ar = new Arrow();
           out.push(a.x, a.nt, ar.f() === ar, ar.g() === Arrow, new Ev().ok, new Sup().ok, new Cap(i).get(), new Empty() instanceof Empty);
           out.push(new R1().z, new R2() instanceof R2, new D1(i).x, new D2().x, new D4() instanceof D4, new D7().k, new F(i).x, new G().v);
           out.push(new bound().x, new prox(i).x, Reflect.construct(A, [i], D2).nt, Object.getPrototypeOf(Reflect.construct(F, [1], D1)) === D1.prototype);
           for (const C of [D3, D5, D6, Boom]) { try { new C(); } catch (e) { out.push(e.name + ':' + e.message); } }
           try { A(i); } catch (e) { out.push(e.name); }
           try { new Boom(); } catch (e) { out.push(e.stack.split('\n').length > 1); }
           if (i === 20) { F.prototype = { tag: 1 }; out.push(Object.getPrototypeOf(new F(1)).tag); F.prototype = 3; out.push(Object.getPrototypeOf(new F(1)) === Object.prototype); F.prototype = { tag: 2 }; }
         }
         out.map(show).join(' ')",
    );
}
