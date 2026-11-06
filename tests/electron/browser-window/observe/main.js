'use strict';
// What Electron's BrowserWindow does, as data. Run it with `npx electron .` (Electron 39): it tries
// the constructor's options, the defaults, the destroyed-object errors, `ready-to-show`, the close
// round trip and the family of parent, child and modal windows, then prints one JSON object,
// `OBSERVE_JSON {...}`, and exits by itself. `OBSERVE_OUT=file` also writes it pretty-printed. Every
// window but a few is hidden; the user data folder is a temporary one, never the user's own.
//
// verve-electron's `tests/browser_window.rs` freezes what this printed (39.8.10) and holds Verve to it.
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const util = require('node:util');

app.on('window-all-closed', () => {});
app.setPath('userData', fs.mkdtempSync(path.join(os.tmpdir(), 'bw-observe-')));

const results = {};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const attempt = (fn) => {
  try {
    return { ok: fn() };
  } catch (error) {
    return { threw: `${error && error.name}: ${error && error.message}` };
  }
};
const within = (promise, ms = 3000) => Promise.race([promise, sleep(ms).then(() => 'timeout')]);
const closeAll = async () => {
  for (const win of BrowserWindow.getAllWindows()) win.destroy();
  await sleep(50);
};
const page = (html) => `data:text/html;charset=utf-8,${encodeURIComponent(html)}`;
const loaded = (win, html = '<p>x</p>') =>
  new Promise((resolve) => {
    win.webContents.once('did-finish-load', resolve);
    win.loadURL(page(html));
  });
/** Logs `names` of `emitter` into `log`, with `tag`. */
const watch = (log, tag, emitter, names) => {
  for (const name of names) emitter.on(name, () => log.push(`${tag} ${name}`));
};

async function options() {
  const out = {};
  const size = (opts) => {
    const win = new BrowserWindow({ show: false, ...opts });
    const got = attempt(() => win.getSize());
    win.destroy();
    return got;
  };
  out.width = {
    default: size({}),
    string: size({ width: '300' }),
    abc: size({ width: 'abc' }),
    fraction: size({ width: 300.7 }),
    negative: size({ width: -5 }),
    nullWidth: size({ width: null }),
    zero: size({ width: 0 }),
    xOnly: (() => {
      const win = new BrowserWindow({ show: false, x: 7 });
      const got = win.getPosition();
      win.destroy();
      return got;
    })(),
  };
  out.badTypes = {
    options: attempt(() => new BrowserWindow(5).destroy()),
    optionsString: attempt(() => new BrowserWindow('x').destroy()),
    showString: attempt(() => {
      const win = new BrowserWindow({ show: 'no' });
      const visible = win.isVisible();
      win.destroy();
      return visible;
    }),
    titleNumber: attempt(() => {
      const win = new BrowserWindow({ show: false, title: 5 });
      const title = win.getTitle();
      win.destroy();
      return title;
    }),
    backgroundColor: attempt(() => new BrowserWindow({ show: false, backgroundColor: 'notacolor' }).destroy()),
    backgroundColorNumber: attempt(() => new BrowserWindow({ show: false, backgroundColor: 5 }).destroy()),
    webPreferencesString: attempt(() => new BrowserWindow({ show: false, webPreferences: 'x' }).destroy()),
    modalWithoutParent: attempt(() => {
      const win = new BrowserWindow({ show: false, modal: true });
      const modal = win.isModal();
      win.destroy();
      return modal;
    }),
    parentNotAWindow: attempt(() => new BrowserWindow({ show: false, parent: {} }).destroy()),
    callWithoutNew: attempt(() => BrowserWindow()),
  };
  out.colors = {};
  for (const color of [undefined, '#FFF', '#abc', '#aabbcc', '#80aabbcc', '#8abc', 'rgb(1,2,3)', 'rgba(1,2,3,0.5)', 'hsl(120,100%,50%)', 'hsla(120,100%,50%,0.5)', 'red', 'RED', 'transparent']) {
    out.colors[String(color)] = attempt(() => {
      const win = new BrowserWindow({ show: false, backgroundColor: color });
      const got = win.getBackgroundColor();
      win.setBackgroundColor('#123456');
      const set = win.getBackgroundColor();
      win.destroy();
      return [got, set];
    });
  }
  out.setColors = attempt(() => {
    const win = new BrowserWindow({ show: false });
    const got = {};
    for (const color of ['#fff', 'rgba(0,0,0,0.5)', 'blue', '#12345678', 'bad']) {
      got[color] = attempt(() => {
        win.setBackgroundColor(color);
        return win.getBackgroundColor();
      });
    }
    win.destroy();
    return got;
  });
  await closeAll();
  return out;
}

async function defaults() {
  const out = {};
  const win = new BrowserWindow({ show: false });
  const names = [
    'getSize', 'getContentSize', 'getPosition', 'getBounds', 'getContentBounds', 'getNormalBounds', 'getMinimumSize', 'getMaximumSize',
    'getTitle', 'isVisible', 'isFocused', 'isMinimized', 'isMaximized', 'isNormal', 'isFullScreen', 'isSimpleFullScreen', 'isKiosk',
    'isResizable', 'isMovable', 'isMinimizable', 'isMaximizable', 'isFullScreenable', 'isClosable', 'isFocusable', 'isAlwaysOnTop',
    'isModal', 'isEnabled', 'isTabletMode', 'isSnapped', 'isDocumentEdited', 'getRepresentedFilename', 'hasShadow', 'isContentProtected',
    'isVisibleOnAllWorkspaces', 'isMenuBarAutoHide', 'isMenuBarVisible', 'getOpacity', 'getBackgroundColor', 'getParentWindow',
    'getChildWindows', 'getMediaSourceId', 'isDestroyed', 'getWindowButtonPosition',
  ];
  for (const name of names) out[name] = attempt(() => win[name]());
  out.handle = attempt(() => ({ length: win.getNativeWindowHandle().length, isBuffer: Buffer.isBuffer(win.getNativeWindowHandle()) }));
  out.properties = {};
  for (const name of ['id', 'webContents', 'autoHideMenuBar', 'simpleFullScreen', 'fullScreen', 'focusable', 'visibleOnAllWorkspaces', 'shadow', 'menuBarVisible', 'kiosk', 'documentEdited', 'representedFilename', 'title', 'minimizable', 'maximizable', 'fullScreenable', 'resizable', 'closable', 'movable', 'excludedFromShownWindowsMenu', 'accessibleTitle', 'snapped', 'tabbingIdentifier']) {
    out.properties[name] = attempt(() => {
      const value = win[name];
      return typeof value === 'object' && value !== null ? `[object ${value.constructor.name}]` : value === undefined ? '#undefined' : value;
    });
  }
  out.ids = { window: win.id, contents: win.webContents.id };
  out.preferences = win.webContents.getLastWebPreferences();
  const node = new BrowserWindow({ show: false, webPreferences: { nodeIntegration: true } });
  out.nodePreferences = { sandbox: node.webContents.getLastWebPreferences().sandbox, contextIsolation: node.webContents.getLastWebPreferences().contextIsolation };
  const insecure = new BrowserWindow({ show: false, webPreferences: { webSecurity: false } });
  out.insecure = { webSecurity: insecure.webContents.getLastWebPreferences().webSecurity, allowRunningInsecureContent: insecure.webContents.getLastWebPreferences().allowRunningInsecureContent };
  const second = new BrowserWindow({ show: false });
  out.secondIds = { window: second.id, contents: second.webContents.id };
  second.destroy();
  const third = new BrowserWindow({ show: false });
  out.thirdIds = { window: third.id, contents: third.webContents.id };
  out.statics = {
    all: BrowserWindow.getAllWindows().map((w) => w.id),
    fromId: BrowserWindow.fromId(win.id) === win,
    fromIdMissing: BrowserWindow.fromId(999),
    fromContents: BrowserWindow.fromWebContents(win.webContents) === win,
    fromContentsBad: attempt(() => BrowserWindow.fromWebContents({})),
    fromBrowserView: attempt(() => BrowserWindow.fromBrowserView({})),
    focused: BrowserWindow.getFocusedWindow(),
  };
  await closeAll();
  return out;
}

async function destroyed() {
  const out = {};
  const win = new BrowserWindow({ show: false });
  const contents = win.webContents;
  const id = win.id;
  const log = [];
  watch(log, 'win', win, ['close', 'closed']);
  watch(log, 'contents', contents, ['destroyed']);
  win.destroy();
  await sleep(100);
  out.log = log;
  out.isDestroyed = win.isDestroyed();
  out.contentsDestroyed = contents.isDestroyed();
  out.calls = {
    getTitle: attempt(() => win.getTitle()),
    getSize: attempt(() => win.getSize()),
    show: attempt(() => win.show()),
    setTitle: attempt(() => win.setTitle('x')),
    destroyAgain: attempt(() => win.destroy()),
    close: attempt(() => win.close()),
    id: attempt(() => win.id),
    webContents: attempt(() => win.webContents),
    getParentWindow: attempt(() => win.getParentWindow()),
    loadURL: attempt(() => win.loadURL('about:blank')),
    contentsGetURL: attempt(() => contents.getURL()),
    contentsId: attempt(() => contents.id),
    fromId: BrowserWindow.fromId(id),
    all: BrowserWindow.getAllWindows().length,
    fromDestroyedContents: attempt(() => BrowserWindow.fromWebContents(contents)),
  };
  out.illegal = attempt(() => BrowserWindow.prototype.getTitle.call({}));
  out.illegalAccessor = attempt(() => Object.getOwnPropertyDescriptor(BrowserWindow.prototype, 'title').get.call({}));
  return out;
}

async function subclassing() {
  const out = {};
  class Mine extends BrowserWindow {
    constructor() {
      super({ show: false, title: 'mine' });
    }
  }
  const mine = new Mine();
  out.classExtends = { instance: mine instanceof BrowserWindow, title: mine.getTitle(), id: mine.id, emits: false };
  mine.on('hide', () => (out.classExtends.emits = true));
  mine.show();
  mine.hide();
  await sleep(100);
  function Old() {
    BrowserWindow.call(this, { show: false });
  }
  out.inherits = attempt(() => {
    util.inherits(Old, BrowserWindow);
    return new Old().id;
  });
  await closeAll();
  return out;
}

async function created() {
  const out = {};
  const log = [];
  const onCreated = (event, win) => {
    log.push({ event: typeof event, hasPrevent: typeof event.preventDefault, win: win instanceof BrowserWindow, contents: !!win.webContents });
  };
  app.on('browser-window-created', onCreated);
  const onContents = (event, contents) => log.push({ webContentsCreated: typeof event, id: contents.id });
  app.on('web-contents-created', onContents);
  const win = new BrowserWindow({ show: false });
  app.off('browser-window-created', onCreated);
  app.off('web-contents-created', onContents);
  out.log = log;
  out.windowOnly = win.id;
  await closeAll();
  return out;
}

async function titles() {
  const out = {};
  for (const mode of ['contents-first', 'window-first', 'prevent-on-contents', 'prevent-on-window']) {
    const win = new BrowserWindow({ show: false, title: 'initial' });
    const log = [];
    const cb = (tag, prevent) => (event, title, explicit) => {
      log.push(`${tag} ${title} ${explicit} ${typeof event.preventDefault}`);
      if (prevent) event.preventDefault();
    };
    if (mode === 'window-first') {
      win.on('page-title-updated', cb('win'));
      win.webContents.on('page-title-updated', cb('contents'));
    } else {
      win.webContents.on('page-title-updated', cb('contents', mode === 'prevent-on-contents'));
      win.on('page-title-updated', cb('win', mode === 'prevent-on-window'));
    }
    await loaded(win, '<title>from page</title><p>x</p>');
    await sleep(100);
    out[mode] = { log, title: win.getTitle() };
    win.destroy();
  }
  const win = new BrowserWindow({ show: false, title: 'initial' });
  await loaded(win, '<p>no title</p>');
  out.noTitle = win.getTitle();
  await win.webContents.executeJavaScript("document.title = 'set by script'");
  await sleep(100);
  out.scriptTitle = win.getTitle();
  win.setTitle('set by app');
  out.appTitle = win.getTitle();
  await closeAll();
  return out;
}

async function stateAfterCalls() {
  const out = {};
  const win = new BrowserWindow({ show: false });
  const step = (label, fn, read) => {
    const outcome = attempt(fn);
    out[label] = { call: outcome, now: attempt(read) };
  };
  step('setBounds', () => win.setBounds({ x: 50, y: 60, width: 500, height: 400 }), () => [win.getBounds(), win.getContentBounds(), win.getSize(), win.getContentSize(), win.getPosition()]);
  step('setSize', () => win.setSize(480, 360), () => [win.getSize(), win.getContentSize()]);
  step('setContentSize', () => win.setContentSize(300, 200), () => [win.getSize(), win.getContentSize()]);
  step('setContentBounds', () => win.setContentBounds({ x: 10, y: 20, width: 310, height: 210 }), () => [win.getBounds(), win.getContentBounds()]);
  step('setBoundsPartial', () => win.setBounds({ width: 450 }), () => win.getBounds());
  step('setBoundsFraction', () => win.setBounds({ x: 10.6, y: 20.4, width: 449.5, height: 300.2 }), () => win.getBounds());
  step('setMinimumSize', () => win.setMinimumSize(600, 500), () => [win.getSize(), win.getMinimumSize()]);
  step('setMaximumSize', () => win.setMaximumSize(700, 550), () => [win.getSize(), win.getMaximumSize()]);
  step('setMinimumSizeResetMax', () => { win.setMinimumSize(0, 0); win.setMaximumSize(0, 0); }, () => [win.getMinimumSize(), win.getMaximumSize()]);
  step('setPosition', () => win.setPosition(111, 222), () => win.getPosition());
  step('center', () => win.center(), () => [win.getPosition(), win.getSize()]);
  step('maximize', () => win.maximize(), () => [win.isMaximized(), win.isNormal(), win.getBounds(), win.getNormalBounds()]);
  step('unmaximize', () => win.unmaximize(), () => [win.isMaximized(), win.isNormal(), win.getBounds()]);
  step('minimize', () => win.minimize(), () => [win.isMinimized(), win.isNormal(), win.isVisible(), win.isFocused()]);
  step('restore', () => win.restore(), () => [win.isMinimized(), win.isVisible()]);
  step('show', () => win.show(), () => [win.isVisible(), win.isFocused()]);
  step('hide', () => win.hide(), () => [win.isVisible(), win.isFocused()]);
  step('showInactive', () => win.showInactive(), () => [win.isVisible(), win.isFocused()]);
  step('fullscreenOn', () => win.setFullScreen(true), () => [win.isFullScreen(), win.fullScreen]);
  step('fullscreenOff', () => win.setFullScreen(false), () => win.isFullScreen());
  step('opacity', () => win.setOpacity(0.5), () => win.getOpacity());
  step('opacityHigh', () => win.setOpacity(2), () => win.getOpacity());
  step('opacityString', () => win.setOpacity('a'), () => win.getOpacity());
  step('title', () => win.setTitle('T'), () => win.getTitle());
  step('titleNumber', () => win.setTitle(5), () => win.getTitle());
  step('resizable', () => win.setResizable(false), () => [win.isResizable(), win.resizable]);
  step('resizableProperty', () => { win.resizable = true; }, () => win.isResizable());
  step('movable', () => win.setMovable(false), () => win.isMovable());
  step('minimizable', () => win.setMinimizable(false), () => win.isMinimizable());
  step('maximizable', () => win.setMaximizable(false), () => win.isMaximizable());
  step('fullScreenable', () => win.setFullScreenable(false), () => win.isFullScreenable());
  step('closable', () => win.setClosable(false), () => win.isClosable());
  step('focusable', () => win.setFocusable(false), () => win.isFocusable());
  step('alwaysOnTop', () => win.setAlwaysOnTop(true), () => win.isAlwaysOnTop());
  step('alwaysOnTopLevel', () => win.setAlwaysOnTop(true, 'floating'), () => win.isAlwaysOnTop());
  step('kiosk', () => win.setKiosk(true), () => [win.isKiosk(), win.isFullScreen()]);
  step('kioskOff', () => win.setKiosk(false), () => [win.isKiosk(), win.isFullScreen()]);
  step('shadow', () => win.setHasShadow(false), () => win.hasShadow());
  step('contentProtection', () => win.setContentProtection(true), () => win.isContentProtected());
  step('menuBarVisibility', () => win.setMenuBarVisibility(false), () => win.isMenuBarVisible());
  step('autoHideMenuBar', () => win.setAutoHideMenuBar(true), () => win.isMenuBarAutoHide());
  step('documentEdited', () => win.setDocumentEdited(true), () => win.isDocumentEdited());
  step('represented', () => win.setRepresentedFilename('/x'), () => win.getRepresentedFilename());
  step('progress', () => win.setProgressBar(0.5), () => 'none');
  step('progressMode', () => win.setProgressBar(0.5, { mode: 'paused' }), () => 'none');
  step('flash', () => win.flashFrame(true), () => 'none');
  step('ignoreMouse', () => win.setIgnoreMouseEvents(true, { forward: true }), () => 'none');
  step('aspect', () => win.setAspectRatio(2), () => 'none');
  step('thumbar', () => win.setThumbarButtons([]), () => 'none');
  step('menu', () => win.setMenu(null), () => 'none');
  step('removeMenu', () => win.removeMenu(), () => 'none');
  step('setIconBad', () => win.setIcon('nonexistent.png'), () => 'none');
  step('setBoundsBad', () => win.setBounds('x'), () => 'none');
  step('setBoundsNoArg', () => win.setBounds(), () => 'none');
  step('setSizeBad', () => win.setSize('a', 'b'), () => 'none');
  step('setSizeOne', () => win.setSize(300), () => 'none');
  step('setPositionBad', () => win.setPosition('a', 1), () => 'none');
  step('setResizableString', () => win.setResizable('yes'), () => win.isResizable());
  step('moveTop', () => win.moveTop(), () => 'none');
  step('capturePage', () => typeof win.capturePage().then, () => 'none');
  step('loadURLNothing', () => win.loadURL(), () => 'none');
  await closeAll();
  return out;
}


async function geometry() {
  const out = {};
  const cases = {
    position: { x: 10, y: 20 },
    centerWithPosition: { center: true, x: 5, y: 5 },
    contentSize: { useContentSize: true, width: 300, height: 200 },
    minWidth: { width: 200, height: 100, minWidth: 300, minHeight: 150 },
    maxHeight: { width: 500, height: 500, maxWidth: 400, maxHeight: 100 },
    huge: { width: 100000, height: 100000 },
    smallHeight: { width: 300, height: 0 },
    frameless: { frame: false, width: 300, height: 200 },
    framelessContent: { frame: false, width: 300, height: 200, useContentSize: true },
    fullscreen: { fullscreen: true },
    kiosk: { kiosk: true },
    opacity: { opacity: 0.5 },
    alwaysOnTop: { alwaysOnTop: true },
    notResizable: { resizable: false },
    notClosable: { closable: false },
    notFocusable: { focusable: false },
    accessible: { webPreferences: { accessibleTitle: 'acc' } },
    transparent: { transparent: true, backgroundColor: '#80FF0000' },
    stringPosition: { x: '10', y: '20' },
    fractionPosition: { x: 10.5, y: 20.5 },
  };
  for (const [name, opts] of Object.entries(cases)) {
    out[name] = attempt(() => {
      const win = new BrowserWindow({ show: false, ...opts });
      const got = {
        bounds: win.getBounds(), content: win.getContentSize(), min: win.getMinimumSize(), max: win.getMaximumSize(),
        fullScreen: win.isFullScreen(), kiosk: win.isKiosk(), opacity: win.getOpacity(), onTop: win.isAlwaysOnTop(),
        resizable: win.isResizable(), closable: win.isClosable(), focusable: win.isFocusable(), visible: win.isVisible(),
        color: win.getBackgroundColor(), title: win.getTitle(),
      };
      win.destroy();
      return got;
    });
  }
  await closeAll();
  return out;
}

async function readyToShow() {
  const out = {};
  const cases = {
    hidden: { show: false },
    shown: { show: true, width: 200, height: 150 },
    hiddenNoPaint: { show: false, paintWhenInitiallyHidden: false },
  };
  for (const [name, opts] of Object.entries(cases)) {
    const win = new BrowserWindow(opts);
    const log = [];
    watch(log, 'win', win, ['ready-to-show', 'show', 'focus']);
    watch(log, 'contents', win.webContents, ['dom-ready', 'did-finish-load', 'did-stop-loading']);
    await loaded(win);
    await sleep(300);
    out[name] = { log: log.slice(), visibleState: await win.webContents.executeJavaScript('document.visibilityState') };
    if (!opts.show) {
      win.show();
      await sleep(300);
      out[name].afterShow = log.slice();
      out[name].visibleStateAfterShow = await win.webContents.executeJavaScript('document.visibilityState');
    }
    win.destroy();
  }
  const never = new BrowserWindow({ show: false });
  const log = [];
  never.on('ready-to-show', () => log.push('ready-to-show'));
  await sleep(300);
  out.neverLoaded = log;
  await closeAll();
  return out;
}

/** A window with a `beforeunload` and `unload` handler that tell the main process through the console. */
async function closer(opts = {}, { block = false } = {}) {
  const win = new BrowserWindow({ show: false, ...opts });
  const log = [];
  win.webContents.on('console-message', (event) => log.push(`page ${event.message}`));
  watch(log, 'win', win, ['close', 'closed', 'hide', 'blur']);
  watch(log, 'contents', win.webContents, ['destroyed']);
  await within(loaded(win));
  await within(win.webContents.executeJavaScript(
    `addEventListener('beforeunload', (e) => { console.log('beforeunload'); ${block ? "e.preventDefault(); e.returnValue = 'stay';" : ''} });
     addEventListener('pagehide', () => console.log('pagehide'));
     addEventListener('unload', () => console.log('unload'));
     addEventListener('visibilitychange', () => console.log('visibilitychange ' + document.visibilityState));
     0`,
    true,
  ));
  return { win, log };
}

async function closing() {
  const out = {};
  {
    const { win, log } = await closer();
    win.close();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'plain');
    out.plain = { log, destroyed: win.isDestroyed() };
  }
  {
    const { win, log } = await closer();
    win.on('close', (event) => event.preventDefault());
    win.close();
    await sleep(300);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'prevented');
    out.prevented = { log, destroyed: win.isDestroyed() };
    win.destroy();
  }
  {
    const { win, log } = await closer();
    win.destroy();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'destroy');
    out.destroy = { log, destroyed: win.isDestroyed() };
  }
  {
    const { win, log } = await closer({}, { block: true });
    win.close();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'beforeunloadBlocksNoListener');
    out.beforeunloadBlocksNoListener = { log, destroyed: win.isDestroyed() };
    win.destroy();
  }
  {
    const { win, log } = await closer({}, { block: true });
    win.webContents.on('will-prevent-unload', (event) => log.push(`contents will-prevent-unload ${typeof event.preventDefault}`));
    win.close();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'willPreventUnloadKept');
    out.willPreventUnloadKept = { log, destroyed: win.isDestroyed() };
    win.destroy();
  }
  {
    const { win, log } = await closer({}, { block: true });
    win.webContents.on('will-prevent-unload', (event) => {
      log.push('contents will-prevent-unload');
      event.preventDefault();
    });
    win.close();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'willPreventUnloadIgnored');
    out.willPreventUnloadIgnored = { log, destroyed: win.isDestroyed() };
  }
  {
    const { win, log } = await closer({}, { block: true });
    win.webContents.on('will-prevent-unload', () => log.push('contents will-prevent-unload'));
    win.on('close', () => log.push('win close listener'));
    win.webContents.close({ waitForBeforeUnload: true });
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'contentsCloseWaiting');
    out.contentsCloseWaiting = { log, destroyed: win.isDestroyed() };
    win.destroy();
  }
  {
    const { win, log } = await closer();
    win.webContents.close();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'contentsClose');
    out.contentsClose = { log, destroyed: win.isDestroyed() };
  }
  {
    const { win, log } = await closer();
    win.close();
    win.close();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'closeTwice');
    out.closeTwice = { log, destroyed: win.isDestroyed() };
  }
  {
    const { win, log } = await closer({ show: true, width: 200, height: 150 });
    await sleep(200);
    win.close();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'shownClose');
    out.shownClose = { log };
  }
  await closeAll();
  return out;
}

async function family() {
  const out = {};
  {
    const log = [];
    const parent = new BrowserWindow({ show: false, title: 'parent' });
    const a = new BrowserWindow({ show: false, parent, title: 'a' });
    const b = new BrowserWindow({ show: false, parent, title: 'b' });
    watch(log, 'parent', parent, ['close', 'closed']);
    watch(log, 'a', a, ['close', 'closed']);
    watch(log, 'b', b, ['close', 'closed']);
    out.children = parent.getChildWindows().map((w) => w.getTitle());
    out.parentOfA = a.getParentWindow() === parent;
    out.allOrder = BrowserWindow.getAllWindows().map((w) => w.getTitle());
    b.setParentWindow(null);
    out.afterDetach = { children: parent.getChildWindows().map((w) => w.getTitle()), parentOfB: b.getParentWindow() };
    b.setParentWindow(parent);
    out.reattached = parent.getChildWindows().map((w) => w.getTitle());
    parent.close();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'parentClose');
    out.parentClose = { log, aDestroyed: a.isDestroyed(), bDestroyed: b.isDestroyed(), parentDestroyed: parent.isDestroyed() };
  }
  {
    const log = [];
    const parent = new BrowserWindow({ show: false, title: 'parent' });
    const child = new BrowserWindow({ show: false, parent, title: 'child' });
    child.on('close', (event) => {
      log.push('child close (prevented)');
      event.preventDefault();
    });
    watch(log, 'parent', parent, ['close', 'closed']);
    watch(log, 'child', child, ['closed']);
    parent.close();
    await sleep(500);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'childPrevents');
    out.childPrevents = { log, parentDestroyed: parent.isDestroyed(), childDestroyed: child.isDestroyed() };
    if (!child.isDestroyed()) child.destroy();
    if (!parent.isDestroyed()) parent.destroy();
  }
  {
    const log = [];
    const parent = new BrowserWindow({ show: false, title: 'parent' });
    const child = new BrowserWindow({ show: false, parent, title: 'child' });
    watch(log, 'parent', parent, ['close', 'closed']);
    watch(log, 'child', child, ['close', 'closed']);
    parent.destroy();
    await sleep(300);
    if (process.env.OBSERVE_TRACE) console.error('closing', 'parentDestroy');
    out.parentDestroy = { log, childDestroyed: child.isDestroyed() };
    if (!child.isDestroyed()) child.destroy();
  }
  {
    const parent = new BrowserWindow({ show: false, title: 'parent' });
    const state = { enabledBefore: parent.isEnabled() };
    const modal = new BrowserWindow({ show: false, parent, modal: true, title: 'modal' });
    state.isModal = modal.isModal();
    state.enabledWhileModalHidden = parent.isEnabled();
    modal.show();
    state.enabledWhileModalShown = parent.isEnabled();
    state.modalEnabled = modal.isEnabled();
    modal.hide();
    state.enabledAfterHide = parent.isEnabled();
    modal.show();
    modal.close();
    await sleep(300);
    state.enabledAfterModalClosed = parent.isEnabled();
    parent.setEnabled(false);
    state.setEnabledFalse = parent.isEnabled();
    parent.setEnabled(true);
    out.modal = state;
    await closeAll();
  }
  {
    // What `setParentWindow` refuses (a window as its own parent is left out: Electron has no check
    // for it), what a modal window does about it, and `getMediaSourceId`.
    const a = new BrowserWindow({ show: false, title: 'a' });
    const p1 = new BrowserWindow({ show: false, title: 'p1' });
    const p2 = new BrowserWindow({ show: false, title: 'p2' });
    const modal = new BrowserWindow({ show: false, parent: p1, modal: true, title: 'modal' });
    modal.show();
    out.parentErrors = {
      notAWindow: attempt(() => a.setParentWindow({})),
      aString: attempt(() => a.setParentWindow('x')),
      nothing: attempt(() => a.setParentWindow()),
      aNumber: attempt(() => a.setParentWindow(5)),
      modalChangesParent: attempt(() => modal.setParentWindow(p2)),
      modalDropsParent: attempt(() => modal.setParentWindow(null)),
      modalParent: modal.getParentWindow() && modal.getParentWindow().getTitle(),
      enabled: [p1.isEnabled(), p2.isEnabled()],
    };
    out.mediaSourceId = a.getMediaSourceId();
    await closeAll();
  }
  return out;
}

async function focusEvents() {
  const out = {};
  const log = [];
  const win = new BrowserWindow({ show: false, width: 200, height: 150, title: 'f' });
  const appFocus = (event, who) => log.push(`app browser-window-focus ${typeof event.preventDefault} ${who === win}`);
  const appBlur = (event, who) => log.push(`app browser-window-blur ${typeof event.preventDefault} ${who === win}`);
  app.on('browser-window-focus', appFocus);
  app.on('browser-window-blur', appBlur);
  watch(log, 'win', win, ['show', 'focus', 'blur', 'hide']);
  win.show();
  await sleep(300);
  out.focusedAfterShow = { focused: win.isFocused(), fromStatic: BrowserWindow.getFocusedWindow() === win };
  win.hide();
  await sleep(300);
  out.focusedAfterHide = { focused: win.isFocused(), fromStatic: BrowserWindow.getFocusedWindow() };
  win.show();
  await sleep(200);
  win.blur();
  await sleep(300);
  out.blurred = { focused: win.isFocused() };
  app.off('browser-window-focus', appFocus);
  app.off('browser-window-blur', appBlur);
  out.log = log;
  await closeAll();
  return out;
}

async function events() {
  const out = {};
  const win = new BrowserWindow({ show: false });
  const log = [];
  watch(log, 'win', win, ['resize', 'resized', 'move', 'moved', 'will-resize', 'will-move', 'maximize', 'unmaximize', 'always-on-top-changed', 'enter-full-screen', 'leave-full-screen', 'show', 'hide']);
  win.setBounds({ x: 50, y: 60, width: 500, height: 400 });
  await sleep(200);
  out.setBounds = log.splice(0);
  win.setSize(450, 350);
  await sleep(200);
  out.setSize = log.splice(0);
  win.setPosition(70, 80);
  await sleep(200);
  out.setPosition = log.splice(0);
  win.setAlwaysOnTop(true);
  await sleep(200);
  out.alwaysOnTop = log.splice(0);
  win.setAlwaysOnTop(true);
  await sleep(200);
  out.alwaysOnTopAgain = log.splice(0);
  win.setAlwaysOnTop(false);
  await sleep(200);
  out.alwaysOnTopOff = log.splice(0);
  win.on('always-on-top-changed', (event, flag) => log.push(`args ${typeof event.preventDefault} ${flag}`));
  win.setAlwaysOnTop(true);
  await sleep(100);
  out.alwaysOnTopArgs = log.splice(0);
  win.setMinimumSize(600, 500);
  await sleep(200);
  out.minimumSize = { log: log.splice(0), size: win.getSize() };
  await closeAll();
  return out;
}

app.whenReady().then(async () => {
  const guard = setTimeout(() => app.exit(9), 90000);
  const scenarios = { options, defaults, destroyed, subclassing, created, titles, geometry, stateAfterCalls, readyToShow, closing, family, focusEvents, events };
  for (const [name, run] of Object.entries(scenarios)) {
    if (process.env.OBSERVE_ONLY && !process.env.OBSERVE_ONLY.split(',').includes(name)) continue;
    if (process.env.OBSERVE_TRACE) console.error('scenario', name);
    try {
      results[name] = await run();
    } catch (error) {
      results[name] = { failed: String(error && error.stack) };
    }
    await closeAll();
  }
  clearTimeout(guard);
  const json = JSON.stringify({ electron: process.versions.electron, platform: process.platform, results }, (key, value) => (value === undefined ? '#undefined' : value));
  console.log(`OBSERVE_JSON ${json}`);
  if (process.env.OBSERVE_OUT) fs.writeFileSync(process.env.OBSERVE_OUT, JSON.stringify(JSON.parse(json), null, 2));
  app.exit(0);
});
