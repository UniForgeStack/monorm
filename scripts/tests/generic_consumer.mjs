import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect } from 'orm';
import { ContentType, Label, Photo, Video } from './models.js';
const locked = JSON.parse(readFileSync('models.ts', 'utf8').split('const SCHEMA: SchemaIR = ')[1].split(';\n')[0]);
const stamp = locked.behavior.packages['orm.generic'];
assert.equal((await import('@orm/generic')).manifestHash, stamp.manifest_hash);
assert.ok(locked.behavior.schema_contract_minor >= 1);
assert.deepEqual(Object.keys(locked.behavior.extension_data['orm.generic']).sort(), ['relations', 'reverse']);
const db = await connect(process.env.ORM_DATABASE_URL);
await db.dropTables();
await db.createTables();
try {
  const photo = await Photo.objects.using(db).insert({ id: 1, url: 'cat.png' });
  await Video.objects.using(db).insert({ id: 1, title: 'cats' });
  await Label.objects.using(db).insertMany([
    { id: 1, name: 'cute', targetType: ContentType.Photo, targetId: photo.id },
    { id: 2, name: 'long', targetType: ContentType.Video, targetId: 1 },
  ]);
  const onPhotos = await Label.objects.using(db).filter(Label.targetType.eq(ContentType.Photo));
  assert.deepEqual(onPhotos.map((x) => x.name), ['cute']);
  await assert.rejects(Label.objects.using(db).insert({ id: 3, name: 'half', targetType: ContentType.Photo }));
} finally { await db.dropTables(); await db.close(); }
console.log('generic consumer js ok');
