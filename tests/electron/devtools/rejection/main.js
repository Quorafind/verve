// What Electron 39.8.10 (Windows) does with a promise the main process rejected and nobody handled,
// held to by crates/verve-electron/tests/inspector.rs and tests/embedder_hooks.rs. Run it with
//   P43_VARIANT=<variant> electron --user-data-dir=<scratch> tests/electron/devtools/rejection
// The error box is replaced before anything is rejected, so no box is ever shown: the replacement
// writes what the box would have said. Variants:
//   error    reject with an Error, no listeners
//   string   reject with a string, no listeners
//   listener reject with an Error while the app has an 'uncaughtException' listener
//   handled  reject with an Error while the app has an 'unhandledRejection' listener
// Each line this prints starts with `P43 `; expected/rejection-*.txt hold them.
const { app, dialog } = require('electron');
const out = [];
const log = (...a) => out.push(a.map((x) => (typeof x === 'string' ? x : JSON.stringify(x))).join(' '));
const first = (text) => String(text).split('\n').slice(0, 3).join(' | ');
dialog.showErrorBox = (title, content) => log('showErrorBox title', JSON.stringify(title), 'content', JSON.stringify(first(content)));
const variant = process.env.P43_VARIANT || 'error';
log('variant', variant, 'listenerCount at start', process.listenerCount('uncaughtException'));
if (variant === 'listener') {
  process.on('uncaughtException', (error, origin) => {
    log('uncaughtException', error && error.message, 'origin', origin, 'listenerCount', process.listenerCount('uncaughtException'));
  });
}
if (variant === 'handled') {
  process.on('unhandledRejection', (reason, promise) => {
    log('unhandledRejection', String(reason && reason.message), 'promise', Object.prototype.toString.call(promise));
  });
}
app.whenReady().then(() => {
  if (variant === 'string') Promise.reject('plain reason');
  else Promise.reject(new Error('main reject'));
  setTimeout(() => {
    for (const line of out) console.log('P43 ' + line);
    app.exit(0);
  }, 1500);
});
