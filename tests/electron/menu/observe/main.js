// What Electron 39.8.10 (Windows) answers for Menu, MenuItem, globalShortcut and Tray, held to by
// crates/verve-electron/tests/menu.rs. Run it with `electron --user-data-dir=<scratch> observe`
// (it opens only a window that is never shown, registers no shortcut that parses and makes no
// tray icon); the lines it prints are expected/probe.txt, without the `P15 ` that marks them.
const { app, BrowserWindow, Menu, MenuItem, Tray, globalShortcut } = require('electron');
const fs = require('fs');
const path = require('path');
const out = [];
const log = (...a) => out.push(a.map(x => typeof x === 'string' ? x : JSON.stringify(x)).join(' '));
const err = (f) => { try { const r = f(); return 'ok ' + JSON.stringify(r); } catch (e) { return `${e.constructor.name}: ${e.message}` + (e.code ? ` [${e.code}]` : ''); } };
const finish = () => {
  for (const line of out) console.log('P15 ' + line);
  try { fs.writeFileSync(path.join(__dirname, 'out.txt'), out.join('\n')); } catch {}
  app.exit(0);
};
setTimeout(finish, 15000);
log('before-ready register', err(() => globalShortcut.register('A', () => {})));
log('before-ready isRegistered', err(() => globalShortcut.isRegistered('A')));
log('before-ready register invalid', err(() => globalShortcut.register('NotAKey', () => {})));
log('before-ready unregister', err(() => globalShortcut.unregister('A')));
log('before-ready unregisterAll', err(() => globalShortcut.unregisterAll()));
log('before-ready registerAll', err(() => globalShortcut.registerAll([], () => {})));
log('before-ready tray', err(() => new Tray()));
log('before-ready tray 123', err(() => new Tray(123)));
log('before-ready buildFromTemplate', err(() => Menu.buildFromTemplate([{ label: 'x' }]).items.length));
log('before-ready getApplicationMenu', err(() => Menu.getApplicationMenu() && Menu.getApplicationMenu().items.length));
const dump = (i) => ({ label: i.label, accelerator: i.accelerator, type: i.type, role: i.role, reg: i.registerAccelerator, enabled: i.enabled, visible: i.visible, checked: i.checked, whenHidden: i.acceleratorWorksWhenHidden, sub: i.submenu ? i.submenu.items.map(s => s.role || s.label || s.type) : undefined });
app.whenReady().then(async () => {
  try { await body(); } catch (e) { log('BODY THREW', String(e && e.stack)); }
  finish();
});
async function body() {
  const am = Menu.getApplicationMenu();
  log('default app menu', am && am.items.map(dump));
  log('default help sub', am && am.items[am.items.length - 1].submenu.items.map(i => i.label));
  log('popup no window', err(() => new Menu().popup()));
  log('register invalid', err(() => globalShortcut.register('NotAKey', () => {})));
  log('register invalid2', err(() => globalShortcut.register('Ctrl+', () => {})));
  log('register nofn', err(() => globalShortcut.register('Ctrl+Alt+Shift+F24')));
  log('registerAll invalid', err(() => globalShortcut.registerAll(['NotAKey'], () => {})));
  log('registerAll empty', err(() => globalShortcut.registerAll([], () => {})));
  log('isRegistered', err(() => globalShortcut.isRegistered('Ctrl+Alt+Shift+F24')));
  log('isRegistered bogus', err(() => globalShortcut.isRegistered('NotAKey')));
  log('unregister bogus', err(() => globalShortcut.unregister('NotAKey')));
  log('unregister none', err(() => globalShortcut.unregister('Ctrl+Alt+Shift+F24')));
  log('unregisterAll', err(() => globalShortcut.unregisterAll()));
  log('tray bad', err(() => new Tray()));
  log('tray bad 123', err(() => new Tray(123)));
  log('tray {}', err(() => new Tray({})));
  const roles = ['undo','redo','cut','copy','paste','pasteAndMatchStyle','delete','selectAll','reload','forceReload','toggleDevTools','resetZoom','zoomIn','zoomOut','togglefullscreen','window','minimize','close','zoom','front','quit','about','hide','hideOthers','unhide','services','editMenu','viewMenu','windowMenu','fileMenu','appMenu','help','recentDocuments','clearRecentDocuments','shareMenu','startSpeaking','stopSpeaking','toggleSpellChecker','showSubstitutions','toggleSmartQuotes','toggleSmartDashes','toggleTextReplacement','bogus','Copy','COPY'];
  for (const r of roles) log('role', r, err(() => dump(new MenuItem({ role: r }))));
  for (const r of ['editMenu','viewMenu','windowMenu','fileMenu','appMenu','help','shareMenu']) {
    const it = new MenuItem({ role: r });
    log('submenu of', r, it.submenu ? it.submenu.items.map(dump) : null);
  }
  log('keys', Object.keys(new MenuItem({ label: 'a' })));
  log('keys2', Object.keys(new MenuItem({ label: 'a', id: 'x', userData: 1, click() {} , accelerator: 'Ctrl+K', icon: undefined})));
  log('desc', err(() => Object.getOwnPropertyDescriptor(new MenuItem({ label: 'a' }), 'label')));
  log('desc type', err(() => Object.getOwnPropertyDescriptor(new MenuItem({ label: 'a' }), 'type')));
  log('desc commandId', err(() => Object.getOwnPropertyDescriptor(new MenuItem({ label: 'a' }), 'commandId')));
  log('commandIds', [new MenuItem({label:'a'}).commandId, new MenuItem({label:'b'}).commandId]);
  log('proto', Object.getOwnPropertyNames(MenuItem.prototype));
  log('menu proto', Object.getOwnPropertyNames(Menu.prototype).sort());
  log('menu statics', Object.getOwnPropertyNames(Menu).sort());
  log('menu inst', Object.keys(new Menu()));
  log('type bogus', err(() => new MenuItem({ type: 'bogus' })));
  log('type submenu none', err(() => new MenuItem({ type: 'submenu' })));
  log('submenu bad', err(() => new MenuItem({ label: 'x', submenu: 5 })));
  log('submenu template', err(() => new MenuItem({ label: 'x', submenu: [{ label: 'y' }] }).submenu.items.length));
  log('type inferred', new MenuItem({ label: 'x', submenu: [] }).type);
  log('role lowercased', new MenuItem({ role: 'CoPy' }).role);
  log('no args', err(() => new MenuItem()));
  log('call without new', err(() => MenuItem({ label: 'a' })));
  log('tmpl not array', err(() => Menu.buildFromTemplate({})));
  log('tmpl bad item', err(() => Menu.buildFromTemplate([{ enabled: true }])));
  log('tmpl null item', err(() => Menu.buildFromTemplate([null])));
  log('tmpl sep ok', err(() => Menu.buildFromTemplate([{ type: 'separator' }]).items.length));
  log('insert bad', err(() => new Menu().insert(0, {})));
  log('insert neg', err(() => new Menu().insert(-1, new MenuItem({ label: 'a' }))));
  log('insert big', err(() => new Menu().insert(5, new MenuItem({ label: 'a' }))));
  log('append str', err(() => new Menu().append('a')));
  log('setApplicationMenu bad', err(() => Menu.setApplicationMenu({})));
  log('setApplicationMenu bad2', err(() => Menu.setApplicationMenu(5)));
  log('popup bad opts', err(() => new Menu().popup(5)));
  const labels = (m) => m.items.map(i => i.label || i.type);
  log('sort sep', labels(Menu.buildFromTemplate([{ type: 'separator' }, { label: 'a' }, { type: 'separator' }, { type: 'separator' }, { label: 'b' }, { type: 'separator' }])));
  log('sort invisible sep', labels(Menu.buildFromTemplate([{ type: 'separator', visible: false }, { label: 'a' }, { type: 'separator', visible: false }])));
  log('sort before/after', labels(Menu.buildFromTemplate([{ id: '1', label: 'one' }, { id: '2', label: 'two', before: ['1'] }, { id: '3', label: 'three', after: ['1'] }])));
  log('sort groups', labels(Menu.buildFromTemplate([{ id: 'a', label: 'a' }, { type: 'separator' }, { id: 'b', label: 'b', beforeGroupContaining: ['a'] }, { type: 'separator' }, { id: 'c', label: 'c', afterGroupContaining: ['a'] }])));
  log('sort submenu', labels(Menu.buildFromTemplate([{ label: 'top', submenu: [{ id: 'x', label: 'x' }, { id: 'y', label: 'y', before: ['x'] }] }]).items[0].submenu));
  log('sort cross-group', labels(Menu.buildFromTemplate([{ id: 'a', label: 'a' }, { type: 'separator' }, { id: 'b', label: 'b', after: ['a'] }, { type: 'separator' }, { id: 'c', label: 'c' }])));
  log('sort cycle', labels(Menu.buildFromTemplate([{ id: 'a', label: 'a', after: ['b'] }, { id: 'b', label: 'b', after: ['a'] }])));
  const calls = [];
  const rm = Menu.buildFromTemplate([
    { id: 'r1', label: 'r1', type: 'radio', checked: true, click: (it, w, ev) => calls.push(['r1', typeof w, JSON.stringify(ev)]) },
    { id: 'r2', label: 'r2', type: 'radio', click: (it, w, ev) => calls.push(['r2', it.checked, typeof w, ev && ev.shiftKey]) },
    { type: 'separator' },
    { id: 'r3', label: 'r3', type: 'radio', click: () => calls.push(['r3']) },
    { id: 'c1', label: 'c1', type: 'checkbox', click: (it) => calls.push(['c1', it.checked]) },
    { id: 'c2', label: 'c2', type: 'checkbox', checked: true },
  ]);
  const st = () => rm.items.map(i => i.checked);
  log('radio initial', st());
  rm.getMenuItemById('r2').click({ shiftKey: true }, undefined, undefined);
  log('radio after r2', st(), calls);
  rm.getMenuItemById('r3').click({}, undefined, undefined);
  log('radio after r3', st());
  rm.getMenuItemById('c1').click({}, undefined, undefined);
  rm.getMenuItemById('c1').click({}, undefined, undefined);
  log('checkbox', st(), calls);
  rm.getMenuItemById('r1').checked = false;
  log('radio set false', st());
  rm.getMenuItemById('r3').checked = true;
  log('radio set true', st());
  log('radio groupId', rm.items.map(i => i.groupId));
  log('menu props', Object.keys(rm), rm.items.map(i => i.menu === rm));
  log('getMenuItemById nested', Menu.buildFromTemplate([{ label: 'a', submenu: [{ id: 'deep', label: 'd' }] }]).getMenuItemById('deep').label);
  log('getMenuItemById missing', Menu.buildFromTemplate([{ label: 'a' }]).getMenuItemById('x'));
  log('click role+click', (() => { let hit = 0; const it = new MenuItem({ role: 'selectAll', click: () => hit++ }); it.click({}, undefined, undefined); return hit; })());
  log('click plain', (() => { const got = []; const it = new MenuItem({ label: 'a', click: (...a) => got.push(a.length, a[0] === it, a[1], JSON.stringify(a[2])) }); it.click({ ctrlKey: true }, 'W', 'C'); return got; })());
  log('app.applicationMenu', err(() => (app.applicationMenu = Menu.buildFromTemplate([{ label: 'z' }]), app.applicationMenu.items.length)));
  log('set null', err(() => (Menu.setApplicationMenu(null), Menu.getApplicationMenu())));
  const win = new BrowserWindow({ show: false, width: 400, height: 300 });
  log('menuBarVisible', win.isMenuBarVisible(), win.isMenuBarAutoHide());
  log('setMenu bad', err(() => win.setMenu({})));
  log('setMenu null', err(() => win.setMenu(null)));
  log('removeMenu', err(() => win.removeMenu()));
  log('setMenu ok', err(() => win.setMenu(Menu.buildFromTemplate([{ label: 'q', accelerator: 'Bogus+Q' }, { label: 'r', accelerator: 'CmdOrCtrl+R' }]))));
  log('setAutoHide', err(() => (win.setAutoHideMenuBar(true), [win.isMenuBarAutoHide(), win.isMenuBarVisible()])));
  log('setMenuBarVisibility', err(() => (win.setMenuBarVisibility(false), [win.isMenuBarVisible(), win.menuBarVisible, win.autoHideMenuBar])));
  win.destroy();
}
