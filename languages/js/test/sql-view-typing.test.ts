// Type-checks a SQL view model: each `// E:` line must fail and no other line may.
import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import ts from "typescript";
import { main as cli } from "../src/cli.js";
import { scratch } from "./scratch.js";

const SCHEMA = `model User {
  id   Int @id
  name String
}

view ActiveUser {
  id   Int @id
  name String
}
`;
const USE = `import { ActiveUser, User } from "./models.js";

declare const row: ActiveUser;

export async function reads() {
  await ActiveUser.objects.filter(ActiveUser.id.eq(1)).all();
  await User.objects.insert({ id: 1, name: "a" });
  await User.objects.filter(User.id.eq(1)).delete();
}

export async function writes() {
  ActiveUser.objects.insert({ id: 1, name: "a" }); // E:
  ActiveUser.objects.insertMany([{ id: 1, name: "a" }]); // E:
  await ActiveUser.objects.getOrInsert({ id: 1, name: "a" }); // E:
  ActiveUser.objects.all().update({ name: "a" }); // E:
  ActiveUser.objects.updateMany([{ id: 1, name: "a" }]); // E:
  await row.update({ name: "a" }); // E:
  await row.delete(); // E:
  ActiveUser.objects.all().delete(); // E:
  ActiveUser.objects.filter(ActiveUser.id.eq(1)).delete(); // E:
}
`;
const EXPECTED = USE.split("\n").flatMap((line, i) => (line.includes("// E:") ? [i + 1] : []));

test("a view model type checks with no write call", async () => {
  const root = scratch("monorm-sv-typing-");
  writeFileSync(join(root, "schema.prisma"), SCHEMA);
  mkdirSync(join(root, "views", "public"), { recursive: true });
  writeFileSync(join(root, "views", "public", "ActiveUser.sql"), 'SELECT id, name FROM "user"');
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
