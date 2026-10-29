// Type-checks the generated soft-delete members: each `// E:` line must fail and no other line may.
// Runs only with an artifact that selects the soft-delete extension.
import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import ts from "typescript";
import { main as cli } from "../src/cli.js";
import { hasExtension } from "./artifact-caps.js";
import { scratch } from "./scratch.js";

const SCHEMA = `model Post {
  id         Int       @id
  title      String
  @@soft_delete.deleted_at
}

model Tag {
  id Int @id
}
`;
const USE = `import { of } from "monorm";
import { Post, Tag } from "./models.js";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
declare function same<A, B>(...check: Equal<A, B> extends true ? [] : [error: "types differ"]): void;
declare const post: Post;
declare const tag: Tag;

export async function go() {
  await Post.objects.deletedOnly().all();
  await Post.objects.allWithDeleted().filter(Post.id.eq(1)).all();
  await Post.objects.hardDelete();
  await Post.objects.undelete();
  await post.undelete();
  await post.hardDelete();
  const bound = of([post]);
  const removed = await bound.hardDelete();
  same<typeof removed, Post[]>();
  const restored = await bound.undelete();
  same<typeof restored, Post[]>();
  await bound.allWithDeleted().deletedOnly().hardDelete();
  Tag.objects.deletedOnly(); // E:
  Tag.objects.allWithDeleted(); // E:
  Tag.objects.hardDelete(); // E:
  Tag.objects.undelete(); // E:
  of([tag]).hardDelete(); // E:
  of([tag]).undelete(); // E:
  of([tag]).deletedOnly(); // E:
  of([tag]).allWithDeleted(); // E:
}
`;
const EXPECTED = USE.split("\n").flatMap((line, i) => (line.includes("// E:") ? [i + 1] : []));

test("the generated soft-delete members type-check on the soft-delete model only", { skip: !hasExtension("monorm.soft-delete") }, async () => {
  const root = scratch("monorm-sd-typing-");
  writeFileSync(join(root, "schema.prisma"), SCHEMA);
  const exit = await cli(["--schema", join(root, "schema.prisma"), "generate", "typescript", "-o", join(root, "models.ts"), "--import", "monorm"]);
  assert.equal(exit, 0);
  writeFileSync(join(root, "package.json"), '{"type":"module"}');
  writeFileSync(join(root, "use.ts"), USE);
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
    paths: { "monorm": [join(src, "index.ts")], "monorm/ext": [join(src, "ext.ts")], "monorm/extensions/*": [join(pkg, "extensions", "*", "index.d.ts")] },
  };
  const program = ts.createProgram([join(root, "use.ts")], options);
  const errors = ts.getPreEmitDiagnostics(program).filter((d) => d.category === ts.DiagnosticCategory.Error);
  const lines = (file: string) => new Set(errors.filter((d) => d.file?.fileName === join(root, file)).map((d) => d.file!.getLineAndCharacterOfPosition(d.start!).line + 1));
  const message = errors.map((d) => `${d.file?.fileName.split("/").pop()}:${d.file && d.start !== undefined ? d.file.getLineAndCharacterOfPosition(d.start).line + 1 : 0} ${ts.flattenDiagnosticMessageText(d.messageText, "\n")}`).join("\n");
  assert.deepEqual([...lines("use.ts")].sort((a, b) => a - b), EXPECTED, message);
  assert.deepEqual([...lines("models.ts")], [], message);
});
