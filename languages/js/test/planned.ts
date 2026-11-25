/** The SQL a migration would hold, without writing it: for tests that check generated statements. */
import { cpSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { Migrations, type Migration } from "../src/index.js";

export function statements(text: string): string[] {
  const out: string[] = [];
  for (const chunk of text.split("\n\n")) {
    const sql = chunk.split("\n").filter((line) => !line.startsWith("--")).join("\n").trim().replace(/;$/, "");
    if (sql) out.push(sql);
  }
  return out;
}

/** The up and down statements of the migration `migrations.make()` would write, in order. */
export function planned(migrations: Migrations, name?: string): [string[], string[]] {
  const scratch = mkdtempSync(join(tmpdir(), "monorm-planned-"));
  try {
    if (existsSync(migrations.directory)) cpSync(migrations.directory, scratch, { recursive: true });
    const made = new Migrations(scratch, migrations.source, migrations.options).make(name);
    return made === null ? [[], []] : [statements(made.upSql), statements(made.downSql)];
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

export function plannedUp(migrations: Migrations, name?: string): string[] {
  return planned(migrations, name)[0]!;
}

/** Adds an operation to the end of a written migration file. */
export function appendOp(migration: Migration, op: Record<string, unknown>): void {
  const data = JSON.parse(readFileSync(migration.path, "utf8")) as { operations: Record<string, unknown>[] };
  data.operations.push(op);
  writeFileSync(migration.path, `${JSON.stringify(data, null, 2)}\n`);
}

/** Writes a hand-made migration file `<name>.json` and returns its path. */
export function writeMigration(directory: string, name: string, operations: Record<string, unknown>[], top: Record<string, unknown> = {}): string {
  const path = join(directory, `${name}.json`);
  writeFileSync(path, `${JSON.stringify({ version: 1, ...top, operations }, null, 2)}\n`);
  return path;
}
