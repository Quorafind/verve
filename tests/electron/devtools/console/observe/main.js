// What Electron 39.8.10 (Windows) does with a page's console, held to by
// crates/verve-electron/tests/devtools.rs. Run it with
//   electron --user-data-dir=<scratch> [--inspect=127.0.0.1:0 ...] tests/electron/devtools/console/observe
// (the window is never shown). Add `--enable-logging` or ELECTRON_ENABLE_LOGGING=1 to see Chromium
// write the console to stderr; the lines it prints are expected/console.txt, expected/console-logging.txt
// and, for the switches, expected/argv.txt, without the `P43 ` that marks them.
const { app, BrowserWindow } = require('electron');
const path = require('path');
const out = [];
const log = (...a) => out.push(a.map(x => typeof x === 'string' ? x : JSON.stringify(x)).join(' '));
const finish = () => {
  for (const line of out) console.log('P43 ' + line);
  app.exit(0);
};
const base = (s) => String(s).split('/').pop();
log('argv', process.argv.slice(1).filter(a => !a.includes('user-data-dir')));
app.whenReady().then(() => {
  const win = new BrowserWindow({ show: false });
  const wc = win.webContents;
  wc.on('console-message', (...args) => {
    const [event, level, message, line, sourceId] = args;
    // Electron's own development-mode security warning, which a page without a policy gets.
    if (String(sourceId).includes('sandbox_bundle')) return;
    log('console-message nargs', args.length, 'level', event.level, 'message', JSON.stringify(event.message),
      'lineNumber', event.lineNumber, 'sourceId', base(event.sourceId), 'scheme', String(event.sourceId).split(':')[0],
      '| positional', level, JSON.stringify(message), line, base(sourceId));
  });
  wc.loadFile(path.join(__dirname, 'page.html'));
  setTimeout(finish, 2500);
});
