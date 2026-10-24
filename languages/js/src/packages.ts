/**
 * Finds the installed extension packages: the direct dependencies of the project's `package.json`
 * whose own `package.json` has `"orm": {"extension": "<id>"}`. The manifest is `orm-extension.toml`
 * in the package root. No extension package is imported. The Rust loader selects the extensions that a schema uses.
 */

import { existsSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

import { SchemaError } from "./errors.js";

const MANIFEST = "orm-extension.toml";
const DEPENDENCY_KEYS = ["dependencies", "devDependencies", "optionalDependencies"] as const;

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
 * `{extension id: manifest file}` of the extension packages that the project of `schema` depends on directly.
 * Without a schema file, the project is the one of the working directory.
 */
export function resolveExtensions(schema?: string): Record<string, string> {
  const root = projectRoot(schema === undefined ? process.cwd() : dirname(resolve(schema)));
  if (root === undefined) return {};
  const project = readJson(join(root, "package.json"));
  const names = new Set(DEPENDENCY_KEYS.flatMap((key) => Object.keys(project[key] ?? {})));
  const found: Record<string, string> = {};
  for (const name of [...names].sort()) {
    const dir = installedRoot(name, root);
    if (dir === undefined) continue;
    const id = readJson(join(dir, "package.json")).orm?.extension;
    if (id === undefined) continue;
    if (typeof id !== "string") throw new SchemaError(`${join(dir, "package.json")}: "orm": {"extension"} is the extension id, a string`);
    const manifest = join(dir, MANIFEST);
    if (!existsSync(manifest)) throw new SchemaError(`the package \`${name}\` declares extension \`${id}\` but has no ${MANIFEST} file in ${dir}`);
    if (id in found && found[id] !== manifest) {
      throw new SchemaError(`extension \`${id}\` is installed twice: ${found[id]} and ${manifest}; uninstall one of the packages`);
    }
    found[id] = manifest;
  }
  return found;
}
