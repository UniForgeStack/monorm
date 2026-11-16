// `@@scope.by` from the schema language to a query; needs the `scope` extension (run `npm run assemble` in languages/js).
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect, loads, Registry, scope, ScopeMismatch, ScopeMissing } from '../../../../languages/js/dist/src/index.js';
import { resolveExtensions } from '../../../../languages/js/dist/src/packages.js';

const fixture = readFileSync(new URL('../fixtures/schema.prisma', import.meta.url), 'utf8');
// Packages are discovered from the consumer's working directory.
process.chdir(new URL('../../../../languages/js/', import.meta.url).pathname);
assert.ok('monorm.scope' in resolveExtensions(), 'run `npm run assemble` in languages/js');

async function run(provider, url) {
  const registry = new Registry();
  const { Order } = loads(fixture.replace('"sqlite"', `"${provider}"`), { registry });
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    const orders = Order.objects.using(db);
    await scope({ shop: 1 }, async () => {
      await orders.insert({ total: 5 });
      assert.equal((await orders.get()).shopId, 1);
    });
    await scope({ shop: 2 }, async () => {
      await orders.insertMany([{ total: 6 }, { total: 7, shopId: 2 }]);
      assert.equal(await orders.count(), 2);
      await assert.rejects(orders.insert({ total: 8, shopId: 1 }), (e) => {
        assert.ok(e instanceof ScopeMismatch);
        assert.deepEqual([e.model, e.key, e.expected, e.actual], ['Order', 'shop', '2', '1']);
        return true;
      });
    });
    await assert.rejects(orders.insert({ total: 9 }), (e) => e instanceof ScopeMissing && e.model === 'Order' && e.key === 'shop');
    for (const bad of [{ nope: 1 }, { shop: null }, { shop: [1] }]) {
      await assert.rejects(scope(bad, async () => {}), TypeError);
    }
  } finally {
    await db.dropTables();
    await db.close();
  }
}

await run('sqlite', 'sqlite://:memory:');
if (process.env.ORM_TEST_DATABASE_URL) await run('postgresql', process.env.ORM_TEST_DATABASE_URL);
console.log('Node scope extension: passed');
