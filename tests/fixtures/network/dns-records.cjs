'use strict';
// A deterministic authoritative DNS fixture: Node and RuntimeX execute the identical source.
const dgram = require('dgram'), dns = require('dns');
const server = dgram.createSocket('udp4');
server.on('message', (query, peer) => {
  let end = 12;
  while (query[end]) end += query[end] + 1;
  end++;
  const type = query.readUInt16BE(end);
  const question = query.subarray(12, end + 4);
  let data;
  if (type === 1) data = Buffer.from([127, 0, 0, 1]);
  if (type === 28) data = Buffer.from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
  if (type === 16) data = Buffer.from([3, 111, 110, 101, 3, 116, 119, 111]);
  if (type === 33) data = Buffer.concat([Buffer.from([0, 1, 0, 2, 32, 251, 6]), Buffer.from('target'), Buffer.from([4]), Buffer.from('test'), Buffer.from([0])]);
  if (!data) throw new Error('Unexpected DNS type ' + type);
  const header = Buffer.alloc(12);
  header.writeUInt16BE(query.readUInt16BE(0), 0);
  header.writeUInt16BE(0x8180, 2);
  header.writeUInt16BE(1, 4); header.writeUInt16BE(1, 6);
  const answer = Buffer.alloc(12);
  answer.writeUInt16BE(0xc00c, 0); answer.writeUInt16BE(type, 2); answer.writeUInt16BE(1, 4);
  answer.writeUInt32BE(300, 6); answer.writeUInt16BE(data.length, 10);
  server.send(Buffer.concat([header, question, answer, data]), peer.port, peer.address);
});
server.bind(0, '127.0.0.1', async () => {
  try {
    dns.setServers(['127.0.0.1:' + server.address().port]);
    globalThis.result = await Promise.all([
      dns.promises.resolve4('fixture.test', { ttl: true }),
      dns.promises.resolve6('fixture.test'),
      dns.promises.resolveTxt('fixture.test'),
      dns.promises.resolveSrv('_fixture._tcp.test')
    ]);
  } catch (error) { globalThis.result = ['threw ' + error]; }
  server.close();
});
