import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { QueryError, connect, scope } from 'orm';
import { Note } from './models.js';
const locked = JSON.parse(readFileSync('models.ts', 'utf8').split('const SCHEMA: SchemaIR = ')[1].split(';\n')[0]);
assert.deepEqual(Object.keys(locked.behavior.packages).sort(), ['acme.outside', 'orm.inherit', 'orm.locking', 'orm.soft-delete', 'orm.timestamps']);
for (const [id, stamp] of Object.entries(locked.behavior.packages)) {
  const [scope, name] = id.split('.');
  assert.equal((await import(`@${scope}/${name}`)).manifestHash, stamp.manifest_hash);
}
const db = await connect(process.env.ORM_DATABASE_URL);
await db.dropTables();
await db.createTables();
try {
  const notes = Note.objects.using(db);
  const fails = async (promise, ...parts) => {
    try { await promise; } catch (error) {
      assert.ok(error instanceof QueryError, `${error} is not a QueryError`);
      for (const part of parts) assert.ok(String(error.message).includes(part), `${part} is missing from ${error.message}`);
      return;
    }
    assert.fail(`expected an error with ${parts}`);
  };
  let row;
  await scope({ shop: 1, user: 'ann' }, async () => {
    // acme.outside.trim (point 1) runs before acme.outside.nonempty (point 2).
    row = await notes.insert({ id: 1, text: '  first ', start: 1, end: 2, shopId: 1 });
    assert.equal(row.text, 'first');
    assert.equal(row.tagLabel(), 'installed');
    assert.equal(row.version, 0);
    assert.equal(row.updatedBy, null);
    await fails(notes.insert({ id: 9, text: '   ', start: 1, end: 2, shopId: 1 }), 'Note.text: must not be empty');
    // acme.outside.ordered (point 3) reads two fields.
    await fails(notes.insert({ id: 9, text: 'x', start: 3, end: 1, shopId: 1 }), 'start 3 must be before end 1');
    assert.equal(await notes.filter(Note.id.eq(9)).count(), 0);
    // acme.outside.actor (point 4) takes the user of the request scope on update.
    await row.update({ text: 'updated' });
    assert.equal(row.version, 1);
    assert.ok(row.changed instanceof Date);
    assert.equal((await notes.get(Note.id.eq(1))).updatedBy, 'ann');
    // A value that the caller sets for that field goes through the hook too.
    await row.update({ updatedBy: 'eve' });
    assert.equal((await notes.get(Note.id.eq(1))).updatedBy, 'ann');
    assert.equal((await notes.tagged().all())[0].text, 'updated');
    // Another shop's row is outside the default filter `shop_id == scope.shop`.
    await notes.insert({ id: 2, text: 'other', start: 1, end: 2, shopId: 2 });
    assert.deepEqual((await notes.all()).map((n) => n.id), [1]);
  });
  await scope({ shop: 2, user: 'bob' }, async () => {
    assert.deepEqual((await notes.all()).map((n) => n.id), [2]);
    assert.equal(await notes.filter(Note.text.eq('other')).update({ text: 'bulk' }), 1);
    assert.equal((await notes.get(Note.id.eq(2))).updatedBy, 'bob');
  });
  await scope({ shop: 1 }, async () => { await fails(row.update({ text: 'again' }), 'no user in the request scope'); });
  await scope({ shop: 1, user: 'ann' }, async () => {
    await row.delete();
    assert.deepEqual(await notes.all(), []);
    assert.equal((await notes.allWithDeleted().all()).length, 1);
  });
} finally { await db.dropTables(); await db.close(); }
console.log('package consumer js ok');
