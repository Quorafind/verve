const value = 3;
Object.defineProperty(exports, 'safe', {enumerable: true, get: function () { return value; }});
Object.defineProperty(exports, 'unsafe', {get() { throw new Error('must not call this getter'); }});
Object.defineProperty(exports, 'arrow', {get: () => value});
