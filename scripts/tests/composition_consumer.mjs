import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect } from 'monorm';
import { Person, Employee, Customer } from './models.js';
const locked = JSON.parse(readFileSync('models.ts', 'utf8').split('const SCHEMA: SchemaIR = ')[1].split(';\n')[0]);
const stamp = locked.behavior.packages['monorm.composition'];
assert.equal((await import('monorm/extensions/model-composition')).manifestHash, stamp.manifest_hash);
const db = await connect(process.env.MONORM_DATABASE_URL);
await db.dropTables();
await db.createTables();
try {
  const alice = await Employee.objects.using(db).insert({ name: 'Alice', salary: 20 });
  assert.deepEqual([alice.name, alice.salary], ['Alice', 20]);
  const customer = await Customer.objects.using(db).attach(alice.id, { points: 5 });
  assert.deepEqual([customer.id, customer.name, customer.points], [alice.id, 'Alice', 5]);
  const counts = [];
  for (const model of [Person, Employee, Customer]) counts.push(await model.objects.using(db).count());
  assert.deepEqual(counts, [1, 1, 1]);
} finally { await db.dropTables(); await db.close(); }
console.log('composition consumer js ok');
