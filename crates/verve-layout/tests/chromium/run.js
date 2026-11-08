// Starts Electron on this folder (main.js) with a throwaway profile and kills it by PID if it takes
// longer than 90 seconds. ELECTRON_EXE is the path of an unpacked Electron 39 `electron.exe`.
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const exe = process.env.ELECTRON_EXE;
if (!exe) {
  console.error('set ELECTRON_EXE to the Electron executable');
  process.exit(2);
}
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'verve-layout-probe-'));
const child = spawn(exe, [__dirname, '--user-data-dir=' + profile], { stdio: 'inherit' });
const timer = setTimeout(() => {
  console.error('timeout');
  child.kill();
}, 90000);
child.on('exit', (code) => {
  clearTimeout(timer);
  fs.rmSync(profile, { recursive: true, force: true });
  console.log('exit', code);
});
