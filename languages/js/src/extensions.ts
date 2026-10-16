/** Extension language code at runtime: the check against the native artifact and the member rules. */
import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import type { ModelMeta } from "./model.js";
import { native } from "./native.js";
import type { QuerySet } from "./query.js";

/** What a generated models module passes to `define()` for one extension. */
export interface ExtensionCode {
  readonly module: Readonly<Record<string, unknown>>;
  /** The hash of the extension build that generated the files. */
  readonly hash: string;
  /** The directory of the generated files. */
  readonly dir: URL;
}

interface LanguageRecord {
  readonly extension: string;
  readonly version: string;
  readonly language: string;
  readonly query_set: string;
  readonly instance: string;
  readonly query_set_members: readonly string[];
  readonly instance_members: readonly string[];
  readonly overrides: readonly string[];
  readonly hash: string;
}

export type Mixin = (base: new (...args: any[]) => QuerySet<never>) => new (...args: any[]) => QuerySet<never>;

function records(): Map<string, LanguageRecord> {
  const artifact = JSON.parse(native().nativeArtifact()) as { language_members?: LanguageRecord[] };
  return new Map((artifact.language_members ?? []).filter((r) => r.language === "typescript").map((r) => [r.extension, r]));
}

const HEADER_LINES = 3;

function digest(files: readonly (readonly [string, Buffer])[]): string {
  const sha = createHash("sha256");
  const length = (n: number) => {
    const b = Buffer.alloc(8);
    b.writeBigUInt64LE(BigInt(n));
    return b;
  };
  for (const [name, content] of [...files].sort((a, b) => Buffer.compare(Buffer.from(a[0]), Buffer.from(b[0])))) {
    const raw = Buffer.from(name);
    sha.update(length(raw.length)).update(raw).update(length(content.length)).update(content);
  }
  return sha.digest("hex");
}

function sources(dir: string): string[] {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return [];
  }
  return entries.flatMap((e) =>
    e.isDirectory() ? sources(join(dir, e.name)) : e.name.endsWith(".ts") && !e.name.endsWith(".d.ts") ? [join(dir, e.name)] : [],
  );
}

/**
 * The artifact record of `id`, after a check that the generated code was made for this artifact.
 * Where the `.ts` files sit next to the module, their content is checked too; a compiled
 * project that has only `.js` files gets the check of the baked hash alone.
 */
function check(id: string, code: ExtensionCode): LanguageRecord {
  const record = records().get(id);
  if (record === undefined) throw new TypeError(`generated models use extension ${id}, which the native artifact does not have; run \`orm generate\``);
  if (code.hash !== record.hash) throw new TypeError(`the generated code of extension ${id} was made for another build; run \`orm generate\``);
  if (code.dir.protocol !== "file:") return record;
  const root = fileURLToPath(code.dir);
  const files = sources(root);
  if (!files.length) return record;
  const parsed = files.map((path): [string, Buffer] => {
    const bytes = readFileSync(path);
    let start = 0;
    for (let i = 0; i < HEADER_LINES; i++) {
      const at = bytes.indexOf(10, start);
      if (at < 0) throw new TypeError(`${path}: not a generated extension file for ${id}; run \`orm generate\``);
      if (i === 1 && bytes.subarray(start, at).toString() !== `// orm-ext ${id} ${record.version} ${record.hash}`) {
        throw new TypeError(`${path}: not a generated extension file for ${id}; run \`orm generate\``);
      }
      start = at + 1;
    }
    return [relative(root, path).split("\\").join("/"), bytes.subarray(start)];
  });
  if (digest(parsed) !== record.hash) throw new TypeError(`the generated files of extension ${id} in ${root} were edited; run \`orm generate\``);
  return record;
}

/**
 * Gives `meta` the members of the extensions `ids` (dependencies first): the query-set
 * mixins go to `meta.extensionMixins`, the instance members onto the row prototype.
 * A member name that a host member, a field, a relation or an earlier extension has is an
 * error, unless the extension declares an override.
 */
export function applyExtensions(meta: ModelMeta, ids: readonly string[], extensions: Readonly<Record<string, ExtensionCode>>, hostQuerySetMember: (name: string) => boolean): void {
  const given = new Map<string, string>();
  const mixins: Mixin[] = [];
  for (const id of ids) {
    const code = extensions[id];
    if (code === undefined) throw new TypeError(`${meta.name}: extension ${id} is not in the generated extensions`);
    const record = check(id, code);
    const overrides = new Set(record.overrides);
    const mixin = code.module[record.query_set];
    const instance = code.module[record.instance];
    if (typeof mixin !== "function" || typeof instance !== "function") throw new TypeError(`extension ${id} has no ${record.query_set} or ${record.instance}; run \`orm generate\``);
    const rules: [string, readonly string[], (name: string) => boolean][] = [
      ["query set", record.query_set_members, hostQuerySetMember],
      ["instance", record.instance_members, (name) => name in meta.Row.prototype],
    ];
    for (const [kind, names, host] of rules) {
      for (const name of names) {
        if (meta.fields.has(name) || meta.relations.has(name)) throw new TypeError(`${meta.name}.${name}: extension ${id} ${kind} member collides with a field or a relation`);
        if (overrides.has(name)) continue;
        if (given.has(name)) throw new TypeError(`${meta.name}.${name}: extension ${id} ${kind} member collides with a member of extension ${given.get(name)}`);
        if (host(name)) throw new TypeError(`${meta.name}.${name}: extension ${id} ${kind} member collides with a host member; declare it as an override`);
      }
    }
    for (const name of [...record.query_set_members, ...record.instance_members]) if (!given.has(name)) given.set(name, id);
    mixins.push(mixin as Mixin);
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors((instance as { prototype: object }).prototype))) {
      if (key !== "constructor") Object.defineProperty(meta.Row.prototype, key, { ...descriptor, configurable: true });
    }
  }
  meta.extensionMixins = mixins;
  meta.resetQuerySet(meta.querySet);
}
