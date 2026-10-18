// Load a schema with the generated soft-delete extension members, as a generated models module does.
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import ts from "typescript";
import { define, loads as plainLoads, type Registry } from "../src/index.js";
import { call, native } from "../src/native.js";

export const ID = "orm.soft-delete";
type Code = { module: Record<string, unknown>; hash: string; dir: URL };
let code: Promise<Code> | undefined;

/** The `orm-ext` package that `orm generate` writes for the loaded artifact, made once for the run. */
function generated(): Promise<Code> {
  code ??= (async () => {
    const root = mkdtempSync(join(tmpdir(), "orm-soft-delete-"));
    writeFileSync(join(root, "schema.prisma"), "model Probe {\n  id Int @id\n}\n");
    const exit = await native().cli(["--schema", join(root, "schema.prisma"), "generate", "typescript", "-o", join(root, "models.ts"), "--import", "orm"]);
    if (exit !== 0) throw new Error("orm generate failed");
    const from = join(root, "orm-ext", "orm-soft-delete");
    const out = join(root, "js");
    mkdirSync(out);
    const host = pathToFileURL(join(import.meta.dirname, "..", "src", "ext.js")).href;
    for (const name of readdirSync(from).filter((n) => n.endsWith(".ts"))) {
      const { outputText } = ts.transpileModule(readFileSync(join(from, name), "utf8"), {
        compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
      });
      writeFileSync(join(out, name.replace(/\.ts$/, ".js")), outputText.replaceAll('"orm/ext"', JSON.stringify(host)));
    }
    const record = (JSON.parse(native().nativeArtifact()).language_members as { extension: string; language: string; hash: string }[])
      .find((r) => r.extension === ID && r.language === "typescript")!;
    return { module: (await import(pathToFileURL(join(out, "index.js")).href)) as Record<string, unknown>, hash: record.hash, dir: pathToFileURL(out + "/") };
  })();
  return code;
}

/** `loads`, with the soft-delete members on each model that declares `@soft_delete.deleted_at`. */
export async function loadsSoftDelete(source: string, options: { registry: Registry }): Promise<ReturnType<typeof plainLoads>> {
  const ir = call(() => native().compileSchema(source, null));
  const declarations = (JSON.parse(ir).behavior?.declarations ?? []) as { attribute: string; model: string }[];
  const models = [...new Set(declarations.filter((d) => d.attribute.startsWith("soft_delete.")).map((d) => d.model))];
  return define(ir, { registry: options.registry, extensions: { [ID]: await generated() }, members: Object.fromEntries(models.map((m) => [m, [ID]])) } as never);
}
