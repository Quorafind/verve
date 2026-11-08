// spawn, exec and fork from the main process. The program run is the app's own executable in
// Node mode (ELECTRON_RUN_AS_NODE), which is what `process.execPath` is for an Electron app.
const { app } = require('electron');
const { execFile, fork, spawn } = require('node:child_process');
const path = require('node:path');

const asNode = { ...process.env, ELECTRON_RUN_AS_NODE: '1' };

function collect(child) {
  return new Promise((resolve) => {
    let out = '';
    child.stdout.on('data', (chunk) => (out += chunk));
    child.on('close', (code) => resolve(`${code} ${out.trim()}`));
  });
}

app.whenReady().then(async () => {
  const spawned = spawn(process.execPath, ['-e', "console.log('spawned')"], { env: asNode });
  console.log('spawn', await collect(spawned));
  const exec = await new Promise((resolve) =>
    execFile(process.execPath, ['-e', 'process.stdout.write(String(1 + 1))'], { env: asNode }, (error, stdout) =>
      resolve(`${error ? error.code : 0} ${stdout}`),
    ),
  );
  console.log('execFile', exec);
  const forked = fork(path.join(__dirname, 'child.js'), ['ping'], { env: asNode });
  forked.on('message', (message) => {
    console.log('fork', JSON.stringify(message));
    forked.disconnect();
  });
  forked.on('exit', (code) => {
    console.log('fork exit', code);
    app.quit();
  });
  setTimeout(() => app.quit(), 8000);
});
