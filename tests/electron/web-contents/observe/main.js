'use strict';
// What Electron's webContents does, as data: loading and its events, history, will-navigate,
// executeJavaScript, insertCSS, window.open and the statics. Run it with `npx electron .`
// (Electron 39): it prints one JSON object, `OBSERVE_JSON {...}`, and exits by itself.
// `OBSERVE_OUT=file` also writes it pretty-printed; `OBSERVE_ONLY=a,b` runs only some scenes.
// Every window is hidden and the user data folder is a temporary one.
//
// verve-electron's `tests/web_contents.rs` freezes what this printed (39.8.10) and holds Verve to it.
const { app, BrowserWindow, webContents } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

app.on('window-all-closed', () => {});
app.setPath('userData', fs.mkdtempSync(path.join(os.tmpdir(), 'wc-observe-')));

const results = {};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const page = (html) => `data:text/html;charset=utf-8,${encodeURIComponent(html)}`;
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'wc-pages-'));
const write = (name, html) => {
  const file = path.join(dir, name);
  fs.writeFileSync(file, html);
  return file;
};
const tidy = (text) => text.split(dir.replace(/\\/g, '/')).join('<dir>');
const brief = (value) => {
  if (value && typeof value === 'object') {
    const name = value.constructor && value.constructor.name;
    if (name === 'WebFrameMain') return '<frame>';
    if (name === 'BrowserWindow') return '<window>';
    return JSON.parse(tidy(JSON.stringify(value, (k, v) => (k === 'frame' || k === 'initiator' ? '<frame>' : v))));
  }
  return typeof value === 'string' ? tidy(value) : value;
};
const NAMES = [
  'did-start-loading', 'did-start-navigation', 'will-navigate', 'will-frame-navigate',
  'did-navigate', 'did-frame-navigate', 'did-navigate-in-page', 'page-title-updated',
  'dom-ready', 'did-frame-finish-load', 'did-finish-load', 'did-fail-load',
  'did-fail-provisional-load', 'did-stop-loading', 'frame-created', 'did-create-window',
  'will-redirect', 'did-redirect-navigation',
];
const watch = (win, log, names = NAMES) => {
  for (const name of names) {
    win.webContents.on(name, (...args) => {
      const [first, ...rest] = args;
      const shown = first && first.defaultPrevented !== undefined ? rest : args;
      log.push([name, shown.map(brief)]);
    });
  }
};
const hidden = (opts = {}) => new BrowserWindow({ show: false, ...opts });
const settle = async (promise) => {
  try {
    return { resolved: brief(await promise) };
  } catch (error) {
    const { message, errno, code, url } = error;
    return { rejected: { name: error.name, message: tidy(String(message)), errno, code, url } };
  }
};

async function loading() {
  const out = {};
  let win = hidden();
  let log = [];
  watch(win, log);
  out.dataResult = await settle(win.loadURL(page('<title>T1</title><p>one</p>')));
  await sleep(100);
  out.dataEvents = [...log];
  out.dataState = {
    url: win.webContents.getURL().slice(0, 40), title: win.webContents.getTitle(),
    loading: win.webContents.isLoading(), canGoBack: win.webContents.canGoBack(),
    len: win.webContents.navigationHistory.length(), active: win.webContents.navigationHistory.getActiveIndex(),
  };
  const file = write('a.html', '<title>File A</title><a id="l" href="b.html">go</a><p>a</p>');
  write('b.html', '<title>File B</title><p>b</p>');
  log.length = 0;
  out.fileResult = await settle(win.loadFile(file));
  await sleep(100);
  out.fileEvents = [...log];
  out.fileState = {
    title: win.webContents.getTitle(), len: win.webContents.navigationHistory.length(),
    active: win.webContents.navigationHistory.getActiveIndex(),
    entries: win.webContents.navigationHistory.getAllEntries().map((e) => ({ title: e.title, tail: e.url.split('/').pop() })),
  };
  log.length = 0;
  await win.webContents.executeJavaScript("location.hash = '#frag'");
  await sleep(100);
  out.hashEvents = [...log];
  out.hashState = { len: win.webContents.navigationHistory.length(), tail: win.webContents.getURL().split('/').pop() };
  log.length = 0;
  out.missing = await settle(win.loadURL('file:///definitely/not/here.html'));
  await sleep(100);
  out.missingEvents = [...log];
  out.afterMissing = { url: win.webContents.getURL(), len: win.webContents.navigationHistory.length() };
  log.length = 0;
  out.badUrl = await settle(win.loadURL('not a url'));
  out.noSuch = await settle(win.loadURL('http://127.0.0.1:1/'));
  out.badEvents = [...log];
  win.destroy();
  win = hidden();
  log = [];
  const first = win.loadURL(page('<p>first</p>'));
  const second = win.loadURL(page('<p>second</p>'));
  out.interrupted = { first: await settle(first), second: await settle(second) };
  win.destroy();
  return out;
}

async function history() {
  const out = {};
  const win = hidden();
  const log = [];
  await win.loadURL(page('<title>1</title>one'));
  await win.loadURL(page('<title>2</title>two'));
  await win.loadURL(page('<title>3</title>three'));
  const nav = win.webContents.navigationHistory;
  out.start = [nav.length(), nav.getActiveIndex(), nav.canGoBack(), nav.canGoForward()];
  watch(win, log, ['did-navigate', 'did-start-navigation', 'did-finish-load', 'did-navigate-in-page']);
  nav.goBack();
  await sleep(300);
  out.back = [nav.length(), nav.getActiveIndex(), nav.canGoBack(), nav.canGoForward(), win.webContents.getTitle()];
  out.backEvents = log.map(([n]) => n);
  nav.goToIndex(0);
  await sleep(300);
  out.zero = [nav.getActiveIndex(), win.webContents.getTitle()];
  out.canGoToOffset = [nav.canGoToOffset(2), nav.canGoToOffset(3), nav.canGoToOffset(-1)];
  nav.goForward();
  await sleep(300);
  out.forward = [nav.getActiveIndex(), win.webContents.getTitle()];
  await win.loadURL(page('<title>4</title>four'));
  out.branch = [nav.length(), nav.getActiveIndex(), nav.canGoForward()];
  log.length = 0;
  win.webContents.reload();
  await sleep(300);
  out.reload = [nav.length(), nav.getActiveIndex(), log.map(([n]) => n)];
  out.entryTitle = nav.getEntryAtIndex(1).title;
  out.entryKeys = Object.keys(nav.getEntryAtIndex(1)).sort();
  nav.removeEntryAtIndex(0);
  out.removed = [nav.length(), nav.getActiveIndex()];
  nav.clear();
  out.cleared = [nav.length(), nav.getActiveIndex()];
  out.outOfRange = (() => { try { return nav.getEntryAtIndex(99); } catch (e) { return `${e.name}: ${e.message}`; } })();
  win.destroy();
  return out;
}

async function navigating() {
  const out = {};
  const win = hidden();
  await win.loadFile(write('n.html', '<a id="l" href="b.html">go</a>'));
  write('b.html', '<title>B</title>b');
  const log = [];
  watch(win, log, ['will-navigate', 'will-frame-navigate', 'did-start-navigation', 'did-navigate', 'did-navigate-in-page']);
  win.webContents.on('will-navigate', (event, url) => { out.details = Object.keys(event).sort(); out.second = url.split('/').pop(); event.preventDefault(); });
  await win.webContents.executeJavaScript("document.getElementById('l').click()");
  await sleep(300);
  out.prevented = { log: log.map(([n, a]) => [n, JSON.stringify(a).slice(0, 300)]), tail: win.webContents.getURL().split('/').pop() };
  win.webContents.removeAllListeners('will-navigate');
  log.length = 0;
  await win.webContents.executeJavaScript("location.href = 'b.html'");
  await sleep(300);
  out.allowed = { log: log.map(([n]) => n), tail: win.webContents.getURL().split('/').pop() };
  log.length = 0;
  await win.loadFile(path.join(dir, 'n.html'));
  out.loadURL = log.map(([n]) => n);
  win.destroy();
  return out;
}

async function scripts() {
  const out = {};
  const win = hidden();
  await win.loadURL(page('<p id="p">hi</p>'));
  const wc = win.webContents;
  out.value = await settle(wc.executeJavaScript('1 + 2'));
  out.object = await settle(wc.executeJavaScript('({ a: [1, 2], b: "x" })'));
  out.promise = await settle(wc.executeJavaScript('new Promise((r) => setTimeout(() => r("later"), 20))'));
  out.undefinedResult = await settle(wc.executeJavaScript('undefined'));
  out.rejectedPromise = await settle(wc.executeJavaScript('Promise.reject(new Error("no"))'));
  out.thrown = await settle(wc.executeJavaScript('throw new Error("boom")'));
  out.thrownString = await settle(wc.executeJavaScript('throw "str"'));
  out.syntax = await settle(wc.executeJavaScript('var ='));
  out.fn = await settle(wc.executeJavaScript('(function(){})'));
  out.notString = await settle(Promise.resolve().then(() => wc.executeJavaScript(5)));
  out.element = await settle(wc.executeJavaScript('document.getElementById("p")'));
  out.isolated = await settle(wc.executeJavaScriptInIsolatedWorld(7, [{ code: 'window.mark = 1; typeof document' }]));
  out.mainSees = await settle(wc.executeJavaScript('typeof window.mark'));
  const key = await wc.insertCSS('#p { color: red }');
  out.cssKey = typeof key;
  out.red = await wc.executeJavaScript('getComputedStyle(document.getElementById("p")).color');
  await wc.removeInsertedCSS(key);
  out.removed = await wc.executeJavaScript('getComputedStyle(document.getElementById("p")).color');
  out.userCss = typeof (await wc.insertCSS('#p { color: blue }', { cssOrigin: 'user' }));
  win.destroy();
  return out;
}

async function opening() {
  const out = {};
  const win = hidden();
  await win.loadURL(page('<p>o</p>'));
  const wc = win.webContents;
  const created = [];
  app.on('browser-window-created', () => created.push('created'));
  out.noHandler = await settle(wc.executeJavaScript("window.open('about:blank#a', 'named', 'width=300,height=200') === null"));
  await sleep(200);
  out.afterOpen = { windows: BrowserWindow.getAllWindows().length, created };
  for (const extra of BrowserWindow.getAllWindows()) if (extra !== win) extra.destroy();
  const seen = [];
  wc.setWindowOpenHandler((details) => { seen.push(brief(details)); return { action: 'deny' }; });
  out.deny = await settle(wc.executeJavaScript("window.open('about:blank#d', 'nm', 'width=111,popup=1') === null"));
  out.denyDetails = seen.map((d) => ({ ...d }));
  seen.length = 0;
  wc.setWindowOpenHandler((details) => { seen.push(details.disposition); return { action: 'allow', overrideBrowserWindowOptions: { width: 321, show: false } }; });
  wc.once('did-create-window', (w, details) => {
    out.createWindowDetails = { keys: Object.keys(details).sort(), url: details.url, frameName: details.frameName, disposition: details.disposition };
    out.createdIsWindow = w.constructor.name;
  });
  await wc.executeJavaScript("window.open('about:blank#x', 'nm2', 'width=150,height=250,left=10,top=20'); 0");
  await sleep(200);
  const child = BrowserWindow.getAllWindows().find((w) => w !== win);
  out.child = child && { size: child.getSize(), type: child.webContents.getType(), hasOpener: !!child.webContents.opener, noParent: child.getParentWindow() === null };
  out.dispositions = [...seen];
  seen.length = 0;
  await wc.executeJavaScript("document.body.innerHTML = '<a id=\"b\" target=\"_blank\" href=\"about:blank#link\">x</a>'; document.getElementById('b').click()");
  await sleep(200);
  out.linkDisposition = [...seen];
  wc.setWindowOpenHandler(() => 'nonsense');
  out.bad = await settle(wc.executeJavaScript("window.open('about:blank#y') === null"));
  for (const w of BrowserWindow.getAllWindows()) w.destroy();
  return out;
}

async function statics() {
  const out = {};
  const win = hidden();
  await win.loadURL(page('<p>s</p>'));
  const wc = win.webContents;
  out.type = wc.getType();
  out.ids = [typeof wc.id, webContents.fromId(wc.id) === wc, webContents.fromId(9999)];
  out.all = webContents.getAllWebContents().length;
  out.focused = webContents.getFocusedWebContents() === null;
  out.frame = { url: wc.mainFrame.url.slice(0, 20), name: wc.mainFrame.name, top: wc.mainFrame.top === wc.mainFrame, parent: wc.mainFrame.parent, frames: wc.mainFrame.frames.length, processIdType: typeof wc.mainFrame.processId, routing: wc.mainFrame.routingId, visibility: wc.mainFrame.visibilityState, detached: wc.mainFrame.detached };
  out.frameFromWc = webContents.fromFrame(wc.mainFrame) === wc;
  out.ua = wc.getUserAgent().includes('Electron/');
  wc.setUserAgent('custom/1');
  out.setUa = wc.getUserAgent();
  out.zoom = [wc.getZoomFactor(), wc.getZoomLevel()];
  wc.setZoomFactor(2);
  out.zoomed = [wc.getZoomFactor(), wc.getZoomLevel()];
  out.crashed = wc.isCrashed();
  out.loadingFlags = [wc.isLoading(), wc.isLoadingMainFrame(), wc.isWaitingForResponse()];
  out.audio = [wc.isAudioMuted(), wc.isCurrentlyAudible()];
  out.osPid = wc.getOSProcessId() > 0;
  out.hostWebContents = wc.hostWebContents;
  const gone = [];
  wc.on('render-process-gone', (e, details) => gone.push(details));
  wc.forcefullyCrashRenderer();
  await sleep(500);
  out.gone = gone;
  out.afterCrash = wc.isCrashed();
  win.destroy();
  out.destroyedGetUrl = (() => { try { return wc.getURL(); } catch (e) { return `${e.name}: ${e.message}`; } })();
  out.destroyedIs = wc.isDestroyed();
  return out;
}

async function main() {
  await app.whenReady();
  for (const [name, fn] of Object.entries({ loading, history, navigating, scripts, opening, statics })) {
    if (process.env.OBSERVE_ONLY && !process.env.OBSERVE_ONLY.split(',').includes(name)) continue;
    try {
      results[name] = await fn();
    } catch (error) {
      results[name] = { error: String(error && error.stack) };
    }
  }
  console.log(`OBSERVE_JSON ${JSON.stringify(results)}`);
  if (process.env.OBSERVE_OUT) fs.writeFileSync(process.env.OBSERVE_OUT, JSON.stringify(results, null, 2));
  app.exit(0);
}
main();
