import assert from 'node:assert/strict';
import { define, loads, Registry, SchemaError } from '../../../../languages/js/dist/src/index.js';
import { resolveExtensions } from '../../../../languages/js/dist/src/packages.js';
// Run from a directory whose package.json does not depend on `@orm/query-defaults`.
assert.ok(!('orm.query-defaults' in resolveExtensions()), 'run this script in an environment without the package');
const registry = new Registry();
const field = { name: 'id', column: 'id', type: 'int', primary_key: true };
define({ models: [{ name: 'Plain', table: 'plain', fields: [field] }], behavior: {
  schema_contract: 3, read_defaults: [{ source: 'another-extension', name: 'selection', model: 'Plain', fields: [] }]
} }, { registry });
const before = registry.ir();
assert.throws(() => loads('model Bad {\n id Int @id\n @@query.filter("positive", "id > 0")\n}', { registry }),
  error => error instanceof SchemaError && /@query.filter.*rebuild/.test(error.message)
    && error.message.includes('@@query.*') && error.message.includes('docs/README.md'));
assert.deepEqual(registry.ir(), before);
console.log('Disabled Node syntax/neutral contract: passed');
