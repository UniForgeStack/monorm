// `@@history.track` from the schema language to a timeline and a restore; needs the `history` extension (run `npm run assemble` in languages/js).
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect, loads, Registry } from '../../../../languages/js/dist/src/index.js';
import { resolveExtensions } from '../../../../languages/js/dist/src/packages.js';

const fixture = readFileSync(new URL('../fixtures/schema.prisma', import.meta.url), 'utf8');
// Packages are discovered from the consumer's working directory.
process.chdir(new URL('../../../../languages/js/', import.meta.url).pathname);
assert.ok('monorm.history' in resolveExtensions(), 'run `npm run assemble` in languages/js');
const { history } = await import('../../../../languages/js/extensions/history/index.js');

async function run(provider, url) {
  const registry = new Registry();
  const providerLine = `"${provider}"` + (provider === "postgresql" ? '\n  bypass_roles = ["postgres"]' : "");
  const { Post, BundleVersion } = loads(fixture.replace('"sqlite"', providerLine), { registry });
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    const posts = Post.objects.using(db);
    const post = await posts.insert({ title: 'a', views: 1 });
    await posts.filter(Post.id.eq(post.id)).update({ title: 'b', views: 2 });
    const entries = await post.history.timeline(post, { diff: true });
    assert.deepEqual(entries.map((e) => e.event.label), ['insert', 'update']);
    assert.deepEqual(entries[1].diff, { title: { old: 'a', new: 'b' }, views: { old: 1, new: 2 } });
    const restored = await post.history.restore(entries[0].event);
    assert.deepEqual([restored.title, restored.views], ['a', 1]);
    assert.deepEqual((await post.history.timeline(post)).map((e) => e.event.label), ['insert', 'update', 'restore']);
    await posts.filter(Post.id.eq(post.id)).delete();
    const last = (await post.history.timeline(post)).at(-1).event;
    const again = await post.history.restore(last);
    assert.deepEqual([again.id, again.title], [post.id, 'a']);
    assert.equal(await post.history.purge({ before: new Date(Date.now() + 86_400_000) }), 5);

    // versions mode: register, record twice, a repeated snapshot writes nothing, restore leaves a restore version
    const bundles = { 1: { id: 1, title: 'a' } };
    const applied = [];
    history(BundleVersion).register({ generate: (id) => ({ ...bundles[id] }), restore: (snapshot) => applied.push(snapshot) });
    const versions = history(BundleVersion).using(db);
    const first = await versions.record(1, 'create');
    bundles[1].title = 'b';
    assert.equal((await versions.record(1, 'update')).versionNumber, 2);
    assert.equal((await versions.record(1, 'update')).versionNumber, 2);
    const restoredVersion = await versions.restore(first);
    assert.deepEqual([restoredVersion.versionNumber, restoredVersion.changeType, restoredVersion.restoredFrom], [3, 'restore', first.id]);
    assert.deepEqual(applied, [{ id: 1, title: 'a' }]);
    assert.deepEqual((await versions.timeline(1)).map((e) => e.event.versionNumber), [1, 2, 3]);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

await run('sqlite', 'sqlite://:memory:');
if (process.env.ORM_TEST_DATABASE_URL) await run('postgresql', process.env.ORM_TEST_DATABASE_URL);
console.log('Node history extension: passed');
