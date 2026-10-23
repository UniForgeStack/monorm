// `modelFor` and the expanded models from the schema language to the database; needs an inherit artifact.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdtempSync, readFileSync, symlinkSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { connect, loads, Registry } from '../../../../languages/js/dist/src/index.js';

const js = new URL('../../../../languages/js/', import.meta.url).pathname;
const source = new URL('../../typescript/src/index.ts', import.meta.url).pathname;
const fixture = readFileSync(new URL('../fixtures/schema.prisma', import.meta.url), 'utf8');

// compile the extension's own source next to a `node_modules/orm` link, as the artifact build does
const dir = mkdtempSync(join(tmpdir(), 'orm-inherit-'));
mkdirSync(join(dir, 'node_modules'));
symlinkSync(js, join(dir, 'node_modules/orm'));
copyFileSync(source, join(dir, 'index.ts'));
execFileSync(join(js, 'node_modules/.bin/tsc'), [join(dir, 'index.ts'), '--outDir', dir, '--module', 'ESNext', '--moduleResolution', 'Bundler', '--target', 'ES2022', '--skipLibCheck', '--types', 'node', '--typeRoots', join(js, 'node_modules/@types')], { stdio: 'inherit' });
const { modelFor } = await import(pathToFileURL(join(dir, 'index.js')).href);

async function run(provider, url) {
  const registry = new Registry();
  const { MyUser, Admin, Role, Team, UserTeam, AdminTeam, ...rest } = loads(fixture.replace('"sqlite"', `"${provider}"`), { registry });
  assert.deepEqual(Object.keys(rest), []);
  assert.equal(modelFor('User', registry), MyUser);
  assert.equal(modelFor('Admin', registry), Admin);
  assert.equal(modelFor('Role', registry), Role);
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    const User = modelFor('User', registry);
    const [user] = await User.objects.using(db).insertMany([{ email: 'a@b.c', bio: 'hi' }]).returning();
    const [admin] = await Admin.objects.using(db).insertMany([{ email: 'x@y.z' }]).returning();
    assert.equal(user.email, 'a@b.c');
    assert.equal(user.bio, 'hi');
    assert.ok(user.createdAt instanceof Date);
    assert.equal(admin.level, 1);
    await Role.objects.using(db).insertMany([{ name: 'staff', userRolesId: user.pk }, { name: 'root', adminRolesId: admin.pk }]);
    assert.deepEqual((await user.roles.using(db).all()).map((r) => r.name), ['staff']);
    assert.deepEqual((await admin.roles.using(db).all()).map((r) => r.name), ['root']);
    const teams = await Team.objects.using(db).insertMany([{ name: 'ops' }, { name: 'dev' }, { name: 'qa' }]).returning();
    await UserTeam.objects.using(db).insertMany([{ userId: user.pk, teamId: teams[0].pk }, { userId: user.pk, teamId: teams[1].pk }]);
    await AdminTeam.objects.using(db).insertMany([{ adminId: admin.pk, teamId: teams[2].pk }]);
    assert.deepEqual((await user.teams.using(db).all()).map((t) => t.name).sort(), ['dev', 'ops']);
    assert.deepEqual((await admin.teams.using(db).all()).map((t) => t.name), ['qa']);
    assert.deepEqual((await teams[0].userTeams.using(db).all()).map((u) => u.email), ['a@b.c']);
    assert.deepEqual((await teams[2].adminTeams.using(db).all()).map((a) => a.email), ['x@y.z']);
  } finally {
    await db.close();
  }
}

await run('sqlite', 'sqlite://:memory:');
console.log('inherit node ok');
