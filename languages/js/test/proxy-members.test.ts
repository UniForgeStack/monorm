// A proxy of a model inherits its root's extension members and its root's QuerySet class.
// Runs only with an artifact that selects the proxy-models and soft-delete extensions.
// Each `// E:` line of the typing use must fail and no other line may.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import ts from "typescript";
import { Registry } from "../src/index.js";
import { native } from "../src/native.js";
import { artifactCapabilities, hasExtension } from "./artifact-caps.js";
import { loadsSoftDelete } from "./soft-delete-ext.js";

const SCHEMA = `model Post {
  id         Int       @id
  title      String
  deleted_at DateTime? @soft_delete.deleted_at
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
  await Draft.objects.titled("a").all();
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
