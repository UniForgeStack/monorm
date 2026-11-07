import assert from 'node:assert/strict';
import { test } from 'node:test';
import { getDatabase, QueryError, scope, TransactionRequired } from '../src/index.js';
import { currentScope } from '../src/db.js';
import { User } from './blog/models.js';
import { useDatabase } from './helpers.js';

useDatabase();

const begin = () => getDatabase().engine.begin(null);

test('statements in the block use the bound transaction and the caller ends it', async () => {
  const db = getDatabase();
  let tx = await begin();
  await db.bindRequest(tx, {}, async () => {
    await User.objects.insert({ email: 'ann@example.com', name: 'Ann' });
    await db.transaction(async () => { await User.objects.insert({ email: 'bob@example.com', name: 'Bob' }); });
    assert.equal(await User.objects.count(), 2);
  });
  await tx.rollback();
  assert.equal(await User.objects.count(), 0);

  tx = await begin();
  await db.bindRequest(tx, {}, async () => { await User.objects.insert({ email: 'cy@example.com', name: 'Cy' }); });
  await tx.commit();
  assert.equal(await User.objects.count(), 1);
});

test('the binding ends with the block and follows tasks started in it', async () => {
  const db = getDatabase();
  const tx = await begin();
  await db.bindRequest(tx, {}, async () => {
    await Promise.resolve().then(() => User.objects.insert({ email: 'ann@example.com', name: 'Ann' }));
  });
  await tx.rollback();
  assert.equal(db.tx(), null);
  assert.equal(await User.objects.count(), 0);
});

test('scope values are set in the block', async () => {
  const db = getDatabase();
  const tx = await begin();
  await scope({ keep: 1 }, () => db.bindRequest(tx, { scope: { shop: 7 } }, async () => {
    assert.deepEqual(currentScope(), { keep: 1, shop: 7 });
  }));
  await tx.rollback();
  assert.deepEqual(currentScope(), {});
});

test('the tenant counts as the one the transaction began under', async () => {
  const db = getDatabase();
  let tx = await begin();
  await db.bindRequest(tx, { tenant: 'a' }, async () => {
    await db.tenant('a', async () => { assert.equal(await User.objects.count(), 0); });
    assert.throws(() => db.tenant('b', async () => {}), /another tenant/);
    // A savepoint keeps the tenant of its transaction.
    await db.transaction(async () => assert.throws(() => db.tenant('b', async () => {}), /another tenant/));
    await db.transaction(async () => { await db.tenant('a', async () => {}); });
  });
  await tx.rollback();

  tx = await begin();
  await db.bindRequest(tx, {}, async () => {
    assert.throws(() => db.tenant('a', async () => {}), (e) => e instanceof QueryError && /another tenant/.test(e.message));
  });
  await tx.rollback();
});

test('a bad tenant id raises before the binding', async () => {
  const db = getDatabase();
  const tx = await begin();
  assert.throws(() => db.bindRequest(tx, { tenant: '' }, async () => {}), RangeError);
  assert.throws(() => db.bindRequest(tx, { tenant: Number.NaN }, async () => {}), TypeError);
  assert.equal(db.tx(), null);
  await tx.rollback();
});

test('on_commit in the block raises because the commit happens outside', async () => {
  const db = getDatabase();
  const tx = await begin();
  await db.bindRequest(tx, {}, async () => {
    await assert.rejects(db.onCommit(() => {}), TransactionRequired);
    await assert.rejects(db.transaction(async () => { await db.onCommit(() => {}); }), /bindRequest/);
    await db.transaction(async () => {}); // a savepoint with no callback is fine
  });
  await tx.rollback();
});
