import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect } from 'orm';
import { Account, ActiveAccount } from './models.js';
const locked = JSON.parse(readFileSync('models.ts', 'utf8').split('const SCHEMA: SchemaIR = ')[1].split(';\n')[0]);
const stamp = locked.behavior.packages['orm.proxy'];
assert.equal((await import('@orm/proxy')).manifestHash, stamp.manifest_hash);
assert.ok(!('proxy_models' in locked.behavior));
const db = await connect(process.env.ORM_DATABASE_URL);
await db.dropTables();
await db.createTables();
try {
  await Account.objects.using(db).insertMany([
    { id: 1, name: 'old one', status: 'OLD', note: 'n' },
    { id: 2, name: 'live', status: 'ACTIVE', note: 'n' },
  ]);
  const rows = await ActiveAccount.objects.using(db).all();
  assert.deepEqual(rows.map((row) => row.name), ['live']);
} finally { await db.dropTables(); await db.close(); }
console.log('proxy consumer js ok');
