// `@@history.track` from the schema language to a timeline and a restore; needs the `history` extension (run `npm run assemble` in languages/js).
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect, loads, Registry } from '../../../../languages/js/dist/src/index.js';
import { resolveExtensions } from '../../../../languages/js/dist/src/packages.js';

const fixture = readFileSync(new URL('../fixtures/schema.prisma', import.meta.url), 'utf8');
// Packages are discovered from the consumer's working directory.
process.chdir(new URL('../../../../languages/js/', import.meta.url).pathname);
assert.ok('monorm.history' in resolveExtensions(), 'run `npm run assemble` in languages/js');

async function run(provider, url) {
  const registry = new Registry();
  const { Post } = loads(fixture.replace('"sqlite"', `"${provider}"`), { registry });
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
  } finally {
    await db.dropTables();
    await db.close();
  }
}

await run('sqlite', 'sqlite://:memory:');
if (process.env.ORM_TEST_DATABASE_URL) await run('postgresql', process.env.ORM_TEST_DATABASE_URL);
console.log('Node history extension: passed');
