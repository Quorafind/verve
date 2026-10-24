import path from '#builtin';
import {value} from '#value';
import feature from '#feature/one';
import external from '#external';
import {createRequire} from 'node:module';
const require = createRequire(import.meta.url);
const codes = [];
for (const name of ['#nested-block', '#missing', 'dual/no-probe', 'dual/private', 'no-such-package']) {
  try { require(name); codes.push('accepted'); } catch (error) { codes.push(error.code); }
}
console.log(JSON.stringify([typeof path.join, value, feature, external, require('#conditional').name, codes]));
