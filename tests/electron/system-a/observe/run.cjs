// Runs one probe of this folder in real Electron with a scratch profile:
//
//   node tests/electron/system-a/observe/run.cjs <electron binary> <probe.cjs> <scratch dir>
//
// Electron is installed outside the repository (`npm install electron@39` in a scratch folder).
// A probe never opens a dialog, a notification or a browser and creates no window; the child is
// killed by its PID if it has not ended within 60 seconds.
'use strict';
const { spawn } = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');

const [electron, probe, scratch] = process.argv.slice(2);
fs.mkdirSync(scratch, { recursive: true });
const profile = path.join(scratch, 'profile');
const child = spawn(electron, [path.resolve(probe), `--user-data-dir=${profile}`], {
  env: { ...process.env, VERVE_PROBE_SCRATCH: scratch },
  stdio: ['ignore', 'inherit', 'inherit'],
});
const timer = setTimeout(() => child.kill(), 60000);
child.on('exit', (code) => {
  clearTimeout(timer);
  process.exitCode = code;
});
