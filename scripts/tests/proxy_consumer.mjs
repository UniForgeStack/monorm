import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect } from 'monorm';
import { Account, ActiveAccount } from './models.js';
const locked = JSON.parse(readFileSync('models.ts', 'utf8').split('const SCHEMA: SchemaIR = ')[1].split(';\n')[0]);
const stamp = locked.behavior.packages['monorm.proxy'];
assert.equal((await import('monorm/extensions/proxy')).manifestHash, stamp.manifest_hash);
assert.ok(!('proxy_models' in locked.behavior));
const db = await connect(process.env.MONORM_DATABASE_URL);
await db.dropTables();
await db.createTables();
try {
  await Account.objects.using(db).insertMany([
    { id: 1, name: 'old one', status: 'old', note: 'n' },
    { id: 2, name: 'live', status: 'active', note: 'n' },
  ]);
  const rows = await ActiveAccount.objects.using(db).all();
  assert.deepEqual(rows.map((row) => row.name).sort(), ['live', 'old one']);
  const created = await ActiveAccount.objects.using(db).insert({ id: 3 });
  assert.equal(created.name, 'new'); assert.equal(created.status, 'active');
} finally { await db.dropTables(); await db.close(); }
console.log('proxy consumer js ok');
