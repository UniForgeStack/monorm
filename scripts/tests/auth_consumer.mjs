// Runs against models generated from `import "app:auth"` in a fresh package consumer, with Node or Bun.
import assert from 'node:assert/strict';
import { connect } from 'monorm';
import { UserQueries, authenticate, makePassword } from 'monorm/auth';
import { User } from './models.js';
assert.ok(User.objects instanceof UserQueries);
const db = await connect(process.env.MONORM_DATABASE_URL);
await db.dropTables();
await db.createTables();
try {
  await User.objects.using(db).insert({ username: 'ann', email: 'ann@example.com', password: makePassword('s3cret') });
  assert.equal((await authenticate('ann', 's3cret', { using: db })).username, 'ann');
  assert.equal(await authenticate('ann', 'wrong', { using: db }), null);
} finally {
  await db.dropTables();
  await db.close();
}
console.log('auth consumer (Node/Bun): passed');
