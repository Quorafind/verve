import {createRequire} from 'node:module';
const require = createRequire(import.meta.url);
const importDual = (await import('dual')).default;
const requireDual = require('dual');
const self = (await import('runtime-x-esm-fixture')).value;
const mapped = (await import('#value')).value;
const named = await import('./named.cjs');
const cjs = [named.name, named.default.name, named.default.default, named.__esModule];
const sync = require('./value.js');
const liveBefore = sync.count;
sync.increment();
const requireEsm = [sync.default, sync.__esModule, liveBefore, sync.count, require('./value.js') === sync, require('./require-value.mjs').answer];
const errors = [];
for (const name of ['./value', './features', 'dual/private', 'dual/invalid', 'dual/no-probe', '#unmapped', './data.json', 'https://example.invalid/a.mjs']) {
  try { await import(name); errors.push('accepted'); } catch (error) { errors.push(error.code); }
}
let asyncRequire;
try { require('./tla-graph.mjs'); } catch (error) { asyncRequire = error.code; }
const json = (await import('./data.json', {with: {type: 'json'}})).default;
const jsonIdentity = json === require('./data.json');
console.log(JSON.stringify({importDual, requireDual, self, mapped, cjs, requireEsm, errors, asyncRequire, jsonIdentity}));
