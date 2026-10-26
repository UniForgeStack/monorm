import { mkdirSync, mkdtempSync } from "node:fs";
import { join } from "node:path";

// Under this package, so the schema finds the installed extension packages as direct dependencies.
export function scratch(prefix: string): string {
  const base = join(import.meta.dirname, "..", "..", ".test-tmp");
  mkdirSync(base, { recursive: true });
  return mkdtempSync(join(base, prefix));
}
