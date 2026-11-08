(async () => {
  // The page sees only what the preload exposed: no `require`, no `process`.
  console.log('page: api', typeof window.api, 'require', typeof require, 'process', typeof process);
  console.log('page: echo', await window.api.echo('hi'));
  window.api.done();
})();
