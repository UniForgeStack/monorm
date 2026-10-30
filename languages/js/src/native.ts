/**
 * The Rust engine (`languages/js/native`), loaded once. Internal: use the package API.
 *
 * The package holds one addon per platform, `prebuilt/<platform>/monorm.node`, and the loader opens the one
 * of the running platform. A source checkout uses `monorm.node` next to `package.json` (`npm run build:native`).
 * `MONORM_NATIVE` names a file that replaces both.
 */

import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

import { packageRoot } from "./builtin.js";
import { Decimal } from "./decimal.js";
import { fromNative } from "./errors.js";

/** Rows of one statement: `n` rows of `width` cells, row after row. The engine hands the
 * cells over packed; `cellsOf` (`rows.ts`) decodes them into `values`. */
export interface NativeRows {
  readonly n: number;
  readonly width: number;
  readonly packed: Buffer;
  values?: unknown[];
}

/** How rows become instances: the root model's fields first, then each
 * `select_related` object (`parent` -1: attached to the root object). */
export type NativeShape = readonly { field: number; slot: number; public: boolean }[] | null;

export interface NativeInstances {
  readonly shape?: NativeShape;
  readonly model: string;
  readonly joins: readonly { parent: number; attr: string; model: string; start: number; pk: number; filtered: boolean; unsetWhenAbsent: boolean; shape?: NativeShape }[];
  /** Labeled expressions of `load()`: the attribute and the column that holds the value. */
  readonly annotations?: readonly { name: string; pos: number }[];
}

/** `select()` rows: per item, the width of a model instance or -1 for one value. */
export interface NativeColumns {
  readonly model: string | null;
  readonly items: readonly number[];
}

export interface NativePrefetch {
  readonly attr: string;
  readonly many: boolean;
  readonly unsetWhenAbsent: boolean;
  readonly keyPos: readonly number[];
  readonly childKeyPos: readonly number[];
  readonly back: string | null;
  /** The `select()` keys of the related rows (`output` is `NativeColumns`). */
  readonly names: readonly string[];
  readonly rows: NativeRows;
  readonly output: NativeInstances | NativeColumns;
  readonly children: readonly NativePrefetch[];
}

export interface NativeSelect {
  readonly rows: NativeRows;
  readonly output: NativeInstances | NativeColumns;
  readonly prefetched: readonly NativePrefetch[];
}

/** Rows a write returned (`RETURNING`). */
export interface NativeReturned {
  readonly shape?: NativeShape;
  readonly model: string;
  readonly rows: NativeRows;
}

export interface NativeTransaction {
  commit(): Promise<void>;
  rollback(): Promise<void>;
  markFailed(message: string): void;
}

export interface NativeSessionLock {
  release(trace?: NativeTrace | null): Promise<void>;
}

export interface NativeInspector {
  inspect(opJson: string, params: unknown[]): string;
  inspectInsert(model: string, fields: string[], rows: unknown[][], options: string, params: unknown[], batchSize: number | null, returning: boolean): string;
  inspectUpdateMany(model: string, fields: string[], rows: unknown[][], filtersJson: string, params: unknown[], batchSize: number | null, returning: boolean, withoutDefaults: string): string;
}

export interface NativeSchema extends NativeInspector {
  /** The static read flags of each model, JSON `{model: {filtered, fields, related}}`. */
  readFlags(): string;
  /** The engine's default order of a read, JSON `[{field, desc, nulls}]`. */
  effectiveOrder(selectJson: string): string;
  currentFilter(selectJson: string): string;
  validateInsert(model: string, fields: string[], rows: unknown[][], allowed: readonly string[]): void;
  uniqueRowUpdate(opJson: string, params: unknown[], allowed: readonly string[]): boolean;
  sql(opJson: string, params: unknown[]): string;
  statement(opJson: string, params: unknown[]): string;
  updateManySql(
    model: string,
    fields: string[],
    rows: unknown[][],
    filtersJson: string,
    params: unknown[],
    batchSize: number | null | undefined,
    disable: string[],
  ): string[];
  ddl(): string[];
  snapshot(): string;
  migration(previous?: string | null): string;
  planMigration(dir: string): string;
  makeMigration(dir: string, name?: string | null, empty?: boolean | null): string | null;
}

/** One traced statement: `start` in Unix milliseconds, `duration` in milliseconds. */
export interface NativeQueryEvent {
  readonly sql: string;
  readonly start: number;
  readonly duration: number;
  readonly rows: number;
  readonly error?: string | null;
}

export interface NativeTrace {
  take(): NativeQueryEvent[];
}

/** The prefetched rows for parent key rows (`prefetch()`), and those parent rows. */
export interface NativePrefetched {
  readonly rows: NativeRows;
  readonly prefetched: readonly NativePrefetch[];
}

export interface NativeEngine extends NativeInspector {
  run(opJson: string, params: unknown[], tx: NativeTransaction | null, allowed?: readonly string[], trace?: NativeTrace | null): Promise<unknown>;
  prefetch(opJson: string, params: unknown[], keys: string[], rows: unknown[][], tx: NativeTransaction | null, trace?: NativeTrace | null): Promise<NativePrefetched>;
  insert(
    model: string,
    fields: string[],
    rows: unknown[][],
    conflict: string[] | null,
    update: string[] | null,
    set: string | null,
    params: unknown[],
    tx: NativeTransaction | null,
    allowed?: readonly string[],
    batchSize?: number | null,
    conflictIndexes?: readonly string[] | null,
    trace?: NativeTrace | null,
    returning?: boolean,
    scope?: string | null,
    withoutDefaults?: string | null,
  ): Promise<unknown>;
  copyInsert(model: string, fields: string[], rows: unknown[][], tx: NativeTransaction | null, allowed?: readonly string[], trace?: NativeTrace | null): Promise<number>;
  attach(model: string, parentId: unknown, fields: string[], rows: unknown[][], tx: NativeTransaction | null, allowed?: readonly string[], trace?: NativeTrace | null): Promise<unknown>;
  updateMany(
    model: string,
    fields: string[],
    rows: unknown[][],
    filtersJson: string,
    params: unknown[],
    returning: boolean,
    batchSize: number | null,
    tx: NativeTransaction | null,
    withoutDefaults: string,
    allowed?: readonly string[],
    trace?: NativeTrace | null,
  ): Promise<unknown>;
  begin(tx: NativeTransaction | null): Promise<NativeTransaction>;
  advisoryLock(key: string, name: Buffer | null, exclusive: boolean, nowait: boolean, tx: NativeTransaction, trace?: NativeTrace | null): Promise<boolean>;
  withSettings(names: string[], values: string[]): NativeEngine;
  sessionLock(key: string, name: Buffer | null, exclusive: boolean, nowait: boolean, timeoutMs: number | null, trace?: NativeTrace | null): Promise<NativeSessionLock | null>;
  execute(sql: string, tx: NativeTransaction | null, trace?: NativeTrace | null): Promise<number>;
  explain(opJson: string, params: unknown[], analyze: boolean, tx: NativeTransaction | null, trace?: NativeTrace | null): Promise<string>;
  fetch(sql: string, params: unknown[], tx: NativeTransaction | null, trace?: NativeTrace | null): Promise<Record<string, unknown>[]>;
  fetchText(sql: string, tx: NativeTransaction | null): Promise<(string | null)[][]>;
  executeScript(statements: string[], tx: NativeTransaction | null): Promise<void>;
  migrationStatus(dir: string): Promise<string>;
  migrateUp(dir: string, target: string | null): Promise<string[]>;
  migrateDown(dir: string, steps: number, target: string | null, force: boolean): Promise<string[]>;
  pullSchema(): Promise<string>;
  migrationDrift(dir: string): Promise<string>;
  migrateBaseline(dir: string): Promise<string>;
  migrationPending(dir: string, target: string | null): Promise<string[][]>;
  migrationBegin(name: string, path: string): Promise<NativeTransaction | null>;
  migrationFinish(tx: NativeTransaction, name: string, path: string): Promise<void>;
  createTables(): Promise<void>;
  dropTables(): Promise<void>;
  close(): Promise<void>;
}

interface Addon {
  /** Without `packages` the hook bindings stay unresolved: migrations and code generation need no library. */
  Schema: new (schemaJson: string, packages?: Record<string, string>) => NativeSchema;
  Trace: new () => NativeTrace;
  connect(url: string, schema: NativeSchema, maxConnections: number, disable: string[]): Promise<NativeEngine>;
  prepareSchema(schemaJson: string, contextJson?: string): string;
  nativeArtifact(): string;
  profileMetadata(): string;
  compileSchema(source: string, path?: string | null, extensions?: Record<string, string> | null): string;
  compileSchemaFile(path: string, apps?: Record<string, string> | null, extensions?: Record<string, string> | null): string;
  generateTypescript(path: string, runtime?: string | null, querySets?: Record<string, string> | null, modelMethods?: Record<string, string> | null, apps?: Record<string, string> | null, extensions?: Record<string, string> | null): string;
  setDecimalClass(ctor: unknown): void;
  cli(argv: string[]): Promise<number>;
  cliSchema(argv: string[]): string;
  cliMigrateArgs(argv: string[]): (string | null)[] | null;
  listMigrations(dir: string): string[][];
  findMigration(dir: string, name: string): string[];
}

/** The platforms of the bundled libraries: `${process.platform}-${process.arch}`. */
export const PLATFORMS = ["darwin-arm64", "linux-x64", "win32-x64"] as const;

/** The artifact's embedded Cargo features, rustc, target, profile and git revision. */
function buildRecord(build: unknown): boolean {
  if (typeof build !== "object" || build === null || Array.isArray(build)) return false;
  const record = build as Record<string, unknown>;
  return Array.isArray(record["features"]) && record["features"].every(f => typeof f === "string") &&
    ["rustc", "target", "profile", "revision"].every(key => typeof record[key] === "string");
}

function validate(addon: Addon): void {
  if (typeof addon.profileMetadata !== "function") throw new Error("incompatible monorm native artifact; rebuild for metadata ABI 1");
  const meta = JSON.parse(addon.profileMetadata()) as {
    abi: number; version: string; language: string; profile: string; backends: string[];
    capabilities: Record<string, boolean>; adapters: string[]; build: Record<string, unknown>;
  };
  if (typeof meta !== "object" || meta === null || Array.isArray(meta) || meta.abi !== 1 || meta.version !== "0.1.0" || meta.language !== "node" ||
      !buildRecord(meta.build) ||
      !Array.isArray(meta.backends) || meta.backends.length === 0 ||
      new Set(meta.backends).size !== meta.backends.length || meta.backends.some(b => !["postgres", "sqlite"].includes(b)) ||
      typeof meta.capabilities !== "object" || meta.capabilities === null ||
      Array.isArray(meta.capabilities) || Object.values(meta.capabilities).some(v => typeof v !== "boolean") ||
      !Array.isArray(meta.adapters) || meta.adapters.some(a => typeof a !== "string" || meta.capabilities[a] !== true) ||
      new Set(meta.adapters).size !== meta.adapters.length) {
    throw new Error("incompatible monorm native artifact; rebuild or install matching monorm 0.1.0 packages");
  }
}

/** The addon file of this platform: the bundled one, else the one a source build writes next to `package.json`. */
export function addonPath(platform = `${process.platform}-${process.arch}`, root = packageRoot): string {
  const candidates = [join(root, "prebuilt", platform, "monorm.node"), join(root, "monorm.node")];
  const found = candidates.find(existsSync);
  if (found !== undefined) return found;
  if (!(PLATFORMS as readonly string[]).includes(platform)) {
    throw new Error(`monorm bundles its native library for ${PLATFORMS.join(", ")}, not for ${platform}; build it from source and set MONORM_NATIVE to the file`);
  }
  throw new Error(`the native library of ${platform} is missing (${candidates[0]}); reinstall monorm, or build it from source and set MONORM_NATIVE to the file`);
}

function load(): Addon {
  const addon = createRequire(import.meta.url)(process.env["MONORM_NATIVE"] || addonPath()) as Addon;
  validate(addon);
  addon.setDecimalClass(Decimal);
  return addon;
}

let addon: Addon | undefined;

export function native(): Addon {
  addon ??= load();
  return addon;
}

/** Calls into the addon, turning its errors into the package's error classes. */
export function call<T>(f: () => T): T {
  try {
    return f();
  } catch (e) {
    throw fromNative(e);
  }
}

/** Awaits an addon promise, turning its errors into the package's error classes. */
export async function wait<T>(f: () => Promise<T>): Promise<T> {
  try {
    return await f();
  } catch (e) {
    throw fromNative(e);
  }
}

/** Fixed adapter set. Bind specialized methods at module/definition initialization. */
let adapters: readonly string[] | undefined;
export function nativeAdapters(): readonly string[] {
  if (adapters === undefined) {
    const meta = JSON.parse(native().profileMetadata()) as { adapters: string[]; capabilities: Record<string, boolean> };
    if (!Array.isArray(meta.adapters) || meta.adapters.some(name => meta.capabilities[name] !== true)) {
      throw new Error("native adapter/capability metadata mismatch");
    }
    adapters = Object.freeze([...meta.adapters]);
  }
  return adapters;
}
