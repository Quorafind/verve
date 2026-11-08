// What Electron 39.8.10 (Windows) does when a window made with `devTools: false` is asked to open
// DevTools: nothing, and no event; expected/devtools-off.txt. The window is never shown and, with
// DevTools off, none can open.
const { app, BrowserWindow } = require('electron');
const out = [];
const log = (...a) => out.push(a.map(x => typeof x === 'string' ? x : JSON.stringify(x)).join(' '));
app.whenReady().then(async () => {
  // devTools: false makes openDevTools a no-op, so no DevTools window can open.
  const win = new BrowserWindow({ show: false, webPreferences: { devTools: false } });
  const wc = win.webContents;
  const events = [];
  for (const n of ['devtools-opened', 'devtools-closed', 'devtools-focused']) wc.on(n, () => events.push(n));
  log('initial', wc.isDevToolsOpened(), wc.isDevToolsFocused(), wc.devToolsWebContents);
  log('open returns', wc.openDevTools());
  log('close returns', wc.closeDevTools());
  log('toggle returns', wc.toggleDevTools());
  log('inspectElement returns', wc.inspectElement(1, 2));
  await new Promise(r => setTimeout(r, 500));
  log('after', wc.isDevToolsOpened(), wc.isDevToolsFocused(), wc.devToolsWebContents, events);
  const plain = new BrowserWindow({ show: false });
  log('default window', plain.webContents.isDevToolsOpened(), plain.webContents.isDevToolsFocused(), plain.webContents.devToolsWebContents);
  log('closeDevTools when closed', plain.webContents.closeDevTools());
  for (const l of out) console.log('P43 ' + l);
  app.exit(0);
});
