// `@@query.defaults` from the schema language to a query; needs the `query-defaults` extension (run `npm run assemble` in languages/js).
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect, loads, NotLoaded, Registry, scope } from '../../../../languages/js/dist/src/index.js';
import { resolveExtensions } from '../../../../languages/js/dist/src/packages.js';

const fixture = readFileSync(new URL('../fixtures/schema.prisma', import.meta.url), 'utf8');
// Packages are discovered from the consumer's working directory.
process.chdir(new URL('../../../../languages/js/', import.meta.url).pathname);
assert.ok('monorm.query-defaults' in resolveExtensions(), 'run `npm run assemble` in languages/js');

async function run(provider, url) {
  const registry = new Registry();
  const { Policy, Scoped, Notice, Draft, Archive } = loads(fixture.replace('"sqlite"', `"${provider}"`), { registry });
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
    await assert.rejects(scoped.insert({ id: first.pk, shopId: 9 }).onConflict(Scoped.id, { update: true }), /Scoped default "tenant" from monorm\.query-defaults \(without_defaults\("tenant"\)\) reads scope\.shop/);
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
    await assert.rejects(Scoped.objects.using(db).count(), /Scoped default "tenant" from monorm\.query-defaults \(without_defaults\("tenant"\)\) reads scope\.shop/);
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
    const rows = [{ owner: 1, published: true }, { owner: 1, published: false }, { owner: 2, published: true }, { owner: 2, published: false }];
    for (const model of [Notice, Draft, Archive]) await model.objects.using(db).insertMany(rows);
    const notices = Notice.objects.using(db);
    assert.equal(await notices.count(), 1);
    assert.equal(await notices.withoutDefaults('published').count(), 2);
    assert.equal(await notices.withoutDefaults('mine').count(), 2);
    assert.equal(await notices.withoutDefaults('mine', 'published').count(), 4);
    assert.equal(await notices.withoutDefaults().count(), 4);
    assert.deepEqual(notices.withoutDefaults('mine').currentFilter().filter((d) => d.name !== 'query').map((d) => [d.name, d.active]), [['mine', false], ['published', true]]);
    await assert.rejects(notices.withoutDefaults('nope').count(), /unknown default "nope"; available defaults: \["mine", "published", "query"\]/);
    assert.equal(await Draft.objects.using(db).count(), 1);
    assert.deepEqual((await Draft.objects.using(db)).map((r) => [r.owner, r.published]), [[1, false]]);
    assert.equal(await Draft.objects.using(db).withoutDefaults('mine').count(), 2);
    assert.equal(await Archive.objects.using(db).count(), 2);
    assert.deepEqual(Archive.objects.currentFilter().filter((d) => d.name !== 'query').map((d) => d.name), ['mine']);
    for (const [source, text] of [
      ['@@query.filter("owner == 1")', /takes a name and an expression: @@query\.filter\("<name>", "<expr>"\)/],
      ['@@query.filter("query", "owner == 1")', /reserved/],
      ['@@query.filter("a", "owner == 1")\n @@query.filter("a", "owner == 2")', /is given twice/],
    ]) assert.throws(() => loads(`model Bad {\n id Int @id\n owner Int\n ${source}\n}`, { registry: new Registry() }), text);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

await run('sqlite', 'sqlite://:memory:');
if (process.env.ORM_TEST_DATABASE_URL) await run('postgresql', process.env.ORM_TEST_DATABASE_URL);
console.log('Node compiled query defaults: passed');
