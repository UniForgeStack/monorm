import assert from 'node:assert/strict';
import { loads, SchemaError } from '../../../../languages/js/dist/src/index.js';
// Run from a directory whose package.json does not depend on `@orm/file-storage`.
assert.throws(() => loads('model Bad {\n id Int @id\n file Json? @storage.file(storage: "reports")\n}'),
  error => error instanceof SchemaError && error.message.includes('@storage.file')
    && error.message.includes('@storage.*') && error.message.includes('install its package'));
console.log('Disabled file storage (Node): passed');
