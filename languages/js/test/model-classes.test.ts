/** `@@query_set` and `@@model_methods`: the module lookup, consumer replace, import cycle and collisions. */
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { test, type TestContext } from "node:test";
import ts from "typescript";
import { connect, loads, Registry, useModelMethods } from "../src/index.js";
import { native } from "../src/native.js";

const SCHEMA = `datasource db { provider = "sqlite" }
model User {
  id       Int    @id
  password String
  @@query_set("UserQueries")
  @@model_methods("UserMethods")
}
`;

/** The runtime's TypeScript source: the compiled tests run from `dist/test`, Bun runs from `test`. */
function runtimeSource(): string {
  return [join(import.meta.dirname, "..", "src", "index.ts"), join(import.meta.dirname, "..", "..", "src", "index.ts")]
    .find((path) => existsSync(path))!;
}

/** The strict-mode errors in the generated `.ts` files of `dir`; the runtime is read from `src`. */
function typeErrors(dir: string): { code: number; text: string }[] {
  const options: ts.CompilerOptions = {
    strict: true, noEmit: true, skipLibCheck: true, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler, baseUrl: dir,
    paths: { "monorm": [runtimeSource()] },
  };
  const files = ["models", "methods", "queries"].map((name) => join(dir, `${name}.ts`));
  return ts.getPreEmitDiagnostics(ts.createProgram(files, options))
    .filter((d) => d.file !== undefined && d.file.fileName.startsWith(dir))
    .map((d) => ({ code: d.code, text: ts.flattenDiagnosticMessageText(d.messageText, "\n") }));
}

function directory(t: TestContext): string {
  const dir = mkdtempSync(join(tmpdir(), "monorm-model-classes-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function schemaIn(dir: string, name?: string): string {
  mkdirSync(dir, { recursive: true });
  if (name !== undefined) writeFileSync(join(dir, "package.json"), JSON.stringify({ name, type: "module", exports: { "./*.js": "./*.js" } }));
  const schema = join(dir, "schema.prisma");
  writeFileSync(schema, SCHEMA);
  return schema;
}

test("the module lookup reads the name of the nearest package.json of an installed schema", (t) => {
  const schema = schemaIn(join(directory(t), "node_modules", "monorm/auth", "src"), "monorm/auth");
  const generated = native().generateTypescript(schema);
  assert.ok(generated.includes('import * as _q0 from "monorm/auth/queries.js";'), generated);
  assert.ok(generated.includes('import * as _m0 from "monorm/auth/methods.js";'));
  assert.ok(generated.includes("useQuerySet(User, () => _q0.UserQueries);"));
  assert.ok(generated.includes("useModelMethods(User, () => _m0.UserMethods);"));
});

test("a schema in the package of the output keeps relative paths", (t) => {
  const dir = directory(t);
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: "myapp" }));
  const generated = native().generateTypescript(schemaIn(join(dir, "src", "db")));
  assert.ok(generated.includes('import * as _q0 from "./queries.js";') && !generated.includes("myapp/"), generated);
  assert.ok(generated.includes('import * as _m0 from "./methods.js";'));
});

test("a schema outside a package gives paths relative to the models file", (t) => {
  const schema = schemaIn(directory(t));
  const generated = native().generateTypescript(schema);
  assert.ok(generated.includes('import * as _q0 from "./queries.js";'), generated);
  assert.ok(generated.includes('import * as _m0 from "./methods.js";'));
});

test("an installed package without a name asks for the full path", (t) => {
  const dir = join(directory(t), "node_modules", "pkg");
  const schema = schemaIn(dir);
  writeFileSync(join(dir, "package.json"), "{}");
  assert.throws(() => native().generateTypescript(schema), /no "name".*--query-set \/ --model-methods/);
  const generated = native().generateTypescript(schema, null, { User: "pkg/queries.js#UserQueries" }, { User: "pkg/methods.js#UserMethods" });
  assert.ok(generated.includes('import * as _m0 from "pkg/methods.js";'));
});

test("a consumer target replaces the schema class", async (t) => {
  const dir = directory(t);
  const schema = schemaIn(join(dir, "src"), "monorm/auth");
  const generated = native().generateTypescript(schema, null, { User: "./mine.js#MyQueries" }, { User: "./mine.js#MyMethods" });
  assert.ok(generated.includes("_q0.MyQueries") && generated.includes("_m0.MyMethods"), generated);
  assert.ok(!generated.includes("monorm/auth/"));
  const output = join(dir, "src", "out.ts");
  const args = ["--schema", schema, "generate", "typescript", "-o", output];
  assert.equal(await native().cli([...args, "--model-methods", "User=./mine.js#MyMethods"]), 0);
  assert.ok(readFileSync(output, "utf8").includes("_m0.MyMethods"));
  assert.notEqual(await native().cli([...args, "--model-methods", "User=./a.js#A", "--model-methods", "User=./b.js#B"]), 0);
  assert.notEqual(await native().cli([...args, "--model-methods", "Nope=./a.js#A"]), 0);
});

// the strict-mode program takes seconds, and Bun times a test out after 5 s
test("the methods module may import the models module (import cycle)", { timeout: 120_000 }, async (t) => {
  const dir = directory(t);
  const schema = schemaIn(dir, "monorm/auth");
  const methodsSource = [
    'import type { User } from "./models.js";',
    "export class UserMethods {",
    "  checkPassword(this: User, raw: string): boolean { return this.password === raw; }",
    "}",
  ].join("\n");
  writeFileSync(join(dir, "methods.ts"), methodsSource);
  writeFileSync(join(dir, "queries.ts"), [
    'import { QuerySet } from "monorm";',
    'import type { User } from "./models.js";',
    "export class UserQueries extends QuerySet<any> { static readonly model?: typeof User; }",
  ].join("\n"));
  const runtime = pathToFileURL(join(import.meta.dirname, "..", "src", "index.js")).href;
  assert.equal(await native().cli(["--schema", schema, "generate", "typescript", "-o", join(dir, "models.ts")]), 0);
  assert.deepEqual(typeErrors(dir), []);
  writeFileSync(join(dir, "methods.ts"), 'export class UserMethods { password!: number; }\n');
  assert.ok(typeErrors(dir).some((d) => d.code === 2320), "a methods member of another type than the field is a type error");
  writeFileSync(join(dir, "methods.ts"), methodsSource);
  assert.equal(await native().cli(["--schema", schema, "generate", "typescript", "-o", join(dir, "models.ts"), "--import", runtime]), 0);
  for (const name of ["models", "methods", "queries"]) {
    let source = readFileSync(join(dir, `${name}.ts`), "utf8").replace('from "monorm"', `from "${runtime}"`);
    // Bun runs every test file in one process, so the default registry already holds other schemas.
    if (name === "models") source = `import { Registry as _R } from "${runtime}";\n${source.replace("define(SCHEMA, {", "define(SCHEMA, { registry: new _R(),")}`;
    writeFileSync(join(dir, `${name}.js`), ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText);
  }
  const models = await import(pathToFileURL(join(dir, "models.js")).href) as { User: any };
  const db = await connect("sqlite://:memory:", { default: false, registry: (models.User._meta as { registry: Registry }).registry });
  try {
    await db.createTables();
    const user = await models.User.objects.using(db).insert({ id: 1, password: "secret" });
    assert.ok(user.checkPassword("secret") && !user.checkPassword("nope"));
    assert.equal(models.User.objects.constructor.name, "UserQueries");
  } finally {
    await db.close();
  }
});

type UserClass = ReturnType<typeof loads>[string];

function user(): { User: UserClass; registry: Registry } {
  const registry = new Registry();
  return { User: loads(SCHEMA, { registry }).User!, registry };
}

test("model methods load on the first instance, not at registration", async () => {
  const { User, registry } = user();
  let loaded = 0;
  class UserMethods { shout(this: { password: string }): string { return this.password.toUpperCase(); } }
  useModelMethods(User as never, () => { loaded += 1; return UserMethods; });
  assert.equal(loaded, 0);
  const db = await connect("sqlite://:memory:", { default: false, registry });
  try {
    await db.createTables();
    const row = await User.objects.using(db).insert({ id: 1, password: "abc" });
    assert.equal((row as unknown as { shout(): string }).shout(), "ABC");
    assert.equal(loaded, 1);
    await User.objects.using(db).insert({ id: 2, password: "x" });
    assert.equal(loaded, 1);
    assert.throws(() => useModelMethods(User as never, UserMethods), /already in use/);
  } finally {
    await db.close();
  }
});

for (const name of ["password", "update", "delete", "pk"]) {
  test(`a method named ${name} collides with a model member`, async () => {
    const { User } = user();
    const methods = class { };
    Object.defineProperty(methods.prototype, name, { value() { return 1; }, configurable: true });
    assert.throws(() => useModelMethods(User as never, methods), new RegExp(`User\\.${name}: model method collision`));
    const lazy = user();
    useModelMethods(lazy.User as never, () => methods);
    const db = await connect("sqlite://:memory:", { default: false, registry: lazy.registry });
    try {
      await db.createTables();
      await assert.rejects(lazy.User.objects.using(db).insert({ id: 1, password: "p" }), new RegExp(`User\\.${name}: model method collision`));
    } finally {
      await db.close();
    }
  });
}

test("a methods class may override toString; other object members still collide", async () => {
  const { User, registry } = user();
  class UserMethods { toString(this: { password: string }): string { return `user ${this.password}`; } }
  useModelMethods(User as never, UserMethods);
  const db = await connect("sqlite://:memory:", { default: false, registry });
  try {
    await db.createTables();
    assert.equal(String(await User.objects.using(db).insert({ id: 1, password: "x" })), "user x");
  } finally {
    await db.close();
  }
  const other = user();
  const methods = class { };
  Object.defineProperty(methods.prototype, "valueOf", { value() { return 1; }, configurable: true });
  assert.throws(() => useModelMethods(other.User as never, methods), /User\.valueOf: model method collision/);
});
