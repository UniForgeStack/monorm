import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect, scope } from 'orm';
import { Wrapped } from '@acme/outside';
import { Note, configureOutside } from './models.js';
const locked = JSON.parse(readFileSync('models.ts', 'utf8').split('const SCHEMA: SchemaIR = ')[1].split(';\n')[0]);
assert.deepEqual(Object.keys(locked.behavior.packages).sort(), ['acme.outside', 'orm.inherit', 'orm.locking', 'orm.query-defaults', 'orm.soft-delete', 'orm.timestamps']);
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
      for (const part of parts) assert.ok(String(error.message).includes(part), `${part} is missing from ${error.message}`);
      return;
    }
    assert.fail(`expected an error with ${parts}`);
  };
  let row;
  await scope({ shop: 1, user: 'ann' }, async () => {
    // `@@query.filter("positive", "id > 0")` hides this row; the guard filter and the package filter both apply.
    await notes.insert({ id: -1, text: 'hidden', start: 1, end: 2, shopId: 1 });
    assert.equal(await notes.count(), 0);
    assert.equal(await notes.withoutDefaults().count(), 1);
    // acme.outside.trim (point 1) runs before acme.outside.nonempty (point 2).
    row = await notes.insert({ id: 1, text: '  first ', start: 1, end: 2, shopId: 1 });
    assert.equal(row.text, 'first');
    assert.equal(row.tagLabel(), 'installed');
    assert.equal(row.version, 0);
    assert.equal(row.updatedBy, null);
    await fails(notes.insert({ id: 9, text: '   ', start: 1, end: 2, shopId: 1 }), 'must not be empty');
    // acme.outside.ordered (point 3) reads two fields.
    await fails(notes.insert({ id: 9, text: 'x', start: 3, end: 1, shopId: 1 }), 'start 3 must be before end 1');
    assert.equal(await notes.filter(Note.id.eq(9)).count(), 0);
    // acme.outside.actor (point 4) takes the user of the request scope on update.
    await row.update({ text: 'updated' });
    assert.equal(row.version, 1);
    assert.ok(row.changed instanceof Date);
    assert.equal((await notes.get(Note.id.eq(1))).updatedBy, 'ann');
    assert.equal((await notes.tagged().all())[0].text, 'updated');
    // `@outside.wrapped("w:")` installs the second consumer of the field adapter runtime: label = runtime prefix + option prefix.
    assert.equal((await notes.get(Note.id.eq(1))).note, null);
    configureOutside('rt-');
    await notes.filter(Note.id.eq(1)).update({ note: { a: 1 } });
    const loaded = await notes.get(Note.id.eq(1));
    assert.ok(loaded.note instanceof Wrapped);
    assert.equal(loaded.note.label, 'rt-w:');
    assert.deepEqual(loaded.note.payload, { a: 1 });
    assert.deepEqual(loaded.noteUnwrap(), { a: 1 });
    await notes.filter(Note.id.eq(1)).update({ note: new Wrapped('ignored', { b: 2 }) });
    row = await notes.get(Note.id.eq(1)); // the bulk updates changed the version
    assert.deepEqual(row.noteUnwrap(), { b: 2 });
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
