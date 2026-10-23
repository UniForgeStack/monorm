// A proxy of a model inherits its root's extension members and its root's QuerySet class.
// Runs only with an artifact that selects the proxy-models and soft-delete extensions.
// Each `// E:` line of the typing use must fail and no other line may.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";
import ts from "typescript";
import { connect, Registry } from "../src/index.js";
import { native } from "../src/native.js";
import { artifactCapabilities, hasExtension } from "./artifact-caps.js";
import { loadsSoftDelete } from "./soft-delete-ext.js";

const SCHEMA = `model Post {
  id         Int       @id
  title      String
  @@soft_delete.deleted_at
  @@map("proxy_members_post")
}

model Draft {
  @@proxy.of(Post)
  @@map("proxy_members_post")
}

model Note {
  id Int @id
  @@map("proxy_members_note")
}
`;
const QUERIES = `import { QuerySet } from "orm";
import { Post, type PostSpec } from "./models.js";

export class PostQueries extends QuerySet<PostSpec> {
  titled(title: string): this {
    return this.filter(Post.title.eq(title)) as unknown as this;
  }
}
`;
const USE = `import { of } from "orm";
import { Draft, Note, Post } from "./models.js";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
declare function same<A, B>(...check: Equal<A, B> extends true ? [] : [error: "types differ"]): void;
declare const draft: Draft;
declare const note: Note;

export async function go() {
  const drafts = await Draft.objects.allWithDeleted().all();
  same<typeof drafts, Draft[]>();
  await Draft.objects.deletedOnly().all();
  await Draft.objects.undelete();
  await draft.undelete();
  await draft.hardDelete();
  const removed = await of([draft]).hardDelete();
  same<typeof removed, Draft[]>();
  await Post.objects.titled("a").allWithDeleted().all();
  // The row type of an inherited method is the root row type today; the rule for both languages is an open owner question.
  const titled = await Draft.objects.titled("a").all();
  same<typeof titled, Post[]>();
  Note.objects.deletedOnly(); // E:
  Note.objects.titled("a"); // E:
  of([note]).undelete(); // E:
}
`;
const EXPECTED = USE.split("\n").flatMap((line, i) => (line.includes("// E:") ? [i + 1] : []));
const skip = !artifactCapabilities().includes("proxy-models") || !hasExtension("orm.soft-delete");

test("a proxy has the soft-delete members and the query-set class of its root", { skip }, async () => {
  const root = mkdtempSync(join(tmpdir(), "orm-proxy-members-"));
  writeFileSync(join(root, "schema.prisma"), SCHEMA);
  writeFileSync(join(root, "package.json"), '{"type":"module"}');
  writeFileSync(join(root, "queries.ts"), QUERIES);
  writeFileSync(join(root, "use.ts"), USE);
  const args = ["--schema", join(root, "schema.prisma"), "generate", "typescript", "-o", join(root, "models.ts"), "--import", "orm"];
  assert.equal(await native().cli([...args, "--query-set", "Post=./queries.js#PostQueries"]), 0);
  const generated = readFileSync(join(root, "models.ts"), "utf8");
  assert.match(generated, /useQuerySet\(Post, \(\) => _q0\.PostQueries\)/);
  assert.match(generated, /useQuerySet\(Draft, \(\(\) => _q0\.PostQueries\) as never\)/);
  assert.doesNotMatch(generated, /useQuerySet\(Note/);
  const pkg = import.meta.dirname.replace(/(\/dist)?\/test$/, "");
  const src = join(pkg, "src");
  const options: ts.CompilerOptions = {
    target: ts.ScriptTarget.ES2022,
    lib: ["lib.es2022.d.ts"],
    module: ts.ModuleKind.NodeNext,
    moduleResolution: ts.ModuleResolutionKind.NodeNext,
    types: ["node"],
    typeRoots: [join(pkg, "node_modules", "@types")],
    strict: true,
    noUncheckedIndexedAccess: true,
    exactOptionalPropertyTypes: true,
    noEmit: true,
    skipLibCheck: true,
    baseUrl: root,
    paths: { orm: [join(src, "index.ts")], "orm/ext": [join(src, "ext.ts")] },
  };
  const program = ts.createProgram([join(root, "use.ts")], options);
  const errors = ts.getPreEmitDiagnostics(program).filter((d) => d.category === ts.DiagnosticCategory.Error);
  const lines = (file: string) => new Set(errors.filter((d) => d.file?.fileName === join(root, file)).map((d) => d.file!.getLineAndCharacterOfPosition(d.start!).line + 1));
  const message = errors.map((d) => `${d.file?.fileName.split("/").pop()}:${d.file && d.start !== undefined ? d.file.getLineAndCharacterOfPosition(d.start).line + 1 : 0} ${ts.flattenDiagnosticMessageText(d.messageText, "\n")}`).join("\n");
  assert.deepEqual([...lines("use.ts")].sort((a, b) => a - b), EXPECTED, message);
  assert.deepEqual([...lines("models.ts")], [], message);
});

test("a proxy gets the soft-delete members at run time through the record of the extension", { skip }, async () => {
  const models = (await loadsSoftDelete(SCHEMA, { registry: new Registry() })) as unknown as Record<string, { objects: Record<string, unknown> }>;
  for (const member of ["allWithDeleted", "deletedOnly", "undelete", "hardDelete"]) {
    assert.equal(typeof models.Draft!.objects[member], "function", member);
    assert.equal(typeof models.Post!.objects[member], "function", member);
  }
  assert.equal(models.Note!.objects.deletedOnly, undefined);
});

const url = process.env["ORM_TEST_DATABASE_URL"] ?? "";

test("an inherited query-set method returns rows of the proxy at run time", { skip: skip || !url }, async () => {
  const root = mkdtempSync(join(tmpdir(), "orm-proxy-members-run-"));
  writeFileSync(join(root, "schema.prisma"), SCHEMA);
  writeFileSync(join(root, "queries.ts"), QUERIES);
  const args = ["--schema", join(root, "schema.prisma"), "generate", "typescript", "-o", join(root, "models.ts"), "--import", "orm", "--query-set", "Post=./queries.js#PostQueries"];
  assert.equal(await native().cli(args), 0);
  const src = join(import.meta.dirname, "..", "src");
  const imports: Record<string, string> = { '"orm"': JSON.stringify(pathToFileURL(join(src, "index.js")).href), '"orm/ext"': JSON.stringify(pathToFileURL(join(src, "ext.js")).href) };
  const emit = (from: string, to: string) => {
    mkdirSync(to, { recursive: true });
    for (const name of readdirSync(from).filter((n) => n.endsWith(".ts") && !n.endsWith(".d.ts"))) {
      let { outputText } = ts.transpileModule(readFileSync(join(from, name), "utf8"), { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } });
      for (const [from, to] of Object.entries(imports)) outputText = outputText.replaceAll(` from ${from}`, ` from ${to}`);
      writeFileSync(join(to, name.replace(/\.ts$/, ".js")), outputText);
    }
  };
  const out = join(root, "js");
  emit(root, out);
  emit(join(root, "orm-ext", "orm-soft-delete"), join(out, "orm-ext", "orm-soft-delete"));
  // Bun runs every test file in one process, so the default registry already holds Post from another file.
  const modelsFile = join(out, "models.js");
  const isolated = readFileSync(modelsFile, "utf8").replace(/define\(SCHEMA(\)|, \{)/, (_, tail: string) => `define(SCHEMA, { registry: __registry${tail === ")" ? " })" : ","}`);
  assert.notEqual(isolated, readFileSync(modelsFile, "utf8"));
  writeFileSync(modelsFile, `import { Registry as __Registry } from ${imports['"orm"']};\nexport const __registry = new __Registry();\n${isolated}`);
  const { Draft, Post, __registry: registry } = (await import(pathToFileURL(join(out, "models.js")).href)) as Record<string, any>;
  const db = await connect(url, { registry, default: false });
  try {
    await db.execute("DROP TABLE IF EXISTS proxy_members_post");
    await db.execute("CREATE TABLE proxy_members_post (id INTEGER PRIMARY KEY, title TEXT NOT NULL, deleted_at TIMESTAMPTZ)");
    await Post.objects.using(db).insertMany([{ id: 1, title: "a" }, { id: 2, title: "b" }]);
    const rows = await Draft.objects.titled("a").using(db).all();
    assert.equal(rows.length, 1);
    assert.equal(rows[0].constructor.name, "Draft");
    assert.equal((await Post.objects.using(db).all())[0].constructor.name, "Post");
  } finally {
    await db.execute("DROP TABLE IF EXISTS proxy_members_post");
    await db.close();
  }
});
