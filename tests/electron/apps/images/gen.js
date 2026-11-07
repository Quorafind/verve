// Makes `index.html`: draws the fixture's pictures in a canvas of a hidden window (so the PNG, JPEG and
// WebP are what Chromium encodes) and inlines them as data: URLs.
// Run: electron --user-data-dir=<scratch> tests/electron/apps/images/gen.js
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');
const path = require('node:path');

function gif() {
  // Three 1x1 frames (red, blue, green) of 100 ms, played twice (NETSCAPE2.0 loop count 1).
  const b = [];
  const push = (...v) => b.push(...v);
  push(...Buffer.from('GIF89a'), 1, 0, 1, 0, 0x81, 0, 0, 255, 0, 0, 0, 0, 255, 0, 160, 0, 255, 255, 255);
  push(0x21, 0xff, 0x0b, ...Buffer.from('NETSCAPE2.0'), 3, 1, 1, 0, 0);
  [0, 1, 2].forEach((color) => {
    push(0x21, 0xf9, 4, 0, 10, 0, 0, 0, 0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 4 | (color << 3) | 64, 1, 0);
  });
  push(0x3b);
  return 'data:image/gif;base64,' + Buffer.from(b).toString('base64');
}

const icons = [
  '<path d="M12 3l9 18H3z" fill="currentColor"/>',
  '<circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="3"/>',
  '<rect x="4" y="4" width="16" height="16" rx="3" fill="currentColor"/>',
  '<path d="M4 12h16M12 4v16" stroke="currentColor" stroke-width="3" fill="none"/>',
  '<path d="M3 20L12 4l9 16z" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linejoin="round"/>',
  '<g fill="currentColor"><rect x="3" y="3" width="8" height="8"/><rect x="13" y="13" width="8" height="8"/></g>',
];
const colors = ['#c0392b', '#2471a3', '#1e8449', '#7d3c98', '#b9770e', '#17202a'];
const svgImg = '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#16a085"/><circle cx="16" cy="16" r="8" fill="#fdfefe"/></svg>';
const svgBack = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="8" height="8" fill="#e67e22"/><rect x="8" y="8" width="8" height="8" fill="#e67e22"/></svg>';

app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false, width: 100, height: 100, webPreferences: { backgroundThrottling: false } });
  await win.loadURL('data:text/html,<canvas id=c></canvas>');
  const draw = (w, h, body, type, quality) => win.webContents.executeJavaScript(`(() => {
    const c = document.getElementById('c'); c.width = ${w}; c.height = ${h};
    const x = c.getContext('2d'); ${body}; return c.toDataURL(${JSON.stringify(type)}, ${quality});
  })()`);
  const photo = `const g = x.createLinearGradient(0, 0, 64, 64); g.addColorStop(0, '#f1c40f'); g.addColorStop(1, '#2980b9');
    x.fillStyle = g; x.fillRect(0, 0, 64, 64);
    x.fillStyle = '#c0392b'; x.beginPath(); x.arc(22, 24, 12, 0, 7); x.fill();
    x.fillStyle = '#ecf0f1'; x.fillRect(36, 36, 20, 14);
    x.fillStyle = '#1e8449'; x.fillRect(0, 54, 64, 10)`;
  const png = await draw(64, 64, photo, 'image/png', 1);
  const jpeg = await draw(64, 64, photo, 'image/jpeg', 0.92);
  const webp = await draw(64, 64, photo, 'image/webp', 0.92);
  const tile = await draw(16, 16, `x.fillStyle = '#8e44ad'; x.fillRect(0, 0, 8, 8); x.fillStyle = '#f39c12'; x.fillRect(8, 8, 8, 8); x.fillStyle = '#d5dbdb'; x.fillRect(8, 0, 8, 8)`, 'image/png', 1);
  const wide = await draw(64, 32, `x.fillStyle = '#1abc9c'; x.fillRect(0, 0, 64, 32); x.fillStyle = '#34495e'; x.fillRect(0, 0, 32, 32); x.fillStyle = '#e74c3c'; x.fillRect(24, 8, 16, 16)`, 'image/png', 1);
  const uri = (s) => 'data:image/svg+xml;utf8,' + encodeURIComponent(s);
  const html = `<!doctype html>
<html><head><meta charset="utf-8"><title>images</title>
<style>
  html, body { margin: 0; background: #fff; }
  .at { position: absolute; }
  .row { display: flex; gap: 10px; }
  .row img { display: block; width: 64px; height: 64px; }
  .icons { display: flex; gap: 8px; }
  .icons svg { width: 32px; height: 32px; }
</style></head><body>
<div class="at row" style="left:10px;top:10px">
  <img src="${png}"><img src="${jpeg}"><img src="${webp}"><img src="${gif()}">
</div>
<div class="at icons" style="left:10px;top:90px">
${icons.map((body, i) => `  <svg viewBox="0 0 24 24" style="color:${colors[i]}">${body}</svg>`).join('\n')}
  <img src="${uri(svgImg)}" width="32" height="32">
  <div style="width:32px;height:32px;background:url('${uri(svgImg)}') center / contain no-repeat"></div>
</div>
<div class="at" style="left:10px;top:140px;width:200px;height:80px;background:url('${tile}')"></div>
<div class="at" style="left:220px;top:140px;width:120px;height:80px;border:2px solid #34495e;background:url('${wide}') center / cover"></div>
<div class="at" style="left:350px;top:140px;width:100px;height:80px;background:url('${uri(svgBack)}')"></div>
<img class="at" src="${png}" style="left:10px;top:236px;width:100px;height:40px;object-fit:cover;object-position:50% 20%">
<img class="at" src="${wide}" style="left:120px;top:236px;width:100px;height:60px;object-fit:contain;background:#eee">
<canvas class="at" id="clock" width="90" height="90" style="left:240px;top:230px"></canvas>
<script>
  // The clock stands at 10:08:30 unless the page was opened with #live, when it runs.
  function face() {
    const c = document.getElementById('clock').getContext('2d');
    const t = location.hash === '#live' ? new Date() : new Date(2020, 0, 1, 10, 8, 30);
    c.clearRect(0, 0, 90, 90);
    c.fillStyle = '#fdfefe'; c.beginPath(); c.arc(45, 45, 42, 0, Math.PI * 2); c.fill();
    c.lineWidth = 3; c.strokeStyle = '#2c3e50'; c.stroke();
    c.lineWidth = 2;
    for (let i = 0; i < 12; i++) {
      const a = i * Math.PI / 6; c.beginPath();
      c.moveTo(45 + Math.sin(a) * 36, 45 - Math.cos(a) * 36); c.lineTo(45 + Math.sin(a) * 40, 45 - Math.cos(a) * 40); c.stroke();
    }
    const hand = (turn, length, width, color) => {
      c.lineWidth = width; c.strokeStyle = color; c.lineCap = 'round'; c.beginPath(); c.moveTo(45, 45);
      c.lineTo(45 + Math.sin(turn * Math.PI * 2) * length, 45 - Math.cos(turn * Math.PI * 2) * length); c.stroke();
    };
    hand(((t.getHours() % 12) + t.getMinutes() / 60) / 12, 22, 4, '#2c3e50');
    hand((t.getMinutes() + t.getSeconds() / 60) / 60, 32, 3, '#2c3e50');
    hand(t.getSeconds() / 60, 36, 1.5, '#c0392b');
  }
  face();
  if (location.hash === '#live') setInterval(face, 1000);
</script>
</body></html>
`;
  fs.writeFileSync(path.join(__dirname, 'index.html'), html);
  console.log('images: wrote index.html', html.length);
  app.quit();
});
