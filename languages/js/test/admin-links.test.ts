/** Admin read-only links: a generic relation and its reverse show as links to the related rows, through `admin.testClient`, on SQLite. */
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, type AdminResponse } from "../src/admin.js";
import { main as cli } from "../src/cli.js";
import { connect, load, Registry } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";
import { scratch as scratchDir } from "./scratch.js";

const SU = { key: 1, is_superuser: true };
const API = "/admin/api";
const scratch = scratchDir("node-admin-links-");

const SCHEMA = `datasource db {
  provider = "sqlite"
}

model GPost {
  id   Int    @id @default(autoincrement())
  name String
  tags GTag[] @generic.reverse
  @@map("gl_posts")
}

model GPhoto {
  id   Int    @id @default(autoincrement())
  name String
  @@map("gl_photos")
}

model GTag {
  id     Int      @id @default(autoincrement())
  name   String
  target Generic? @generic.relation(targets: ["GPost", "GPhoto"])
  @@map("gl_tags")
}
`;

type Loose = any;
const items = (reply: AdminResponse): [string, string][] => reply.json().items.map((i: Loose) => [i.resource, i.label]);

async function world(run: (client: Loose) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(scratch, "links-"));
  const path = join(dir, "schema.prisma");
  writeFileSync(path, SCHEMA);
  assert.equal(await cli(["--schema", path, "identities"]), 0);
  const registry = new Registry();
  const models = load(path, { registry }) as unknown as Record<string, Loose>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const using = (name: string) => models[name].objects.using(db);
    await using("GPost").insertMany([{ name: "Hello" }, { name: "Other" }]);
    await using("GPhoto").insertMany([{ name: "Sunset" }]);
    const kind = registry.getEnum("ContentType");
    await using("GTag").insertMany([
      { name: "red", targetType: kind["GPost"], targetId: 1 },
      { name: "blue", targetType: kind["GPost"], targetId: 1 },
      { name: "warm", targetType: kind["GPhoto"], targetId: 1 },
      { name: "none" },
      { name: "gone", targetType: kind["GPost"], targetId: 9 },
    ]);
    const { GPost: post, GPhoto: photo, GTag: tag } = models;
    const site = new Admin(registry);
    site.register(post, { list: { columns: [post.name], order: [post.id] }, relations: { tags: {} } } as never);
    site.register(photo, { list: { columns: [photo.name], order: [photo.id] } } as never);
    site.register(tag, { list: { columns: [tag.name, tag.targetId, tag.targetType], order: [tag.id] }, relations: { target: {} } } as never);
    await run(site.testClient(db, { user: SU }));
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

const generic = hasExtension("monorm.generic");

// case: admin_links::a_union_link_leads_to_the_row_that_the_type_selects
test("a union link leads to the row that the type selects", { skip: !generic }, () => world(async (client) => {
  assert.deepEqual(items(await client.get(`${API}/links/GTag/1/target`)), [["GPost", "Hello"]]);
  assert.deepEqual(items(await client.get(`${API}/links/GTag/3/target`)), [["GPhoto", "Sunset"]]);
  for (const key of [4, 5]) assert.deepEqual(items(await client.get(`${API}/links/GTag/${key}/target`)), [], "no type, or a row that is gone, leaves no link");
  assert.equal((await client.get(`${API}/links/GTag/99/target`)).status, 404);
}));

// case: admin_links::a_conditioned_to_many_link_lists_the_rows_that_match
test("a conditioned to-many link lists the rows that match", { skip: !generic }, () => world(async (client) => {
  assert.deepEqual(items(await client.get(`${API}/links/GPost/1/tags`)), [["GTag", "red"], ["GTag", "blue"]]);
  assert.deepEqual(items(await client.get(`${API}/links/GPost/2/tags`)), []);
}));

// case: admin_links::a_link_has_no_picker_and_the_meta_names_it
test("a link has no picker and the meta names it", { skip: !generic }, () => world(async (client) => {
  assert.equal((await client.get(`${API}/autocomplete/GTag/target`)).status, 404);
  const meta = (await client.get(`${API}/site`)).json();
  const tag = meta.resources.find((r: Loose) => r.name === "GTag");
  assert.deepEqual(tag.linked, [{ name: "target", title: "Target", many: false }]);
  const column = tag.fields.find((c: Loose) => c.label === "target_id");
  assert.deepEqual(column.ref_by, { column: "target_type", targets: { GPost: "GPost", GPhoto: "GPhoto" } });
}));
