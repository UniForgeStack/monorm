/** Build-selected reference loading adapter. Baseline profiles do not load this module. */
import { clearAbsent, isAbsent } from "./reference-absence.js";
import { active as debugging, relationLoad } from "./debug.js";
import { key } from "./value-key.js";
import { IntegrityError, NotLoaded } from "./errors.js";
import type { Database } from "./db.js";
import type { ModelMeta, Row } from "./model.js";
import type { RelationMeta } from "./meta.js";

// Read at call time: define() sets ModelMeta.filtered after the getters exist, so do not hoist it.
function targetFiltered(meta: ModelMeta, target: string): boolean {
  return meta.registry.get(target).filtered;
}

type Host = {
  DB: symbol; RELATED: symbol; INTERNAL: symbol;
  resolve(db: Database | undefined): Database;
  related(row: object): Record<string, unknown>;
  instanceUpdate(this: Row, values: object): Promise<void>;
};
const MISSING = Symbol("orm.missingReferenceKey");
function peekField(row: Row, name: string, internal: symbol): unknown {
  if (Object.hasOwn(row, name)) return row[name];
  const values = row[internal] as Record<string, unknown> | undefined;
  return values && Object.hasOwn(values, name) ? values[name] : MISSING;
}
/** One field's value, or an array of the values of several fields (null when a part is null). */
function peekKey(row: Row, names: readonly string[], internal: symbol): unknown {
  if (names.length === 1) return peekField(row, names[0]!, internal);
  const parts = names.map((n) => peekField(row, n, internal));
  if (parts.includes(MISSING)) return MISSING;
  return parts.some((v) => v === null || v === undefined) ? null : parts;
}
function sameKey(a: unknown, b: unknown): boolean {
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((v, i) => key(v) === key(b[i]));
  return key(a) === key(b);
}
export function keyValue(row: Row, names: readonly string[], internal: symbol): unknown {
  const value = peekKey(row, names, internal);
  if (value === MISSING) throw new NotLoaded(`${names.join(", ")} is not loaded`);
  return value;
}
function fieldNames(meta: ModelMeta, irs: readonly string[]): string[] {
  return irs.map((ir) => meta.fieldByIr.get(ir)!.name);
}

const REFERENCE_STATE: unique symbol = Symbol("orm.referenceState");
type ReferenceLoad = { context: readonly unknown[]; promise: Promise<unknown> };
type ReferenceState = { pending: Map<string, ReferenceLoad>; tokens: Map<string, object>; keys: Map<string, unknown> };
function referenceState(row: Row): ReferenceState {
  return ((row as ReferenceRow)[REFERENCE_STATE] ??= { pending: new Map(), tokens: new Map(), keys: new Map() });
}

type ReferenceRow = Row & { [REFERENCE_STATE]?: ReferenceState };
/** One query per relation/key/context; successful loads enter the ordinary cache. */
export async function loadReference(owner: Row, meta: ModelMeta, r: RelationMeta, reload: boolean, host: Host): Promise<unknown> {
  const from = fieldNames(meta, r.from);
  const key = keyValue(owner, from, host.INTERNAL);
  if (!reload) {
    try { return owner[r.name]; }
    catch (error) { if (!(error instanceof NotLoaded)) throw error; }
  }
  const state = referenceState(owner);
  const db = host.resolve(owner[host.DB] as Database | undefined);
  const tx = db.tx();
  const target = meta.registry.get(r.target);
  const to = r.to.map((ir) => target.fieldByIr.get(ir)!);
  const parts = to.length === 1 ? [key] : key as unknown[];
  const fetch = async (): Promise<unknown> => {
    const query = target.objects.using(db).filter(...to.map((f, i) => target.column(f).eq(parts[i] as never) as never));
    const filtered = query.readDefaults().filtered;
    const value = key === null ? null : await query.first();
    if (value === null && r.kind === "belongsTo" && r.from.every((ir) => !meta.fieldByIr.get(ir)!.nullable) && !filtered) {
      throw new IntegrityError(`${meta.name}.${r.name}: required target ${r.target} is missing`);
    }
    return value;
  };
  const load = () => coalescedLoad(owner, r.name, [...(key === null ? [key] : parts), db, tx], fetch, (value) => {
    if (sameKey(keyValue(owner, from, host.INTERNAL), key)) {
      clearAbsent(owner, r.name);
      host.related(owner)[r.name] = value;
      state.keys.set(r.name, key);
    }
  });
  // The loader may share one running fetch, so the call site is captured here.
  return debugging() ? relationLoad(meta.name, r.name, "load", load) : load();
}

/** Internal seam: caller supplies resolved dependency keys plus database/transaction context. */
export function coalescedLoad(owner: Row, name: string, context: readonly unknown[], fetch: () => Promise<unknown>, publish: (value: unknown) => void): Promise<unknown> {
  const state = referenceState(owner);
  const running = state.pending.get(name);
  if (running && running.context.length === context.length && running.context.every((value, i) => value === context[i])) return running.promise;
  const token = {};
  state.tokens.set(name, token);
  const promise = fetch().then((value) => {
    if (state.tokens.get(name) === token) publish(value);
    return value;
  }).finally(() => {
    if (state.pending.get(name)?.promise === promise) state.pending.delete(name);
  });
  state.pending.set(name, { context, promise });
  return promise;
}

export function invalidateReference(owner: Row, name: string): void {
  clearAbsent(owner, name);
  const state = (owner as ReferenceRow)[REFERENCE_STATE];
  state?.keys.delete(name);
  state?.tokens.delete(name);
  state?.pending.delete(name);
}

export function getter(meta: ModelMeta, r: RelationMeta, baseline: (this: Row) => unknown, internal: symbol, related: symbol): (this: Row) => unknown {
  if (r.kind !== "belongsTo") return baseline;
  const from = fieldNames(meta, r.from);
  const nullable = r.from.some((ir) => meta.fieldByIr.get(ir)!.nullable);
  return function (this: Row): unknown {
    // The target may be declared after this model (or be this model itself).
    const to = fieldNames(meta.registry.get(r.target), r.to);
    const key = keyValue(this, from, internal);
    const loaded = this[related] as Record<string, unknown> | undefined;
    if (loaded && Object.hasOwn(loaded, r.name)) {
      const value = loaded[r.name] as Row | null;
      const state = (this as ReferenceRow)[REFERENCE_STATE];
      if (value === null && (key === null || targetFiltered(meta, r.target) || (nullable && !state?.keys.has(r.name)) || (state?.keys.has(r.name) && sameKey(state.keys.get(r.name), key)))) return null;
      if (value !== null) {
        const targetKey = peekKey(value, to, internal);
        if (sameKey(targetKey, key) || (targetKey === MISSING && state?.keys.has(r.name) && sameKey(state.keys.get(r.name), key))) return value;
      }
    } else if (key === null) return null;
    const dangling = (loaded && Object.hasOwn(loaded, r.name) && loaded[r.name] === null) || isAbsent(this, r.name, from.length > 1 && key !== null ? key as unknown[] : [key]);
    throw new NotLoaded(`${meta.name}.${r.name} is not loaded${dangling ? ": the key is set and no row of the target has it" : ""}; use await obj.load(${meta.name}.${r.name})`);
  };
}

export function install(meta: ModelMeta, proto: Record<string, unknown>, host: Host): void {
  const refs = [...meta.relations.values()].filter((r) => r.kind === "belongsTo" || r.kind === "hasOne");
  if (!refs.length) return;
  const dependencies = refs.map((relation) => ({ relation, source: fieldNames(meta, relation.from) }));
  const invalidate = (row: Row, before: Map<RelationMeta, unknown>): void => {
    for (const { relation: r, source: from } of dependencies) {
      if (sameKey(before.get(r), peekKey(row, from, host.INTERNAL))) continue;
      const loaded = row[host.RELATED] as Record<string, unknown> | undefined;
      if (loaded) delete loaded[r.name];
      invalidateReference(row, r.name);
    }
  };
  const keys = (row: Row): Map<RelationMeta, unknown> => new Map(dependencies.map(({ relation, source }) => [relation, peekKey(row, source, host.INTERNAL)]));
  Object.defineProperty(proto, "update", { value: async function (this: Row, values: object) {
    const before = keys(this);
    await host.instanceUpdate.call(this, values);
    invalidate(this, before);
  }});
}
