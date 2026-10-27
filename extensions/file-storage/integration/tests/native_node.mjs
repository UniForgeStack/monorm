// Public tests against this worktree's selected native addon only.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { AdapterWriteError, FieldAdapterError, allowWrites, connect, define, Registry as ModelRegistry, WriteProtected } from 'orm';
import { Reference, Registry, Upload, FileField, FileFieldError } from '@orm/file-storage';
import { installModel } from '@orm/file-storage/model';
import { Operator } from 'opendal';

class CountingRegistry extends Registry {
  uploads = 0;
  upload(...args) { this.uploads++; return super.upload(...args); }
}
async function fixture(run, { protectedWrite = false } = {}) {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'orm-file07-')));
  const storage = new CountingRegistry(new Map([['reports', new Operator('fs', { root })]]));
  const providers = storage;
  const url = process.env.FILE_STORAGE_DATABASE_URL ?? 'sqlite://:memory:';
  const ir = {
    dialect: url.startsWith('sqlite:') ? 'sqlite' : 'postgres',
    models: [{ name: 'FileReport07', table: 'file_reports_07_node', fields: [
      { name: 'id', column: 'id', type: 'int', primary_key: true },
      { name: 'file', column: 'file', type: 'json', nullable: true }] }],
    behavior: { schema_contract: 4,
      field_adapters: [{ model: 'FileReport07', field: 'file', adapter: 'file-storage.reference.v1' }] },
  };
  ir.models[0].protected_write = protectedWrite;
  const registry = new ModelRegistry();
  const Report = define(ir, { registry }).FileReport07;
  installModel(Report._meta.Row.prototype, new Map([['file', new FileField('file', 'reports', true)]]), providers, Report._meta);
  const db = await connect(url, { registry });
  try { await db.dropTables(); await db.createTables(); await run({ Report, db, storage }); }
  finally { await db.dropTables(); await db.close(); await rm(root, { recursive: true, force: true }); }
}
async function bytes(stream) { const chunks = []; for await (const chunk of stream) chunks.push(chunk); return Buffer.concat(chunks); }

test('reference writes, shared objects, null and explicit opening', () => fixture(async ({ Report, storage }) => {
  const ref = await storage.upload('reports', Buffer.from('report'));
  const row = await Report.objects.insert({ id: 1, file: ref });
  await Report.objects.insert({ id: 2, file: ref });
  await Report.objects.insert({ id: 3, file: null });
  assert.equal(storage.uploads, 1);
  assert.ok(row.file instanceof Reference);
  const rows = await Report.objects.orderBy(Report.id);
  assert.deepEqual([rows[0].file.toJSON(), rows[2].file], [ref.toJSON(), null]);
  assert.equal((await bytes(row.fileOpen())).toString(), 'report');
  await Report.objects.filter(Report.id.eq(1)).update({ file: null });
  await Report.objects.filter(Report.id.eq(2)).delete();
  assert.equal((await bytes(storage.open(ref))).toString(), 'report');
}));

test('unsupported shapes and invalid ordinary values reject before upload', () => fixture(async ({ Report, storage }) => {
  for (const run of [() => Report.objects.insertMany([{ id: 1, file: new Upload(Buffer.from('x')) }], { copy: true }),
    () => Report.objects.insertMany([{ id: 1, file: new Upload(Buffer.from('x')) }, { id: 'invalid', file: new Upload(Buffer.from('y')) }]),
    () => Report.objects.insert({ id: 1, file: new Upload(Buffer.from('x')) }).onConflict(Report.id, { update: false }),
    () => Report.objects.insert({ id: 'invalid', file: new Upload(Buffer.from('x')) })]) {
    await assert.rejects(async () => run());
  }
  assert.equal(storage.uploads, 0);
}));

test('a protected file write outside the scope gives a rejected promise before upload', () => fixture(async ({ Report, storage }) => {
  const writes = [() => Report.objects.insert({ id: 1, file: new Upload(Buffer.from('x')) }),
    () => Report.objects.filter(Report.id.eq(1)).update({ file: new Upload(Buffer.from('y')) })];
  for (const write of writes) {
    let pending;
    assert.doesNotThrow(() => { pending = write(); });
    await assert.rejects(pending, WriteProtected);
  }
  assert.equal(storage.uploads, 0);
  await allowWrites([Report], async () => { for (const write of writes) await write(); });
  assert.equal(storage.uploads, 2);
}, { protectedWrite: true }));

test('SQL failure exposes reference and explicit retry never uploads again', () => fixture(async ({ Report, storage }) => {
  await Report.objects.insert({ id: 1, file: null });
  let operation;
  try { await Report.objects.insert({ id: 1, file: new Upload(Buffer.from('generated')) }); }
  catch (error) { assert.ok(error instanceof AdapterWriteError); operation = error.operation; }
  assert.ok(operation);
  const reference = Reference.fromJSON(operation.stored.file);
  await operation.execute(data => Report.objects.insert({ ...data, id: 2 }));
  assert.equal(storage.uploads, 1);
  await Report.objects.filter(Report.id.eq(2)).update({ file: new Upload(Buffer.from('replacement')) });
  assert.equal(storage.uploads, 2);
  assert.equal((await bytes(storage.open(reference))).toString(), 'generated');
}));

test('outer rollback and savepoint retain successfully uploaded references', () => fixture(async ({ Report, db, storage }) => {
  const retained = Report.objects.insert({ id: 1, file: new Upload(Buffer.from('rollback')) });
  await assert.rejects(db.transaction(async () => { await retained; throw new Error('outer rollback'); }));
  assert.equal(await Report.objects.count(), 0);
  const ref = Reference.fromJSON(retained.operation.stored.file);
  assert.equal((await bytes(storage.open(ref))).toString(), 'rollback');
  let nested;
  await db.transaction(async () => {
    await Report.objects.insert({ id: 2, file: ref });
    nested = Report.objects.insert({ id: 3, file: new Upload(Buffer.from('savepoint')) });
    await assert.rejects(db.transaction(async () => { await nested; throw new Error('savepoint rollback'); }));
  });
  assert.equal(await Report.objects.count(), 1);
  assert.equal((await bytes(storage.open(Reference.fromJSON(nested.operation.stored.file)))).toString(), 'savepoint');
}));

test('batch writes prepare each upload and a filtered update shares one', () => fixture(async ({ Report, storage }) => {
  const ref = await storage.upload('reports', Buffer.from('report'));
  await Report.objects.insertMany([{ id: 2, file: ref }, { id: 3, file: null }]);
  await Report.objects.updateMany([{ id: 2, file: null }, { id: 3, file: ref }]);
  let rows = await Report.objects.orderBy(Report.id);
  assert.equal(rows[0].file, null);
  assert.deepEqual(rows[1].file.toJSON(), ref.toJSON());
  assert.equal(storage.uploads, 1);
  const read = async (id) => (await bytes((await Report.objects.filter(Report.id.eq(id)).get()).fileOpen())).toString();

  assert.equal(await Report.objects.insertMany([{ id: 4, file: new Upload(Buffer.from('four')) }, { id: 5, file: new Upload(Buffer.from('five')) }, { id: 6, file: null }]), 3);
  assert.equal(storage.uploads, 3);
  assert.deepEqual([await read(4), await read(5)], ['four', 'five']);
  assert.equal((await Report.objects.filter(Report.id.eq(6)).get()).file, null);

  await Report.objects.updateMany([{ id: 2, file: new Upload(Buffer.from('two')) }, { id: 6, file: new Upload(Buffer.from('six')) }]);
  assert.equal(storage.uploads, 5);
  assert.deepEqual([await read(2), await read(6)], ['two', 'six']);

  assert.equal(await Report.objects.filter(Report.id.gt(3)).update({ file: new Upload(Buffer.from('shared')) }), 3);
  assert.equal(storage.uploads, 6);
  rows = await Report.objects.filter(Report.id.gt(3)).orderBy(Report.id);
  assert.deepEqual(rows.map(r => r.file.toJSON()), [rows[0].file.toJSON(), rows[0].file.toJSON(), rows[0].file.toJSON()]);
  assert.equal(await read(6), 'shared');
}));

test('a failed bulk insert keeps every upload and retries without one', () => fixture(async ({ Report, storage }) => {
  await Report.objects.insert({ id: 1, file: null });
  const statement = Report.objects.insertMany([{ id: 2, file: new Upload(Buffer.from('two')) }, { id: 1, file: new Upload(Buffer.from('one')) }]);
  await assert.rejects(async () => statement, (error) => { assert.ok(error instanceof AdapterWriteError); return error.operation === statement.operation; });
  assert.equal(storage.uploads, 2);
  assert.deepEqual(Object.keys(statement.operation.stored).sort(), ['0.file', '1.file']);
  await Report.objects.filter(Report.id.eq(1)).delete();
  assert.equal(await statement.operation.execute(), 2);
  assert.equal(storage.uploads, 2);
  assert.equal((await bytes((await Report.objects.filter(Report.id.eq(1)).get()).fileOpen())).toString(), 'one');
}));

test('models of the previous contract fail define', () => {
  const ir = { models: [{ name: 'Old', table: 'old', fields: [{ name: 'id', column: 'id', type: 'int', primary_key: true }] }], behavior: { schema_contract: 3 } };
  assert.throws(() => define(ir, { registry: new ModelRegistry() }), /schema contract 3 is older.*regenerate the models and rebuild/);
});
