'use strict';
// Extract only the vendored, integrity-pinned fixtures; no npm lifecycle script can run.
// Usage: node prepare.cjs <scratch-directory>. The runtime test uses RUNTIME_X_NETWORK_NPM.
const fs = require('fs');
const path = require('path');
const cp = require('child_process');
const target = process.argv[2];
if (!target || !path.isAbsolute(target) || path.resolve(target) === path.resolve(__dirname)) {
  throw new Error('Pass an absolute scratch directory outside the fixture source');
}
fs.mkdirSync(target, { recursive: true });
for (const name of ['package.json', 'package-lock.json', 'npm-smoke.cjs']) {
  fs.copyFileSync(path.join(__dirname, name), path.join(target, name));
}
fs.cpSync(path.join(__dirname, 'vendor'), path.join(target, 'vendor'), { recursive: true });
const args = ['ci', '--offline', '--ignore-scripts', '--no-audit', '--no-fund', '--global=false', '--cache', path.join(target, 'cache')];
if (process.platform === 'win32') {
  cp.execFileSync(process.execPath,
    [path.join(path.dirname(process.execPath), 'node_modules', 'npm', 'bin', 'npm-cli.js'), ...args],
    { cwd: target, stdio: 'inherit' });
} else {
  cp.execFileSync('npm', args, { cwd: target, stdio: 'inherit' });
}
console.log('RUNTIME_X_NETWORK_NPM=' + target);
