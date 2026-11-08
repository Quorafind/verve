// Drives the app through one lifecycle and writes what it hears, in order, to the file named by
// --log. The scenario is --scenario=<name>; the names are read by crates/verve-electron/tests/app.rs.
const { app, BrowserWindow } = require('electron');
const fs = require('fs');

const scenario = app.commandLine.getSwitchValue('scenario');
const log = (...parts) => fs.appendFileSync(app.commandLine.getSwitchValue('log'), parts.join(' ') + '\n');
const wants = (word) => scenario.includes(word);

for (const name of ['will-finish-launching', 'ready', 'window-all-closed', 'before-quit', 'will-quit', 'quit',
                    'browser-window-created', 'web-contents-created']) {
  if (name === 'window-all-closed' && wants('nolistener')) continue;
  app.on(name, (event, ...rest) => {
    log('app', name, name === 'quit' ? rest[0] : '');
    if (name === 'before-quit' && wants('prevent-before')) event.preventDefault();
    if (name === 'will-quit' && wants('prevent-will')) event.preventDefault();
  });
}
process.on('exit', (code) => log('process exit', code));

let win;
function makeWindow() {
  win = new BrowserWindow({ show: false });
  win.on('close', (event) => {
    log('win close');
    if (wants('prevent-close')) event.preventDefault();
  });
  win.on('closed', () => log('win closed'));
}

log('entry isReady=' + app.isReady());
app.whenReady().then(() => {
  log('whenReady');
  if (!wants('nowin')) makeWindow();
  setTimeout(act, 300);
});

function act() {
  log('--- act');
  if (scenario.startsWith('close')) {
    win.close();
    setTimeout(() => { log('--- quit()'); app.quit(); log('after quit()'); }, 300);
  } else if (scenario.startsWith('quit')) {
    app.quit();
    log('after quit()');
    if (wants('twice')) { app.quit(); log('after quit()'); }
  } else if (scenario.startsWith('exit')) {
    app.exit(3);
    log('after exit()');
  } else if (scenario.startsWith('pexit')) {
    process.exit(4);
  } else if (scenario.startsWith('code')) {
    process.exitCode = 6;
    app.quit();
  }
  // A quit that was stopped leaves the app running; the test ends it from here.
  if (wants('prevent')) setTimeout(() => { log('--- app.exit(9)'); app.exit(9); }, 800);
}
