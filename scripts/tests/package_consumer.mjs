import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect } from 'orm';
import { Note } from './models.js';
const locked = JSON.parse(readFileSync('models.ts', 'utf8').split('const SCHEMA: SchemaIR = ')[1].split(';\n')[0]);
for (const [id, stamp] of Object.entries(locked.behavior.packages)) {
  const [scope, name] = id.split('.');
  assert.equal((await import(`@${scope}/${name}`)).manifestHash, stamp.manifest_hash);
}
const db = await connect(process.env.ORM_DATABASE_URL);
await db.dropTables();
await db.createTables();
try {
  const row = await Note.objects.using(db).insert({ id: 1, text: 'first' });
  assert.equal(row.tagLabel(), 'installed');
  assert.equal(row.version, 0);
  await row.update({ text: 'updated' });
  assert.equal(row.version, 1);
  assert.ok(row.changed instanceof Date);
  assert.equal((await Note.objects.using(db).tagged().all())[0].text, 'updated');
  await row.delete();
  assert.deepEqual(await Note.objects.using(db).all(), []);
  assert.equal((await Note.objects.using(db).allWithDeleted().all()).length, 1);
} finally { await db.dropTables(); await db.close(); }
console.log('package consumer js ok');
