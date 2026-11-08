(async () => {
  const say = (...args) => console.log(...args);
  const fail = (e) => (e && e.name ? e.name + ': ' + e.message : String(e));
  say('page: globals', typeof require, typeof module, typeof process, typeof Buffer, typeof api, typeof contextBridge, typeof window.fromPreload, typeof window.fromPage);
  const descriptor = Object.getOwnPropertyDescriptor(window, 'api');
  say('page: api descriptor', Object.keys(descriptor).sort().join(), descriptor.writable, descriptor.enumerable, descriptor.configurable, typeof descriptor.get);
  say('page: api keys', Object.keys(api).join());
  say('page: api proto', Object.getPrototypeOf(api) === Object.prototype, Object.isFrozen(api), Object.getOwnPropertyDescriptor(api, 'echo').writable, Object.getOwnPropertyDescriptor(api, 'echo').configurable);
  window.fromPage = 1;

  // Preload to page: a property exposed at load, and the value a call returns.
  for (let i = 0; i < api.labels.length; i++) {
    const label = api.labels[i];
    const present = 'direct' + i in window;
    const quiet = (v) => (v instanceof Promise ? (v.catch(() => {}), v) : v);
    say('direct', label, '=>', present ? describe(quiet(window['direct' + i])) : 'absent');
    try { say('returned', label, '=>', describe(quiet(api.get(label)))); } catch (e) { say('returned', label, 'throws', fail(e)); }
  }
  say('getter calls', api.get('getter calls'));

  // Page to preload: what the preload received.
  for (const [label, make] of cases) {
    try { say('received', label, '=>', api.take(label, make())); } catch (e) { say('received', label, 'throws', fail(e)); }
  }

  // Promises, both ways.
  say('promise resolved', describe(await api.get('resolved promise')));
  try { await api.get('rejected promise'); } catch (e) { say('promise rejected', describe(e)); }
  say('promise later', describe(await api.later()));
  say('promise async', describe(await api.async({ v: [1] })));
  try { await api.rejecter(); } catch (e) { say('promise rejecter', describe(e)); }
  say('promise sent', await api.takeLater(Promise.resolve({ p: 1 })));
  try { await api.takeLater(Promise.reject(new Error('page reject'))); } catch (e) { say('promise sent rejected', describe(e)); }
  say('promise thenable', await api.takeLater({ then(r) { r('thenable value'); } }));

  // Functions: calling, identity, errors, `this`, copies.
  say('call', api.callback((a, b) => a + b, 2, 3));
  say('call object', describe(api.callback((o) => ({ got: o, extra: [1] }), { x: [1, 2] })));
  say('call this', api.callback(function () { return typeof this; }));
  say('callback throws', api.callbackSafe(() => { throw new RangeError('page threw'); }));
  say('callback throws string', api.callbackSafe(() => { throw 'a string'; }));
  say('keep', api.keep(() => 'kept fn'), api.callKept());
  say('identity echo', api.echo(api.original) === api.original, api.original === api.original, api.isOriginal(api.original));
  const mine = () => 1;
  say('identity own', api.echo(mine) === mine, api.isOriginal(mine));
  api.keep(mine);
  say('identity kept', api.sameAsKept(mine));
  say('this', api.thisIs(), api.thisIs.call({ a: 1 }));
  const sent = { a: 1, list: [1] };
  say('mutate object', api.mutate(sent), JSON.stringify(sent));
  const sentArray = [1];
  say('mutate array', api.mutate(sentArray), JSON.stringify(sentArray));
  try { api.thrower(); } catch (e) { say('thrower', describe(e), e instanceof TypeError, e instanceof Error); }
  for (const v of ['text', 5, { a: 1 }, new RangeError('thrown error'), null, undefined]) {
    try { api.throwValue(v); say('throwValue', 'no throw'); } catch (e) { say('throwValue', describe(v), '=>', describe(e)); }
  }
  say('counter', describe(api.counter), api.bump(), describe(api.counter), api.snapshot());
  Array.prototype.push = null;
  say('proto', api.proto(), api.arrayPush());
  say('args', api.args(1, 'a', { b: 2 }), api.count(), api.count(undefined), api.defaults(1), api.defaults(1, undefined), api.defaults(1, null));
  say('nested', api.nested.deep.fn(21), typeof api.nested.deep);
  say('class instance', api.get('class instance') instanceof Object, Object.getPrototypeOf(api.get('class instance')) === Object.prototype);
  say('page proto', Object.getPrototypeOf(api.get('object')) === Object.prototype, Array.isArray(api.get('array')), api.get('array') instanceof Array, api.get('error') instanceof Error, api.get('date') instanceof Date, api.get('function') instanceof Function);
  say('ipc', describe(await api.ipc({ viaMain: [1, 2] })));
  say('page: window', window === globalThis, typeof api.constructor, api.constructor === Object);
  say('page: end');
  api.done();
})();
