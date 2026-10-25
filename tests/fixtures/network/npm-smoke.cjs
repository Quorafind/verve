'use strict';
// The identical fixture runs under Node and Verve. Its result is frozen in network.rs.
(async () => {
  const { once } = require('events');
  const WS = require('ws');
  const axios = require('axios');
  const fetch = require('node-fetch');
  const express = require('express');
  const results = [];

  const wss = new WS.Server({ host: '127.0.0.1', port: 0, perMessageDeflate: false });
  wss.on('connection', socket => socket.on('message', value => socket.send(value)));
  await once(wss, 'listening');
  const url = 'ws://127.0.0.1:' + wss.address().port;
  const ws = new WS(url, { perMessageDeflate: false });
  await once(ws, 'open');
  ws.send('npm-ws');
  const [reply] = await once(ws, 'message');
  results.push(reply.toString());
  ws.close();
  await once(ws, 'close');

  const web = new WebSocket(url);
  web.binaryType = 'arraybuffer';
  await new Promise((resolve, reject) => { web.onopen = resolve; web.onerror = reject; });
  const message = new Promise((resolve, reject) => { web.onmessage = e => resolve(e.data); web.onerror = reject; });
  web.send(new Uint8Array([1, 2, 3]));
  results.push(Array.from(new Uint8Array(await message)).join(','));
  const closed = new Promise(resolve => { web.onclose = resolve; });
  web.close(1000, 'done');
  results.push((await closed).code);
  await new Promise(resolve => wss.close(resolve));

  const app = express();
  app.get('/json', (req, res) => res.json({ answer: 42 }));
  app.get('/redirect', (req, res) => res.redirect('/json'));
  const server = app.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const base = 'http://127.0.0.1:' + server.address().port;
  const a = await axios.get(base + '/json', { adapter: 'http', proxy: false, maxRedirects: 0 });
  results.push(a.status, a.data.answer);
  const b = await fetch(base + '/json');
  results.push(b.status, (await b.json()).answer);
  const c = await fetch(base + '/redirect', { redirect: 'manual' });
  results.push(c.status);
  await c.text();
  await new Promise(resolve => server.close(resolve));
  globalThis.result = results;
})().catch(error => { globalThis.result = ['threw ' + (error && error.stack || error)]; });
