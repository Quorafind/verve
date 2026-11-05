// Freezes what real Electron does with a fixture app into `apps/<name>/expected.json`.
//
//   node tests/electron/record.cjs [--electron <electron binary>] [--timeout <ms>] [app...]
//
// The Electron binary comes from `--electron`, `$ELECTRON_BIN`, or `require('electron')`; install
// it outside the repository (`npm install electron@39` in a scratch folder) and pass the path of
// `node_modules/electron/dist/electron(.exe)`. The same file is also the recorder: Electron loads
// it with `-r` in the app's main process, where it notes the order of app and window events and
// every console line, and writes them when the process exits.
//
// Every app is run with its user data in a folder of its own, and is killed by its PID if it has
// not ended within the timeout: a fixture is meant to end by itself.
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const util = require('node:util');
const { spawn, spawnSync } = require('node:child_process');

const APPS = path.join(__dirname, 'apps');
const RECORDED = ['hello', 'ipc', 'window-state'];

if (process.versions.electron) record();
else drive();

/** The recorder: runs inside the app's main process. */
function record() {
  const { app } = require('electron');
  const note = { electron: process.versions.electron, events: [], console: [], exitCode: null };
  const appDir = process.argv.find((arg) => path.dirname(arg) === APPS);
  // What a line says of where the app lives or which stack threw is the machine's, not Electron's.
  const plain = (text) =>
    text
      .split(/\r?\n/)
      .filter((line) => !/^\s+at /.test(line))
      .join('\n')
      .split(appDir)
      .join('<app>');
  app.setPath('userData', process.env.VERVE_RECORD_DATA);

  for (const level of ['log', 'info', 'warn', 'error']) {
    const write = console[level].bind(console);
    console[level] = (...args) => {
      note.console.push({ from: 'main', text: plain(util.format(...args)) });
      write(...args);
    };
  }
  const watch = (emitter, label, names) => {
    for (const name of names) emitter.on(name, () => note.events.push(`${label} ${name}`));
  };
  watch(app, 'app', ['will-finish-launching', 'ready', 'window-all-closed', 'before-quit', 'will-quit', 'quit']);
  app.on('browser-window-created', (event, win) => {
    const id = win.id;
    watch(win, `window ${id}`, ['ready-to-show', 'show', 'hide', 'close', 'closed']);
    const contents = win.webContents;
    watch(contents, `contents ${contents.id}`, ['did-start-loading', 'dom-ready', 'did-finish-load', 'did-stop-loading']);
    contents.on('console-message', (...args) => {
      // Electron 39 passes one event with `message`; older ones passed (event, level, message).
      const text = typeof args[2] === 'string' ? args[2] : args[0].message;
      note.console.push({ from: 'renderer', text: plain(text) });
    });
  });
  process.on('exit', (code) => {
    note.events.push('process exit');
    note.exitCode = code;
    fs.writeFileSync(process.env.VERVE_RECORD_OUT, JSON.stringify(note));
  });
}

/** The driver: runs each app under Electron and writes what the recorder saw. */
function drive() {
  const args = process.argv.slice(2);
  const option = (name) => {
    const at = args.indexOf(name);
    return at < 0 ? undefined : args.splice(at, 2)[1];
  };
  const electron = option('--electron') || process.env.ELECTRON_BIN || require('electron');
  const timeout = Number(option('--timeout') || 20000);
  const apps = args.length ? args : RECORDED;
  (async () => {
    for (const name of apps) await run(electron, name, timeout);
  })();
}

async function run(electron, name, timeout) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), `verve-record-${name}-`));
  const out = path.join(scratch, 'out.json');
  const env = {
    ...process.env,
    VERVE_RECORD_OUT: out,
    VERVE_RECORD_DATA: path.join(scratch, 'data'),
    APPDATA: path.join(scratch, 'roaming'),
    LOCALAPPDATA: path.join(scratch, 'local'),
    XDG_CONFIG_HOME: path.join(scratch, 'config'),
    XDG_CACHE_HOME: path.join(scratch, 'cache'),
  };
  const child = spawn(electron, ['-r', __filename, path.join(APPS, name)], { env, stdio: 'ignore' });
  const killer = setTimeout(() => kill(child.pid), timeout);
  await new Promise((resolve) => child.on('exit', resolve));
  clearTimeout(killer);
  if (!fs.existsSync(out)) throw new Error(`${name}: Electron did not end by itself within ${timeout} ms`);
  const note = JSON.parse(fs.readFileSync(out, 'utf8'));
  fs.writeFileSync(path.join(APPS, name, 'expected.json'), JSON.stringify(note, null, 2) + '\n');
  fs.rmSync(scratch, { recursive: true, force: true });
  console.log(`${name}: ${note.events.length} events, ${note.console.length} console lines, exit ${note.exitCode}`);
}

/** Ends the process `pid` and what it started, and nothing else. */
function kill(pid) {
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(pid), '/T', '/F']);
  else process.kill(pid, 'SIGKILL');
}
