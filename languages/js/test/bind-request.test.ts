import assert from 'node:assert/strict';
import { test } from 'node:test';
import { getDatabase, QueryError, scope, TransactionRequired } from '../src/index.js';
import { currentScope, declareScopeKeys } from '../src/db.js';
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
  declareScopeKeys(db, ['keep', 'shop']);
  const tx = await begin();
  await scope({ keep: 1 }, () => db.bindRequest(tx, { scope: { shop: 7 } }, async () => {
    assert.deepEqual(currentScope(), { keep: 1, shop: 7 });
  }));
  await tx.rollback();
  assert.deepEqual(currentScope(), {});
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

test('bind request takes no tenant argument', async () => {
  const db = getDatabase();
  const tx = await begin();
  let ran = false;
  await assert.rejects(
    async () => db.bindRequest(tx, { tenant: 1 } as never, async () => { ran = true; }),
    (e: unknown) => e instanceof TypeError && /takes no `tenant` option/.test(e.message),
  );
  assert.equal(ran, false);
  await tx.rollback();
});
