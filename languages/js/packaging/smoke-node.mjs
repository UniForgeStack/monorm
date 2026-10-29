// The consumer of the packed `monorm` tarball, run by Node and by Bun in a fresh project (see check-node-install.mjs).
//   smoke.mjs         one attribute of each built-in extension, generate, a query on SQLite and on PostgreSQL, the auth app
//   smoke.mjs plain   after the check script deleted the extension libraries: a schema without an extension attribute still loads
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { connect, load, loads, Registry } from "monorm";
import { checkPassword, makePassword } from "monorm/auth";

const POSTGRES = process.env.ORM_TEST_DATABASE_URL;
const header = (provider) => `datasource db { provider = "${provider}" }\n`;
const PLAIN = (provider) => `${header(provider)}model Probe {\n  id Int @id @default(autoincrement())\n  name String @unique\n}\n`;
const EXTENDED = (provider) => `${header(provider)}
model Base {
  id Int @id @default(autoincrement())
  @@inherit.abstract
  @@timestamps.updated_at("updated_at")
  @@locking.version("version")
  @@soft_delete.deleted_at("deleted_at")
}

model Note {
  name String
  @@inherit.extends(Base)
  @@query.filter("positive", "id > 0")
}

model Person {
  id Int @id @default(autoincrement())
  name String
}

model Employee {
  salary Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
}

model Account {
  id Int @id
  name String?
}

model Mirror {
  name String?
  @@proxy.of(Account)
}
`;

// File fields need generated models, so the file schema is generated only.
const FILES = (provider) => `${header(provider)}
model Report {
  id Int @id
  file Json? @storage.file(storage: "reports")
}
`;

// A schema with an abstract model and Generic freezes the abstract model's identity, so Generic has its own schema.
const GENERIC = (provider) => `${header(provider)}
${GENERIC_MODELS}`;
const GENERIC_MODELS = `model Photo {
  id Int @id
  tags Label[] @generic.reverse
}

model Label {
  id Int @id
  target Generic? @generic.relation(targets: ["Photo"])
}
`;

const runtime = "Bun" in globalThis ? "bun" : "node";

if (process.argv[2] === "plain") {
  const { Probe } = loads(PLAIN("sqlite"), { registry: new Registry() });
  assert.ok(Probe, "a schema without an extension attribute loads without the extension libraries");
  assert.throws(() => loads(EXTENDED("sqlite"), { registry: new Registry() }), undefined, "an extension attribute needs its library");
  console.log(`${runtime}: plain ok`);
  process.exit(0);
}

const monorm = (...argv) => {
  const run = spawnSync(process.execPath, ["node_modules/monorm/dist/src/cli.js", ...argv], { encoding: "utf8" });
  assert.equal(run.status, 0, `monorm ${argv.join(" ")}: ${run.stdout}${run.stderr}`);
};
const IDS = {
  "schema.prisma": ["monorm.inherit", "monorm.timestamps", "monorm.locking", "monorm.soft-delete", "monorm.query-defaults", "monorm.composition", "monorm.proxy"],
  "generic.prisma": ["monorm.generic"],
  "files.prisma": ["monorm.file-storage"],
};

// Generate, as `npx monorm generate` does.
writeFileSync("schema.prisma", EXTENDED("sqlite"));
writeFileSync("generic.prisma", GENERIC("sqlite"));
writeFileSync("files.prisma", FILES("sqlite"));
for (const [schema, ids] of Object.entries(IDS)) {
  if (schema === "generic.prisma") monorm("--schema", schema, "identities");
  const out = schema.replace(".prisma", ".models.ts");
  monorm("--schema", schema, "generate", "typescript", "-o", out);
  const generated = readFileSync(out, "utf8");
  assert.match(generated, /from "monorm"/);
  for (const id of ids) assert.ok(generated.includes(id), `${id} is not in ${out}`);
}

// One query per database.
for (const provider of ["sqlite", "postgresql"]) {
  if (provider === "postgresql" && !POSTGRES) continue;
  writeFileSync("schema.prisma", EXTENDED(provider));
  const registry = new Registry();
  const models = load("schema.prisma", { registry });
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : POSTGRES, { registry });
  try {
    await db.dropTables();
    await db.createTables();
    const notes = models.Note.objects.using(db);
    const note = await notes.insert({ name: "first" });
    await notes.filter(models.Note.id.eq(note.id)).update({ name: "second" });
    const [row] = await notes.filter(models.Note.id.eq(note.id));
    assert.equal(row.name, "second");
    assert.equal(await notes.count(), 1);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

// The runtime modules of the bundled extensions resolve by self-reference.
for (const short of ["timestamps", "soft-delete", "locking", "inherit", "query-defaults", "generic", "proxy", "model-composition", "file-storage"]) {
  await import(`monorm/extensions/${short}`);
}
await import("monorm/extensions/file-storage/decoder");

// The auth app: its models load from the schema, its password functions run.
writeFileSync("auth.prisma", `${header("sqlite")}import "app:auth" (prefix: "auth_")\n`);
const authRegistry = new Registry();
const auth = load("auth.prisma", { registry: authRegistry });
assert.ok(auth.User && auth.Role && auth.Permission);
assert.ok(checkPassword("secret", makePassword("secret")));
assert.ok(!checkPassword("wrong", makePassword("secret")));
console.log(`${runtime}: ok`);
