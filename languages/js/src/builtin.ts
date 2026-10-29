/**
 * What the `monorm` package bundles: extensions and apps that a project uses without a dependency on them.
 * The list is `"monorm": {"bundled": {"extensions": {id: dir}, "apps": {name: schema}}}` in the package's own `package.json`.
 * A listed path that is not on disk (a source checkout before `scripts/assemble-bundle.mjs` ran) is left out.
 */

import { existsSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const NAME = "monorm";

export interface BundledExtension { id: string; manifest: string; specifier: string }

interface Bundled { extensions?: Record<string, string>; apps?: Record<string, string> }

/** The root of this package: the nearest directory above this file whose `package.json` is named `monorm`. */
export const packageRoot: string = (() => {
  for (let dir = dirname(fileURLToPath(import.meta.url)); ; dir = dirname(dir)) {
    const file = join(dir, "package.json");
    if (existsSync(file) && (JSON.parse(readFileSync(file, "utf8")) as { name?: string }).name === NAME) return realpathSync(dir);
    if (dirname(dir) === dir) throw new Error(`the ${NAME} package root is not above ${fileURLToPath(import.meta.url)}`);
  }
})();

let cached: Bundled | undefined;

function bundled(): Bundled {
  cached ??= ((JSON.parse(readFileSync(join(packageRoot, "package.json"), "utf8")) as { monorm?: { bundled?: Bundled } }).monorm?.bundled ?? {});
  return cached;
}

/** The bundled extensions by id. `specifier` is the runtime module name; it resolves from the package root by self-reference. */
export function bundledExtensions(): Record<string, BundledExtension> {
  const found: Record<string, BundledExtension> = {};
  for (const [id, dir] of Object.entries(bundled().extensions ?? {})) {
    const manifest = join(packageRoot, dir, "monorm-extension.toml");
    if (existsSync(manifest)) found[id] = { id, manifest, specifier: `${NAME}/${dir.replace(/^\.?\//, "")}` };
  }
  return found;
}

/** The schema file of the bundled app `name`, or undefined. */
export function bundledApp(name: string): string | undefined {
  const file = bundled().apps?.[name];
  return file !== undefined && existsSync(join(packageRoot, file)) ? realpathSync(join(packageRoot, file)) : undefined;
}
