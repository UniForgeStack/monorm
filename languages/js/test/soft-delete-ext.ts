// Load a schema with the generated extension members (soft-delete and locking), as a generated models module does.
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import ts from "typescript";
import { define, loads as plainLoads, type Registry } from "../src/index.js";
import { call, native } from "../src/native.js";

export const ID = "orm.soft-delete";
type Code = { module: Record<string, unknown>; hash: string; dir: URL };
type Extension = { id: string; dir: string; attribute: string };
const EXTENSIONS: readonly Extension[] = [
  { id: ID, dir: "orm-soft-delete", attribute: "soft_delete." },
  { id: "orm.locking", dir: "orm-locking", attribute: "locking." },
];
let code: Promise<Map<string, Code>> | undefined;

/** The `orm-ext` packages that `orm generate` writes for the loaded artifact, made once for the run. */
function generated(): Promise<Map<string, Code>> {
  code ??= (async () => {
    const root = mkdtempSync(join(tmpdir(), "orm-soft-delete-"));
    writeFileSync(join(root, "schema.prisma"), "model Probe {\n  id Int @id\n}\n");
    const exit = await native().cli(["--schema", join(root, "schema.prisma"), "generate", "typescript", "-o", join(root, "models.ts"), "--import", "orm"]);
    if (exit !== 0) throw new Error("orm generate failed");
    const host = pathToFileURL(join(import.meta.dirname, "..", "src", "ext.js")).href;
    const index = pathToFileURL(join(import.meta.dirname, "..", "src", "index.js")).href;
    const records = JSON.parse(native().nativeArtifact()).language_members as { extension: string; language: string; hash: string }[];
    const found = new Map<string, Code>();
    for (const extension of EXTENSIONS) {
      const record = records.find((r) => r.extension === extension.id && r.language === "typescript");
      if (record === undefined) continue;
      const from = join(root, "orm-ext", extension.dir);
      const out = join(root, "js", extension.dir);
      mkdirSync(out, { recursive: true });
      for (const name of readdirSync(from).filter((n) => n.endsWith(".ts"))) {
        const { outputText } = ts.transpileModule(readFileSync(join(from, name), "utf8"), {
          compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
        });
        writeFileSync(join(out, name.replace(/\.ts$/, ".js")), outputText.replaceAll('"orm/ext"', JSON.stringify(host)).replaceAll('"orm"', JSON.stringify(index)));
      }
      found.set(extension.id, { module: (await import(pathToFileURL(join(out, "index.js")).href)) as Record<string, unknown>, hash: record.hash, dir: pathToFileURL(out + "/") });
    }
    return found;
  })();
  return code;
}

/** `loads`, with the soft-delete and locking members on each model the extensions have a record for, a proxy too. */
export async function loadsSoftDelete(source: string, options: { registry: Registry }): Promise<ReturnType<typeof plainLoads>> {
  const ir = call(() => native().compileSchema(source, null));
  const behavior = JSON.parse(ir).behavior ?? {};
  const declarations = (behavior.declarations ?? []) as { attribute: string; model: string }[];
  const records = Object.values(behavior).filter(Array.isArray).flat() as { source?: string; model?: string }[];
  const found = await generated();
  const members: Record<string, string[]> = {};
  const extensions: Record<string, Code> = {};
  for (const extension of EXTENSIONS) {
    const code = found.get(extension.id);
    if (code === undefined) continue;
    extensions[extension.id] = code;
    const declared = declarations.filter((d) => d.attribute.startsWith(extension.attribute)).map((d) => d.model);
    const named = records.filter((r) => r.source === extension.id && r.model !== undefined).map((r) => r.model!);
    for (const model of new Set([...declared, ...named])) (members[model] ??= []).push(extension.id);
  }
  return define(ir, { registry: options.registry, extensions, members } as never);
}
