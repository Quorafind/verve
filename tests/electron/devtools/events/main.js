// The order in which Electron 39.8.10 (Windows) says the DevTools events, held to by
// crates/verve-electron/tests/inspector.rs. Run it with
//   P43_MODE=<right|detach> electron --user-data-dir=<scratch> tests/electron/devtools/events
// The window is never shown. `right` docks the DevTools inside the hidden window, so nothing appears
// on the screen; `detach` opens their own window, which is the only way to see them take the focus,
// and the script closes it before it ends. Each line starts with `P43 `; expected/events-*.txt hold them.
const { app, BrowserWindow } = require('electron');
const out = [];
let t0 = 0;
const log = (...a) => out.push(a.map((x) => (typeof x === 'string' ? x : JSON.stringify(x))).join(' '));
const mode = process.env.P43_MODE || 'right';
app.whenReady().then(() => {
  const win = new BrowserWindow({ show: false });
  const wc = win.webContents;
  const state = (tag) => log(tag, 'opened', wc.isDevToolsOpened(), 'focused', wc.isDevToolsFocused(), 'devToolsWebContents', wc.devToolsWebContents === null ? 'null' : 'object');
  for (const name of ['devtools-opened', 'devtools-closed', 'devtools-focused', 'devtools-reload-page', 'devtools-search-query', 'devtools-open-url']) {
    wc.on(name, () => state('event ' + name));
  }
  wc.loadURL('data:text/html,<title>t</title>hello').then(() => {
    state('loaded');
    wc.openDevTools({ mode });
    state('after openDevTools() returned');
    setTimeout(() => {
      state('after 3s');
      wc.closeDevTools();
      state('after closeDevTools() returned');
      setTimeout(() => {
        state('after close + 1s');
        wc.toggleDevTools();
        state('after toggleDevTools() returned');
        setTimeout(() => {
          state('after toggle + 3s');
          wc.toggleDevTools();
          state('after second toggle returned');
          setTimeout(() => {
            for (const line of out) console.log('P43 ' + line);
            app.exit(0);
          }, 1000);
        }, 3000);
      }, 1000);
    }, 3000);
  });
});
