// What Electron 39 answers from the system modules that can be asked without a person or a visible
// effect: screen, nativeTheme, powerMonitor, powerSaveBlocker, systemPreferences, Notification,
// clipboard (the text on it is saved and put back; the other formats it held are lost) and shell
// on a path that is not there. Prints JSON.
'use strict';
const path = require('node:path');
const {
  app, screen, nativeTheme, powerMonitor, powerSaveBlocker, systemPreferences, Notification,
  clipboard, shell, nativeImage, dialog,
} = require('electron');

const hex = (b) => Buffer.from(b).toString('hex');
const attempt = (call) => {
  try {
    return { value: call() };
  } catch (error) {
    return { name: error.name, message: error.message, code: error.code };
  }
};
const keysOf = (object) => Object.keys(object).sort();

app.whenReady().then(async () => {
  const out = {};
  const scratch = process.env.VERVE_PROBE_SCRATCH;

  const primary = screen.getPrimaryDisplay();
  out.display = { keys: keysOf(primary), types: Object.fromEntries(Object.entries(primary).map(([k, v]) => [k, Array.isArray(v) ? 'array' : typeof v])), boundsKeys: keysOf(primary.bounds), size: keysOf(primary.size), workAreaSize: keysOf(primary.workAreaSize), rotation: primary.rotation, internal: primary.internal, touchSupport: primary.touchSupport, monitorKeys: keysOf(primary).filter((k) => k.includes('onitor')) };
  out.displays = screen.getAllDisplays().length >= 1;
  out.cursor = { keys: keysOf(screen.getCursorScreenPoint()) };
  out.nearest = screen.getDisplayNearestPoint({ x: 0, y: 0 }).id === primary.id || 'other';
  out.matching = screen.getDisplayMatching({ x: 0, y: 0, width: 10, height: 10 }).id === primary.id || 'other';
  out.dipToScreen = Object.keys(screen.dipToScreenPoint({ x: 1, y: 2 })).sort();
  out.screenKeys = keysOf(screen).concat(Object.getOwnPropertyNames(Object.getPrototypeOf(screen))).sort();
  out.screenBadArg = attempt(() => screen.getDisplayNearestPoint());
  out.screenBadArg2 = attempt(() => screen.getDisplayMatching(1));

  const themeEvents = [];
  nativeTheme.on('updated', () => themeEvents.push('updated'));
  out.theme = {
    source: nativeTheme.themeSource,
    dark: nativeTheme.shouldUseDarkColors,
    highContrast: nativeTheme.shouldUseHighContrastColors,
    inverted: nativeTheme.shouldUseInvertedColorScheme,
    forced: nativeTheme.inForcedColorsMode,
    transparency: nativeTheme.prefersReducedTransparency,
  };
  nativeTheme.themeSource = 'dark';
  out.themeDark = { source: nativeTheme.themeSource, dark: nativeTheme.shouldUseDarkColors };
  nativeTheme.themeSource = 'light';
  out.themeLight = { dark: nativeTheme.shouldUseDarkColors };
  out.themeBad = attempt(() => { nativeTheme.themeSource = 'blue'; return nativeTheme.themeSource; });
  nativeTheme.themeSource = 'system';
  await new Promise((resolve) => setTimeout(resolve, 200));
  out.themeEvents = themeEvents;

  out.notification = { supported: Notification.isSupported() };
  out.power = {
    idleState: powerMonitor.getSystemIdleState(60),
    idleTimeType: typeof powerMonitor.getSystemIdleTime(),
    thermal: powerMonitor.getCurrentThermalState(),
    battery: typeof powerMonitor.isOnBatteryPower(),
    onBattery: typeof powerMonitor.onBatteryPower,
    badThreshold: attempt(() => powerMonitor.getSystemIdleState()),
  };
  const id = powerSaveBlocker.start('prevent-app-suspension');
  out.blocker = { idType: typeof id, started: powerSaveBlocker.isStarted(id) };
  powerSaveBlocker.stop(id);
  out.blocker.afterStop = powerSaveBlocker.isStarted(id);
  out.blocker.unknownStop = attempt(() => powerSaveBlocker.stop(9999));
  out.blocker.badType = attempt(() => powerSaveBlocker.start('nope'));
  out.blocker.second = powerSaveBlocker.start('prevent-display-sleep') - id;
  powerSaveBlocker.stop(id + 1);

  out.prefs = {
    accent: systemPreferences.getAccentColor(),
    window: systemPreferences.getColor('window'),
    windowText: systemPreferences.getColor('window-text'),
    badColor: attempt(() => systemPreferences.getColor('nope')),
    functions: Object.getOwnPropertyNames(Object.getPrototypeOf(systemPreferences)).concat(Object.keys(systemPreferences)).sort(),
    aero: typeof systemPreferences.isAeroGlassEnabled,
    animation: systemPreferences.getAnimationSettings(),
    media: systemPreferences.getMediaAccessStatus('microphone'),
    screenMedia: systemPreferences.getMediaAccessStatus('screen'),
    badMedia: attempt(() => systemPreferences.getMediaAccessStatus('nope')),
    ask: typeof systemPreferences.askForMediaAccess,
    invertedColors: typeof systemPreferences.isInvertedColorScheme,
    highContrast: typeof systemPreferences.isHighContrastColorScheme,
    macMethods: Object.fromEntries(['isTrustedAccessibilityClient', 'getUserDefault', 'getEffectiveAppearance', 'getSystemColor', 'subscribeNotification', 'isDarkMode'].map((name) => [name, typeof systemPreferences[name]])),
  };

  const saved = clipboard.readText();
  const formats = {};
  clipboard.clear();
  formats.afterClear = clipboard.availableFormats();
  clipboard.writeText('plain');
  formats.text = clipboard.availableFormats();
  clipboard.writeHTML('<b>bold</b>');
  formats.html = clipboard.availableFormats();
  out.htmlRead = clipboard.readHTML();
  clipboard.writeRTF('{\\rtf1 hello}');
  formats.rtf = clipboard.availableFormats();
  out.rtfRead = clipboard.readRTF();
  clipboard.writeBookmark('Title', 'https://example.com/');
  formats.bookmark = clipboard.availableFormats();
  out.bookmarkRead = clipboard.readBookmark();
  clipboard.writeImage(nativeImage.createFromPath(path.join(scratch, 'sample.png')));
  formats.image = clipboard.availableFormats();
  out.imageRead = hex(clipboard.readImage().toBitmap());
  out.imageReadSize = clipboard.readImage().getSize();
  clipboard.write({ text: 'both', html: '<i>both</i>' });
  formats.both = clipboard.availableFormats();
  out.has = { text: clipboard.has('text/plain'), html: clipboard.has('text/html'), png: clipboard.has('image/png'), other: clipboard.has('x/y') };
  clipboard.writeBuffer('verve-test-format', Buffer.from('abc'));
  out.buffer = { read: hex(clipboard.readBuffer('verve-test-format')), formats: clipboard.availableFormats(), missing: hex(clipboard.readBuffer('verve-absent')) };
  clipboard.clear();
  out.empty = { text: clipboard.readText(), html: clipboard.readHTML(), rtf: clipboard.readRTF(), bookmark: clipboard.readBookmark(), image: clipboard.readImage().isEmpty() };
  out.clipboardSelection = { text: attempt(() => clipboard.readText('selection')) };
  out.clipboardFind = attempt(() => clipboard.readFindText());
  out.clipboardBad = attempt(() => clipboard.writeText(1));
  out.clipboardImageBad = attempt(() => clipboard.writeImage('x'));
  out.clipboardWriteBad = attempt(() => clipboard.write({ image: 1 }));
  out.formats = formats;
  clipboard.writeText(saved);

  out.shell = {
    missing: await shell.openPath(path.join(scratch, 'does-not-exist.txt')),
    trashMissing: await shell.trashItem(path.join(scratch, 'does-not-exist.txt')).then(() => 'trashed', (e) => e.message),
    readLink: attempt(() => shell.readShortcutLink(path.join(scratch, 'none.lnk'))),
    openExternalNone: attempt(() => shell.openExternal()),
    openPathBad: attempt(() => shell.openPath(1)),
  };
  out.dialogBad = {
    errorBoxNoArg: attempt(() => dialog.showErrorBox()),
  };
  const icon = await app.getFileIcon(process.execPath, { size: 'small' });
  out.fileIcon = { size: icon.getSize(), kind: icon.constructor.name };
  out.fileIconNormal = (await app.getFileIcon(process.execPath)).getSize();
  out.fileIconLarge = (await app.getFileIcon(process.execPath, { size: 'large' })).getSize();
  out.fileIconMissing = await app.getFileIcon(path.join(scratch, 'none.txt')).then((i) => i.getSize(), (e) => e.message);
  console.log(JSON.stringify(out, null, 1));
  app.quit();
});
