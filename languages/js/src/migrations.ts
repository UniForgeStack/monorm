/**
 * Migrations: generated from the schema file, stored as JSON operations, applied in order. The
 * same files and bookkeeping as the Python package (`monorm_migrations`, SHA-256 checksums), so
 * either can manage a database. A `run_code` operation names a Python or a TypeScript function;
 * only its own host runs it.
 *
 * ```
 * migrations/
 *   0001_initial.json    {"version": 1, "operations": [...]}
 *   0002_backfill.json
 *   0002_backfill.ts     a script the file names in a `run_code` operation
 * ```
 *
 * Generating a migration (`crates/core/src/migrate`) replays the operations of the existing files
 * to get the schema they reach, and diffs the schema against it, so it needs no database.
 * Applying and reverting (`crates/engine/src/migrate.rs`) records each migration with a checksum
 * and refuses to continue if an applied file changed. Both are the Rust code the `monorm` command
 * line and the Python package run too.
 *
 * A live database under migrations: `pull()` writes its schema file, `Migrator.baseline()` marks
 * the first migration as applied (it does not run it), and `Migrator.drift()` compares the
 * database with the schema the migrations reach.
 */

import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

import { resolveApps } from "./apps.js";
import { resolveExtensions } from "./packages.js";
import { connect, inTransaction, restoreCause, type Database } from "./db.js";
import { MigrationError } from "./errors.js";
import { Registry, load, registry as defaultRegistry } from "./model.js";
import { call, native, wait, type NativeMigrationPlan, type NativeSchema } from "./native.js";

export { MigrationError };

export interface Step {
  readonly summary: string;
  readonly sql: string;
  readonly warning?: string | null;
  /** The step can lose rows; `allowDestructive` lets it run on a table with rows. */
  readonly destructive: boolean;
  /** `false` for a step PostgreSQL refuses in a transaction (`CREATE INDEX CONCURRENTLY`). */
  readonly transaction: boolean;
}

/** One operation of the migration `Migrations.plan()` would write. */
export interface PlanOperation {
  readonly summary: string;
  readonly warning: string | null;
  /** The operation can lose rows; `allowDestructive` lets it run on a table with rows. */
  readonly destructive: boolean;
  /** `false` for an operation that runs outside a transaction (`CREATE INDEX CONCURRENTLY`). */
  readonly atomic: boolean;
}

/** What the next migration would contain. */
export interface Plan {
  readonly name: string;
  readonly operations: readonly PlanOperation[];
}

export class Migration {
  constructor(
    readonly name: string,
    /** The JSON file. */
    readonly path: string,
    private readonly schema: NativeSchema | null = null,
  ) {}

  /** The operations of the file, as JSON objects. */
  get operations(): Record<string, unknown>[] {
    return (JSON.parse(readFileSync(this.path, "utf8")) as { operations: Record<string, unknown>[] }).operations;
  }

  /** The numbered operations with their SQL, for the schema's dialect (what `sqlmigrate` prints). */
  get upSql(): string {
    return this.render(false);
  }

  get downSql(): string {
    return this.render(true);
  }

  private render(down: boolean): string {
    if (this.schema === null) throw new MigrationError("rendering SQL needs the schema: use Migrations.get() or Migrations.sqlmigrate()");
    return call(() => this.schema!.sqlmigrate(dirname(this.path), this.name, down));
  }

  get checksum(): string {
    return call(() => native().migrationChecksum(dirname(this.path), this.name));
  }

  toString(): string {
    return this.name;
  }
}

/** A schema file path, a registry (e.g. of models from `load()`), or a compiled schema. */
export type SchemaSource = Registry | NativeSchema | string;

function nativeSchema(schema: SchemaSource): NativeSchema {
  if (schema instanceof Registry) {
    return schema.native();
  }
  if (typeof schema === "string") {
    return call(() => new (native().Schema)(native().compileSchemaFile(schema, resolveApps(schema), resolveExtensions(schema))));
  }
  return schema;
}

/**
 * A migrations directory and the schema it migrates to. `online: true` writes PostgreSQL migrations
 * in the online form (the `migrations.online` setting of the command line): an index on an existing
 * table in an `"atomic": false` operation. It changes nothing on SQLite. `nameTemplate` is the file name template (the
 * `migrations.name` setting; default `"{number}_{slug}"`).
 */
export class Migrations {
  constructor(
    readonly directory: string,
    readonly source: SchemaSource,
    readonly options: { readonly online?: boolean; readonly nameTemplate?: string } = {},
  ) {}

  get schema(): NativeSchema {
    return nativeSchema(this.source);
  }

  all(): Migration[] {
    const schema = this.schema;
    return call(() => native().listMigrations(this.directory)).map(([name, path]) => new Migration(name!, path!, schema));
  }

  /** A migration by file name or order key (`"3"`, `"0003_add_tags"`). */
  get(name: string): Migration {
    const [found, path] = call(() => native().findMigration(this.directory, name));
    return new Migration(found!, path!, this.schema);
  }

  /** What the next migration would contain (no operations if the schema is unchanged). */
  plan(name?: string): Plan {
    return JSON.parse(call(() => this.schema.planMigration(this.directory, this.options.nameTemplate ?? null, this.options.online ?? false, name ?? null))) as Plan;
  }

  /** Writes the next migration and returns it, or `null` when there is nothing to migrate.
   * `empty` writes one even without schema changes (for `run_sql`, `run_code` or `separate`
   * operations you add by hand). */
  make(name?: string, options: { readonly empty?: boolean } = {}): Migration | null {
    return this.write(name, options.empty ?? false, false);
  }

  /** `make` with the baseline form: a trigger that replaces a database rule is left out, and the rule is recorded. */
  write(name: string | undefined, empty: boolean, baseline: boolean): Migration | null {
    const schema = this.schema;
    const made = call(() => schema.makeMigration(this.directory, name ?? null, empty, this.options.online ?? false, baseline, this.options.nameTemplate ?? null));
    return made === null ? null : new Migration(made, join(this.directory, `${made}.json`), schema);
  }

  /** The numbered operations of a migration with their SQL, in one direction. */
  sqlmigrate(name: string, options: { readonly down?: boolean } = {}): string {
    return call(() => this.schema.sqlmigrate(this.directory, name, options.down ?? false));
  }

  /** Folds the migrations 1..`to` (a number or a name) into one file.
   * The first run of schema operations becomes one diff from an empty database. A `run_sql` or
   * `run_code` with `elidable: true` is dropped; one without it stops the squash. `separate`,
   * `commit` and the operations after them stay as written.
   * The new file lists the replaced migrations; a database that applied all of them adopts it with no SQL.
   * Keep the old files until every database has passed them. Reads no database. */
  squash(options: { readonly to: string; readonly name?: string }): Migration {
    const [name, path] = call(() => this.schema.squashMigration(this.directory, options.to, options.name ?? null, this.options.nameTemplate ?? null));
    return new Migration(name!, path!, this.schema);
  }
}

/** A schema file read from a live database (`pull()`). */
export class Pulled {
  constructor(
    readonly schema: string,
    /** What the schema leaves out (rules, views, unsupported types, ...), one line each. */
    readonly gaps: readonly string[],
    /** What a migration from `schema` would still change on the database; empty when the
     * schema reproduces it. */
    readonly differences: readonly Step[],
    /** The SQL body of each view, by its path (`views/<db schema>/<Name>.sql`) next to the schema file. */
    readonly views: Readonly<Record<string, string>> = {},
  ) {}

  /** Writes the schema file and the view bodies next to it. */
  write(path: string): void {
    writeFileSync(path, this.schema);
    for (const [name, body] of Object.entries(this.views)) {
      const file = join(dirname(path), name);
      mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, body);
    }
  }
}

/**
 * Reads the database `db` connects to (Postgres: its current schema) as a schema file,
 * and checks the result by creating it again in a shadow schema that is rolled back
 * (SQLite: an in-memory database).
 */
export async function pull(db: Database): Promise<Pulled> {
  const out = JSON.parse(await wait(() => db.engine.pullSchema(resolveExtensions()))) as {
    schema: string;
    gaps: string[];
    steps: Step[];
    views: Record<string, string>;
  };
  return new Pulled(out.schema, out.gaps, out.steps, out.views);
}

/** The live database against the schema the migrations reach (`Migrator.drift()`). */
export interface Drift {
  /** The migration compared with, or `null` for an empty directory. */
  readonly migration: string | null;
  /** Steps that bring the database to that schema; empty when they match. */
  readonly steps: readonly Step[];
  /** Live objects drift does not compare (rules, views, ...). */
  readonly gaps: readonly string[];
}

type Code = (db: Database) => Promise<unknown>;

/** The function of a `file:function` entry (the file is relative to `directory`). */
async function loadCode(directory: string, entry: string): Promise<Code> {
  const cut = entry.lastIndexOf(":");
  const fileName = cut < 0 ? "" : entry.slice(0, cut);
  const func = entry.slice(cut + 1);
  const file = join(directory, fileName);
  if (fileName === "" || !existsSync(file)) throw new MigrationError(`${entry}: the script ${file} does not exist`);
  // the version query makes an edited file load again: ESM caches modules by URL
  const url = `${pathToFileURL(file).href}?v=${statSync(file).mtimeMs}`;
  let module: Record<string, unknown>;
  try {
    module = (await import(url)) as Record<string, unknown>;
  } catch (e) {
    if ((e as { code?: string }).code === "ERR_UNKNOWN_FILE_EXTENSION") {
      throw new MigrationError(`${file}: this runtime does not load TypeScript; use Node 22.18+ (type stripping) or Bun`);
    }
    throw e;
  }
  const run = module[func];
  if (typeof run !== "function") throw new MigrationError(`${entry}: needs \`export async function ${func}(db)\``);
  return run as Code;
}

/** Whether a migration of `directory` has a `run_code` operation. */
export function hasCode(directory: string): boolean {
  return call(() => native().listMigrations(directory)).some(([, path]) => readFileSync(path!, "utf8").includes('"run_code"'));
}

interface Group {
  readonly atomic: boolean;
  readonly items: readonly { readonly first: number; readonly last: number; readonly entry: string | null }[];
}

/** A migration plan with the code of its `run_code` operations, loaded before anything runs. */
interface Loaded {
  readonly plan: NativeMigrationPlan;
  readonly groups: readonly Group[];
  readonly code: Map<string, Code>; // "group:item" -> function
}

async function loadPlans(directory: string, plans: readonly NativeMigrationPlan[]): Promise<Loaded[]> {
  const out: Loaded[] = [];
  for (const plan of plans) {
    const groups = JSON.parse(plan.groups) as Group[];
    const code = new Map<string, Code>();
    for (const [k, g] of groups.entries()) {
      for (const [i, item] of g.items.entries()) {
        if (item.entry !== null) code.set(`${k}:${i}`, await loadCode(directory, item.entry));
      }
    }
    out.push({ plan, groups, code });
  }
  return out;
}

/** @internal `npx monorm migrate` for a directory with `run_code` operations: applies with
 * `Migrator.upgrade()`. The scripts of the pending migrations are imported first, so the models
 * they import are in the default registry when the database connects. */
export async function migrateCommand(
  schema: string,
  directory: string,
  url: string,
  target: string | null,
  allowDestructive = false,
  ops: string | null = null,
): Promise<number> {
  const dialect = url.startsWith("sqlite:") ? "sqlite" : undefined;
  const probe = await connect(url, { maxConnections: 1, default: false, registry: emptyRegistry(dialect) });
  let loaded: Loaded[];
  try {
    loaded = await new Migrator(probe, new Migrations(directory, schema)).loadUp(target, allowDestructive, ops);
  } finally {
    await probe.close();
  }
  if ([...defaultRegistry].length === 0 && existsSync(schema)) load(schema);
  // scripts that use db.execute only, and no schema file (e.g. a deploy image)
  const registry = [...defaultRegistry].length === 0 ? emptyRegistry(dialect) : defaultRegistry;
  const db = await connect(url, { maxConnections: 2, registry });
  try {
    const done = await new Migrator(db, new Migrations(directory, schema)).run(loaded, allowDestructive);
    for (const m of done) process.stdout.write(`Applied ${m.name}\n`);
    if (done.length === 0) process.stdout.write("Nothing to apply.\n");
  } finally {
    await db.close();
  }
  return 0;
}

function emptyRegistry(dialect: string | undefined): Registry {
  const registry = new Registry();
  if (dialect !== undefined) registry.addExtra({ dialect, models: [] });
  return registry;
}

export interface Status {
  readonly migration: Migration;
  readonly applied: boolean;
  readonly appliedAt: string | null;
  /** The operations done and the total of a migration that stopped part way, else `null`. */
  readonly partial: { readonly done: number; readonly total: number } | null;
}

/**
 * Applies and reverts the migrations of a directory on a database. A migration runs in one
 * transaction by default (on a connection of the pool, not in the caller's transaction) together
 * with its `monorm_migrations` row, under an advisory lock: a failing migration leaves the
 * database at the previous one, and concurrent migrators apply each migration once. A `commit`
 * operation ends a transaction early. An `"atomic": false` operation runs alone, outside a
 * transaction; a failure there leaves the migration partial (see `ops`).
 */
export class Migrator {
  constructor(
    readonly db: Database,
    readonly migrations: Migrations,
  ) {}

  private get dir(): string {
    return this.migrations.directory;
  }

  private migration(name: string): Migration {
    return new Migration(name, join(this.dir, `${name}.json`));
  }

  async status(): Promise<Status[]> {
    const json = await wait(() => this.db.engine.migrationStatus(this.dir));
    const rows = JSON.parse(json) as { name: string; path: string; applied: boolean; appliedAt: string | null; partial: { done: number; total: number } | null }[];
    return rows.map((r) => ({ migration: new Migration(r.name, r.path), applied: r.applied, appliedAt: r.appliedAt, partial: r.partial }));
  }

  /**
   * Applies pending migrations up to and including `target` (default: all).
   *
   * A `run_code` operation names `file:function` (`export async function function(db)`). It runs between
   * the schema operations in the same transaction and under the same lock; queries on `db` inside it go
   * into that transaction. An error rolls back its group and the record.
   *
   * An operation that drops a table or a column, or changes a column type so that data can be
   * lost, throws a `MigrationError` when the table has rows, unless `{ allowDestructive: true }`.
   * A missing or empty table passes.
   *
   * `ops: "2-4"` (or `"3-"`) runs only those operations (numbered from 1) of the one migration
   * `target`; it continues a partial migration.
   */
  async upgrade(target?: string, options: { readonly allowDestructive?: boolean; readonly ops?: string } = {}): Promise<Migration[]> {
    const allow = options.allowDestructive ?? false;
    return this.run(await this.loadUp(target ?? null, allow, options.ops ?? null), allow);
  }

  /** @internal The plans of the pending migrations with their loaded code; refuses before anything runs. */
  async loadUp(target: string | null, allowDestructive: boolean, ops: string | null): Promise<Loaded[]> {
    const plans = await wait(() => this.db.engine.migrationPlanUp(this.dir, target, allowDestructive, ops));
    return loadPlans(this.dir, plans);
  }

  /** @internal */
  async run(loaded: readonly Loaded[], allowDestructive = false): Promise<Migration[]> {
    const done: Migration[] = [];
    for (const item of loaded) {
      if (await this.runPlan(item, allowDestructive)) done.push(this.migration(item.plan.name));
    }
    return done;
  }

  /** Runs the groups of one plan; `false` when a concurrent migrator did the work. */
  private async runPlan(item: Loaded, allowDestructive: boolean): Promise<boolean> {
    const engine = this.db.engine;
    const { plan, groups } = item;
    if (groups.length === 0) {
      await wait(() => engine.migrationRecordEmpty(plan));
      return true;
    }
    for (const [k, group] of groups.entries()) {
      if (!group.atomic) {
        if (!(await wait(() => engine.migrationRunOutside(plan, k, allowDestructive)))) return false;
        continue;
      }
      const tx = await wait(() => engine.migrationBeginGroup(plan, k, allowDestructive));
      if (tx === null) return false;
      let after: (() => unknown)[];
      try {
        after = await inTransaction(this.db, tx, async () => {
          for (const i of group.items.keys()) {
            const run = item.code.get(`${k}:${i}`);
            if (run === undefined) await wait(() => engine.migrationRunItem(tx, plan, k, i));
            else await run(this.db);
          }
        });
      } catch (e) {
        await wait(() => tx.rollback()).catch(() => undefined);
        throw e;
      }
      await restoreCause(wait(() => engine.migrationFinishGroup(tx, plan, k)), tx);
      for (const fn of after) await fn();
    }
    return true;
  }

  /** Compares the database with the schema the migrations reach. That schema is created in a
   * shadow (a Postgres schema in a transaction that is rolled back, or an in-memory SQLite
   * database) and read back, so both sides use the database's text. */
  async drift(): Promise<Drift> {
    return JSON.parse(await wait(() => this.db.engine.migrationDrift(this.dir))) as Drift;
  }

  /** Marks the first migration as applied and does not run it, for a database that
   * already has the schema (after `pull()`). Writes the first migration from the schema
   * when the directory has none. Fails once any migration is applied. */
  async baseline(): Promise<Migration> {
    if (this.migrations.all().length === 0) {
      await this.status(); // fails first if the database has applied migrations
      this.migrations.write(undefined, false, true);
    }
    const name = await wait(() => this.db.engine.migrateBaseline(this.dir));
    return this.migration(name);
  }

  /** Reverts the last `steps` applied migrations (default 1), or every one after `target`
   * (`"zero"` reverts all). Operations revert from the last to the first. A baselined migration is
   * refused unless `force`: reverting it would drop the live tables. Every reverse is resolved
   * before the first change: a `run_sql` without `reverse_sql` or a `run_code` without `reverse`
   * is refused with its operation number.
   *
   * `migration` with `ops: "2-4"` reverts only those operations of that one migration. */
  async downgrade(
    options: {
      readonly steps?: number;
      readonly target?: string;
      readonly force?: boolean;
      readonly allowDestructive?: boolean;
      readonly migration?: string;
      readonly ops?: string;
    } = {},
  ): Promise<Migration[]> {
    const steps = Math.max(0, options.steps ?? 1);
    const allow = options.allowDestructive ?? false;
    const slice = options.migration !== undefined || options.ops !== undefined;
    const plans = await wait(() =>
      this.db.engine.migrationPlanDown(this.dir, steps, options.target ?? null, options.force ?? false, allow, slice ? (options.migration ?? "") : null, slice ? (options.ops ?? "") : null),
    );
    return this.run(await loadPlans(this.dir, plans), allow);
  }
}
