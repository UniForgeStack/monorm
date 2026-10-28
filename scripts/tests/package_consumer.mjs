import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { QueryError, connect, scope } from '@ufstack/monorm';
import { Wrapped } from '@acme/outside';
import { sum } from '@acme/outside/model';
import { Note, configureOutside } from './models.js';
// The subpath export resolves, and its dependency was installed with the package.
assert.equal(sum(['0.1', '0.2']), '0.3');
const locked = JSON.parse(readFileSync('models.ts', 'utf8').split('const SCHEMA: SchemaIR = ')[1].split(';\n')[0]);
assert.deepEqual(Object.keys(locked.behavior.packages).sort(), ['acme.outside', 'monorm.inherit', 'monorm.locking', 'monorm.query-defaults', 'monorm.soft-delete', 'monorm.timestamps']);
for (const [id, stamp] of Object.entries(locked.behavior.packages)) {
  const [scope, name] = id.split('.');
  assert.equal((await import(`@${scope}/${name}`)).manifestHash, stamp.manifest_hash);
}
// The lock stores an expression default as a record: `region` from a hook that generate evaluated, `owner` still a hook.
const defaults = Object.fromEntries(locked.models.find((m) => m.name === 'Note').fields.map((f) => [f.name, f.client_default]));
assert.deepEqual(defaults.shop_id, { expr: { t: 'scope', name: 'shop' } });
assert.deepEqual(defaults.region, { expr: { t: 'text', value: 'eu' } });
assert.deepEqual(defaults.owner, { hook: { export: 'acme.outside.owner' } });
const db = await connect(process.env.MONORM_DATABASE_URL);
await db.dropTables();
await db.createTables();
try {
  const notes = Note.objects.using(db);
  const fails = async (promise, ...parts) => failsWith([QueryError], promise, ...parts);
  const failsWith = async (errors, promise, ...parts) => {
    try { await promise; } catch (error) {
      assert.ok(errors.some((type) => error instanceof type), `${error} is not a ${errors.map((type) => type.name).join(' or a ')}`);
      for (const part of parts) assert.ok(String(error.message).includes(part), `${part} is missing from ${error.message}`);
      return;
    }
    assert.fail(`expected an error with ${parts}`);
  };
  let row;
  await scope({ shop: 1, user: 'ann' }, async () => {
    // `@@query.filter("positive", "id > 0")` hides this row; the guard filter and the package filter both apply.
    await notes.insert({ id: -1, text: 'hidden', start: 1, end: 2, shopId: 1, secret: '' });
    assert.equal(await notes.count(), 0);
    assert.equal(await notes.withoutDefaults().count(), 1);
    // acme.outside.trim (point 1) runs before acme.outside.nonempty (point 2).
    row = await notes.insert({ id: 1, text: '  first ', start: 1, end: 2, shopId: 1, secret: '' });
    assert.equal(row.text, 'first');
    assert.equal(row.tagLabel(), 'installed');
    assert.equal(row.version, 0);
    // Hook 7 gives `region` and `owner` an expression default; `updated_by` and `shop_id` default to the request scope.
    const created = await notes.get(Note.id.eq(1));
    assert.deepEqual([created.region, created.owner, created.updatedBy, created.shopId], ['eu', 'by:ann', 'ann', 1]);
    await fails(notes.insert({ id: 9, text: '   ', start: 1, end: 2, shopId: 1, secret: '' }), 'Note.text: must not be empty');
    // acme.outside.ordered (point 3) reads two fields.
    await fails(notes.insert({ id: 9, text: 'x', start: 3, end: 1, shopId: 1, secret: '' }), 'start 3 must be before end 1');
    assert.equal(await notes.filter(Note.id.eq(9)).count(), 0);
    // acme.outside.actor (point 4) takes the user of the request scope on update.
    await row.update({ text: 'updated' });
    assert.equal(row.version, 1);
    assert.ok(row.changed instanceof Date);
    assert.equal((await notes.tagged().all())[0].text, 'updated');
    // Expression assignments run on update: `slug` is `lower(text)` and reads the row as it was before the update.
    // `stamp` comes from a hook that reads no context and `editor` from one that reads the user of the request scope.
    const changed = await notes.get(Note.id.eq(1));
    assert.deepEqual([changed.updatedBy, changed.slug, changed.stamp, changed.editor], ['ann', 'first', 'guarded', 'by:ann']);
    // `touchedBy` is an expression assignment that reads the request scope: `scope.user`.
    assert.equal(changed.touchedBy, 'ann');
    // A value that the caller sets for that field goes through the hook too.
    await row.update({ updatedBy: 'eve' });
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
    // acme.outside.initials (point 5) computes `initials` from `first` and `last` on read.
    await notes.insert({ id: 3, text: 'named', start: 1, end: 2, first: 'ada', last: 'lovelace', secret: '' });
    assert.equal((await notes.get(Note.id.eq(3))).initials, 'AL');
    assert.deepEqual((await notes.filter(Note.id.in([1, 3])).orderBy(Note.id).all()).map((n) => n.initials), ['', 'AL']);
    // The rows that writes return carry the computed field too: insert, update, updateMany and the refreshed instance.
    const inserted = await notes.insert({ id: 5, text: 'w', start: 1, end: 2, first: 'grace', last: 'hopper', secret: '' });
    assert.equal(inserted.initials, 'GH');
    await inserted.update({ first: 'mary', last: 'shelley' });
    assert.equal(inserted.initials, 'MS');
    assert.deepEqual((await notes.filter(Note.id.eq(5)).update({ first: 'ada', last: 'byron' }, { returning: true })).map((n) => n.initials), ['AB']);
    assert.deepEqual((await notes.updateMany([{ id: 5, first: 'emmy', last: 'noether' }], { returning: true })).map((n) => n.initials), ['EN']);
    await notes.filter(Note.id.eq(5)).delete();
    // The bindings drop a computed field from the update input, so the call can fail before the engine runs.
    await failsWith([QueryError, TypeError], notes.filter(Note.id.eq(3)).update({ initials: 'ZZ' }), 'initials');
    await fails(notes.filter(Note.initials.eq('AL')).count(), 'computed fields');
    await notes.filter(Note.id.eq(3)).delete();
    // Hook 1 seals `secret` before the write; hook 5 reads it back as `secretPlain`.
    await notes.insert({ id: 4, text: 'vault', start: 1, end: 2, secret: 'open' });
    const vault = await notes.get(Note.id.eq(4));
    assert.deepEqual([vault.secret, vault.secretPlain], ['enc:nepo', 'open']);
    await notes.filter(Note.id.eq(4)).delete();
    // Another shop's row is outside the default filter `shop_id == scope.shop`.
    await notes.insert({ id: 2, text: 'other', start: 1, end: 2, shopId: 2, secret: '' });
    assert.deepEqual((await notes.all()).map((n) => n.id), [1]);
  });
  await scope({ shop: 2, user: 'bob' }, async () => {
    assert.deepEqual((await notes.all()).map((n) => n.id), [2]);
    assert.equal(await notes.filter(Note.text.eq('other')).update({ text: 'bulk' }), 1);
    assert.equal((await notes.get(Note.id.eq(2))).updatedBy, 'bob');
    assert.equal((await notes.get(Note.id.eq(2))).editor, 'by:bob');
    assert.equal((await notes.get(Note.id.eq(2))).touchedBy, 'bob');
  });
  await scope({ shop: 1 }, async () => {
    await fails(row.update({ text: 'again' }), 'no user in the request scope');
    await fails(notes.insert({ id: 8, text: 'x', start: 1, end: 2, secret: '' }), 'user');
    assert.equal(await notes.filter(Note.id.eq(8)).count(), 0);
  });
  await scope({ shop: 1, user: 'ann' }, async () => {
    await row.delete();
    assert.deepEqual(await notes.all(), []);
    // The soft-deleted rows 3 and 4 stay visible next to row 1.
    assert.deepEqual((await notes.allWithDeleted().all()).map((n) => n.id).sort(), [1, 3, 4, 5]);
  });
} finally { await db.dropTables(); await db.close(); }
console.log('package consumer js ok');
