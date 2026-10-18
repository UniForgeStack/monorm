/** Additive instance loading through the queryset load vocabulary. */
import { clearAbsent, copyAbsent, mergeAbsent } from "./reference-absence.js";
import { Column, Labeled, PATH, RelationPath } from "./expr.js";
import { NotLoaded } from "./errors.js";
import { DB, INTERNAL, RELATED, fieldValue, type ModelMeta, type Row } from "./model.js";
import { prefetch, QuerySet, type LoadItem, type LoadEntry } from "./query.js";
import type { Hop, ModelSpec } from "./meta.js";

function metaOf(row: Row): ModelMeta { return (row.constructor as unknown as { meta: ModelMeta }).meta; }
function loaded(row: Row, path: readonly string[], field?: string, label?: string, query?: Pick<QuerySet<ModelSpec>, "state">): boolean {
  if (!path.length && query) {
    const fields = query.state.modelFields ?? metaOf(row).fieldList.map((f) => f.ir);
    return fields.every((n) => Object.hasOwn(row, metaOf(row).fieldByIr.get(n)!.name)) && (query.state.loads ?? []).every((i) => itemLoaded(row, i));
  }
  if (!path.length) return field !== undefined ? Object.hasOwn(row, field) : metaOf(row).fieldList.every((f) => Object.hasOwn(row, f.name));
  const [name, ...rest] = path;
  const relation = metaOf(row).relationByIr.get(name!)!;
  const attr = !rest.length && label !== undefined ? label : relation.name;
  let value: unknown;
  try {
    value = row[attr];
    if (!rest.length && label !== undefined && !Object.hasOwn(row, attr)) return false;
    if (attr === relation.name && (relation.kind === "hasMany" || relation.kind === "manyToMany")) value = (value as { cached: unknown }).cached;
  } catch (e) { if (e instanceof NotLoaded) return false; throw e; }
  return (Array.isArray(value) ? value : [value]).every((v) => v === null || (v !== undefined && loaded(v as Row, rest, field, label, query)));
}

function itemPath(item: LoadEntry | LoadItem<ModelSpec>): Column<unknown, string> | RelationPath<ModelSpec, string, readonly Hop[]> {
  if (item instanceof QuerySet) return item.state.via!;
  if (item instanceof Column || item instanceof RelationPath) return item;
  throw new TypeError("load() takes columns, relation paths or relation query sets");
}

function itemLoaded(row: Row, item: LoadEntry | LoadItem<ModelSpec>): boolean {
  if (item instanceof Labeled) return Object.hasOwn(row, item.name);
  const path = itemPath(item);
  const hops = path instanceof Column ? path.path : path[PATH].path;
  return loaded(row, hops, path instanceof Column ? path.field.name : undefined,
    item instanceof QuerySet ? item.state.label : undefined, item instanceof QuerySet ? item : undefined);
}

function isRow(value: unknown): value is Row {
  return value !== null && typeof value === "object" && (value.constructor as { meta?: ModelMeta }).meta !== undefined;
}
function mergeValue(old: unknown, fresh: unknown, reload: boolean, invalidate?: (row: Row, name: string) => void, seen = new Map<Row, Set<Row>>()): unknown {
  if (isRow(old) && isRow(fresh) && metaOf(old) === metaOf(fresh) && old["pk"] === fresh["pk"]) { merge(old, fresh, reload, invalidate, seen); return old; }
  if (Array.isArray(old) && Array.isArray(fresh)) {
    const rows = reload ? fresh : old;
    const matches = new Map<ModelMeta, Map<unknown, Row>>();
    for (const v of reload ? old : fresh) if (isRow(v)) {
      const meta = metaOf(v);
      if (!matches.has(meta)) matches.set(meta, new Map());
      matches.get(meta)!.set(v["pk"], v);
    }
    return rows.map((v) => {
      const prior = isRow(v) ? matches.get(metaOf(v))?.get(v["pk"]) : undefined;
      return prior === undefined ? v : reload ? mergeValue(prior, v, true, invalidate, seen) : mergeValue(v, prior, false, invalidate, seen);
    });
  }
  return fresh;
}
function merge(row: Row, fresh: Row, reload: boolean, invalidate?: (row: Row, name: string) => void, seen = new Map<Row, Set<Row>>()): void {
  if (row === fresh || seen.get(row)?.has(fresh)) return;
  if (!seen.has(row)) seen.set(row, new Set());
  seen.get(row)!.add(fresh);
  const meta = metaOf(row);
  for (const name of Object.keys(fresh)) {
    const old = Object.hasOwn(row, name) ? row[name] : undefined, value = fresh[name];
    if (meta.fields.has(name) && !reload && Object.hasOwn(row, name)) continue;
    if (reload || !Object.hasOwn(row, name) || isRow(old) || Array.isArray(old)) {
      let before: unknown = old;
      if (meta.fields.has(name)) {
        try { before = fieldValue(row, name); }
        catch (e) { if (!(e instanceof NotLoaded)) throw e; }
      }
      if (meta.fields.has(name) && before !== value) {
        for (const r of meta.relations.values()) if (r.from.some((ir) => meta.fieldByIr.get(ir)!.name === name)) {
          if (row[RELATED]) delete row[RELATED][r.name];
          invalidate?.(row, r.name);
        }
      }
      row[name] = mergeValue(old, value, reload, invalidate, seen);
    }
  }
  if (fresh[INTERNAL]) {
    const internal = (row[INTERNAL] ??= {}) as Record<string, unknown>;
    for (const [name, value] of Object.entries(fresh[INTERNAL] as object)) if (!Object.hasOwn(internal, name)) internal[name] = value;
  }
  if (meta.fieldList.every((f) => Object.hasOwn(row, f.name))) delete row[INTERNAL];
  mergeAbsent(row, fresh);
  const cache = (row[RELATED] ??= {});
  for (const [name, value] of Object.entries(fresh[RELATED] ?? {})) {
    clearAbsent(row, name);
    cache[name] = mergeValue(cache[name], value, reload, invalidate, seen);
  }
}

export async function loadInstance(
  owner: Row, items: readonly LoadItem<ModelSpec>[], reload: boolean,
  loadReference?: (owner: Row, name: string, reload: boolean) => Promise<unknown>,
  invalidate?: (row: Row, name: string) => void,
): Promise<void> {
  const meta = metaOf(owner);
  const requested = items.length ? items : meta.fieldList.map((f) => meta.column(f));
  // Validate all arguments and relation query options before any I/O.
  const query = (meta.objects.withoutDefaults().load as (...items: readonly LoadItem<ModelSpec>[]) => QuerySet<ModelSpec>)(...requested);
  query.inspect();
  const own = requested.filter((item): item is Column<unknown, string> | Labeled<unknown> =>
    (item instanceof Column && !item.path.length && (reload || !Object.hasOwn(owner, item.field.name)))
    || (item instanceof Labeled && (reload || !Object.hasOwn(owner, item.name))));
  if (own.length) {
    // a labeled aggregate alone must not read every column
    if (!own.some((i) => i instanceof Column)) own.unshift(...meta.key.map((f) => meta.column(f)));
    const rowQuery = meta.objects.withoutDefaults().using(owner[DB]).filter(meta.keyMatch(meta.key, owner["pk"]) as never);
    const fresh = await (rowQuery.load as (...items: (Column<unknown, string> | Labeled<unknown>)[]) => QuerySet<ModelSpec>)(...own).require().get() as Row;
    merge(owner, fresh, reload, invalidate);
  }
  const relations: LoadItem<ModelSpec>[] = [];
  for (const item of requested) {
    if ((item instanceof Column && !item.path.length) || item instanceof Labeled) continue;
    const path = itemPath(item);
    const hops = path instanceof Column ? path.path : path[PATH].path;
    if (!reload && itemLoaded(owner, item)) continue;
    if (loadReference && item instanceof RelationPath && hops.length === 1) {
      const relation = meta.relationByIr.get(hops[0]!)!;
      if (relation.kind === "belongsTo" || relation.kind === "hasOne") {
        let cached: unknown;
        try { cached = owner[relation.name]; } catch (e) { if (!(e instanceof NotLoaded)) throw e; }
        if (reload || cached == null) { await loadReference(owner, relation.name, reload); continue; }
      }
    }
    relations.push(item);
  }
  if (relations.length) {
    const staged = Object.assign(Object.create(Object.getPrototypeOf(owner)) as Row, owner);
    staged[RELATED] = { ...owner[RELATED] };
    const keys = new Map<string, unknown>();
    for (const item of relations) {
      const path = itemPath(item);
      const hops = path instanceof Column ? path.path : path[PATH].path;
      for (const ir of meta.relationByIr.get(hops[0]!)!.from) {
        const source = meta.fieldByIr.get(ir)!.name;
        keys.set(source, fieldValue(owner, source));
      }
    }
    // A load that leaves a name unset must not keep the old cached value.
    for (const item of relations) {
      const path = itemPath(item);
      const hops = path instanceof Column ? path.path : path[PATH].path;
      const label = item instanceof QuerySet ? item.state.label : undefined;
      if (label && hops.length === 1) delete staged[label];
      else delete staged[RELATED]![meta.relationByIr.get(hops[0]!)!.name];
    }
    await prefetch([staged], ...relations);
    const fresh = Object.create(Object.getPrototypeOf(owner)) as Row;
    fresh[RELATED] = {};
    for (const item of relations) {
      const path = itemPath(item);
      const hops = path instanceof Column ? path.path : path[PATH].path;
      const sources = meta.relationByIr.get(hops[0]!)!.from.map((ir) => meta.fieldByIr.get(ir)!.name);
      try { if (sources.some((source) => fieldValue(owner, source) !== keys.get(source))) continue; }
      catch (e) { if (e instanceof NotLoaded) continue; throw e; }
      const label = item instanceof QuerySet ? item.state.label : undefined;
      if (label && hops.length === 1) {
        if (Object.hasOwn(staged, label)) fresh[label] = staged[label];
        else delete owner[label];
      } else {
        const name = meta.relationByIr.get(hops[0]!)!.name;
        if (Object.hasOwn(staged[RELATED]!, name)) fresh[RELATED][name] = staged[RELATED]![name];
        else delete owner[RELATED]?.[name];
        copyAbsent(owner, staged, name);
      }
    }
    merge(owner, fresh, reload, invalidate);
  }
}
