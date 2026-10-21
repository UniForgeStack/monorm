/**
 * Finds the schema of each installed app that a schema imports with `import "app:<name>"`.
 *
 * Build time only (`orm generate`, `orm migrate`): generated models embed the compiled schema.
 * An app declares its schema in its `package.json`: `"orm": {"app": "./schema.prisma"}`.
 * Resolution is the one of Node (`node_modules` from the importing schema's real directory), so an
 * app that only another app depends on resolves under npm, pnpm and bun.
 */

import { existsSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

import { SchemaError } from "./errors.js";

const IMPORT = /^\s*import\s+"([^"]+)"/gm;

/** The schema file of app `name` as seen from directory `from`; undefined when no such package is installed. */
function schemaOf(name: string, from: string): string | undefined {
  for (let dir = from; ; dir = dirname(dir)) {
    const manifest = join(dir, "node_modules", name, "package.json");
    if (existsSync(manifest)) {
      const orm = (JSON.parse(readFileSync(manifest, "utf8")) as { orm?: { app?: unknown } }).orm;
      if (typeof orm?.app !== "string") {
        throw new SchemaError(
          `import "app:${name}": the \`${name}\` package declares no schema; its package.json needs "orm": {"app": "./schema.prisma"}`,
        );
      }
      const file = resolve(dirname(manifest), orm.app);
      if (!existsSync(file)) throw new SchemaError(`import "app:${name}": ${orm.app} of \`${name}\` is not a file (${file})`);
      // the real path: a pnpm symlink would hide the app's own node_modules siblings
      return realpathSync(file);
    }
    if (dirname(dir) === dir) return undefined;
  }
}

/** `{name: schema file}` of every app the schema, its imports and their apps import.
 * An app that is not installed is left out: the schema loader reports it. */
export function resolveApps(schema: string): Record<string, string> {
  const apps: Record<string, string> = {};
  const pending = [resolve(schema)];
  const seen = new Set<string>();
  while (pending.length > 0) {
    const file = pending.pop()!;
    if (seen.has(file)) continue;
    seen.add(file);
    let text: string;
    try {
      text = readFileSync(file, "utf8");
    } catch {
      continue; // the loader reports an unreadable file
    }
    const here = dirname(file);
    for (const [, target] of text.matchAll(IMPORT)) {
      if (target!.startsWith("app:")) {
        const name = target!.slice("app:".length);
        const found = name in apps ? undefined : schemaOf(name, here);
        if (found !== undefined) {
          apps[name] = found;
          pending.push(found);
        }
      } else if (target!.endsWith(".prisma")) {
        pending.push(resolve(dirname(file), target!));
      }
    }
  }
  return apps;
}
