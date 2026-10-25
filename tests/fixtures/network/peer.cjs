'use strict';
// Test peer only: Verve's child_process capability is deliberately never enabled.
const mode = process.argv[2];
setTimeout(() => { console.error('peer timeout'); process.exit(1); }, 20000).unref();
if (mode === 'server') {
  const server = require('net').createServer(socket => {
    socket.pipe(socket);
    socket.on('close', () => server.close());
  });
  server.listen(0, '127.0.0.1', () => console.log(server.address().port));
} else if (mode === 'client') {
  const order = [], socket = require('net').connect(Number(process.argv[3]), '127.0.0.1');
  socket.on('connect', () => order.push('connect'));
  socket.on('ready', () => { order.push('ready'); socket.end('from-node'); });
  socket.setEncoding('utf8');
  socket.on('data', value => order.push(value));
  socket.on('end', () => order.push('end'));
  socket.on('close', () => { order.push('close'); console.log(JSON.stringify(order)); });
  socket.on('error', error => { console.error(error); process.exitCode = 1; });
} else throw new Error('Unknown peer mode');
