// The window features P34 gives a person to look at: a frameless window dragged by its own title
// bar, a window that is shown only when ready, a first frame already dark, a transparent card, a
// modal child and the window calls of a short scripted tour. It quits when the tour is over, or
// when every window is closed; pass `--stay` to keep the windows open to try them by hand:
//
//   electron tests/electron/apps/windows [--stay]
//
// Where a BrowserWindow method is not there yet (P13), the tour logs that it skipped it.
const { app, BrowserWindow } = require('electron');

const stay = process.argv.includes('--stay');
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const page = (html) => 'data:text/html;charset=utf-8,' + encodeURIComponent(html);
const say = (...parts) => console.log(...parts);

const MAIN = `<!doctype html><title>frameless</title>
<style>
  html, body { margin: 0; height: 100%; background: #1e1e1e; color: #ddd; font: 14px sans-serif; }
  .bar { -webkit-app-region: drag; height: 32px; background: #2d2d2d; display: flex; align-items: center;
         justify-content: space-between; padding-left: 12px; }
  .bar button { -webkit-app-region: no-drag; height: 32px; width: 46px; border: 0; background: transparent; color: #ddd; }
  p { padding: 12px; }
</style>
<div class="bar"><span>Drag me. Double click maximizes.</span>
  <span><button id="min">_</button><button id="max">[]</button><button id="close">x</button></span></div>
<p id="dpr"></p>
<script>document.getElementById('dpr').textContent = 'devicePixelRatio ' + window.devicePixelRatio;</script>`;

const CARD = `<!doctype html><title>card</title>
<style>
  html, body { margin: 0; background: transparent; }
  .card { margin: 10px; height: 160px; border-radius: 16px; background: #fff; color: #333; font: 16px sans-serif;
          padding: 16px; -webkit-app-region: drag; box-shadow: 0 2px 12px #0006; }
</style><div class="card">A transparent window with a rounded card. Drag it by the card.</div>`;

const CHILD = `<!doctype html><title>modal</title><body style="font:14px sans-serif"><p>Modal: the parent takes no input until this closes.</p>`;

function skipping(win, method) {
  if (typeof win[method] === 'function') return false;
  say('skipped', method, '(not in this build yet)');
  return true;
}

app.whenReady().then(async () => {
  // Shown only when it is ready, with its dark background from the first frame.
  const main = new BrowserWindow({
    width: 640, height: 420, frame: false, show: false, backgroundColor: '#1e1e1e', title: 'frameless',
  });
  main.once('ready-to-show', () => { say('main ready-to-show'); main.show(); });
  main.on('closed', () => say('main closed'));
  main.loadURL(page(MAIN));
  if (!(typeof main.once === 'function')) say('no events on BrowserWindow yet');
  // Without `ready-to-show` (P13) the window is shown at once, so the tour can still be looked at.
  setTimeout(() => { if (typeof main.show === 'function') main.show(); }, 1500);

  const card = new BrowserWindow({
    width: 340, height: 200, x: 40, y: 40, frame: false, transparent: true, show: true, title: 'card',
  });
  card.loadURL(page(CARD));
  await wait(1500);
  say('windows open', BrowserWindow.getAllWindows ? BrowserWindow.getAllWindows().length : 'unknown');

  const tour = [
    ['setAlwaysOnTop', () => main.setAlwaysOnTop(true)],
    ['setAlwaysOnTop', () => main.setAlwaysOnTop(false)],
    ['setBounds', () => main.setBounds({ x: 120, y: 120, width: 520, height: 360 })],
    ['center', () => main.center()],
    ['setProgressBar', () => main.setProgressBar(0.5)],
    ['flashFrame', () => main.flashFrame(true)],
  ];
  for (const [method, step] of tour) {
    if (skipping(main, method)) continue;
    say('tour', method);
    step();
    await wait(700);
  }
  if (!skipping(main, 'setProgressBar')) main.setProgressBar(-1);
  if (!skipping(main, 'flashFrame')) main.flashFrame(false);

  const modal = new BrowserWindow({ width: 360, height: 160, parent: main, modal: true, title: 'modal' });
  modal.loadURL(page(CHILD));
  await wait(1500);
  modal.close();
  await wait(300);

  if (stay) { say('tour done; windows stay open'); return; }
  say('tour done');
  app.quit();
});

// A fixture that hangs must still end.
if (!stay) setTimeout(() => app.exit(9), 30000);
