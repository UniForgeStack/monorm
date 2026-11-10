import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

/** Imports the `module:attr` (or `./file.ts:attr`) of a setting. The error names the setting as `what`. */
export async function loadSpec(spec: string, what: string): Promise<unknown> {
  const at = spec.lastIndexOf(":");
  const file = spec.slice(0, at);
  const target = file.startsWith(".") || file.startsWith("/") ? pathToFileURL(resolve(file)).href : file;
  try {
    let value: unknown = await import(target);
    for (const part of spec.slice(at + 1).split(".")) value = (value as Record<string, unknown> | undefined)?.[part];
    if (value === undefined) throw new Error(`no ${spec.slice(at + 1)} in ${file}`);
    return value;
  } catch (e) {
    throw new Error(`${what} \`${spec}\`: ${e instanceof Error ? e.message : String(e)}`);
  }
}
