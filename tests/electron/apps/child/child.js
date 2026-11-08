process.on('message', () => {});
process.send({ argv: process.argv.slice(2) });
