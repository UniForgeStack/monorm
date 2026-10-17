// `@@query.defaults` from the schema language to a query; needs a query-defaults artifact.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect, loads, NotLoaded, Registry, scope } from '../../../../languages/js/dist/src/index.js';
import { native } from '../../../../languages/js/dist/src/native.js';

const fixture = readFileSync(new URL('../fixtures/schema.prisma', import.meta.url), 'utf8');
assert.ok(JSON.parse(native().nativeArtifact()).capabilities.includes('query-defaults'), 'select orm-query-defaults in the build');

async function run(provider, url) {
  const registry = new Registry();
  const { Policy, Scoped } = loads(fixture.replace('"sqlite"', `"${provider}"`), { registry });
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    await Policy.objects.using(db).insertMany([{ name: 'Alice', bio: 'large' }, { name: 'hidden', active: false, bio: 'large' }]);
    assert.equal(await Policy.objects.using(db).count(), 1);
    assert.equal(await Policy.objects.using(db).withoutDefaults().count(), 2);
    const row = await Policy.objects.using(db).get();
    assert.equal(row.name, 'Alice');
    assert.throws(() => row.bio, NotLoaded);
    const full = await Policy.objects.using(db).withoutDefaults().get(Policy.name.eq('hidden'));
    assert.equal(full.bio, 'large');
    assert.equal(full.active, false);
    const scoped = Scoped.objects.using(db);
    const [first, second] = await scoped.insertMany([{ shopId: 1 }, { shopId: 2 }]).returning();
    await scoped.insert({ id: first.pk, shopId: 9 }).onConflict(Scoped.id, { update: false });
    await assert.rejects(scoped.insert({ id: first.pk, shopId: 9 }).onConflict(Scoped.id, { update: true }), /scope\.shop/);
    await scope({ shop: 2 }, async () => {
      scoped.insert({ id: first.pk, shopId: 9 }).onConflict(Scoped.id, { update: true }).inspect();
      assert.equal(await scoped.insert({ id: first.pk, shopId: 9 }).onConflict(Scoped.id, { update: true }), 0);
      assert.equal(await scoped.insertMany([{ id: second.pk, shopId: 2 }]).onConflict(Scoped.id, { update: true, updateValues: { shopId: 3 } }), 1);
    });
    assert.equal((await scoped.withoutDefaults().get(Scoped.id.eq(first.pk))).shopId, 1);
    assert.equal((await scoped.withoutDefaults().get(Scoped.id.eq(second.pk))).shopId, 3);
    await scope({ shop: 3 }, async () => {
      await scoped.insert({ id: second.pk, shopId: 2 }).onConflict(Scoped.id, { update: true });
    });
    const hidden = await Policy.objects.using(db).withoutDefaults().get(Policy.name.eq('hidden'));
    assert.equal(await Policy.objects.using(db).insert({ id: hidden.pk, name: 'changed', bio: 'new' }).onConflict(Policy.id, { update: true }), 0);
    assert.equal((await Policy.objects.using(db).withoutDefaults().get(Policy.id.eq(hidden.pk))).name, 'hidden');
    await assert.rejects(Scoped.objects.using(db).count(), /scope\.shop/);
    await scope({ shop: 2 }, async () => assert.deepEqual((await Scoped.objects.using(db)).map((r) => r.shopId), [2]));
    await scope({ shop: null }, async () => {
      await assert.rejects(Scoped.objects.using(db).count(), /scope\.shop is None\/null/);
      assert.equal(await Scoped.objects.using(db).withoutDefaults().count(), 2);
    });
    await scope({ shop: undefined }, async () => assert.rejects(Scoped.objects.using(db).count(), /undefined is not a value/));
    await scope({ shop: 0 }, async () => assert.equal(await Scoped.objects.using(db).count(), 0));
    for (const falsy of ['', false]) {
      await scope({ shop: falsy }, async () => {
        await Scoped.objects.using(db).count().catch((e) => assert.doesNotMatch(String(e), /None\/null/)); // a type error is fine
      });
    }
  } finally {
    await db.dropTables();
    await db.close();
  }
}

await run('sqlite', 'sqlite://:memory:');
if (process.env.ORM_TEST_DATABASE_URL) await run('postgresql', process.env.ORM_TEST_DATABASE_URL);
console.log('Node compiled query defaults: passed');
