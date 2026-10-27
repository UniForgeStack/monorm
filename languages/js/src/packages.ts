/**
 * Finds the installed extension packages: the direct dependencies of the project's `package.json`
 * whose own `package.json` has `"orm": {"extension": "<id>"}`. The manifest is `orm-extension.toml`
 * in the package root. Discovery reads metadata; definition imports the selected runtime packages.
 */

import { createRequire } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";
import { moduleResolve } from "import-meta-resolve";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

import { SchemaError } from "./errors.js";

const MANIFEST = "orm-extension.toml";
const DEPENDENCY_KEYS = ["dependencies", "devDependencies", "optionalDependencies"] as const;

interface InstalledPackage { manifest: string; name: string; root: string }

type PackageJson = { orm?: { extension?: unknown } } & Partial<Record<(typeof DEPENDENCY_KEYS)[number], Record<string, string>>>;

function readJson(file: string): PackageJson {
  try {
    return JSON.parse(readFileSync(file, "utf8")) as PackageJson;
  } catch (e) {
    throw new SchemaError(`${file}: ${e instanceof Error ? e.message : String(e)}`);
  }
}

/** The directory of the nearest `package.json` at or above `from`. */
function projectRoot(from: string): string | undefined {
  for (let dir = resolve(from); ; dir = dirname(dir)) {
    if (existsSync(join(dir, "package.json"))) return dir;
    if (dirname(dir) === dir) return undefined;
  }
}

/** The root of package `name` as Node resolves it from `from`; undefined when it is not installed. */
function installedRoot(name: string, from: string): string | undefined {
  for (let dir = from; ; dir = dirname(dir)) {
    if (existsSync(join(dir, "node_modules", name, "package.json"))) return realpathSync(join(dir, "node_modules", name));
    if (dirname(dir) === dir) return undefined;
  }
}

/**
 * Installed extensions that the project of `schema` depends on directly.
 * Without a schema file, the project is the one of the working directory.
 */
function discover(schema?: string): Record<string, InstalledPackage> {
  const root = projectRoot(schema === undefined ? process.cwd() : dirname(resolve(schema)));
  if (root === undefined) return {};
  const project = readJson(join(root, "package.json"));
  const names = new Set(DEPENDENCY_KEYS.flatMap((key) => Object.keys(project[key] ?? {})));
  const found: Record<string, InstalledPackage> = {};
  for (const name of [...names].sort()) {
    const dir = installedRoot(name, root);
    if (dir === undefined) continue;
    const id = readJson(join(dir, "package.json")).orm?.extension;
    if (id === undefined) continue;
    if (typeof id !== "string") throw new SchemaError(`${join(dir, "package.json")}: "orm": {"extension"} is the extension id, a string`);
    const manifest = join(dir, MANIFEST);
    if (!existsSync(manifest)) throw new SchemaError(`the package \`${name}\` declares extension \`${id}\` but has no ${MANIFEST} file in ${dir}`);
    if (id in found && found[id]?.manifest !== manifest) {
      throw new SchemaError(`extension \`${id}\` is installed twice: ${found[id]?.manifest} and ${manifest}; uninstall one of the packages`);
    }
    found[id] = { manifest, name, root };
  }
  return found;
}

/** Extension ids mapped to manifest files, without importing runtime code. */
export function resolveExtensions(schema?: string): Record<string, string> {
  return Object.fromEntries(Object.entries(discover(schema)).map(([id, p]) => [id, p.manifest]));
}

/** Package directories of the extensions whose hooks the schema binds, by extension id. */
export function hookPackages(behavior: { hooks?: readonly { source: string }[]; packages?: Record<string, unknown> } | undefined, schema?: string): Record<string, string> | undefined {
  const ids = new Set((behavior?.hooks ?? []).map((h) => h.source).filter((id) => id in (behavior?.packages ?? {})));
  if (!ids.size) return undefined;
  const found = discover(schema);
  return Object.fromEntries([...ids].flatMap((id) => (found[id] === undefined ? [] : [[id, dirname(found[id].manifest)]])));
}

/** Extension ids mapped to runtime import names, without importing runtime code. */
export function runtimeModules(schema?: string): Record<string, string> {
  return Object.fromEntries(Object.entries(discover(schema)).map(([id, p]) => [id, p.name]));
}

export function importRuntime(id: string, schema?: string): Readonly<Record<string, unknown>> {
  const pkg = discover(schema)[id];
  if (pkg === undefined) throw new SchemaError(`extension ${id}: runtime package is not installed`);
  const parent = join(pkg.root, "package.json");
  // Resolve with import conditions so generated imports and synchronous loading share class identities.
  const conditions = new Set(["node", "import", ...("Bun" in globalThis ? ["bun"] : ["module-sync"])]);
  const target = moduleResolve(pkg.name, pathToFileURL(parent), conditions);
  return createRequire(parent)(fileURLToPath(target)) as Readonly<Record<string, unknown>>;
}
